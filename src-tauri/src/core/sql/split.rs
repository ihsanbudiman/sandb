//! Splits editor SQL into statements with `sqlparser`, keeping each statement's original text.
//! If the text does not parse, it runs as one statement and the server decides.

use sqlparser::ast::Statement;
use sqlparser::dialect::{Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Token, Tokenizer};

use crate::core::engine::SqlEngine;

pub(crate) fn dialect(engine: SqlEngine) -> Box<dyn Dialect> {
    match engine {
        SqlEngine::Pg => Box::new(PostgreSqlDialect {}),
        SqlEngine::Mysql => Box::new(MySqlDialect {}),
        SqlEngine::Sqlite => Box::new(SQLiteDialect {}),
    }
}

/// MySQL runs the body of `/*! … */` (MariaDB also `/*M! … */`) as SQL. sqlparser's MySQL
/// tokenizer expands `/*!` bodies with spans that point at the comment start, and keeps `/*M!`
/// as a comment, so neither can be split or analysed reliably. Matches inside string literals
/// too, which only costs a false "not a plain read".
pub(crate) fn has_executable_comment(engine: SqlEngine, sql: &str) -> bool {
    engine == SqlEngine::Mysql && (sql.contains("/*!") || sql.contains("/*M!"))
}

/// Statements in order, without the separating semicolons. Empty if there is nothing to run.
pub fn split(engine: SqlEngine, sql: &str) -> Vec<String> {
    let d = dialect(engine);
    let whole = sql.trim();
    let parts = if has_executable_comment(engine, sql) {
        None
    } else {
        split_by_tokens(&*d, sql)
    };
    let Some(parts) = parts else {
        return if whole.is_empty() {
            vec![]
        } else {
            vec![whole.to_string()]
        };
    };
    if parts.len() <= 1 {
        return parts;
    }
    // The token split is only trusted when the parser sees the same number of statements, so
    // a `;` inside a construct the tokenizer cannot see into (e.g. a trigger body) never cuts it.
    match Parser::parse_sql(&*d, sql) {
        Ok(stmts) if stmts.len() == parts.len() => parts,
        _ => vec![whole.to_string()],
    }
}

/// `sql` starts a transaction (`BEGIN`, `START TRANSACTION`) and does not end it. False when the
/// text does not parse. Only a fallback: the server's own answer is preferred.
pub fn leaves_transaction_open(engine: SqlEngine, sql: &str) -> bool {
    let Ok(stmts) = Parser::parse_sql(&*dialect(engine), sql) else {
        return false;
    };
    stmts.iter().fold(false, |open, s| match s {
        Statement::StartTransaction { .. } => true,
        Statement::Commit { .. }
        | Statement::Rollback {
            savepoint: None, ..
        } => false,
        _ => open,
    })
}

/// Byte offset of a 1-based (line, column-in-chars) location.
fn offset(sql: &str, line_starts: &[usize], loc: Location) -> usize {
    let Some(&start) = line_starts.get((loc.line as usize).saturating_sub(1)) else {
        return sql.len();
    };
    sql[start..]
        .char_indices()
        .nth((loc.column as usize).saturating_sub(1))
        .map_or(sql.len(), |(i, _)| start + i)
}

/// Text between top-level semicolons, skipping parts that hold only whitespace or comments.
/// `None` when the text does not tokenize.
fn split_by_tokens(d: &dyn Dialect, sql: &str) -> Option<Vec<String>> {
    let tokens = Tokenizer::new(d, sql)
        .with_unescape(false)
        .tokenize_with_location()
        .ok()?;
    let mut line_starts = vec![0];
    line_starts.extend(sql.match_indices('\n').map(|(i, _)| i + 1));
    let mut parts = Vec::new();
    let mut start = 0;
    let mut has_content = false;
    for t in &tokens {
        match &t.token {
            Token::SemiColon => {
                let end = offset(sql, &line_starts, t.span.start);
                if has_content {
                    parts.push(sql[start..end].trim().to_string());
                }
                start = offset(sql, &line_starts, t.span.end);
                has_content = false;
            }
            Token::Whitespace(_) | Token::EOF => {}
            _ => has_content = true,
        }
    }
    if has_content {
        parts.push(sql[start..].trim().to_string());
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_statement_keeps_text() {
        assert_eq!(split(SqlEngine::Pg, "  SELECT 1;  "), vec!["SELECT 1"]);
        assert_eq!(split(SqlEngine::Pg, "SELECT 1"), vec!["SELECT 1"]);
    }

    #[test]
    fn nothing_to_run() {
        assert!(split(SqlEngine::Pg, "   ").is_empty());
        assert!(split(SqlEngine::Pg, "-- just a comment\n;;").is_empty());
        assert!(split(SqlEngine::Mysql, "/* c */").is_empty());
    }

    #[test]
    fn pg_semicolons_in_strings_comments_and_dollar_quotes() {
        let sql = "SELECT 1;\nSELECT 'a;b';\n-- note; here\nSELECT $$x;y$$ AS z;";
        assert_eq!(
            split(SqlEngine::Pg, sql),
            vec![
                "SELECT 1",
                "SELECT 'a;b'",
                "-- note; here\nSELECT $$x;y$$ AS z"
            ]
        );
    }

    #[test]
    fn pg_set_then_select_and_temp_table() {
        let sql = "SET search_path TO analytics;\nCREATE TEMP TABLE t (id int);\nINSERT INTO t VALUES (1);\nSELECT * FROM t";
        assert_eq!(split(SqlEngine::Pg, sql).len(), 4);
    }

    #[test]
    fn mysql_backticks_and_hash_comments() {
        let sql = "SELECT `a;b` FROM t; # one; two\nSELECT \"x;y\"";
        assert_eq!(
            split(SqlEngine::Mysql, sql),
            vec!["SELECT `a;b` FROM t", "# one; two\nSELECT \"x;y\""]
        );
    }

    #[test]
    fn mysql_executable_comment_is_never_split() {
        // Split by the expanded hint tokens this would cut inside the comment and send
        // `*!; DELETE FROM t */` as its own statement.
        let sql = "SELECT 1 /*!; DELETE FROM t */";
        assert_eq!(split(SqlEngine::Mysql, sql), vec![sql]);
        let sql = "SELECT 1; /*M! SELECT 2 */ SELECT 3";
        assert_eq!(split(SqlEngine::Mysql, sql), vec![sql]);
        // Postgres has no executable comments.
        assert_eq!(split(SqlEngine::Pg, "SELECT 1; /*! x */ SELECT 2").len(), 2);
    }

    #[test]
    fn sqlite_two_statements_with_unicode() {
        let sql = "SELECT '日本;語' AS v;\nSELECT 2";
        assert_eq!(
            split(SqlEngine::Sqlite, sql),
            vec!["SELECT '日本;語' AS v", "SELECT 2"]
        );
    }

    #[test]
    fn unparsable_text_runs_as_one_statement() {
        let sql = "SELEC 1; SELECT 2";
        assert_eq!(split(SqlEngine::Pg, sql), vec!["SELEC 1; SELECT 2"]);
        let unterminated = "SELECT 'abc";
        assert_eq!(split(SqlEngine::Pg, unterminated), vec!["SELECT 'abc"]);
    }

    #[test]
    fn open_transactions() {
        assert!(leaves_transaction_open(
            SqlEngine::Pg,
            "BEGIN; UPDATE t SET a = 1"
        ));
        assert!(leaves_transaction_open(
            SqlEngine::Mysql,
            "START TRANSACTION; UPDATE t SET a = 1"
        ));
        assert!(leaves_transaction_open(
            SqlEngine::Mysql,
            "BEGIN; UPDATE t SET a = 1"
        ));
        assert!(leaves_transaction_open(
            SqlEngine::Sqlite,
            "BEGIN IMMEDIATE; UPDATE t SET a = 1"
        ));
        assert!(!leaves_transaction_open(
            SqlEngine::Pg,
            "BEGIN; UPDATE t SET a = 1; COMMIT"
        ));
        assert!(!leaves_transaction_open(
            SqlEngine::Pg,
            "BEGIN; UPDATE t SET a = 1; ROLLBACK"
        ));
        assert!(!leaves_transaction_open(
            SqlEngine::Pg,
            "BEGIN; UPDATE t SET a = 1; END"
        ));
        assert!(leaves_transaction_open(
            SqlEngine::Pg,
            "BEGIN; SAVEPOINT s; UPDATE t SET a = 1; ROLLBACK TO SAVEPOINT s"
        ));
        assert!(!leaves_transaction_open(SqlEngine::Pg, "UPDATE t SET a = 1"));
    }

    #[test]
    fn pg_do_block_is_one_unsplit_run() {
        // sqlparser has no DO statement, so the whole text goes to the server as one run.
        let sql = "DO $$ BEGIN PERFORM 1; END $$; SELECT 1 AS a; SELECT 'x' AS b, 'y' AS c";
        assert_eq!(split(SqlEngine::Pg, sql), vec![sql]);
    }
}
