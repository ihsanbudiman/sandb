//! The AI read and approved-write paths for MongoDB. The tools send one mongosh statement as
//! text; this module parses it once more, applies the same guard the editor uses, and runs it
//! through the read stream of `run.rs` or the write path of `write.rs`.

use std::time::{Duration, Instant};

use mongodb::bson::{Bson, Document, doc};
use tokio_util::sync::CancellationToken;

use crate::core::connections::{Connection, target_database};
use crate::core::error::{AppError, ErrorCode};
use crate::core::mongo::client::Access;
use crate::core::mongo::command::{Command, Find, Method, Statement};
use crate::core::mongo::comment::OpComment;
use crate::core::mongo::conn::{MAX_TIME_MS_EXPIRED, server_code};
use crate::core::mongo::flatten::Page;
use crate::core::mongo::guard::{self, Kind};
use crate::core::mongo::options::whole_number;
use crate::core::mongo::{client, parse, run, write};
use crate::core::result::{AiReadResult, GRACE, ReadLimits, WriteInfo, WriteResult, timeout_error};

pub(crate) fn one_statement(text: &str) -> Result<Statement, AppError> {
    let stmts = parse::parse(text).map_err(AppError::from)?;
    guard::exactly_one(&stmts)
        .cloned()
        .map_err(AppError::invalid)
}

/// The write the assistant proposes: exactly one statement, not `use`, a write rather than a
/// read, classified for the preview the user sees. The write parses again when it runs.
pub fn propose(text: &str, database: &str) -> Result<WriteInfo, AppError> {
    let s = one_statement(text)?;
    if matches!(s.command, Command::Use(_)) {
        return Err(AppError::invalid(
            "\"use\" is not a write: the statement runs on the source's database",
        ));
    }
    if guard::kind(&s.command) == Kind::Read {
        return Err(AppError::invalid("This is a read. Use run_mongo_read."));
    }
    Ok(guard::classify_write(text, Some(database)))
}

/// The read guard of an AI read: exactly one statement, not `use`, and a provable read by the
/// phase 3 rule (`guard::kind`, fail closed). Every refusal is `guard_rejected`.
pub(crate) fn read_statement(text: &str) -> Result<Statement, AppError> {
    let rejected = |reason: String| {
        AppError::new(
            ErrorCode::GuardRejected,
            format!("Blocked by the read-only guard: {reason}"),
        )
    };
    let s = one_statement(text).map_err(|e| rejected(e.message))?;
    if matches!(s.command, Command::Use(_)) {
        return Err(rejected(
            "\"use\" is not allowed: the statement runs on the source's database".into(),
        ));
    }
    if guard::kind(&s.command) != Kind::Read {
        return Err(rejected(guard::reason(&s.command)));
    }
    Ok(s)
}

/// 0 means "no limit" to the server, so a zero `maxTimeMS` becomes `ms` too.
fn capped_ms(user: Option<i64>, ms: i64) -> i64 {
    user.filter(|u| *u > 0).map_or(ms, |u| u.min(ms))
}

fn whole(v: Option<&Bson>) -> Option<i64> {
    whole_number(v?, 0, i64::MAX)
}

fn ms_options(options: &mut Option<Document>, ms: i64) {
    let o = options.get_or_insert_with(Document::new);
    let user = whole(o.get("maxTimeMS"));
    o.insert("maxTimeMS", capped_ms(user, ms));
}

fn limit_value(stage: &Document) -> Option<u64> {
    whole(stage.get("$limit")).map(|n| n as u64)
}

fn with_read_limits(cmd: Command, cap: usize, timeout: Duration) -> Command {
    let ms = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    let cap = i64::try_from(cap).unwrap_or(i64::MAX);
    match cmd {
        Command::Collection {
            collection,
            method: Method::FindOne { filter, projection },
        } => with_read_limits(
            Command::Collection {
                collection,
                method: Method::Find(Find {
                    filter,
                    projection,
                    limit: Some(1),
                    ..Find::default()
                }),
            },
            usize::try_from(cap).unwrap_or(usize::MAX),
            timeout,
        ),
        Command::Collection {
            collection,
            method: Method::Find(mut f),
        } => {
            // A missing, zero, negative or over-cap user limit becomes the cap; a smaller one
            // stays. `.count()` returns one number, so its limit stays the user's.
            if !f.count && f.limit.is_none_or(|n| n <= 0 || n > cap) {
                f.limit = Some(cap);
            }
            let user = f.max_time_ms.map(|u| i64::try_from(u).unwrap_or(i64::MAX));
            f.max_time_ms = Some(capped_ms(user, ms) as u64);
            Command::Collection {
                collection,
                method: Method::Find(f),
            }
        }
        Command::Collection {
            collection,
            method:
                Method::Aggregate {
                    mut pipeline,
                    mut options,
                },
        } => {
            let last_is_small_limit = pipeline
                .last()
                .and_then(limit_value)
                .is_some_and(|n| n <= cap as u64);
            if !last_is_small_limit {
                pipeline.push(doc! { "$limit": cap });
            }
            ms_options(&mut options, ms);
            Command::Collection {
                collection,
                method: Method::Aggregate { pipeline, options },
            }
        }
        Command::Collection {
            collection,
            method:
                Method::CountDocuments {
                    filter,
                    mut options,
                },
        } => {
            ms_options(&mut options, ms);
            Command::Collection {
                collection,
                method: Method::CountDocuments { filter, options },
            }
        }
        Command::RunCommand { admin, mut command } => {
            // The guard already proved the command reads. Every command of its read list takes
            // `maxTimeMS` on the local MongoDB 8 replica set and standalone (the sweep in
            // `tests/it_ai_mongo.rs`); an `explain` gets it on the outer command.
            if guard::run_command_reads(&command) {
                let user = whole(command.get("maxTimeMS"));
                command.insert("maxTimeMS", capped_ms(user, ms));
            }
            Command::RunCommand { admin, command }
        }
        other => other,
    }
}

/// `run_ai_read` for a MongoDB connection.
pub async fn read(
    access: &impl Access,
    conn: &Connection,
    database: Option<&str>,
    text: &str,
    limits: ReadLimits,
) -> Result<AiReadResult, AppError> {
    let s = read_statement(text)?;
    let database = target_database(conn, database);
    if database.is_empty() {
        return Err(AppError::invalid("Choose a database for this source"));
    }
    let client = client::client(access, &conn.id).await?;
    let started = Instant::now();
    let limits = limits.sanitized();
    let max = limits.max_rows;
    // One document more than the page tells whether more exist.
    let cmd = with_read_limits(s.command, max + 1, limits.timeout);
    let query_id = uuid::Uuid::new_v4().to_string();
    let batch = u32::try_from(max + 1).ok();
    let work = async {
        let mut stream = run::open_read(
            &client,
            &database,
            &cmd,
            None,
            OpComment::new(&query_id),
            batch,
            Some(limits.timeout),
        )
        .await?;
        run::page(&mut stream, max).await
    };
    // The server's `maxTimeMS` is `limits.timeout`; this net fires a little later and kills the
    // op through its `comment`, like the editor's cancel.
    let (page, end) = match tokio::time::timeout(limits.timeout + GRACE, work).await {
        Ok(r) => r.map_err(|e| time_limit(e, limits.timeout))?,
        Err(_) => {
            let _ = tokio::time::timeout(GRACE * 2, run::kill(access, &conn.id, &query_id)).await;
            return Err(timeout_error(limits.timeout));
        }
    };
    let Page {
        columns,
        rows,
        documents,
        ..
    } = page;
    Ok(AiReadResult {
        columns,
        rows,
        truncated: end != run::PageEnd::Stream,
        elapsed_ms: started.elapsed().as_millis() as u64,
        documents: Some(documents),
    })
}

/// The server's `MaxTimeMSExpired` (code 50) is the AI read's time limit: the same `timeout`
/// error as the client-side net.
fn time_limit(e: AppError, limit: Duration) -> AppError {
    if server_code(&e) == Some(MAX_TIME_MS_EXPIRED) {
        timeout_error(limit)
    } else {
        e
    }
}

/// `db::write::run_approved` for a MongoDB connection. One statement is atomic on one document
/// only.
pub async fn run_approved(
    access: &impl Access,
    conn: &Connection,
    database: &str,
    text: &str,
    stop: &CancellationToken,
) -> Result<WriteResult, AppError> {
    let s = one_statement(text)?;
    if matches!(s.command, Command::Use(_)) {
        return Err(AppError::invalid(
            "\"use\" is not allowed: the statement runs on the source's database",
        ));
    }
    if database.is_empty() {
        return Err(AppError::invalid("Choose a database for this source"));
    }
    if stop.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let started = Instant::now();
    let client = client::client(access, &conn.id).await?;
    let query_id = uuid::Uuid::new_v4().to_string();
    let comment = OpComment::new(&query_id);
    let affected = match &s.command {
        Command::RunCommand { admin, command } => {
            let mut command = command.clone();
            comment.apply(&mut command);
            let target = if *admin { "admin" } else { database };
            let reply = client.database(target).run_command(command).await?;
            command_affected(&reply)?
        }
        cmd if matches!(guard::kind(cmd), Kind::Write | Kind::Schema) => {
            write::execute(&client, database, cmd, comment)
                .await?
                .affected
        }
        cmd => {
            let mut stream =
                run::open_read(&client, database, cmd, None, comment, None, None).await?;
            while stream.next().await?.is_some() {}
            0
        }
    };
    Ok(WriteResult {
        affected,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// A write command replies `ok: 1` even when it wrote nothing: its `writeErrors` and
/// `writeConcernError` become the same `Database` error as a refused write elsewhere. The
/// changed count is `nModified` (update) or `n`, else 0.
fn command_affected(reply: &Document) -> Result<u64, AppError> {
    if let Some(error) = crate::core::mongo::conn::reply_write_error(reply) {
        return Err(error);
    }
    let n = whole(reply.get("nModified")).or_else(|| whole(reply.get("n")));
    Ok(n.map_or(0, |n| n as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::Engine;
    use crate::core::mongo::client::MongoClients;
    use crate::core::query_registry::QueryRegistry;
    use crate::core::secrets::{MemoryStore, SecretStore};

    struct TestAccess {
        conn: Connection,
        clients: MongoClients,
        secrets: MemoryStore,
        queries: QueryRegistry,
    }

    impl Access for TestAccess {
        fn connection(&self, _id: &str) -> Result<Connection, AppError> {
            Ok(self.conn.clone())
        }

        fn mongo(&self) -> &MongoClients {
            &self.clients
        }

        fn secrets(&self) -> &dyn SecretStore {
            &self.secrets
        }

        fn queries(&self) -> &QueryRegistry {
            &self.queries
        }
    }

    fn access() -> TestAccess {
        TestAccess {
            conn: mongo_conn(),
            clients: MongoClients::default(),
            secrets: MemoryStore::new(),
            queries: QueryRegistry::default(),
        }
    }

    fn command(text: &str) -> Command {
        one_statement(text).unwrap().command
    }

    fn limited(text: &str, cap: usize) -> Command {
        with_read_limits(command(text), cap, Duration::from_secs(7))
    }

    fn find(cmd: Command) -> crate::core::mongo::command::Find {
        match cmd {
            Command::Collection {
                method: Method::Find(f),
                ..
            } => f,
            other => panic!("{other:?}"),
        }
    }

    fn aggregate(cmd: Command) -> (Vec<Document>, Option<Document>) {
        match cmd {
            Command::Collection {
                method: Method::Aggregate { pipeline, options },
                ..
            } => (pipeline, options),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn find_gets_the_row_cap_and_max_time() {
        let f = find(limited("db.orders.find({})", 101));
        assert_eq!((f.limit, f.max_time_ms), (Some(101), Some(7000)));
        // Over the cap and zero (no limit) become the cap.
        for text in ["db.orders.find().limit(500)", "db.orders.find().limit(0)"] {
            assert_eq!(find(limited(text, 101)).limit, Some(101), "{text}");
        }
        // A smaller user limit and the user's maxTimeMS win.
        let f = find(limited("db.orders.find().limit(5).maxTimeMS(500)", 101));
        assert_eq!((f.limit, f.max_time_ms), (Some(5), Some(500)));
        // `.count()` keeps its own limit: the cap would cut the count.
        let f = find(limited("db.orders.find().count()", 101));
        assert_eq!(f.limit, None);
    }

    #[test]
    fn aggregate_gets_a_limit_stage_and_max_time() {
        let (pipeline, options) = aggregate(limited(
            r#"db.orders.aggregate([{ $match: { status: "paid" } }])"#,
            11,
        ));
        assert_eq!(pipeline.len(), 2);
        assert_eq!(pipeline[1], doc! { "$limit": 11_i64 });
        assert_eq!(options.unwrap(), doc! { "maxTimeMS": 7000_i64 });

        // A last $limit at or under the cap is kept, and the user's maxTimeMS wins.
        let (pipeline, options) = aggregate(limited(
            "db.orders.aggregate([{ $limit: 5 }], { maxTimeMS: 50 })",
            11,
        ));
        assert_eq!(pipeline.len(), 1);
        assert_eq!(options.unwrap().get("maxTimeMS"), Some(&Bson::Int64(50)));

        // A larger last $limit gets the cap after it; a larger or zero maxTimeMS is capped.
        for text in [
            "db.orders.aggregate([{ $limit: 50 }], { maxTimeMS: 3600000 })",
            "db.orders.aggregate([{ $limit: 50 }], { maxTimeMS: 0 })",
        ] {
            let (pipeline, options) = aggregate(limited(text, 11));
            assert_eq!(pipeline.len(), 2);
            assert_eq!(pipeline[1], doc! { "$limit": 11_i64 });
            assert_eq!(options.unwrap(), doc! { "maxTimeMS": 7000_i64 }, "{text}");
        }
    }

    #[test]
    fn every_time_limit_is_capped() {
        for text in [
            "db.orders.find().maxTimeMS(3600000)",
            "db.orders.find().maxTimeMS(0)",
        ] {
            assert_eq!(find(limited(text, 101)).max_time_ms, Some(7000), "{text}");
        }
        // findOne becomes a find with limit 1 and the time limit.
        let f = find(limited(r#"db.orders.findOne({ a: 1 }, { b: 1 })"#, 101));
        assert_eq!(f.limit, Some(1));
        assert_eq!(f.max_time_ms, Some(7000));
        assert_eq!(f.filter, doc! { "a": 1 });
        assert_eq!(f.projection, Some(doc! { "b": 1 }));
        let Command::Collection {
            method: Method::CountDocuments { options, .. },
            ..
        } = limited("db.orders.countDocuments({}, { maxTimeMS: 0 })", 101)
        else {
            unreachable!()
        };
        assert_eq!(options.unwrap(), doc! { "maxTimeMS": 7000_i64 });
    }

    #[test]
    fn run_command_reads_get_max_time() {
        let run_command = |cmd: Command| match cmd {
            Command::RunCommand { command, .. } => command,
            other => panic!("{other:?}"),
        };
        for text in [
            "db.runCommand({ ping: 1 })",
            "db.runCommand({ buildInfo: 1 })",
            "db.adminCommand({ serverStatus: 1 })",
            "db.runCommand({ dbStats: 1 })",
            "db.runCommand({ collStats: 'orders' })",
            "db.runCommand({ listCollections: 1 })",
            "db.runCommand({ listIndexes: 'orders' })",
            "db.runCommand({ explain: { find: 'orders' } })",
        ] {
            let d = run_command(limited(text, 101));
            assert_eq!(d.get("maxTimeMS"), Some(&Bson::Int64(7000)), "{text}");
        }
        // The user's smaller value wins; zero and a larger value are capped.
        let d = run_command(limited(
            "db.runCommand({ listCollections: 1, maxTimeMS: 50 })",
            101,
        ));
        assert_eq!(d.get("maxTimeMS"), Some(&Bson::Int64(50)));
        for text in [
            "db.runCommand({ dbStats: 1, maxTimeMS: 0 })",
            "db.runCommand({ dbStats: 1, maxTimeMS: 3600000 })",
        ] {
            let d = run_command(limited(text, 101));
            assert_eq!(d.get("maxTimeMS"), Some(&Bson::Int64(7000)), "{text}");
        }
        // `explain` gets it on the outer command, not on the command it explains.
        let d = run_command(limited(
            "db.runCommand({ explain: { find: 'orders' } })",
            101,
        ));
        assert_eq!(
            d.get_document("explain").unwrap().get("maxTimeMS"),
            None,
            "{d:?}"
        );
        // A command the guard does not allow as a read, and a write, stay as written.
        for text in [
            "db.runCommand({ setParameter: 1, logLevel: 1 })",
            "db.runCommand({ dropDatabase: 1 })",
        ] {
            let d = run_command(limited(text, 101));
            assert_eq!(d.get("maxTimeMS"), None, "{text}");
        }
    }

    #[test]
    fn command_replies_with_write_errors_are_refused() {
        let e = command_affected(&doc! {
            "n": 0,
            "writeErrors": [{ "index": 0, "code": 11000, "errmsg": "E11000 duplicate key" }],
            "ok": 1.0,
        })
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Database);
        assert_eq!(server_code(&e), Some(11000));
        assert_eq!(e.message, "E11000 duplicate key");
        let e = command_affected(&doc! {
            "n": 1,
            "writeConcernError": { "code": 64, "errmsg": "waiting for replication timed out" },
            "ok": 1.0,
        })
        .unwrap_err();
        assert_eq!(server_code(&e), Some(64));
        // An error without a code keeps the server's message and never becomes "MongoDB error 0".
        let e = command_affected(&doc! {
            "n": 0,
            "writeErrors": [{ "index": 0, "errmsg": "no code here" }],
            "ok": 1.0,
        })
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Database);
        assert_eq!(e.message, "no code here");
        assert_eq!(server_code(&e), None);
        assert_eq!(e.detail, None);
        assert_eq!(command_affected(&doc! { "n": 3, "ok": 1.0 }).unwrap(), 3);
        assert_eq!(
            command_affected(&doc! { "n": 3, "nModified": 2, "ok": 1.0 }).unwrap(),
            2
        );
        assert_eq!(command_affected(&doc! { "ok": 1.0 }).unwrap(), 0);
    }

    #[test]
    fn read_guard_refusals() {
        for text in [
            "db.orders.insertOne({ a: 1 })",
            "db.orders.deleteMany({})",
            r#"db.orders.aggregate([{ $match: {} }, { $out: "copy" }])"#,
            r#"db.orders.aggregate([{ $merge: { into: "copy" } }])"#,
            r#"db.orders.find({ $where: "this.a > 1" })"#,
            "db.runCommand({ drop: \"orders\" })",
            "db.dropDatabase()",
            "db.orders.find()\ndb.orders.find()",
            "use admin",
            "",
            "db.orders.find({",
        ] {
            let e = read_statement(text).unwrap_err();
            assert_eq!(e.code, ErrorCode::GuardRejected, "{text}: {e:?}");
            assert!(
                e.message.starts_with("Blocked by the read-only guard: "),
                "{text}: {}",
                e.message
            );
        }
        for text in [
            "db.orders.find({ status: \"paid\" }).sort({ _id: -1 }).limit(20)",
            "db.orders.aggregate([{ $group: { _id: \"$status\", n: { $sum: 1 } } }])",
            "db.orders.countDocuments({})",
            "db.orders.distinct(\"status\")",
            "db.orders.estimatedDocumentCount()",
            "db.orders.findOne()",
        ] {
            assert!(read_statement(text).is_ok(), "{text}");
        }
    }

    fn mongo_conn() -> Connection {
        Connection {
            id: "c".into(),
            name: "m".into(),
            engine: Engine::Mongo,
            version: String::new(),
            env: crate::core::connections::Env::Local,
            host: String::new(),
            port: 0,
            database: String::new(),
            user: String::new(),
            ssl: crate::core::connections::SslMode::Disable,
            file: String::new(),
            ai_write: false,
            read_only: false,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    /// These checks run before any client is asked for, so no server is needed.
    #[tokio::test]
    async fn refusals_before_the_server() {
        let access = access();
        let conn = mongo_conn();
        let limits = ReadLimits::default();
        let e = read(
            &access,
            &conn,
            Some("shop"),
            "db.orders.insertOne({})",
            limits,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::GuardRejected, "{e:?}");
        let e = read(&access, &conn, None, "db.orders.find({})", limits)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
        assert_eq!(e.message, "Choose a database for this source");

        // A stop before the start runs nothing.
        let stop = CancellationToken::new();
        stop.cancel();
        let e = run_approved(&access, &conn, "shop", "db.orders.deleteMany({})", &stop)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
        let live = CancellationToken::new();
        for text in ["db.a.deleteMany({})\ndb.b.deleteMany({})", "use admin"] {
            let e = run_approved(&access, &conn, "shop", text, &live)
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{text}: {e:?}");
        }
    }
}
