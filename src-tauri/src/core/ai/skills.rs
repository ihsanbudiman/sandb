//! Built-in skills: instructions for one dialect or one kind of task. The system prompt lists the
//! skills that fit the chat's engines by name and description, and the model loads one with
//! `load_skill`. The skills the user picks go into the system prompt in full.

use crate::core::engine::Engine;
use crate::core::error::AppError;
use crate::core::result::Source;

pub struct Skill {
    /// The id: `load_skill` argument, request field and `/` menu key.
    pub name: &'static str,
    /// For the UI and the step summary.
    pub title: &'static str,
    /// When to use it. The model decides from this line alone, so it names the trigger.
    pub description: &'static str,
    /// The skill fits a chat with a source of one of these engines.
    pub engines: &'static [Engine],
    pub body: &'static str,
}

const ALL: &[Engine] = &[
    Engine::Pg,
    Engine::Mysql,
    Engine::Sqlite,
    Engine::Mongo,
    Engine::Redis,
];

pub static SKILLS: [Skill; 14] = [
    Skill {
        name: "postgres",
        title: "PostgreSQL",
        description: "PostgreSQL syntax: quoting, casts, dates and time zones, jsonb, arrays, \
                      FILTER, DISTINCT ON, generate_series.",
        engines: &[Engine::Pg],
        body: include_str!("skills/postgres.md"),
    },
    Skill {
        name: "postgres-tuning",
        title: "PostgreSQL performance",
        description: "Slow PostgreSQL queries: EXPLAIN plan nodes, B-tree, partial, \
                      expression, covering, GIN and BRIN indexes, unused indexes, dead rows \
                      and stale statistics.",
        engines: &[Engine::Pg],
        body: include_str!("skills/postgres-tuning.md"),
    },
    Skill {
        name: "mysql",
        title: "MySQL and MariaDB",
        description: "MySQL and MariaDB syntax: backticks, collations, DATE_FORMAT and \
                      intervals, JSON paths, GROUP_CONCAT, version limits.",
        engines: &[Engine::Mysql],
        body: include_str!("skills/mysql.md"),
    },
    Skill {
        name: "mysql-tuning",
        title: "MySQL performance",
        description: "Slow MySQL and MariaDB queries: EXPLAIN type, key and Extra columns, \
                      composite and covering indexes, filesort, temporary tables, statistics.",
        engines: &[Engine::Mysql],
        body: include_str!("skills/mysql-tuning.md"),
    },
    Skill {
        name: "sqlite",
        title: "SQLite",
        description: "SQLite syntax: loose types, dates stored as text or numbers, strftime, \
                      json_extract, PRAGMA, version limits.",
        engines: &[Engine::Sqlite],
        body: include_str!("skills/sqlite.md"),
    },
    Skill {
        name: "sqlite-tuning",
        title: "SQLite performance",
        description: "Slow SQLite queries: EXPLAIN QUERY PLAN, SCAN against SEARCH, automatic \
                      and covering indexes, type affinity, ANALYZE.",
        engines: &[Engine::Sqlite],
        body: include_str!("skills/sqlite-tuning.md"),
    },
    Skill {
        name: "mongo-aggregation",
        title: "MongoDB aggregation",
        description: "MongoDB pipelines: stage order, $group, date buckets, $unwind, $lookup, \
                      $facet, top N per group, mixed field types.",
        engines: &[Engine::Mongo],
        body: include_str!("skills/mongo-aggregation.md"),
    },
    Skill {
        name: "mongo-tuning",
        title: "MongoDB performance",
        description: "Slow MongoDB queries and pipelines: explain plans, COLLSCAN against \
                      IXSCAN, the ESR index rule, $indexStats, in-memory sorts, collStats.",
        engines: &[Engine::Mongo],
        body: include_str!("skills/mongo-tuning.md"),
    },
    Skill {
        name: "redis",
        title: "Redis",
        description: "Redis commands: key names, SCAN patterns and cursors, data types, \
                      reply columns, and read-only Lua.",
        engines: &[Engine::Redis],
        body: include_str!("skills/redis.md"),
    },
    Skill {
        name: "redis-tuning",
        title: "Redis performance",
        description: "Slow Redis commands and memory use: large keys, MEMORY USAGE, \
                      SLOWLOG GET, LATENCY DOCTOR, INFO and key expiry.",
        engines: &[Engine::Redis],
        body: include_str!("skills/redis-tuning.md"),
    },
    Skill {
        name: "query-tuning",
        title: "Query tuning",
        description: "Slow queries, general method for any database: read a plan, check the \
                      indexes, rewrite. Use the performance skill of the database first when \
                      the list has one.",
        engines: ALL,
        body: include_str!("skills/query-tuning.md"),
    },
    Skill {
        name: "data-quality",
        title: "Data quality",
        description: "Check data quality: missing values, duplicates, orphan rows, values out \
                      of range, inconsistent categories.",
        engines: ALL,
        body: include_str!("skills/data-quality.md"),
    },
    Skill {
        name: "time-series",
        title: "Time series",
        description: "Trends over time: buckets, time zones, missing periods, growth against \
                      the period before, moving averages, cohorts.",
        engines: ALL,
        body: include_str!("skills/time-series.md"),
    },
    Skill {
        name: "schema-review",
        title: "Schema review",
        description: "Review a schema or data model: keys, types, nullability, naming and \
                      indexes, with concrete fixes.",
        engines: ALL,
        body: include_str!("skills/schema-review.md"),
    },
];

/// The skills that fit at least one source of the chat, in catalog order.
pub fn available(sources: &[Source]) -> Vec<&'static Skill> {
    SKILLS
        .iter()
        .filter(|k| sources.iter().any(|s| k.engines.contains(&s.conn.engine)))
        .collect()
}

/// The skills the user picked, in catalog order and without repeats. A name that does not exist
/// or does not fit the chat's databases is an error: the UI offers only the ones that fit.
pub fn picked(names: &[String], sources: &[Source]) -> Result<Vec<&'static Skill>, AppError> {
    let fits = available(sources);
    for name in names {
        if !fits.iter().any(|k| k.name == name) {
            let why = if SKILLS.iter().any(|k| k.name == name) {
                "does not apply to the databases of this chat"
            } else {
                "does not exist"
            };
            return Err(AppError::invalid(format!(
                "The skill {} {why}",
                serde_json::Value::from(name.as_str())
            )));
        }
    }
    Ok(fits
        .into_iter()
        .filter(|k| names.iter().any(|n| n == k.name))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{Connection, Env, SslMode};

    fn source(id: &str, engine: Engine) -> Source {
        Source {
            id: id.into(),
            conn: Connection {
                id: format!("c-{id}"),
                name: id.into(),
                engine,
                version: String::new(),
                env: Env::Local,
                host: "h".into(),
                port: 1,
                database: "shop".into(),
                user: "u".into(),
                ssl: SslMode::Prefer,
                file: String::new(),
                ai_write: true,
                read_only: false,
                uri: String::new(),
                auth_source: String::new(),
                topology: None,
                redis: None,
            },
            database: "shop".into(),
            schema: None,
            allow_writes: false,
        }
    }

    fn names(skills: &[&Skill]) -> Vec<&'static str> {
        skills.iter().map(|k| k.name).collect()
    }

    #[test]
    fn a_chat_gets_only_the_skills_of_its_engines() {
        let pg = [source("db1", Engine::Pg)];
        assert_eq!(
            names(&available(&pg)),
            [
                "postgres",
                "postgres-tuning",
                "query-tuning",
                "data-quality",
                "time-series",
                "schema-review"
            ]
        );
        let mixed = [source("db1", Engine::Mysql), source("db2", Engine::Mongo)];
        assert_eq!(
            names(&available(&mixed)),
            [
                "mysql",
                "mysql-tuning",
                "mongo-aggregation",
                "mongo-tuning",
                "query-tuning",
                "data-quality",
                "time-series",
                "schema-review"
            ]
        );
    }

    #[test]
    fn redis_skills_fit_redis_alone_and_with_other_sources() {
        let redis = [source("db1", Engine::Redis)];
        assert_eq!(
            names(&available(&redis)),
            [
                "redis",
                "redis-tuning",
                "query-tuning",
                "data-quality",
                "time-series",
                "schema-review"
            ]
        );
        let mixed = [
            source("db1", Engine::Redis),
            source("db2", Engine::Pg),
            source("db3", Engine::Mongo),
        ];
        assert_eq!(
            names(&available(&mixed)),
            [
                "postgres",
                "postgres-tuning",
                "mongo-aggregation",
                "mongo-tuning",
                "redis",
                "redis-tuning",
                "query-tuning",
                "data-quality",
                "time-series",
                "schema-review"
            ]
        );
        let selection = ["redis-tuning".into()];
        assert_eq!(
            names(&picked(&selection, &redis).unwrap()),
            ["redis-tuning"]
        );
        let err = picked(&selection, &[source("db2", Engine::Pg)])
            .err()
            .unwrap();
        assert!(err.message.contains("does not apply"));
    }

    #[test]
    fn picked_skills_must_exist_and_fit_the_chat() {
        let pg = [source("db1", Engine::Pg)];
        let names_of = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Catalog order, repeats dropped.
        let got = picked(&names_of(&["time-series", "postgres", "postgres"]), &pg).unwrap();
        assert_eq!(names(&got), ["postgres", "time-series"]);
        let e = picked(&names_of(&["mongo-aggregation"]), &pg).err().unwrap();
        assert!(e.message.contains("does not apply"), "{}", e.message);
        let e = picked(&names_of(&["nope"]), &pg).err().unwrap();
        assert!(e.message.contains("does not exist"), "{}", e.message);
    }

    #[test]
    fn names_are_unique_and_every_skill_has_text() {
        for (i, k) in SKILLS.iter().enumerate() {
            assert!(
                SKILLS[i + 1..].iter().all(|o| o.name != k.name),
                "{}",
                k.name
            );
            assert!(!k.engines.is_empty() && !k.body.trim().is_empty(), "{}", k.name);
        }
    }
}
