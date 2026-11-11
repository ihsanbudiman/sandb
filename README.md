# sandb

sandb is a light desktop database client with an AI assistant. You use your own AI server and your own API key (BYOK).

## Features

### Connections

- Connect to PostgreSQL, MySQL, MariaDB and SQLite.
- Connect to MongoDB with fields (host, port, database, user, password, auth source and TLS) or a `mongodb://` / `mongodb+srv://` connection string. A test of the connection shows the server version and the topology: standalone, replica set or sharded. A password, also one inside the string, is saved in the credential store, and the connections file keeps the string without it.
- Set the environment of each connection: local, staging or production. A production connection asks you to confirm dangerous changes.
- Mark a connection as read-only. MongoDB has no read-only session, so a read-only MongoDB connection relies on the app's guard, and the dialog recommends a database user with the `read` role for full protection.
- The connection tree shows tables, views and PostgreSQL materialized views in separate folders. A MongoDB connection shows its databases, then a Collections and a Views folder. Expanding a collection lists its field paths (`address.city`, `items[].sku`) with their types and the share of sampled documents that have each one.
- The app keeps passwords and the API key in the credential store of the operating system.

### Query editor

- Write SQL or, on a MongoDB tab, mongosh-style statements with autocomplete and syntax highlighting. sandb reads the mongosh text with its own parser and never runs JavaScript.
- Open more than one query in tabs.
- Run the statement at the cursor, the selected text, all statements, or a `.sql` file.
- Format the query (⇧⌘F, or **Format query** in the editor menu). With a selection, only the selection changes. SQL breaks into one clause on each line. Mongosh text breaks documents and arrays that do not fit on one line. The format keeps the case of keywords and every value as you wrote it, for example `1.0`.
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

### Table structure

- Table Properties shows the columns, constraints, foreign keys, references, indexes and DDL of a table.
- A MongoDB collection shows its Fields, Indexes, Validator, Options, Stats and Script. You can add or drop an index, and add, edit or remove a `$jsonSchema` validator. The dialog shows the mongosh command before it runs; a production connection asks you to confirm. **Script** shows the mongosh text that makes the collection again, and Generate script in the tree opens it in an editor. A view shows Fields, Options and Script.
- Add, edit or drop columns, constraints, foreign keys and indexes. The dialog shows the SQL before it runs.
- The ER diagram shows the table and its related tables. You can move the boxes and zoom.

### AI assistant

- Ask questions about your data in a chat. The assistant can use up to 8 databases in one chat, also MongoDB databases next to SQL databases.
- The assistant reads the schema first. Then it runs read-only queries through a read guard. On MongoDB it reads with one mongosh statement that the same parser and read guard check as in the editor.
- Before a write runs, you must approve it in the chat. That covers MongoDB statements too: an approved write runs as one statement, so a change of many documents is not one transaction.
- The assistant makes bar, line, area, pie and scatter charts.
- Ask for a report, and the assistant writes one document with a summary, charts and tables.
- Export a reply as a PDF.
- With the "Schema only" privacy setting, the AI model gets the column names and the row count, but no row values.
- The chat history stays after a restart.

sandb supports two AI server formats:

| Option | Examples |
|--------|----------|
| OpenAI-compatible | Ollama, LM Studio, OpenRouter, OpenAI |
| Anthropic-compatible | Anthropic |

In Settings > AI, set the provider, the base URL, the model and the API key.

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
