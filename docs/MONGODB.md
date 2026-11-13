# sandb: MongoDB support plan

Created: 2026-11-07. Status: phases 0 to 7 are done (2026-11-10).

## 1. Goal

Add MongoDB as a fifth engine. A user can connect, browse, query, edit, export and use the AI chat on MongoDB with the same safety rules that sandb has for SQL. One AI chat can use SQL databases and MongoDB databases together.

Why: users of the product ask for MongoDB support. A chat that can use SQL and MongoDB together is a strong feature, because few clients have it.

## 2. Decisions from the brainstorm

| Topic | Decision |
|---|---|
| Scope | Full parity with the SQL engines, where the feature has a MongoDB meaning. Delivered in phases (section 5). |
| Query syntax | mongosh-style text, for example `db.users.find({ age: { $gt: 30 } })`. The app parses a fixed subset. The app never runs the text as JavaScript. |
| Results | Two views with a toggle: flattened columns (`address.city`) and a JSON tree. |
| AI chat | One chat can use SQL and MongoDB databases together. |
| Server types | Standalone, replica set, Atlas (`mongodb+srv://`). |

## 3. Assumptions (change them before phase 1 if they are wrong)

- No JavaScript engine in the app. Text that the parser does not know is refused with a clear error.
- The AI does not write mongosh text. It sends JSON arguments (collection, filter, pipeline) in Extended JSON. This is more reliable to parse and to guard.
- The schema of a collection is a guess from a sample. Default sample: 1,000 documents or 8 MB, whichever comes first.
- MongoDB connections have no tab session with manual commit (the Production mode of phase 3b). MongoDB aborts a transaction after 60 seconds by default, so the app cannot keep a transaction open while the user thinks. Production MongoDB connections use a confirmation before each write.
- The ER diagram and the foreign key jump are not available for MongoDB. MongoDB has no foreign keys.
- Sharded clusters work for reads and writes, but the plan does not test shard-specific features.

## 4. How the current code is shaped

All engine-specific code dispatches on `Engine` (`core/connections.rs`) and on `DbPool`, `OneConnection` and `Conn` (`core/db/pool.rs`, `core/db/session.rs`). A new `Engine::Mongo` variant makes the compiler show every `match` that needs a Mongo arm or an explicit refusal. Use the LSP references of `Engine` to list them before phase 1 starts.

The main difference: every part below the connection layer expects SQL text, a SQL parser (`sqlparser`) and rows with fixed columns. MongoDB needs a parallel path for these parts:

| Area | SQL today | MongoDB path |
|---|---|---|
| Driver | `sqlx` | `mongodb` crate (tokio, rustls) and `bson` |
| Editor text | `sql/split.rs`, `sql/facts.rs` | New mongosh subset parser |
| Write guard | `guard/classify.rs`, `conn_ro.rs` | New classifier on the parsed command |
| Schema | `introspect/{pg,mysql,sqlite}.rs` | Collections, views, schema from a sample |
| Results | `query.rs` cells from rows | Documents, flattened to columns, plus raw documents for the JSON view |
| Edits | `edit.rs`, one SQL transaction | `$set` by `_id`. One transaction on a replica set; one write for each document on a standalone server |
| AI tools | SQL text in `run_read_query`, `render_chart`, `propose_write` | Mongo tools with JSON arguments |
| Export | CSV, DDL, SQL with data | CSV (flattened), JSON, NDJSON, a script with `createCollection` and `createIndex` |

New backend code goes in `src-tauri/src/core/mongo/` so that the SQL modules do not get Mongo branches inside their logic. The SQL modules only get a dispatch at the entry point (`if engine == Mongo { return mongo::... }`).

## 5. Phases

Each phase uses the same workflow as `docs/REMAINING.md`: a worker builds, a reviewer reviews, the worker fixes P0 and P1, and QA tests on the final code. Each phase ends with these gates:

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (also with `--features dev-bridge` and `--features it`).
- `cargo test --lib`, and `cargo test --features it` against the `testdb` stack.
- `pnpm check`, `pnpm build`.
- A bridge smoke test of the new feature in the browser.

Each phase must leave the app in a state that you can release. A feature that is not ready yet must be hidden for MongoDB connections. It must not show a broken screen.

### Phase 0: Test stack and size check

Goal: a test database and real numbers before you write product code.

Tasks:
1. Add two services to `testdb/compose.yaml`:
   - `mongo-rs`: `mongo:8`, a one-node replica set (`--replSet rs0`), on `127.0.0.1:57017`. Use it for transactions.
   - `mongo-standalone`: `mongo:8` without a replica set, on `127.0.0.1:57018`. Use it for the path without transactions.
   - Both use tmpfs, like the SQL services. The healthcheck logs in as `sandb_ro` and counts `analytics.events`, so it passes only after the seed finishes.
2. Add `testdb/seed/mongo/` with:
   - Databases `shop_staging` and `analytics`, the same business data as the SQL seed where possible, so the AI tests can compare.
   - `analytics.events` with 10,000 documents.
   - Nested documents, arrays, arrays of documents, and missing fields.
   - Every BSON type that the app must keep: `ObjectId`, `Int32`, `Int64`, `Double`, `Decimal128`, `Date`, `Timestamp`, `Binary`, `UUID`, `Regex`, `Boolean`, `Null`, `MinKey`, `MaxKey`.
   - One collection with large documents (about 2 MB each) for the memory tests.
   - One view and one time series collection.
   - A field with mixed types in different documents (`price` as `Int32` and as `String`).
   - User `sandb_ro` with the `read` role, and user `sandb_rw` with `readWrite`.
3. Update `testdb/.env.test`, `testdb/README.md` and the scripts in `testdb/scripts/`.
4. Size and memory check, on a throwaway branch:
   - Build a release binary without and with the `mongodb` crate. Record the binary size and the app bundle size.
   - Open one Mongo client and read 100 large documents. Record the memory of the `sandb` process and of the WebView process.
   - Record the numbers in this file (section 8).

Done when: `testdb` starts both Mongo services and the healthchecks pass; the size numbers are in section 8.

### Phase 1: Connection

Goal: a user can save, test, edit, duplicate and delete a MongoDB connection.

Backend:
1. Add `Engine::Mongo` (`serde` name `mongo`). `default_port` is 27017.
2. Connection fields. Two ways to enter a connection in the dialog:
   - Fields: host, port, database, user, password, TLS mode (map `SslMode`: `Disable` → no TLS, `Prefer` → not available in the driver, so show `Disable` and `Require` only for Mongo), and a new optional `authSource`.
   - Connection string: `mongodb://` or `mongodb+srv://`. A connection string can contain a password, so the app stores the full string in the OS credential store (`core/secrets.rs`), not in the connections JSON file. The JSON file keeps only a copy without the password, for display.
   - `reuse_secret` must compare the host list and the SRV host, so the app never sends a stored secret to a different server.
3. `DbPool::Mongo(mongodb::Client)`. One client serves all databases of the connection, so the PG rule "one pool for each database" does not apply. Keep the 10 s connect timeout (`CONNECT_TIMEOUT`) as `serverSelectionTimeoutMS` and `connectTimeoutMS`. Set `maxPoolSize` to 5, as `MAX_CONNECTIONS`.
4. `probe`: run `buildInfo` and `hello`. Return a version text such as `MongoDB 8.0.3`. Also return the topology: standalone, replica set or sharded. Save the topology with the connection, because later phases need it (transactions).
5. Read-only flag: MongoDB has no read-only session. The app check (phase 3) is the only barrier inside the app. The connection dialog shows a note for a read-only Mongo connection: "Use a database user with the read role for full protection." In the `it` tests, prove that `sandb_ro` gets an error from the server for a write.
6. Every place that the compiler shows for the new variant gets a Mongo arm or a refusal with a clear error code (`unsupported` or an existing equivalent in `core/error.rs`). No panics, no silent empty results.
7. Bridge (`src/bin/bridge.rs`): no new endpoints in this phase. Check that the existing endpoints accept `engine: "mongo"`.

Frontend:
1. `src/lib/api/types.ts`: add `'mongo'` to the engine type, and the new fields.
2. `ConnectionDialog.svelte`: engine option "MongoDB", a switch between "Fields" and "Connection string", the `authSource` field, the read-only note.
3. `ConnectionTree.svelte`: a MongoDB icon.
4. Hide or turn off every SQL-only action for Mongo connections until its phase is done.
5. `src/lib/api/mock/`: one Mongo connection, so the UI works without a backend.

Tests:
- Unit: connection string parse and the copy without the password; `reuse_secret` with a different host and with a different SRV host; field validation.
- `it`: test the connection on `mongo-rs` and `mongo-standalone`; a wrong password gives `auth_failed`; the topology is correct on both.

Done when: a user can create a Mongo connection with fields and with a connection string, "Test" shows the version, and the password is only in the credential store.

### Phase 2: Browse and schema

Goal: the connection tree shows databases, collections, views and fields.

Backend (`core/mongo/introspect.rs`, `core/mongo/schema.rs`):
1. `list_databases`: `listDatabases` with `authorizedDatabases: true`. If the user cannot list databases, return the database of the connection.
2. `list_schemas`: MongoDB has no schemas. Return an empty list, as MySQL does.
3. `list_tables`: `listCollections`. Collections map to `TableKind::Table`, views to `TableKind::View`. Add `TableKind::TimeSeries` only if the UI needs it; else show time series as `Table`. Hide `system.*` collections. `row_estimate` comes from `estimatedDocumentCount`.
4. `list_columns`: the schema from a sample.
   - `$sample` with the sample size and a byte limit. Read the documents as a stream and merge each document into a field summary at once. Do not keep the documents in memory.
   - The summary for each field path: the BSON types and how many documents have the field.
   - Nested paths up to depth 5. An array of documents adds paths as `items[].name`.
   - Map to `ColumnInfo`: `name` is the path, `data_type` is the type list (`int32 | string`), `nullable` is true when some documents do not have the field, `is_pk` is true for `_id` only.
   - Add an optional `frequency` (0.0 to 1.0) to `ColumnInfo`. SQL engines leave it out.
   - Use the `$jsonSchema` validator of the collection when it exists, together with the sample.
5. `schema_snapshot`: collection names and field paths for autocomplete. Use the same caps (`SNAPSHOT_MAX_TABLES`, `SNAPSHOT_MAX_COLUMNS`). Use a smaller sample (100 documents) so the snapshot stays fast.

Frontend:
1. The tree shows Collections and Views folders for a Mongo database.
2. The fields list shows the type list and the frequency, for example `price  int32 | string  92%`.
3. A label says that the fields come from a sample of N documents.

Tests:
- Unit: the field summary merge (nested paths, arrays, mixed types, missing fields, depth limit).
- `it`: the tree of the seed databases; the mixed-type field; the `sandb_ro` user without `listDatabases` rights.
- Memory: the sample of the large-document collection stays under the byte limit.

Done when: a user can browse all seed databases and see correct field types.

### Phase 3: Query editor and results

Goal: a user can write mongosh-style queries, run them, see the results in two views, sort, filter, load more and cancel.

Parser (`core/mongo/parse.rs`):
1. Shapes:
   - `db.<collection>.<method>(<args>)` and `db.getCollection("<name>").<method>(<args>)`.
   - Chained cursor methods after `find`: `.sort()`, `.limit()`, `.skip()`, `.project()`, `.hint()`, `.collation()`, `.count()`.
   - `db.runCommand({...})` and `db.adminCommand({...})`.
   - Shell helpers: `show dbs`, `show collections`, `use <db>` (changes the target of the run only, like the editor target).
2. Methods.
   - Read: `find`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`, `getIndexes`, `stats`.
   - Write: `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `deleteOne`, `deleteMany`, `findOneAndUpdate`, `findOneAndReplace`, `findOneAndDelete`, `bulkWrite`, `createIndex`, `createIndexes`, `dropIndex`, `dropIndexes`, `drop`, `renameCollection`, `createCollection`.
   - Any other method: refuse with a clear error, for example `Unknown method: mapReduce`.
3. Argument syntax: JavaScript object literals as mongosh accepts them: keys without quotes, single or double quotes, trailing commas, comments, and these helpers: `ObjectId()`, `ISODate()`, `new Date()`, `NumberInt()`, `NumberLong()`, `NumberDecimal()`, `Decimal128()`, `UUID()`, `BinData()`, `Timestamp()`, `MinKey()`, `MaxKey()`, `/regex/flags`. The output is a `bson::Document` with the correct types.
4. Several statements in one editor: split on `;` and on new statements at the start of a line. "Run at cursor", "Run selection" and "Run all" work as for SQL.
5. Error positions: each parse error has a line and a column, so the editor can mark it.

Run path (`core/mongo/run.rs`):
1. `query::run` sends Mongo connections to `mongo::run` at the start of `run_inner`.
2. Reads: limit with `DEFAULT_LIMIT` and `MAX_LIMIT`, as SQL does. Add a byte limit for each page (16 MB) so that large documents cannot fill the memory. When the run stops at the byte limit, the result says so.
3. Results: `StatementResult` gets the flattened columns and cells, and a new optional `documents` field with the raw documents as relaxed Extended JSON, for the JSON view. Send `documents` only when the user opens the JSON view, or send it for small pages only. Choose one in the phase 3 brief after the memory numbers of phase 0.
4. Flatten rule: nested document paths up to depth 3 become columns (`address.city`). Arrays show as one cell with a short text (`[3 items]`) and the full value in the cell viewer (`cell_value`). The column order follows the first document, then new fields at the end.
5. Writes: return the counts (`matchedCount`, `modifiedCount`, `insertedIds`, `deletedCount`) as a command result, as SQL does with `rows_affected`.
6. Cancel: every operation gets `comment: <queryId>`. Cancel runs `$currentOp` with that comment, then `killOp`. Also drop the future. All reads get `maxTimeMS` from the existing time limit.
7. Sort by column: add the sort to `find` or add a `$sort` stage at the end of the pipeline. Never change the text in the editor (same rule as `sorted_statements`).
8. Filter bar (`TableFilterBar.svelte`): build a filter document from column, operator and value. Map the operators: `=` → `$eq`, `contains` → `$regex` with escaped text, `is null` → `$in: [null]`, and so on. Convert the value to the type of the field from the schema.

Guard (`core/mongo/guard.rs`):
1. Classify each parsed command as read, write, schema change or unknown.
2. Inside a read, look at all stages and operators in depth. These make a read a write or refuse it: `$out`, `$merge`. These run JavaScript on the server and are refused: `$where`, `$function`, `$accumulator`. `runCommand` is unknown unless its first key is on a short list of read commands (`ping`, `buildInfo`, `hello`, `isMaster`, `serverStatus`, `hostInfo`, `connectionStatus`, `dbStats`, `collStats`, `listDatabases`, `listCollections`, `listIndexes`, `getParameter`, `getCmdLineOpts`, `currentOp`, `connPoolStats`, `top`, `listCommands`, `whatsmyuri`, `explain`).
3. Read-only connection: refuse every command that is not a read, with `connection_read_only`, as `conn_ro.rs` does for SQL. Fail closed: unknown is refused.
4. Production warnings, like `WarningCode`: `updateMany` or `deleteMany` with an empty filter, `drop`, `dropIndexes`, `renameCollection`. The existing safeguard dialog shows them.
5. `guard_check` and `classify_write` commands accept Mongo text and use this guard.

Frontend:
1. Editor: CodeMirror JavaScript mode (`@codemirror/lang-javascript`) for Mongo tabs. Autocomplete: collection names after `db.`, method names, field paths inside the filter, and `$` operators.
2. `ResultsGrid.svelte`: a "Columns / JSON" toggle. The JSON view is a tree. It builds the child nodes only when the user opens a node.
3. The results show the type of each cell on hover (`Int64`, `ObjectId`, `Date`).
4. "Open table data" on a collection opens `db.<name>.find({})` with the default limit.
5. CSV export of a result uses the flattened columns.

Tests:
- Unit, parser: each method; each helper; nested objects; comments; trailing commas; regex; errors with line and column; text that looks like JavaScript code (`function`, `=>`, `for`) is refused.
- Unit, guard: each stage on the lists, also when it is deep inside `$facet`, `$lookup.pipeline` or `$unionWith`; unknown commands; empty-filter warnings.
- Unit, flatten: depth, arrays, missing fields, column order.
- `it`: find, aggregate, sort, filter bar, load more, the byte limit on the large documents, cancel of a slow aggregation (for example a large `$lookup` on `analytics.events`), a write on a read-only connection is refused by the app and by the server.

Done when: a user can query all seed collections in both views, and the read-only and Production rules work.

### Phase 4: Edit documents

Goal: a user can change cells, add documents and delete documents in the grid, and save all changes at once.

Backend (`core/mongo/edit.rs`):
1. `editable_result`: a result is editable when it comes from `find` on one collection and every document has `_id`. Aggregation results are not editable.
2. Values keep their BSON type. The cell editor sends the value with its type (`{ "type": "int64", "value": "42" }`). The backend converts it with that type. If the user does not change the type, the type stays the same. This rule stops the main data risk: an `Int64` or `Decimal128` that comes back as a `Double`.
3. Change of a cell: `updateOne({ _id }, { $set: { "<path>": <value> } })`. Change of many cells in one document: one `updateOne` with all paths.
4. New document: a JSON editor dialog, then `insertOne`.
5. Delete: `deleteOne({ _id })`.
6. Save:
   - Replica set or sharded: all changes in one transaction. If one change fails, nothing changes.
   - Standalone: no transactions. Run the changes one by one. Stop at the first error. The result lists the changes that succeeded and the change that failed. The save dialog tells the user before the save that the server has no transactions.
7. Conflict check: the filter of each update includes `_id` and the old value of each changed field. If `matchedCount` is 0, another user changed the document. Report a conflict and do not overwrite.
8. Read-only and Production: the same rules as SQL edits. Production shows the confirmation dialog with the commands before they run.
9. Audit log: each saved change goes into the audit log (`audit.svelte.ts`), as for SQL.

Frontend:
1. `EditSaveDialog.svelte` shows the Mongo commands that will run.
2. The cell editor has a type selector, set to the current type.
3. Cells that are arrays or documents open a JSON editor.

Tests:
- Unit: each BSON type converts to the cell value and back without a change.
- `it`: edit on `mongo-rs` (one transaction, rollback on error); edit on `mongo-standalone` (partial result); conflict; type kept for `Int64`, `Decimal128`, `Date`, `ObjectId`; read-only refusal.

Done when: a user can edit the seed data and every BSON type stays the same after the save.

Built (2026-11-10), decisions made in the phase:
- Code: `core/mongo/edit.rs`, with a dispatch at the entry of `editable_result`, `apply_edits` and `cell_value` in `core/db/edit.rs`. Tests: `tests/it_mongo_edit.rs`.
- The cell editor sends a typed value: `{ "type": "int64", "value": "42" }`. The type `shell` sends one mongosh value as text, for documents, arrays and **Edit as JSON…**. The backend reads it with the phase 3 parser (`parse::parse_value`).
- The conflict check matches the exact old value: the same BSON type, no array that only contains the value, `$type: "null"` for null, `$exists: false` for a missing field. The save uses the simple collation, so a default collation of the collection cannot loosen the match.
- Each Mongo `StatementResult` has its `database`. After `use <db>` in a run, edits, **Load more** and sort use that database.
- Not editable: an aggregate, a `count`, a view, a `system.` collection, a projection that computes fields or hides `_id`, and a row whose `_id` is a document or an array. Cells of types such as `binData` or `regex` can change only with **Edit as JSON…**.

### Phase 5: AI chat

Goal: the AI chat can read, chart and propose writes on MongoDB, also in a chat with SQL databases.

Backend (`core/ai/`):
1. Tools (`tools.rs`). `definitions` gets the engine of each source and gives the model only the tools that the sources need:
   - Shared tools: `list_tables` (collections for Mongo) and `describe_table` (the schema from a sample for Mongo, with the type list and the frequency).
   - SQL tools as today: `run_read_query`, `render_chart`, `propose_write`.
   - New Mongo tools:
     - `run_mongo_read`: `{ source, collection, pipeline }` or `{ source, collection, filter, projection, sort, limit }`. Arguments in canonical Extended JSON.
     - `render_mongo_chart`: the same arguments plus the chart fields of `render_chart`. The pipeline must return flat documents.
     - `propose_mongo_write`: `{ source, collection, operation, filter, update, documents }`. It never runs. The user sees it and decides, as with `propose_write`.
   - Each tool refuses a source of the wrong engine with a clear message, so the model can correct itself.
2. AI read path: the phase 3 guard, then the run with `maxTimeMS`, the row limit, the byte limit and `MAX_CONTENT_BYTES`. Add `$limit` at the end of each pipeline when it does not have one.
3. Privacy `schema`: the model gets the field paths, the types and the count, but no values. Database error text becomes the error code number only.
4. Prompt (`prompt.rs`): the list of sources shows the engine of each source. For Mongo sources, add short rules: use Extended JSON; use `$match` early; one query runs on one source; to combine SQL and Mongo data, run one query on each source and combine the results in the answer. For a large combined result, the model must aggregate on each side first.
5. `approve_write` checks the saved connection again (read-only, AI write flag), as for SQL.
6. Reports and PDF export work without changes, because they use the tool results.

Frontend:
1. `ChatScope.svelte` lists Mongo databases with a Mongo icon, next to SQL databases.
2. The write preview in the chat shows the Mongo command as mongosh text, so the user can read it.

Tests:
- Unit: tool definitions for a SQL-only chat, a Mongo-only chat, and a mixed chat; wrong-engine refusal; Extended JSON argument parse; privacy `schema`.
- `it` with the mock LLM (`testdb/mock-llm`): a Mongo read; a chart; a write proposal that does not run; a mixed chat with one PG and one Mongo source; a guard refusal of `$out`.
- Manual check with a real model on the seed data, from the list of example questions in the phase 5 brief.

Done when: the mock LLM tests pass, and a real model answers a question that needs data from PG and from Mongo in one chat.

Built (2026-11-10), decisions made in the phase:
- The Mongo tools take one mongosh statement as text in `query`, not Extended JSON. Why: the phase 3 parser and guard check the same text as in the editor, models write mongosh well, and the user can copy the step into the editor and run it.
- Code: `core/mongo/ai.rs` (read path, approved write). `readonly::run_ai_read` and `write::run_approved` send a Mongo connection there. `tools::definitions` takes the sources: the `source` enum of each tool lists only the sources of its engine, and SQL-only chats get the same tool JSON and prompt as before.
- An AI read: one statement that `guard::kind` proves is a read. `find` gets `limit = max_rows + 1`, `aggregate` gets a final `$limit`, and both get `maxTimeMS`. The read stops at 16 MB. A client timeout kills the operation through its `comment`.
- An approved Mongo write is one statement. A statement that changes many documents is not one transaction.
- In `schema` privacy, a Mongo error reaches the model as its label only, for example `MongoDB error 11000`.
- Tests: `tests/it_ai_mongo.rs`. Each chat samples every collection for the prompt, so these tests run one at a time. Parallel samples of `big_docs` ran the test container out of memory.

### Phase 6: Structure, indexes and export

Goal: the remaining parity features.

Backend:
1. `table_properties` for a collection: indexes (`listIndexes`, with keys, unique, sparse, TTL, partial filter), the `$jsonSchema` validator, collection options (capped, time series, collation) and stats (`collStats`: size, count, average document size).
2. Index changes: create and drop an index. The dialog shows the command before it runs (`SchemaChangeDialog.svelte`), with the Production and read-only rules.
3. Validator change: edit the `$jsonSchema` with `collMod`. The dialog shows the command first.
4. `table_ddl` for a collection: a mongosh script with `db.createCollection(...)` (options and validator) and `createIndex` for each index.
5. Export (`export.rs`, `dump.rs`): a collection or a result as JSON array, NDJSON (one document on each line) or CSV (flattened). The JSON exports use relaxed or canonical Extended JSON; the user selects. "Export with data" writes the script of item 4 plus `insertMany` in batches of 1,000.
6. The export reads with a cursor and writes to the file as it reads. It does not keep the full collection in memory.

Frontend:
1. `TableProperties.svelte`: Mongo tabs for Indexes, Validator, Options, Stats. Hide the Constraints, Foreign keys, References and ER diagram tabs for Mongo.
2. The export menu shows the Mongo formats.

Tests:
- `it`: the properties of the seed collections; create and drop an index; the script of item 4 creates the same collection on an empty database; export and import of all BSON types gives the same data; the export of the large-document collection stays under the memory limit.

Done when: a user can look at and change indexes and validators, and export a collection and import it again without data loss.

Built (2026-11-10), decisions made in the phase:
- Code: `core/mongo/introspect.rs` (`properties`), `core/mongo/shell.rs` (one mongosh value renderer, also used by the grid edits), `core/mongo/dump.rs` (script, export with data), `core/mongo/export.rs` (`export_documents`). Frontend: `TableProperties.svelte` Mongo sections, `MongoSchemaDialog.svelte`, `ExportDocumentsDialog.svelte`.
- `TableProperties` and `IndexInfo` get an optional `mongo` field. Constraints, foreign keys and references are empty for MongoDB.
- Stats come from `$collStats` with `storageStats`, because the `collStats` command is deprecated. When the user may not read stats, the Stats section is empty and the other sections still load.
- Index and validator changes run as mongosh text through `run_query`, as in the editor, with the read-only guard and the Production confirmation. A separate dialog (`MongoSchemaDialog.svelte`) builds the text. Before it runs, the text must parse as exactly one statement, so text in the filter or validator field cannot add a second statement.
- The script makes a view with `db.createCollection(name, { viewOn, pipeline })`. The script writes the `createIndex` options only; server-only index fields such as `v` are not written.
- Values without a mongosh constructor (Symbol, Undefined, DBPointer, JavaScript code with scope) stop the script export with an error that names the type and the document `_id`.
- The `it` tests that need `collMod` or a new database use the root user of the test stack, because `sandb_rw` has `readWrite` only.
- Tests: `tests/it_mongo_struct.rs`.

### Phase 7: Hardening and release

Tasks:
1. Memory and size: measure again as in phase 0, with the full feature. Record the numbers in section 8.
2. Atlas: test with a real Atlas cluster (`mongodb+srv://`, TLS). This test uses a real service, so the owner must give permission and a test cluster first.
3. Network errors: the server stops during a run, during a save, and during a chat. The app shows an error and stays usable.
4. Mock backend and bridge: every Mongo feature works in the mock (`src/lib/api/mock/`) and through the bridge, so QA can test in the browser.
5. Documents: `README.md` (Connections, editor and AI sections), `CHANGELOG.md`, the What's New dialog (`src/lib/changelog.ts`).
6. Final review and QA on the final code.

Done when: all gates pass, QA passes, and the documents describe MongoDB support.

Built (2026-11-10), decisions made in the phase:
- Cancel: each statement races with the cancel flag (`until_cancelled` in `core/mongo/run.rs`), so a server that does not answer cannot hold a run until the driver's monitor timeout (about 25 s). The server kill in `query::cancel` stops after 2 s. A cancelled write waits 2 s more for its reply. When the reply does not come, the error says that the statement may have changed data, because a paused server applied such a write after it came back.
- Every server operation carries the query id as `comment`, also the index commands, so a cancel can kill them. A collection `drop` cannot: the driver has no `comment` option for it.
- Load more runs the statement again with twice the row limit. It is offered only when the rows fill the row limit (`moreRows` in `src/lib/results.ts`). A page that stops at 16 MB has no Load more, because a larger limit brings no new documents.
- Without Cancel, a run against a server that does not answer ends with `connect_failed` after 10 to 25 s. No app timeout was added.
- The mock backend shows MongoDB data for browse, find, Properties and Script, and refuses writes with a clear message. The bridge serves every MongoDB feature except the file dialogs, as for SQL.
- Tests: `tests/it_mongo_fault.rs`, with a TCP proxy that can cut or black-hole the connection. The test connections use `directConnection=true`, because the driver otherwise finds the replica set member from `hello` and goes around the proxy.
- Atlas (2026-11-10, M0 cluster, MongoDB 8.0.37, with the owner's permission): `mongodb+srv://` with TLS, Test, browse, Properties, schema, find, aggregate, writes, a unique index and its duplicate-key error, grid edits in a transaction (a conflict rolls back the whole save) and Cancel all work. The test found one defect: `Client::shutdown` takes up to 60 s on an SRV cluster, so the probe and `MongoClients::close` now shut the client down in a background task. The M0 tier refuses `$where`; the error shows the server's message.
- Grid save cancel: `ApplyEditsRequest.queryId` registers the save in the query registry, so `cancel_query` cancels it, and every save command carries that id as `comment` for the server kill. A cancel before the commit aborts the transaction. A cancel during `commitTransaction` waits 2 s for the reply, and then reports that the save may have applied. The SQL save takes the same id and cancel: its batch runs in a savepoint inside the tab's transaction, and a cancel before COMMIT rolls the batch back (outside a tab it rolls the transaction back).
- `db.dropDatabase()` drops the database of the tab, as in mongosh, with the optional `writeConcern` document. The guard calls it a write (`dropDatabase`): a read-only connection refuses it, a production connection asks for confirmation that names each statement's database, and the assistant needs the user's approval. The command carries the query id as `comment`, so Cancel can stop it.
- The AI read gives an allow-listed read `runCommand` / `adminCommand` a `maxTimeMS`: the chat timeout, or the user's smaller value. The server stops the command at that time also when the command ignores `comment`. An `explain` gets the limit on the outer command. Both the local replica set and the standalone accept the field for every command of the guard's read list, so every read command gets it.

## 6. Risks

| Risk | Effect | Prevention |
|---|---|---|
| A BSON type changes on save | Wrong data without an error | Typed cell values (phase 4, item 2); round-trip test for each type |
| The guard misses a write or JavaScript stage | The AI or a read-only connection writes data | Allow list, deep scan of nested pipelines, fail closed, tests for each stage |
| Large documents | High memory in Rust and in the WebView | Byte limits for pages, samples and AI output; lazy JSON tree; stream exports |
| No transactions on a standalone server | A save can stop half done | Detect the topology; tell the user before the save; report what succeeded |
| The schema guess misses rare fields | Autocomplete and AI miss a field | Show "sample of N"; use the validator when it exists |
| No read-only session in MongoDB | The app check is the only barrier inside the app | Fail-closed guard; recommend a user with the `read` role |
| The parser accepts too little mongosh syntax | Users paste queries that fail | Clear errors with position; add methods when users ask for them |
| Larger binary and slower build | Bigger download | Measure in phase 0; decide there |

## 7. Open questions

1. Do most users connect to Atlas or to their own servers? If most use Atlas, phase 1 tests `mongodb+srv://` first.
2. The sample size of 1,000 documents or 8 MB: is that correct for your users?
3. Decided in phase 3: each read result sends its raw documents always, as relaxed Extended JSON. The 16 MB page limit keeps the size bounded, and the JSON view and **Copy value** need no second request.
4. Should the AI also accept mongosh text from the user in the chat (for example "explain this query"), or only JSON tools?
5. What maximum increase of the binary size is acceptable?

## 8. Measurements

Phase 0 numbers, 2026-11-09, macOS arm64, `cargo build --release --bin sandb`, `mongodb` 3.10 with default features (rustls with ring, DNS resolver for `mongodb+srv://`).

| Item | Without MongoDB | With MongoDB |
|---|---|---|
| Release binary size | 28,976,176 bytes (27.6 MiB) | 34,985,088 bytes (33.4 MiB), +6.0 MB, +21% |
| App bundle size | Not measured: `tauri build` needs signing. Measure in phase 7. | |
| Memory of a Mongo client, idle | | 6.1 MiB RSS (separate probe process, see below) |
| Memory of a Mongo client, 100 large documents read | | 216 MiB RSS |
| Memory of the WebView, 100 large documents shown | Not measured: no Mongo results view before phase 3. | |

The memory numbers come from a throwaway probe binary, not from the `sandb` process: it reads all 100 documents of `shop_staging.big_docs` (about 2 MB each, 200 MB total) into a `Vec<Document>`. The result shows that a read without a byte limit keeps the full data in memory. Phase 3 must keep the 16 MB page limit.

Phase 7 numbers, 2026-11-10, macOS arm64, with all MongoDB features (phases 1 to 6). "Without MongoDB" is the installed v0.11.0 app. The two builds have different code (more changes than MongoDB) and maybe a different build setup, so the difference is not only the `mongodb` crate.

| Item | v0.11.0 (without MongoDB) | With MongoDB |
|---|---|---|
| Release binary size | 32,087,136 bytes (30.6 MiB) | 40,539,888 bytes (38.7 MiB), +8.5 MB, +26% |
| App bundle (`sandb.app`) | 31.1 MiB on disk, 11.7 MB as `.tar.gz` | 39.2 MiB on disk, 16.5 MB as `.tar.gz` (+4.8 MB download) |
| Process memory, idle | | 10.7 MiB RSS; 16.3 MiB with an open Mongo client |
| Process memory, `big_docs.find()` with limit 100 | | One page holds 7 documents (14.7 MB, the 16 MB page limit). 75 MiB RSS after one run; 174 MiB after 8 runs back to back; 55 MiB 5 s later. |
| WebView JS heap, `big_docs` page open (Chromium through the bridge, after gc) | | 41.9 MB before; 56.5 MB in Columns view; 58.7 MB in JSON view with one document open |

The bundle comes from `pnpm tauri build --bundles app` with `createUpdaterArtifacts` off, because the updater key is not on this machine. The process numbers come from the release `bridge` binary (`--features dev-bridge`). It uses the same core code as the app, but it has no WebView. The RSS goes up on each run and comes down when the allocator gives the memory back, so there is no leak. A page never holds more than 16 MB of documents.
