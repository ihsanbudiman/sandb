//! Facts about a piece of SQL, collected in one walk over the parsed AST. Facts only: what to
//! allow is decided by a policy over `SqlFacts`, such as [`is_plain_read`] for the editor or
//! `guard::check_ai_read` for AI-written SQL.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use sqlparser::ast::{
    AccessExpr, AlterTableOperation, BinaryOperator, ContextModifier, Expr, Ident, LimitClause,
    ObjectName, ObjectNamePart, Query, Set, SetExpr, Statement, TableAliasColumnDef, TableFactor,
    Value, Visit, Visitor,
};
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};

use crate::core::engine::SqlEngine;
use crate::core::sql::functions;
use crate::core::sql::split::{dialect, has_executable_comment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// SELECT, VALUES, TABLE, or WITH … SELECT.
    Query,
    /// SHOW …, DESCRIBE / DESC t.
    Show,
    Explain {
        analyze: bool,
    },
    Insert,
    Update,
    Delete,
    Merge,
    Transaction,
    Pragma,
    Create,
    Alter,
    Drop,
    Truncate,
    /// RENAME TABLE … (MySQL; PG spells it `ALTER TABLE … RENAME`, which is [`Alter`](Self::Alter)).
    Rename,
    /// Anything else (SET, USE, CALL, COPY, LOCK, GRANT, …).
    Other,
}

impl StatementKind {
    /// CREATE / ALTER / DROP / TRUNCATE / RENAME TABLE: the statement changes the schema, so a
    /// catalog has to reload after it.
    pub fn is_ddl(self) -> bool {
        matches!(
            self,
            Self::Create | Self::Alter | Self::Drop | Self::Truncate | Self::Rename
        )
    }
}

/// How a SQLite `PRAGMA` is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PragmaForm {
    /// `PRAGMA name`
    Read,
    /// `PRAGMA name(arg)`
    Call,
    /// `PRAGMA name = value`
    Assign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pragma {
    /// Lowercase, without a schema prefix.
    pub name: String,
    pub form: PragmaForm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementFacts {
    /// For `WITH … INSERT/UPDATE/DELETE` the kind is the write, not `Query`.
    pub kind: StatementKind,
    /// Upper-case statement name for messages: `SELECT`, `DROP TABLE`, `COPY`, `SET`, ….
    pub label: String,
    /// The object a write or DDL statement targets, as written (e.g. `public.orders`).
    pub target: Option<String>,
    /// UPDATE / DELETE: has a WHERE clause. `None` for other kinds.
    pub has_where: Option<bool>,
    /// ALTER TABLE with at least one DROP operation (column, constraint, index, …).
    pub alter_drops: bool,
    /// EXPLAIN: the kind of the explained statement.
    pub explained: Option<Box<StatementKind>>,
    /// MySQL commits the open transaction before running this statement (DDL, `SET autocommit`,
    /// LOCK/UNLOCK TABLES, CALL, GRANT/REVOKE, ANALYZE, FLUSH, RESET). Always `false` for the
    /// other engines, whose DDL is transactional. MySQL statements the parser cannot read carry
    /// no fact: their text does not parse at all.
    pub implicit_commit: bool,
    pub pragma: Option<Pragma>,
    /// Every function called, anywhere in the statement (subqueries, FROM, EXPLAIN targets):
    /// unquoted name parts lowercased, quoted parts as written, joined with `.`.
    pub functions: BTreeSet<String>,
    /// FOR UPDATE / FOR SHARE / LOCK IN SHARE MODE on any query, subqueries included.
    pub has_locking: bool,
    /// SELECT … INTO (a table or variables).
    pub has_into: bool,
    /// A CTE, subquery or set-operation branch whose body is INSERT / UPDATE / DELETE / MERGE.
    pub has_modifying_cte: bool,
    /// MySQL `@v := …`.
    pub has_variable_assignment: bool,
    /// Every relation a statement reads, as written (`analytics.events`, `pg_catalog.pg_authid`).
    /// Used to refuse reads of privacy-sensitive catalog tables.
    pub tables: BTreeSet<String>,
    /// Aliases of FROM items and CTEs that are not plain tables: function-in-FROM, UNNEST,
    /// VALUES, derived tables, CTEs. `x.f` where `x` names a function's row can be an attribute
    /// call `f(x)`, which the function check would never see.
    pub from_aliases: BTreeMap<String, FromAlias>,
    /// Qualified references `q.f` (and field accesses such as `(g).f`, `a.b.c`), as possible
    /// function calls. PostgreSQL resolves `g.f` to `f(g)` when `g` has no such column.
    pub field_accesses: Vec<FieldAccess>,
    /// The field name of every `AccessExpr::Dot` step in a `CompoundFieldAccess`: PG can run
    /// the step as a one-argument function on the whole root value, and the root here is never a
    /// plain column qualifier.
    pub dot_fields: Vec<String>,
    /// MySQL `SHOW [FULL] PROCESSLIST`: shows what other sessions run.
    pub show_processlist: bool,
    /// The first word after `SHOW` when the statement parses as `Statement::ShowVariable`
    /// (`SHOW ENGINE …`, `SHOW BINLOG EVENTS`, `SHOW ALL`, `SHOW <setting>`): lowercased.
    pub show_variable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromAlias {
    /// A set-returning function in FROM or UNNEST: `x.f` may address the function's row column
    /// (recorded here) — or call `f(x)` on the whole scalar row.
    RowFunction {
        /// Output column names when they can be computed.
        columns: Option<BTreeSet<String>>,
    },
    /// A CTE, derived table, VALUES list or other row source: `x.f` addresses the row, never
    /// a scalar, so only the dangerous-name rule can apply to it.
    Rows,
}

/// One qualified reference `q.f`. `qualifier` is `None` when the field has no named qualifier
/// (e.g. the root of `(g).f` is not an identifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldAccess {
    /// Lowercased alias or table qualifier (`c` in `c.name`), if there is one.
    pub qualifier: Option<String>,
    /// The field that follows the qualifier (`name` in `c.name`), folded the way PG stores
    /// column names (quoted keeps case, unquoted folds), so it can be checked against the
    /// output columns of a row-function source.
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlFacts {
    pub engine: SqlEngine,
    pub statements: Vec<StatementFacts>,
    /// MySQL `/*! … */` or `/*M! … */`: the server runs SQL the parser may not see.
    pub has_executable_comment: bool,
    /// A `/*+ … */` optimizer hint comment. MySQL hints such as `MAX_EXECUTION_TIME(0)` or
    /// `SET_VAR(…)` change limits for the statement; PG's pg_hint_plan reads them too.
    pub has_optimizer_hint: bool,
}

impl SqlFacts {
    pub fn statement_count(&self) -> usize {
        self.statements.len()
    }
}

/// Parses `sql` and collects facts per statement. MySQL text that does not parse (e.g.
/// `INTO OUTFILE`, `INTO @v` after FROM) is an error; callers treat it as unknown.
pub fn analyze(engine: SqlEngine, sql: &str) -> Result<SqlFacts, ParserError> {
    let statements = match sqlite_pragma(engine, sql)? {
        Some(p) => vec![pragma_facts(p)],
        None => Parser::parse_sql(&*dialect(engine), sql)?
            .iter()
            .map(|s| statement_facts(engine, s))
            .collect(),
    };
    Ok(SqlFacts {
        engine,
        statements,
        has_executable_comment: has_executable_comment(engine, sql),
        has_optimizer_hint: has_optimizer_hint(engine, sql),
    })
}

fn has_optimizer_hint(engine: SqlEngine, sql: &str) -> bool {
    let d = dialect(engine);
    match Tokenizer::new(&*d, sql).tokenize() {
        Ok(tokens) => tokens.iter().any(|t| {
            matches!(t, Token::Whitespace(Whitespace::MultiLineComment(c)) if c.starts_with('+'))
        }),
        // Text that does not tokenize does not parse either; `analyze` fails on it.
        Err(_) => false,
    }
}

fn empty_facts(kind: StatementKind, label: String) -> StatementFacts {
    StatementFacts {
        kind,
        label,
        target: None,
        has_where: None,
        alter_drops: false,
        explained: None,
        implicit_commit: false,
        pragma: None,
        functions: BTreeSet::new(),
        has_locking: false,
        has_into: false,
        has_modifying_cte: false,
        has_variable_assignment: false,
        tables: BTreeSet::new(),
        from_aliases: BTreeMap::new(),
        field_accesses: Vec::new(),
        dot_fields: Vec::new(),
        show_processlist: false,
        show_variable: None,
    }
}

fn pragma_facts(p: Pragma) -> StatementFacts {
    let mut f = empty_facts(StatementKind::Pragma, "PRAGMA".into());
    f.pragma = Some(p);
    f
}

/// sqlparser only accepts literal PRAGMA arguments (`PRAGMA table_info('t')`, not
/// `PRAGMA table_info(t)`), so a lone SQLite PRAGMA is read from the tokens:
/// `PRAGMA [schema.]name [= value | (value)] [;]`. `Ok(None)` when the text is not a lone PRAGMA,
/// so the normal parser handles it (and sees every statement of a multi-statement text).
fn sqlite_pragma(engine: SqlEngine, sql: &str) -> Result<Option<Pragma>, ParserError> {
    if engine != SqlEngine::Sqlite {
        return Ok(None);
    }
    let d = dialect(engine);
    let Ok(tokens) = Tokenizer::new(&*d, sql).tokenize() else {
        return Ok(None);
    };
    let mut t: Vec<&Token> = tokens
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace(_) | Token::EOF))
        .collect();
    match t.first() {
        Some(Token::Word(w))
            if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("pragma") => {}
        _ => return Ok(None),
    }
    while t.last() == Some(&&Token::SemiColon) {
        t.pop();
    }
    if t.contains(&&Token::SemiColon) {
        return Ok(None);
    }
    let bad = || ParserError::ParserError(format!("Unsupported PRAGMA syntax: {}", sql.trim()));
    let mut i = 1;
    let word = |i: usize| match t.get(i) {
        Some(Token::Word(w)) => Some(w.value.to_lowercase()),
        _ => None,
    };
    let mut name = word(i).ok_or_else(bad)?;
    i += 1;
    if t.get(i) == Some(&&Token::Period) {
        name = word(i + 1).ok_or_else(bad)?;
        i += 2;
    }
    let is_value = |t: Option<&&Token>| {
        matches!(
            t,
            Some(Token::Word(_) | Token::Number(..) | Token::SingleQuotedString(_))
        )
    };
    let form = match &t[i..] {
        [] => PragmaForm::Read,
        [Token::Eq, v] if is_value(Some(v)) => PragmaForm::Assign,
        [Token::Eq, Token::Minus | Token::Plus, Token::Number(..)] => PragmaForm::Assign,
        [Token::LParen, v, Token::RParen] if is_value(Some(v)) => PragmaForm::Call,
        [
            Token::LParen,
            Token::Minus | Token::Plus,
            Token::Number(..),
            Token::RParen,
        ] => PragmaForm::Call,
        _ => return Err(bad()),
    };
    Ok(Some(Pragma { name, form }))
}

fn statement_facts(engine: SqlEngine, s: &Statement) -> StatementFacts {
    let k = kind(s);
    let mut facts = empty_facts(k, label(s, k));
    facts.implicit_commit = engine == SqlEngine::Mysql && commits_implicitly(s, k);
    facts.show_processlist = matches!(s, Statement::ShowProcessList { .. });
    if let Statement::ShowVariable { variable } = s {
        // Quoted names count too: `SHOW "primary_conninfo"` is valid on PG, and a quoted
        // keyword MySQL would reject still fails closed.
        facts.show_variable = variable.first().map(|i| i.value.to_lowercase());
    }
    facts.target = target(s);
    facts.has_where = match s {
        Statement::Update(u) => Some(u.selection.is_some()),
        Statement::Delete(d) => Some(d.selection.is_some()),
        _ => None,
    };
    facts.alter_drops = match s {
        Statement::AlterTable(a) => a.operations.iter().any(|op| {
            matches!(
                op,
                AlterTableOperation::DropColumn { .. }
                    | AlterTableOperation::DropConstraint { .. }
                    | AlterTableOperation::DropPrimaryKey { .. }
                    | AlterTableOperation::DropForeignKey { .. }
                    | AlterTableOperation::DropIndex { .. }
                    | AlterTableOperation::DropPartitions { .. }
                    | AlterTableOperation::DropProjection { .. }
                    | AlterTableOperation::DropClusteringKey
            )
        }),
        _ => false,
    };
    if let Statement::Explain { statement, .. } = s {
        facts.explained = Some(Box::new(kind(statement)));
    }
    let mut v = Collector { engine, facts };
    let _ = s.visit(&mut v);
    v.facts
}

fn kind(s: &Statement) -> StatementKind {
    match s {
        Statement::Query(q) => match &*q.body {
            SetExpr::Insert(_) => StatementKind::Insert,
            SetExpr::Update(_) => StatementKind::Update,
            SetExpr::Delete(_) => StatementKind::Delete,
            SetExpr::Merge(_) => StatementKind::Merge,
            _ => StatementKind::Query,
        },
        // Any ANALYZE option counts, even `ANALYZE false`: only the plan-only form is a read.
        Statement::Explain {
            analyze, options, ..
        } => StatementKind::Explain {
            analyze: *analyze
                || options.iter().flatten().any(|o| {
                    o.name.value.eq_ignore_ascii_case("analyze")
                        || o.name.value.eq_ignore_ascii_case("analyse")
                }),
        },
        Statement::ExplainTable { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowVariable { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCatalogs { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowProcessList { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowCharset(_)
        | Statement::ShowObjects(_)
        | Statement::ShowTables { .. }
        | Statement::ShowViews { .. }
        | Statement::ShowCollation { .. } => StatementKind::Show,
        Statement::Insert(_) => StatementKind::Insert,
        Statement::Update(_) => StatementKind::Update,
        Statement::Delete(_) => StatementKind::Delete,
        Statement::Merge(_) => StatementKind::Merge,
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::ReleaseSavepoint { .. } => StatementKind::Transaction,
        Statement::Pragma { .. } => StatementKind::Pragma,
        Statement::Drop { .. } => StatementKind::Drop,
        Statement::Truncate(_) => StatementKind::Truncate,
        Statement::RenameTable(_) => StatementKind::Rename,
        _ => match first_word(s).as_str() {
            "CREATE" => StatementKind::Create,
            "ALTER" => StatementKind::Alter,
            "DROP" => StatementKind::Drop,
            _ => StatementKind::Other,
        },
    }
}

/// MySQL statements that commit the open transaction before they run, as far as the parser
/// exposes them. MySQL spellings it cannot parse (`OPTIMIZE TABLE`, `REPAIR TABLE`, `LOAD DATA`,
/// `INSTALL PLUGIN`, …) reach the callers as unparsable text, which is already reported as an
/// unknown effect; the approved write treats it as a possible commit.
fn commits_implicitly(s: &Statement, k: StatementKind) -> bool {
    if k.is_ddl() {
        return true;
    }
    match s {
        Statement::Set(set) => sets_autocommit(set),
        Statement::LockTables { .. }
        | Statement::UnlockTables
        | Statement::Call(_)
        | Statement::Grant(_)
        | Statement::Revoke(_)
        | Statement::Analyze(_)
        | Statement::Flush { .. }
        | Statement::Reset(_) => true,
        _ => false,
    }
}

/// `SET [SESSION | LOCAL] autocommit = …` commits implicitly; `SET GLOBAL autocommit` only
/// changes the default for new sessions, and every other variable (`FOREIGN_KEY_CHECKS`, `@x`)
/// commits nothing. The `@@` spelling carries the scope inside the variable name:
/// `@@autocommit` and `@@session.autocommit` are the session variable, `@@global.autocommit`
/// is the global one.
fn sets_autocommit(s: &Set) -> bool {
    let autocommit = |name: &ObjectName| {
        // MySQL tokenizes `@@global.autocommit` as the parts `@@global`, `autocommit`.
        let mut parts = name.0.iter().filter_map(|p| match p {
            ObjectNamePart::Identifier(i) => Some(i.value.trim_start_matches('@')),
            _ => None,
        });
        let first = parts.next();
        let last = parts.next_back().or(first);
        first.is_none_or(|f| !f.eq_ignore_ascii_case("global"))
            && last.is_some_and(|l| l.eq_ignore_ascii_case("autocommit"))
    };
    let session = |scope: &Option<ContextModifier>| !matches!(scope, Some(ContextModifier::Global));
    match s {
        Set::SingleAssignment { scope, variable, .. } => session(scope) && autocommit(variable),
        Set::MultipleAssignments { assignments } => assignments
            .iter()
            .any(|a| session(&a.scope) && autocommit(&a.name)),
        _ => false,
    }
}

fn first_word(s: &Statement) -> String {
    s.to_string()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// `CREATE OR REPLACE TEMP VIEW v` → `CREATE VIEW`; `SET ROLE x` → `SET`.
fn label(s: &Statement, k: StatementKind) -> String {
    match k {
        StatementKind::Query => return "SELECT".into(),
        StatementKind::Insert => return "INSERT".into(),
        StatementKind::Update => return "UPDATE".into(),
        StatementKind::Delete => return "DELETE".into(),
        StatementKind::Merge => return "MERGE".into(),
        StatementKind::Explain { .. } => return "EXPLAIN".into(),
        StatementKind::Truncate => return "TRUNCATE".into(),
        StatementKind::Pragma => return "PRAGMA".into(),
        _ => {}
    }
    if let Statement::ExplainTable { .. } = s {
        return "DESCRIBE".into();
    }
    let text = s.to_string();
    let mut words = text.split_whitespace().map(str::to_ascii_uppercase);
    let first = words.next().unwrap_or_default();
    if !matches!(first.as_str(), "CREATE" | "ALTER" | "DROP") {
        return first;
    }
    const SKIP: &[&str] = &[
        "OR",
        "REPLACE",
        "TEMP",
        "TEMPORARY",
        "UNLOGGED",
        "GLOBAL",
        "LOCAL",
        "UNIQUE",
        "IF",
        "NOT",
        "EXISTS",
        "RECURSIVE",
        "DEFINER",
        "ALGORITHM",
        "SQL",
        "SECURITY",
        "MATERIALIZED",
    ];
    let mut out = first;
    let mut materialized = false;
    for w in words {
        if w == "MATERIALIZED" {
            materialized = true;
        }
        if SKIP.contains(&w.as_str()) || w.contains('=') {
            continue;
        }
        out.push(' ');
        if materialized {
            out.push_str("MATERIALIZED ");
        }
        out.push_str(&w);
        break;
    }
    out
}

fn relation_name(t: &TableFactor) -> Option<String> {
    match t {
        TableFactor::Table { name, .. } => Some(name.to_string()),
        _ => None,
    }
}

fn target(s: &Statement) -> Option<String> {
    match s {
        Statement::Insert(i) => Some(i.table.to_string()),
        Statement::Update(u) => relation_name(&u.table.relation),
        Statement::Delete(d) => {
            if let Some(n) = d.tables.first() {
                return Some(n.to_string());
            }
            let (sqlparser::ast::FromTable::WithFromKeyword(f)
            | sqlparser::ast::FromTable::WithoutKeyword(f)) = &d.from;
            f.first().and_then(|t| relation_name(&t.relation))
        }
        Statement::Merge(m) => relation_name(&m.table),
        Statement::Drop { names, .. } => Some(
            names
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Statement::Truncate(t) => Some(
            t.table_names
                .iter()
                .map(|n| n.name.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Statement::AlterTable(a) => Some(a.name.to_string()),
        Statement::CreateTable(c) => Some(c.name.to_string()),
        Statement::CreateView(v) => Some(v.name.to_string()),
        Statement::CreateIndex(i) => Some(i.table_name.to_string()),
        Statement::Query(q) => match &*q.body {
            SetExpr::Insert(s) | SetExpr::Update(s) | SetExpr::Delete(s) | SetExpr::Merge(s) => {
                target(s)
            }
            _ => None,
        },
        _ => None,
    }
}

fn function_name(n: &ObjectName) -> String {
    n.0.iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(i) if i.quote_style.is_some() => i.value.clone(),
            ObjectNamePart::Identifier(i) => i.value.to_lowercase(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The body writes, directly or in a branch of a set operation (`SELECT … UNION (DELETE …)`).
fn modifies(body: &SetExpr) -> bool {
    match body {
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => true,
        SetExpr::SetOperation { left, right, .. } => modifies(left) || modifies(right),
        SetExpr::Query(q) => modifies(&q.body),
        _ => false,
    }
}

struct Collector {
    engine: SqlEngine,
    facts: StatementFacts,
}

/// Column names an alias declares in parentheses (`AS t(a, b)`), folded the way PG stores
/// them: a quoted name keeps its exact case, an unquoted name folds to lower case. A false
/// match here removes the attribute-call check, so PG's own folding is the only safe one.
fn alias_columns<'a>(cols: impl Iterator<Item = &'a TableAliasColumnDef>) -> BTreeSet<String> {
    cols.map(|c| column_name(&c.name)).collect()
}

/// Output columns of allow-listed set-returning functions that return a record rather than a
/// single column named after the function, so `e.key FROM jsonb_each(…) e` counts as a known
/// column and not a possible attribute call. PG's json SRFs return `(key, value)`; the
/// `*_object_keys` functions return a single column named after the function, which the
/// default `{function_name}` fallback already covers. Other engines get the same fallback.
fn srf_columns(engine: SqlEngine, name: &str) -> &'static [&'static str] {
    match engine {
        SqlEngine::Pg => match name {
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => &["key", "value"],
            _ => &[],
        },
        _ => &[],
    }
}

/// Lowercased `i.value`, quoted or not. Used for alias keys and qualifier lookups: PG folds
/// unquoted names to lower case, so `"É"` and `É` address the same source; folding both sides
/// of a lookup the same way means a case variant can only merge to the stricter reading,
/// never slip through.
fn ident_name(i: &Ident) -> String {
    i.value.to_lowercase()
}

/// PG's own folding for a column or field name: a quoted name keeps `i.value` exactly, an
/// unquoted name folds to lower case. Membership checks against row-function output columns
/// must use this folding — a quoted `"UPPER"` column does not answer to `x.upper`.
fn column_name(i: &Ident) -> String {
    if i.quote_style.is_some() {
        i.value.clone()
    } else {
        i.value.to_ascii_lowercase()
    }
}

/// Output columns of a row-value source in FROM whose rows are scalars: the declared alias
/// column list when there is one, else the function's own name plus the record columns of the
/// SRFs that have them.
fn row_function_columns(
    engine: SqlEngine,
    name: &ObjectName,
    alias: Option<&sqlparser::ast::TableAlias>,
    with_ordinality: bool,
) -> BTreeSet<String> {
    let fname = function_name(name);
    let mut cols = alias
        .map(|a| alias_columns(a.columns.iter()))
        .unwrap_or_default();
    if cols.is_empty() {
        cols.insert(fname.clone());
        cols.extend(srf_columns(engine, &fname).iter().map(|s| s.to_string()));
    }
    if with_ordinality {
        cols.insert("ordinality".into());
    }
    cols
}

/// Merges a new source under `alias`. `RowFunction` sources keep their output columns; the
/// stricter kind wins over a `Rows` entry in either order (a FROM alias shadows a same-named
/// CTE), and when two `RowFunction` sources share a name the strictest interpretation wins
/// (unknown stays unknown, known sets intersect) so the check fails closed.
fn record_alias(map: &mut BTreeMap<String, FromAlias>, alias: Option<&Ident>, kind: FromAlias) {
    let Some(a) = alias else { return };
    let name = ident_name(a);
    let strictest = match (map.get(&name), kind) {
        (Some(FromAlias::Rows), FromAlias::Rows) | (None, FromAlias::Rows) => FromAlias::Rows,
        (Some(FromAlias::RowFunction { columns }), new) => {
            let new_cols = match new {
                FromAlias::RowFunction { columns } => columns,
                FromAlias::Rows => None,
            };
            FromAlias::RowFunction {
                columns: match (columns.as_ref(), new_cols.as_ref()) {
                    (None, _) | (_, None) => None,
                    (Some(a), Some(b)) => Some(a.intersection(b).cloned().collect()),
                },
            }
        }
        (None, new) | (Some(FromAlias::Rows), new) => new,
    };
    map.insert(name, strictest);
}

impl Visitor for Collector {
    type Break = ();

    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        self.facts.has_locking |= !q.locks.is_empty();
        if let Some(w) = &q.with {
            self.facts.has_modifying_cte |= w.cte_tables.iter().any(|c| modifies(&c.query.body));
            for cte in &w.cte_tables {
                // A CTE is a non-table source, but its rows are rows: `x.f` only addresses a
                // field, so the dangerous-name rule (not the column rule) applies to it.
                record_alias(
                    &mut self.facts.from_aliases,
                    Some(&cte.alias.name),
                    FromAlias::Rows,
                );
            }
        }
        // Nested queries (derived tables, subqueries, set-operation branches) are visited too,
        // so a write anywhere below the top level is caught here.
        if let SetExpr::SetOperation { left, right, .. } = &*q.body {
            self.facts.has_modifying_cte |= modifies(left) || modifies(right);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, t: &TableFactor) -> ControlFlow<()> {
        match t {
            TableFactor::Table {
                name,
                alias,
                args,
                with_ordinality,
                ..
            } => {
                self.facts.tables.insert(function_name(name));
                if args.is_some() {
                    // `FROM generate_series(1, 3) g`: a function in table position; `g.f` can
                    // be an attribute call on the scalar row.
                    self.facts.functions.insert(function_name(name));
                    let cols =
                        row_function_columns(self.engine, name, alias.as_ref(), *with_ordinality);
                    let a = alias.as_ref().map(|a| &a.name).or_else(|| {
                        name.0.last().and_then(|p| match p {
                            ObjectNamePart::Identifier(i) => Some(i),
                            _ => None,
                        })
                    });
                    record_alias(
                        &mut self.facts.from_aliases,
                        a,
                        FromAlias::RowFunction {
                            columns: Some(cols),
                        },
                    );
                }
            }
            TableFactor::Function {
                name,
                alias,
                with_ordinality,
                ..
            } => {
                self.facts.functions.insert(function_name(name));
                let cols =
                    row_function_columns(self.engine, name, alias.as_ref(), *with_ordinality);
                let a = alias.as_ref().map(|a| &a.name).or_else(|| {
                    name.0.last().and_then(|p| match p {
                        ObjectNamePart::Identifier(i) => Some(i),
                        _ => None,
                    })
                });
                record_alias(
                    &mut self.facts.from_aliases,
                    a,
                    FromAlias::RowFunction {
                        columns: Some(cols),
                    },
                );
            }
            TableFactor::UNNEST {
                alias,
                array_exprs,
                with_offset,
                with_ordinality,
                ..
            } => {
                // `FROM unnest(a) x`: the function is not in `functions` through the visitor,
                // so record it and its arguments.
                self.facts.functions.insert("unnest".into());
                let mut cols = alias
                    .as_ref()
                    .map(|a| alias_columns(a.columns.iter()))
                    .unwrap_or_default();
                if cols.is_empty() {
                    cols.insert("unnest".into());
                }
                if *with_offset || *with_ordinality {
                    cols.insert("offset".into());
                    cols.insert("ordinality".into());
                }
                // Without an alias PG still names the source `unnest`, so `unnest.f` must be
                // checked like an explicit alias.
                let implicit = Ident::new("unnest");
                record_alias(
                    &mut self.facts.from_aliases,
                    Some(alias.as_ref().map(|a| &a.name).unwrap_or(&implicit)),
                    FromAlias::RowFunction {
                        columns: Some(cols),
                    },
                );
                for e in array_exprs {
                    let _ = e.visit(self);
                }
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                self.facts.has_modifying_cte |= modifies(&subquery.body);
                // A derived table's rows are rows: `x.f` addresses a field, so only the
                // dangerous-name rule applies to the alias.
                record_alias(
                    &mut self.facts.from_aliases,
                    alias.as_ref().map(|a| &a.name),
                    FromAlias::Rows,
                );
            }
            // Every other FROM source (VALUES, joins, PIVOT, …) is non-table; mark it so a
            // dangerous-name field access on the alias is still caught.
            _ => {
                let a = table_factor_alias(t);
                record_alias(&mut self.facts.from_aliases, a, FromAlias::Rows);
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        match e {
            Expr::Function(f) => {
                self.facts.functions.insert(function_name(&f.name));
            }
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                // `q.f` can be an attribute notation call `f(q)`. The first part qualifies the
                // second; deeper parts apply to the value `q.f`, not to `q`.
                for (i, field) in parts.iter().enumerate().skip(1) {
                    self.facts.field_accesses.push(FieldAccess {
                        qualifier: (i == 1).then(|| ident_name(&parts[0])),
                        field: column_name(field),
                    });
                }
            }
            Expr::CompoundFieldAccess { root, access_chain } => {
                // `(g).f` and access chains with subscripts: each `Dot` step is a field access.
                // Only the first step takes the root's name as its qualifier.
                let root_name = match &**root {
                    Expr::Identifier(i) => Some(ident_name(i)),
                    Expr::Nested(e) => match &**e {
                        Expr::Identifier(i) => Some(ident_name(i)),
                        _ => None,
                    },
                    _ => None,
                };
                let mut first = true;
                for step in access_chain {
                    if let AccessExpr::Dot(e) = step {
                        // The step can run as a one-argument call on the root value; a non-
                        // identifier step is reported by its text.
                        self.facts.dot_fields.push(match e {
                            Expr::Identifier(i) => ident_name(i),
                            _ => e.to_string(),
                        });
                    }
                    if let AccessExpr::Dot(Expr::Identifier(i)) = step {
                        self.facts.field_accesses.push(FieldAccess {
                            qualifier: if first { root_name.clone() } else { None },
                            field: column_name(i),
                        });
                        first = false;
                    }
                }
            }
            Expr::BinaryOp {
                op: BinaryOperator::Assignment,
                ..
            } => self.facts.has_variable_assignment = true,
            Expr::Subquery(q) | Expr::Exists { subquery: q, .. } => {
                self.facts.has_modifying_cte |= modifies(&q.body);
            }
            Expr::InSubquery { subquery, .. } => {
                self.facts.has_modifying_cte |= modifies(&subquery.body);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, s: &sqlparser::ast::Select) -> ControlFlow<()> {
        self.facts.has_into |= s.into.is_some();
        ControlFlow::Continue(())
    }
}

fn table_factor_alias(t: &TableFactor) -> Option<&Ident> {
    match t {
        TableFactor::NestedJoin { alias, .. }
        | TableFactor::Pivot { alias, .. }
        | TableFactor::Unpivot { alias, .. }
        | TableFactor::MatchRecognize { alias, .. }
        | TableFactor::XmlTable { alias, .. }
        | TableFactor::JsonTable { alias, .. }
        | TableFactor::OpenJsonTable { alias, .. }
        | TableFactor::SemanticView { alias, .. }
        | TableFactor::TableFunction { alias, .. } => alias.as_ref().map(|a| &a.name),
        _ => None,
    }
}

/// Field accesses that PostgreSQL could run as attribute notation function calls `f(q)`, which
/// the `functions` check never sees. Three shapes:
/// - every `AccessExpr::Dot` step in a compound access (`('…').f`, `(x.c).f`): the root is not
///   a column name, so the step can only be a field of a composite or a call `f(root)`;
/// - the field is a dangerous function name (`pg_*`, `lo_*`, `dblink*`, sequence setters and
///   every other name the guard's deny side knows) that is not on the pure allow-list — the
///   call is suspicious whatever the qualifier;
/// - the qualifier is the alias of a scalar row source (a set-returning function or UNNEST)
///   and the field is not a known output column, so `q.f` is `f(q)`.
///
/// Callers reject when the list is not empty. Only for `SqlEngine::Pg`: other dialects have no
/// attribute notation, so nothing is reported.
fn attribute_calls(facts: &SqlFacts, s: &StatementFacts) -> Vec<String> {
    if facts.engine != SqlEngine::Pg {
        return Vec::new();
    }
    let mut out = BTreeSet::new();
    out.extend(s.dot_fields.iter().cloned());
    for fa in &s.field_accesses {
        if is_dangerous_field_name(&fa.field.to_lowercase())
            && !functions::is_pure(SqlEngine::Pg, &fa.field.to_lowercase())
        {
            out.insert(fa.field.clone());
        }
        if let Some(q) = &fa.qualifier
            && let Some(FromAlias::RowFunction { columns }) = s.from_aliases.get(q)
            && !columns.as_ref().is_some_and(|c| c.contains(&fa.field))
        {
            out.insert(format!("{q}.{}", fa.field));
        }
    }
    out.into_iter().collect()
}

/// A name PostgreSQL could read as a function call in `x.f`: built-in dangerous prefixes and
/// the function names the guard refuses outright. Allow-listed names are filtered by the caller.
/// Also the deny side of the read-only editor pre-check (`conn_ro`), so both share one list.
pub(crate) fn is_dangerous_field_name(name: &str) -> bool {
    name.starts_with("pg_")
        || name.starts_with("lo_")
        || name.starts_with("dblink")
        || name.starts_with("query_to_xml")
        || name.starts_with("cursor_to_xml")
        || name.starts_with("table_to_xml")
        || name.starts_with("crosstab")
        || matches!(
            name,
            "set_config"
                | "nextval"
                | "setval"
                | "currval"
                | "sleep"
                | "benchmark"
                | "get_lock"
                | "release_lock"
                | "load_file"
                | "load_extension"
                | "readfile"
                | "writefile"
                | "txid_current"
                | "current_setting"
                | "last_insert_id"
                | "changes"
                | "edit"
                | "fts3_tokenizer"
                | "ts_stat"
                | "xpath_table"
        )
}

const READ_KINDS: &str = "Only one SELECT, WITH … SELECT, VALUES, SHOW, DESCRIBE or EXPLAIN \
                          (without ANALYZE) statement is allowed";

/// Who asks whether SQL only reads. The shared conjunction is the same; the profile picks which
/// statements count as reads, which functions are allowed, and how a refusal reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadProfile {
    /// The editor: one statement that can run again ("Load more", export), so every function
    /// must be a pure built-in and its connection may go back to the pool.
    Editor,
    /// The AI read: the editor's rules, plus the [`AMPLIFYING`] overlay.
    AiRead,
    /// The read-only editor: the database barrier refuses writes, so the pre-check only has to
    /// reject SQL that could write and session-mode changes. User-defined functions pass.
    ReadOnlyEditor,
}

/// What the shared rules say about one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadVerdict {
    /// The statement only reads; `kind` is the word to show (`SELECT`, `SHOW`, …).
    Read { kind: &'static str },
    /// A non-read the profile lets through on a read-only connection: transaction control, a
    /// `SET`, a SQLite PRAGMA. The caller adds its session-escape rules.
    Mode,
    /// Refused, with the sentence the caller shows.
    Refused(String),
}

/// The executable-comment refusal for a profile, or `None`. MySQL runs the body of
/// `/*! … */`, so the SQL the parser saw is not the SQL the server runs; every profile refuses
/// it. Exposed so a caller can apply it before its own text checks.
pub fn executable_comment_error(facts: &SqlFacts, profile: ReadProfile) -> Option<String> {
    facts.has_executable_comment.then(|| match profile {
        ReadProfile::AiRead => {
            "MySQL executable comments (/*! … */ or /*M! … */) are not allowed".to_string()
        }
        ReadProfile::ReadOnlyEditor => {
            "MySQL executable comments (/*! … */ or /*M! … */) are refused".to_string()
        }
        ReadProfile::Editor => "An executable comment is not a plain read".to_string(),
    })
}

/// The shared read policy over a parsed text. `Err` refuses the text as a whole: executable
/// comments, or a statement count the profile does not allow. Callers add their own rules
/// (privacy, session-mode escapes) to a [`ReadVerdict::Read`].
pub fn read_policy(facts: &SqlFacts, profile: ReadProfile) -> Result<Vec<ReadVerdict>, String> {
    if let Some(reason) = executable_comment_error(facts, profile) {
        return Err(reason);
    }
    match facts.statements.as_slice() {
        [] => Err(match profile {
            ReadProfile::AiRead => "The query has no statement".into(),
            _ => "No statement".into(),
        }),
        [s] => Ok(vec![statement_read(facts, s, profile)]),
        many if profile == ReadProfile::ReadOnlyEditor => {
            // Each split statement is analysed on its own; text the splitter could not cut is
            // still checked statement by statement.
            Ok(many
                .iter()
                .map(|s| statement_read(facts, s, profile))
                .collect())
        }
        many => Err(format!(
            "Exactly one statement is allowed, found {}",
            many.len()
        )),
    }
}

fn refused_label(s: &StatementFacts) -> String {
    format!("{} statements are refused", s.label)
}

fn statement_read(facts: &SqlFacts, s: &StatementFacts, profile: ReadProfile) -> ReadVerdict {
    let refuse = |reason: String| ReadVerdict::Refused(reason);
    let not_a_read = || refuse(format!("{} is not a read. {READ_KINDS}", s.label));
    let kind = match s.kind {
        StatementKind::Query => "SELECT",
        StatementKind::Show => {
            if s.label == "DESCRIBE" {
                "DESCRIBE"
            } else {
                "SHOW"
            }
        }
        StatementKind::Explain { analyze: true } => {
            return refuse(match profile {
                ReadProfile::AiRead => {
                    "EXPLAIN ANALYZE runs the statement; use EXPLAIN without ANALYZE".into()
                }
                _ => refused_label(s),
            });
        }
        StatementKind::Explain { analyze: false } => {
            if s.explained.as_deref() != Some(&StatementKind::Query) {
                return refuse(match profile {
                    ReadProfile::AiRead => {
                        "EXPLAIN is only allowed for a SELECT query".into()
                    }
                    _ => refused_label(s),
                });
            }
            "EXPLAIN"
        }
        StatementKind::Pragma => match profile {
            ReadProfile::AiRead if facts.engine == SqlEngine::Sqlite => "PRAGMA",
            ReadProfile::ReadOnlyEditor => return ReadVerdict::Mode,
            _ => return not_a_read(),
        },
        // Transaction control and SET leave a read-only connection's data alone; the
        // read-only editor checks them for session-mode escapes separately.
        StatementKind::Transaction | StatementKind::Other
            if profile == ReadProfile::ReadOnlyEditor =>
        {
            return ReadVerdict::Mode;
        }
        _ => return not_a_read(),
    };
    let flag = |set: bool, ai: &str, other: &str| {
        if !set {
            None
        } else if profile == ReadProfile::AiRead {
            Some(refuse(ai.to_string()))
        } else {
            Some(refuse(other.to_string()))
        }
    };
    if let Some(v) = flag(
        s.has_into,
        "SELECT … INTO (a table, file or variable) is not allowed",
        &refused_label(s),
    ) {
        return v;
    }
    if let Some(v) = flag(
        s.has_locking,
        "Locking reads (FOR UPDATE / FOR SHARE / LOCK IN SHARE MODE) are not allowed",
        &refused_label(s),
    ) {
        return v;
    }
    if let Some(v) = flag(
        s.has_modifying_cte,
        "A CTE or subquery that changes data (INSERT / UPDATE / DELETE / MERGE) is not allowed",
        &refused_label(s),
    ) {
        return v;
    }
    if let Some(v) = flag(
        s.has_variable_assignment,
        "Assigning user variables (@v := …) is not allowed",
        &refused_label(s),
    ) {
        return v;
    }
    if matches!(profile, ReadProfile::Editor | ReadProfile::AiRead) {
        // `x.f` can be the attribute notation call `f(x)`, which `s.functions` never records.
        let attr = attribute_calls(facts, s);
        if !attr.is_empty() {
            return refuse(match profile {
                ReadProfile::AiRead => format!(
                    "Possible attribute-notation function call{}: {}. PostgreSQL reads x.f as \
                     f(x); write the column or an allowed function call instead",
                    if attr.len() == 1 { "" } else { "s" },
                    attr.join(", ")
                ),
                _ => refused_label(s),
            });
        }
    }
    if profile == ReadProfile::AiRead {
        // The overlay next to the allow-list it narrows (`functions::AMPLIFYING`).
        let amplifying: Vec<&str> = s
            .functions
            .iter()
            .filter(|f| functions::is_amplifying(f))
            .map(String::as_str)
            .collect();
        if !amplifying.is_empty() {
            return refuse(format!(
                "Function{} can build arbitrarily large values and is not allowed: {}",
                if amplifying.len() == 1 { "" } else { "s" },
                amplifying.join(", ")
            ));
        }
    }
    if matches!(profile, ReadProfile::Editor | ReadProfile::AiRead) {
        let blocked: Vec<&str> = s
            .functions
            .iter()
            .filter(|f| !functions::is_pure(facts.engine, f))
            .map(String::as_str)
            .collect();
        if !blocked.is_empty() {
            return refuse(match profile {
                ReadProfile::AiRead => format!(
                    "Function{} not on the read-only allow-list: {}. Use built-in string, math, \
                     date, JSON, aggregate or window functions instead",
                    if blocked.len() == 1 { "" } else { "s" },
                    blocked.join(", ")
                ),
                _ => refused_label(s),
            });
        }
    }
    ReadVerdict::Read { kind }
}

/// The editor's phase 2 policy: one statement that only reads and leaves no session state, so
/// "Load more" and export may run it again and its connection may go back to the pool.
pub fn is_plain_read(facts: &SqlFacts) -> bool {
    read_policy(facts, ReadProfile::Editor)
        .is_ok_and(|v| v.iter().all(|r| matches!(r, ReadVerdict::Read { .. })))
}

/// [`analyze`] then [`is_plain_read`]; SQL that does not parse is not a plain read.
pub fn is_plain_read_sql(engine: SqlEngine, sql: &str) -> bool {
    analyze(engine, sql).is_ok_and(|f| is_plain_read(&f))
}

/// The literal row count of the last statement's own `LIMIT` (also `LIMIT n OFFSET m` and
/// MySQL/SQLite `LIMIT m, n`); `None` when it has none or the text does not parse.
pub fn row_limit(engine: SqlEngine, sql: &str) -> Option<u64> {
    let parsed = Parser::parse_sql(&*dialect(engine), sql).ok()?;
    let Some(Statement::Query(q)) = parsed.last() else {
        return None;
    };
    let n = match q.limit_clause.as_ref()? {
        LimitClause::LimitOffset { limit, .. } => limit.as_ref()?,
        LimitClause::OffsetCommaLimit { limit, .. } => limit,
    };
    match n {
        Expr::Value(v) => match &v.value {
            Value::Number(s, _) => s.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(e: SqlEngine, sql: &str) -> StatementFacts {
        let f = analyze(e, sql).unwrap_or_else(|err| panic!("{sql}: {err}"));
        assert_eq!(f.statement_count(), 1, "{sql}");
        f.statements.into_iter().next().unwrap()
    }

    #[test]
    fn collects_function_names_everywhere() {
        let f = facts(
            SqlEngine::Pg,
            "SELECT lower(a), analytics.Score(b), \"MyFn\"(c) FROM generate_series(1, 3) g \
             WHERE id IN (SELECT max(id) FROM t WHERE x = nextval('s'))",
        );
        let names: Vec<&str> = f.functions.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            vec![
                "MyFn",
                "analytics.score",
                "generate_series",
                "lower",
                "max",
                "nextval"
            ]
        );
        assert_eq!(f.kind, StatementKind::Query);
    }

    #[test]
    fn statement_kinds_and_flags() {
        assert_eq!(
            facts(SqlEngine::Pg, "EXPLAIN ANALYZE SELECT 1").kind,
            StatementKind::Explain { analyze: true }
        );
        assert_eq!(
            facts(SqlEngine::Pg, "EXPLAIN (ANALYZE) SELECT 1").kind,
            StatementKind::Explain { analyze: true }
        );
        assert_eq!(
            facts(SqlEngine::Pg, "EXPLAIN (ANALYSE) SELECT 1").kind,
            StatementKind::Explain { analyze: true }
        );
        assert_eq!(
            facts(SqlEngine::Pg, "WITH x AS (SELECT 1) DELETE FROM t").kind,
            StatementKind::Delete
        );
        assert_eq!(facts(SqlEngine::Pg, "BEGIN").kind, StatementKind::Transaction);
        assert_eq!(facts(SqlEngine::Mysql, "DESCRIBE t").kind, StatementKind::Show);
        let f = facts(
            SqlEngine::Pg,
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
        );
        assert!(f.has_modifying_cte && f.kind == StatementKind::Query);
        assert!(facts(SqlEngine::Mysql, "SELECT @v := 1").has_variable_assignment);
        assert!(facts(SqlEngine::Pg, "SELECT * INTO t2 FROM t").has_into);
        assert_eq!(
            analyze(SqlEngine::Pg, "SELECT 1; SELECT 2")
                .unwrap()
                .statement_count(),
            2
        );
    }

    #[test]
    fn mysql_implicit_commits_are_found() {
        for sql in [
            "CREATE TABLE t (id int)",
            "ALTER TABLE t ADD COLUMN b int",
            "DROP TABLE t",
            "TRUNCATE TABLE t",
            "RENAME TABLE a TO b",
            "SET autocommit = 0",
            "SET SESSION autocommit = 1",
            "SET @@autocommit = 1",
            "SET @@session.autocommit = 1",
            "LOCK TABLES t WRITE",
            "UNLOCK TABLES",
            "CALL p()",
            "GRANT SELECT ON db.* TO u",
            "REVOKE SELECT ON db.* FROM u",
            "ANALYZE TABLE t",
            "FLUSH TABLES",
            "RESET MASTER",
        ] {
            assert!(facts(SqlEngine::Mysql, sql).implicit_commit, "{sql}");
        }
        // MySQL `OPTIMIZE TABLE`, `REPAIR TABLE` and `LOAD DATA` do not parse under this
        // dialect, so they reach the callers as unparsable text instead of a fact.
        for sql in [
            "OPTIMIZE TABLE t",
            "REPAIR TABLE t",
            "LOAD DATA INFILE 'f' INTO TABLE t",
        ] {
            assert!(analyze(SqlEngine::Mysql, sql).is_err(), "{sql}");
        }
        for sql in [
            "SELECT * FROM t",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1 WHERE id = 2",
            "DELETE FROM t WHERE id = 3",
            "SET FOREIGN_KEY_CHECKS = 0",
            "SET @x = 1",
            "SET SESSION sql_mode = ''",
            "SET GLOBAL autocommit = 1",
            "SET @@global.autocommit = 1",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
            "USE db",
        ] {
            assert!(!facts(SqlEngine::Mysql, sql).implicit_commit, "{sql}");
        }
        // DDL is transactional outside MySQL, so the fact stays false there.
        assert!(!facts(SqlEngine::Pg, "CREATE TABLE t (id int)").implicit_commit);
        assert!(!facts(SqlEngine::Sqlite, "DROP TABLE t").implicit_commit);
    }

    #[test]
    fn ddl_kinds() {
        for k in [
            StatementKind::Create,
            StatementKind::Alter,
            StatementKind::Drop,
            StatementKind::Truncate,
            StatementKind::Rename,
        ] {
            assert!(k.is_ddl(), "{k:?}");
        }
        for k in [
            StatementKind::Query,
            StatementKind::Insert,
            StatementKind::Other,
        ] {
            assert!(!k.is_ddl(), "{k:?}");
        }
    }

    #[test]
    fn nested_writes_are_found() {
        for sql in [
            "SELECT 1 UNION (DELETE FROM t RETURNING 1)",
            "SELECT * FROM (DELETE FROM t RETURNING *) d",
            "WITH a AS (WITH b AS (DELETE FROM t RETURNING *) SELECT * FROM b) SELECT * FROM a",
            "WITH a AS (SELECT 1 UNION ALL (UPDATE t SET x = 1 RETURNING 1)) SELECT * FROM a",
        ] {
            let f = analyze(SqlEngine::Pg, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert!(f.statements[0].has_modifying_cte, "{sql}");
        }
    }

    #[test]
    fn labels_targets_and_where() {
        let f = facts(SqlEngine::Pg, "CREATE OR REPLACE TEMP VIEW v AS SELECT 1");
        assert_eq!(
            (f.kind, f.label.as_str()),
            (StatementKind::Create, "CREATE VIEW")
        );
        assert_eq!(f.target.as_deref(), Some("v"));
        let f = facts(SqlEngine::Pg, "CREATE MATERIALIZED VIEW m AS SELECT 1");
        assert_eq!(f.label, "CREATE MATERIALIZED VIEW");
        let f = facts(SqlEngine::Pg, "DROP TABLE IF EXISTS a, s.b");
        assert_eq!(
            (f.kind, f.label.as_str()),
            (StatementKind::Drop, "DROP TABLE")
        );
        assert_eq!(f.target.as_deref(), Some("a, s.b"));
        let f = facts(SqlEngine::Pg, "UPDATE s.t SET a = 1");
        assert_eq!(
            (f.target.as_deref(), f.has_where),
            (Some("s.t"), Some(false))
        );
        let f = facts(SqlEngine::Pg, "DELETE FROM t WHERE id = 1");
        assert_eq!((f.target.as_deref(), f.has_where), (Some("t"), Some(true)));
        let f = facts(SqlEngine::Mysql, "ALTER TABLE t DROP COLUMN a");
        assert_eq!((f.kind, f.alter_drops), (StatementKind::Alter, true));
        assert!(!facts(SqlEngine::Mysql, "ALTER TABLE t ADD COLUMN b int").alter_drops);
        assert_eq!(facts(SqlEngine::Pg, "SET ROLE admin").label, "SET");
        assert_eq!(facts(SqlEngine::Pg, "COPY t TO PROGRAM 'x'").label, "COPY");
        assert_eq!(facts(SqlEngine::Mysql, "DESC t").label, "DESCRIBE");
        let f = facts(SqlEngine::Pg, "EXPLAIN DELETE FROM t");
        assert_eq!(f.explained.as_deref(), Some(&StatementKind::Delete));
    }

    #[test]
    fn sqlite_pragmas() {
        let p = |sql: &str| facts(SqlEngine::Sqlite, sql).pragma.unwrap();
        assert_eq!(
            p("PRAGMA table_info(t)"),
            Pragma {
                name: "table_info".into(),
                form: PragmaForm::Call
            }
        );
        assert_eq!(p("pragma main.Index_List('t');").name, "index_list");
        assert_eq!(p("PRAGMA journal_mode = delete").form, PragmaForm::Assign);
        assert_eq!(p("PRAGMA cache_size = -2000").form, PragmaForm::Assign);
        assert_eq!(p("PRAGMA writable_schema=1").form, PragmaForm::Assign);
        assert_eq!(p("PRAGMA user_version").form, PragmaForm::Read);
        assert!(analyze(SqlEngine::Sqlite, "PRAGMA table_info(t) junk").is_err());
        // A second statement goes to the normal parser, which sees both statements or fails.
        assert!(
            analyze(SqlEngine::Sqlite, "PRAGMA table_info('t'); DELETE FROM t")
                .is_ok_and(|f| f.statement_count() == 2)
        );
    }

    #[test]
    fn optimizer_hints_are_seen_in_comments_only() {
        assert!(
            analyze(SqlEngine::Mysql, "SELECT /*+ MAX_EXECUTION_TIME(0) */ 1")
                .unwrap()
                .has_optimizer_hint
        );
        assert!(
            analyze(SqlEngine::Pg, "/*+ Set(statement_timeout 0) */ SELECT 1")
                .unwrap()
                .has_optimizer_hint
        );
        assert!(
            !analyze(SqlEngine::Pg, "SELECT '/*+ x */'")
                .unwrap()
                .has_optimizer_hint
        );
        assert!(
            !analyze(SqlEngine::Pg, "SELECT 1 /* plain */")
                .unwrap()
                .has_optimizer_hint
        );
    }

    #[test]
    fn plain_reads() {
        for (e, sql) in [
            (SqlEngine::Pg, "SELECT 1"),
            (SqlEngine::Pg, "with x as (select 1) select * from x"),
            (SqlEngine::Pg, "SELECT 1 UNION SELECT 2"),
            (SqlEngine::Pg, "VALUES (1)"),
            (
                SqlEngine::Pg,
                "SELECT c.name, count(o.id) AS n, sum(o.total), \
                 row_number() OVER (ORDER BY count(o.id) DESC) AS r, \
                 lag(sum(o.total)) OVER (ORDER BY c.name) \
                 FROM customers c LEFT JOIN orders o ON o.customer_id = c.id \
                 WHERE o.created_at > now() - interval '1 day' AND lower(c.email) LIKE '%@x' \
                 GROUP BY c.name HAVING count(*) > 1 ORDER BY 2 DESC",
            ),
            (SqlEngine::Pg, "SELECT * FROM generate_series(1, 10) g"),
            // qualified columns on real tables and aliases stay plain reads
            (
                SqlEngine::Pg,
                "SELECT c.name, o.id FROM customers c JOIN orders o ON o.customer_id = c.id",
            ),
            (SqlEngine::Pg, "SELECT a.b.c FROM analytics.events a"),
            (SqlEngine::Pg, "SELECT x.unnest FROM unnest(ARRAY[1, 2]) x"),
            (
                SqlEngine::Pg,
                "SELECT u.generate_series FROM generate_series(1, 5) u",
            ),
            (
                SqlEngine::Pg,
                "SELECT d.a FROM (SELECT id AS a FROM customers) d",
            ),
            (SqlEngine::Pg, "SELECT v.x FROM (VALUES (1, 'a')) v(x, y)"),
            // the implicit unnest alias names its own row column
            (SqlEngine::Pg, "SELECT unnest.unnest FROM unnest(ARRAY[1])"),
            // a PG SRF's real record columns stay readable
            (SqlEngine::Pg, "SELECT e.key FROM jsonb_each('{}') e"),
            // column references that match the declared name exactly, quoted or not
            (
                SqlEngine::Pg,
                "SELECT x.upper FROM unnest(ARRAY['abc']) AS x(\"upper\")",
            ),
            (
                SqlEngine::Pg,
                "SELECT x.\"UPPER\" FROM unnest(ARRAY['abc']) AS x(\"UPPER\")",
            ),
            // CTE/derived/VALUES aliases address rows: any field name is allowed
            (
                SqlEngine::Pg,
                "WITH x AS (SELECT * FROM orders) SELECT x.id FROM x",
            ),
            (
                SqlEngine::Pg,
                "SELECT s.customer_id FROM (SELECT customer_id, sum(total) FROM orders GROUP BY 1) s",
            ),
            (
                SqlEngine::Pg,
                "SELECT o.id, c.customer_id FROM orders o JOIN (SELECT customer_id, count(*) FROM orders GROUP BY 1) c ON o.customer_id = c.customer_id",
            ),
            (
                SqlEngine::Pg,
                "SELECT o.id FROM (SELECT o.*, 1 AS k FROM orders o) o",
            ),
            (SqlEngine::Pg, "SELECT t.column1 FROM (VALUES (1)) t"),
            (
                SqlEngine::Pg,
                "SELECT n.i FROM (SELECT i FROM generate_series(1, 5) i) n",
            ),
            (
                SqlEngine::Pg,
                "SELECT CAST(a AS text), EXTRACT(year FROM d) FROM t",
            ),
            (SqlEngine::Pg, "SHOW search_path"),
            (SqlEngine::Pg, "EXPLAIN SELECT * FROM t"),
            (SqlEngine::Mysql, "SHOW TABLES"),
            (SqlEngine::Mysql, "DESCRIBE t"),
            (
                SqlEngine::Mysql,
                "SELECT CONCAT(a, '\\n', b), GROUP_CONCAT(c) FROM t",
            ),
            (SqlEngine::Mysql, "SELECT 1 /* plain comment */"),
            (SqlEngine::Sqlite, "SELECT * FROM t"),
            (SqlEngine::Sqlite, "SELECT 'a' || char(10) || 'b'"),
        ] {
            assert!(is_plain_read_sql(e, sql), "{sql}");
        }
    }

    #[test]
    fn not_plain_reads() {
        for (e, sql) in [
            // functions with side effects or session state
            (SqlEngine::Pg, "SELECT nextval('s')"),
            (
                SqlEngine::Pg,
                "SELECT nextval('s') FROM generate_series(1, 1000)",
            ),
            (SqlEngine::Pg, "SELECT set_config('search_path', 'x', false)"),
            (SqlEngine::Pg, "SELECT pg_advisory_lock(1)"),
            (SqlEngine::Mysql, "SELECT GET_LOCK('k', 0)"),
            (SqlEngine::Pg, "SELECT my_func()"),
            (SqlEngine::Pg, "SELECT * FROM my_set_returning_func()"),
            (SqlEngine::Pg, "SELECT public.lower(a) FROM t"),
            (SqlEngine::Mysql, "SELECT @v := 1"),
            // PG attribute notation calls a function the functions list never sees
            (
                SqlEngine::Pg,
                "SELECT x.pg_terminate_backend FROM pg_stat_activity a, unnest(ARRAY[a.pid]) x",
            ),
            (
                SqlEngine::Pg,
                "SELECT g.pg_terminate_backend FROM generate_series(1, 100000) g",
            ),
            (
                SqlEngine::Pg,
                "SELECT g.pg_sleep FROM unnest(ARRAY[10::float8]) g",
            ),
            (
                SqlEngine::Pg,
                "SELECT f.pg_read_file FROM unnest(ARRAY['/etc/passwd']) f",
            ),
            (
                SqlEngine::Pg,
                "SELECT (g).pg_cancel_backend FROM generate_series(1, 10) g",
            ),
            (
                SqlEngine::Pg,
                "SELECT d.pg_sleep FROM (SELECT pid FROM pg_stat_activity) d",
            ),
            (SqlEngine::Pg, "SELECT c.pg_sleep FROM (VALUES (1)) c(x)"),
            // dot steps on a non-column root call a one-argument function on the value
            (
                SqlEngine::Pg,
                "SELECT ('SELECT to_tsvector(pg_cancel_backend(0)::text)'::text).ts_stat",
            ),
            (
                SqlEngine::Pg,
                "SELECT (x.c).ts_stat FROM unnest(ARRAY[$$SELECT to_tsvector(pg_cancel_backend(0)::text)$$]) AS x(c)",
            ),
            (SqlEngine::Pg, "SELECT (CAST('q' AS text)).ts_stat"),
            // a CTE name shadowed by a row-function alias, and the implicit unnest alias
            (
                SqlEngine::Pg,
                "WITH x AS (SELECT 1) SELECT x.gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) x",
            ),
            (
                SqlEngine::Pg,
                "WITH x AS (SELECT 1) SELECT x.http_get FROM unnest(ARRAY['http://e']) x",
            ),
            (
                SqlEngine::Pg,
                "SELECT unnest.http_get FROM unnest(ARRAY['http://e'])",
            ),
            (
                SqlEngine::Pg,
                "WITH x AS (SELECT 1) SELECT x.upper FROM unnest(ARRAY['abc']) x",
            ),
            // quoted/unquoted and ASCII/non-ASCII case variants of the same alias
            (SqlEngine::Pg, "SELECT \"É\".upper FROM unnest(ARRAY['abc']) É"),
            (SqlEngine::Pg, "SELECT É.upper FROM unnest(ARRAY['abc']) \"É\""),
            (
                SqlEngine::Pg,
                "SELECT \"É\".gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) É",
            ),
            (SqlEngine::Pg, "SELECT \"X\".upper FROM unnest(ARRAY['abc']) x"),
            // an explicit column list wins over the SRF's own columns on PG
            (SqlEngine::Pg, "SELECT e.path FROM jsonb_each('{}') AS e(k, v)"),
            // a quoted column name does not answer to a differently-cased field reference
            (
                SqlEngine::Pg,
                "SELECT x.upper FROM unnest(ARRAY['abc']) AS x(\"UPPER\")",
            ),
            (
                SqlEngine::Pg,
                "SELECT x.gin_clean_pending_list FROM unnest(ARRAY['orders_pkey'::regclass]) AS x(\"GIN_CLEAN_PENDING_LIST\")",
            ),
            (
                SqlEngine::Pg,
                "SELECT x.upper FROM generate_series(1,2) AS x(\"Upper\")",
            ),
            // locking, INTO, data-modifying CTE
            (SqlEngine::Pg, "SELECT * FROM t FOR UPDATE"),
            (SqlEngine::Pg, "SELECT * FROM (SELECT * FROM t FOR UPDATE) s"),
            (
                SqlEngine::Pg,
                "SELECT * FROM t WHERE id IN (SELECT id FROM u FOR SHARE)",
            ),
            (SqlEngine::Mysql, "SELECT * FROM t LOCK IN SHARE MODE"),
            (SqlEngine::Pg, "SELECT * INTO t2 FROM t"),
            (SqlEngine::Mysql, "SELECT a INTO @v FROM t"),
            (SqlEngine::Mysql, "SELECT a FROM t INTO OUTFILE '/tmp/x'"),
            (
                SqlEngine::Pg,
                "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            ),
            (SqlEngine::Pg, "SELECT 1 UNION (DELETE FROM t RETURNING 1)"),
            // EXPLAIN that runs the statement, or explains a write
            (SqlEngine::Pg, "EXPLAIN ANALYZE SELECT * FROM t"),
            (SqlEngine::Pg, "EXPLAIN (ANALYZE) DELETE FROM t"),
            (SqlEngine::Mysql, "EXPLAIN ANALYZE SELECT * FROM t"),
            (SqlEngine::Pg, "EXPLAIN DELETE FROM t"),
            // writes, session state, several statements, unparsable text
            (SqlEngine::Pg, "UPDATE t SET a = 1 RETURNING a"),
            (SqlEngine::Pg, "SET search_path TO analytics"),
            (SqlEngine::Pg, "BEGIN"),
            (SqlEngine::Pg, "SELECT 1; SELECT 2"),
            (SqlEngine::Mysql, "USE analytics"),
            (SqlEngine::Sqlite, "PRAGMA journal_mode = WAL"),
            (SqlEngine::Pg, "SELEC 1"),
            (
                SqlEngine::Pg,
                "DO $$ BEGIN PERFORM 1; END $$; SELECT 1 AS a; SELECT 'x' AS b, 'y' AS c",
            ),
        ] {
            assert!(!is_plain_read_sql(e, sql), "{sql}");
        }
    }

    /// sqlparser's MySQL tokenizer does not treat `/*! … */` as a comment: it expands the body
    /// into tokens, so the parser sees `SELECT 1; DELETE FROM t`. MySQL runs that body too.
    /// Other bodies (`/*!50000 FOR UPDATE */`, MariaDB `/*M! … */`) may not be visible to the
    /// parser at all, so any executable comment makes the text "not a plain read".
    #[test]
    fn mysql_executable_comments() {
        let f = analyze(SqlEngine::Mysql, "SELECT 1 /*!; DELETE FROM t */").unwrap();
        assert_eq!(f.statement_count(), 2);
        assert_eq!(f.statements[1].kind, StatementKind::Delete);
        assert!(f.has_executable_comment);
        assert!(!is_plain_read(&f));

        for sql in [
            "SELECT * FROM t /*!50000 FOR UPDATE */",
            "SELECT 1 /*!*/",
            "SELECT 1 /*M! , GET_LOCK('k', 0) */",
        ] {
            let f = analyze(SqlEngine::Mysql, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert!(f.has_executable_comment, "{sql}");
            assert!(!is_plain_read(&f), "{sql}");
        }
        // Not an executable comment for PG/SQLite.
        assert!(is_plain_read_sql(SqlEngine::Pg, "SELECT 1 /*! x */"));
    }

    /// The row count, not the offset, sizes the first page: `LIMIT 10, 500` asks for 500 rows.
    #[test]
    fn row_limit_is_the_count_of_the_last_query() {
        assert_eq!(row_limit(SqlEngine::Pg, "SELECT * FROM t LIMIT 500 OFFSET 10"), Some(500));
        assert_eq!(row_limit(SqlEngine::Mysql, "SELECT * FROM t LIMIT 10, 500"), Some(500));
        assert_eq!(row_limit(SqlEngine::Sqlite, "SELECT 1 LIMIT 5; SELECT 2 LIMIT 700"), Some(700));
        assert_eq!(row_limit(SqlEngine::Pg, "SELECT * FROM t LIMIT ALL"), None);
        assert_eq!(row_limit(SqlEngine::Pg, "SELECT * FROM (SELECT 1 LIMIT 900) s"), None);
        assert_eq!(row_limit(SqlEngine::Pg, "DELETE FROM t"), None);
    }
}
