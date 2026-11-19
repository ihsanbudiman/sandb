# sandb

sandb is a light desktop database client with an AI assistant. You use your own AI server and your own API key (BYOK).

## Features

### Connections

- Connect to PostgreSQL, MySQL, MariaDB and SQLite.
- Connect to MongoDB with fields (host, port, database, user, password, auth source and TLS) or a `mongodb://` / `mongodb+srv://` connection string. A test of the connection shows the server version and the topology: standalone, replica set or sharded. A password, also one inside the string, is saved in the credential store, and the connections file keeps the string without it.
- Connect to Redis standalone, Cluster or Sentinel. Use connection fields, `redis://` or `rediss://`. Use a Redis ACL user for server-side read-only access.
- Set the environment of each connection: local, staging or production. A production connection asks you to confirm dangerous changes.
- Mark a connection as read-only. MongoDB has no read-only session, so a read-only MongoDB connection relies on the app's guard, and the dialog recommends a database user with the `read` role for full protection.
- The connection tree shows tables, views and PostgreSQL materialized views in separate folders. A MongoDB connection shows its databases, then a Collections and a Views folder. Expanding a collection lists its field paths (`address.city`, `items[].sku`) with their types and the share of sampled documents that have each one.
- Browse Redis databases and keys. Find keys with a pattern, such as `user:*`. Expand a key to see its fields.
- The app keeps passwords and the API key in the credential store of the operating system.

### Query editor

- Write SQL or, on a MongoDB tab, mongosh-style statements with autocomplete and syntax highlighting. sandb reads the mongosh text with its own parser and never runs JavaScript.
- On a Redis tab, write one redis-cli command per line. Use `SELECT` to change the database for later commands in the same run. sandb refuses writes on read-only connections and asks before writes on production connections.
- Open more than one query in tabs.
- Run the statement at the cursor, the selected text, all statements, or a `.sql` file.
- Format the query (⇧⌘F, or **Format query** in the editor menu). With a selection, only the selection changes. SQL breaks into one clause on each line. Mongosh text breaks documents and arrays that do not fit on one line. The format keeps the case of keywords and every value as you wrote it, for example `1.0`.
- Type a short word at the start of a statement and press Tab: `sel` gives `SELECT * FROM `, `selc` gives `SELECT COUNT(*) FROM `, `ins`, `upd` and `del` give `INSERT INTO `, `UPDATE ` and `DELETE FROM `. The autocomplete list also opens by itself 200 ms after you type a space, a comma or `(` at the end of a line. In that list, Tab inserts the item and Enter makes a new line.
- On a read-only MongoDB connection, sandb runs only statements that read and refuses everything else. On a production MongoDB connection, sandb asks before a statement that changes data runs, and warns about, for example, an `updateMany` or `deleteMany` with an empty filter and about `drop`.
- Save a query with a name (⌘S). Find saved queries in the Queries list or in the command palette (⌘K).

### Results and table data

- Scroll the results grid. The grid loads more rows when you come near the last row.
- A MongoDB result has a **Columns** and a **JSON** view. Columns flattens nested fields to three levels (`address.city`), arrays show as a short text (`[3 items]`), and a hover on a cell shows its type (`int64`, `objectId`). JSON shows each document as a tree, one level at a time. A result stops at the row limit or at 16 MB of documents; **Load more**, sorting and **Cancel** work as for SQL.
- Double-click a cell to edit it. Save applies all row changes in one transaction. On MongoDB, a save keeps the type of every value (`int64`, `decimal128`, `date`) and changes only the fields that you edited. A standalone server has no transactions, so a save there runs the changes one by one. If another user changed a document after you loaded it, sandb does not overwrite their change.
- Sort by a column. Filter table data by column, operator and value.
- Double-click a collection in the tree to open its documents in a data tab.
- Click the arrow in a foreign key cell to open the row that it refers to.
- PostGIS `geometry` and `geography` values show as WKT.
- Export a table as CSV, as DDL, or as SQL with data. A MongoDB collection exports as CSV, a JSON array or NDJSON (one document on each line) in relaxed or canonical Extended JSON, and as a mongosh script, alone or with the data in `insertMany` batches.
- View Redis results in the grid and export them as CSV. Edit supported string, hash, list, set and sorted set results. A save checks the old values before it writes.

### Table structure

- Table Properties shows the columns, constraints, foreign keys, references, indexes and DDL of a table.
- A MongoDB collection shows its Fields, Indexes, Validator, Options, Stats and Script. You can add or drop an index, and add, edit or remove a `$jsonSchema` validator. The dialog shows the mongosh command before it runs; a production connection asks you to confirm. **Script** shows the mongosh text that makes the collection again, and Generate script in the tree opens it in an editor. A view shows Fields, Options and Script.
- Redis key properties show the type, encoding, TTL, length and memory use. Generate or export a `.redis` script to restore the key's data and TTL.
  Use the desktop app to export a full key. Both browser modes can show the script preview.
- Add, edit or drop columns, constraints, foreign keys and indexes. The dialog shows the SQL before it runs.
- The ER diagram shows the table and its related tables. You can move the boxes and zoom.

### AI assistant

- Ask questions about your data in a chat. The assistant can use up to 8 sources in one chat, with SQL, MongoDB and Redis together.
- The assistant reads the schema first. Then it runs read-only queries through a read guard. On MongoDB it reads with one mongosh statement that the same parser and read guard check as in the editor.
- The assistant can propose a write only for a source that allows AI writes and has write mode on in the chat. For other sources it can draft the change without running it. Before a write runs, you must approve it in the chat. That covers MongoDB statements too: an approved write runs as one statement, so a change of many documents is not one transaction.
- For Redis, the assistant and MCP share the same read guard. It refuses writes, `KEYS`, blocking commands, and some commands that show private server details. A Redis write that the assistant proposes needs your approval.
  The editor and user-written query schedules can read those server details if the Redis account has permission.
- The assistant makes bar, line, area, pie and scatter charts.
- Ask for a report, and the assistant writes one document with a summary, charts and tables.
- Skills give the assistant extra instructions for one database or one task. sandb has 14 built-in skills: PostgreSQL, MySQL and MariaDB, SQLite, MongoDB aggregation, and Redis commands; performance tuning for each of these five database engines; and general query tuning, data quality, time series and schema review. The assistant sees only the skills for the databases of the chat, and loads a skill when your question matches its description. Type `/` in the chat box to pick one or more skills yourself.
- Run `EXPLAIN ANALYZE` yourself in the editor, then click **Review with AI** under the plan. The assistant reviews the plan, but it cannot run `EXPLAIN ANALYZE` itself.
- Export a reply as a PDF.
- With the "Schema only" privacy setting, the AI model gets the column names and the row count, but no row values.
- The chat history stays after a restart.

sandb supports two AI server formats:

| Option | Examples |
|--------|----------|
| OpenAI-compatible | Ollama, LM Studio, OpenRouter, OpenAI |
| Anthropic-compatible | Anthropic |

In Settings > AI, set the provider, the base URL, the model and the API key.

### MCP clients

MCP tools are read-only. Start with `list_connections` and pass the returned id as `connection`, not a chat `source` id. If you select a non-default database, pass its name on every call (for Redis, use a string such as `"1"`). Omit `database` only when you want the saved default. Unexpected arguments or malformed targets return an error instead of silently reading the default.

### Schedules

- Run a query or ask the AI assistant a question at set times: every N minutes, every N hours, every day, or on some days of the week.
- A query schedule is read-only (one read statement in a read-only session) or allows writes. On a production connection, you must confirm scheduled writes first.
- Each AI run starts a new chat in the chat history. In a scheduled run, the assistant can only read.
- Each schedule keeps a run history with the rows or the reply of its newest runs. sandb shows a message in the app or a system notification when a run ends.
- Schedules run only while sandb is open. A run that was due while sandb was closed runs one time after sandb starts.

### Appearance and updates

- Use a light or dark theme.
- Show an image behind the app, or make the window see-through.
- The app finds new versions and asks before it installs an update.

For the full list of changes, see [CHANGELOG.md](CHANGELOG.md).

## Build

### Requirements

- Rust 1.90 or later
- Node.js and pnpm

### Steps

1. Install the dependencies:

   ```sh
   pnpm install
   ```

2. Build the app without the updater files:

   ```sh
   pnpm tauri build --config '{"bundle":{"createUpdaterArtifacts":false}}'
   ```

3. Find the app in `src-tauri/target/release/bundle/`.

To build for one Mac architecture, add a target:

```sh
pnpm tauri build --target aarch64-apple-darwin --config '{"bundle":{"createUpdaterArtifacts":false}}'  # Apple Silicon
pnpm tauri build --target x86_64-apple-darwin --config '{"bundle":{"createUpdaterArtifacts":false}}'   # Intel
```

A build for the other architecture needs its Rust target. Install it one time, for example `rustup target add x86_64-apple-darwin`.

A release build makes updater files, and it needs the signing key. For this procedure, see [docs/RELEASING.md](docs/RELEASING.md).
