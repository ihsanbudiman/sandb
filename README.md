# sandb

sandb is a light desktop database client with an AI assistant. Use your own AI server and API key (BYOK), or an eligible ChatGPT plan.

## Features

### Connections

- Connect to PostgreSQL, MySQL, MariaDB and SQLite.
- Set the environment of each connection: local, staging or production. A production connection asks you to confirm dangerous changes.
- Mark a connection as read-only.
- The connection tree shows tables, views and PostgreSQL materialized views in separate folders.
- The app keeps passwords and the API key in the credential store of the operating system.

### SQL editor

- Write SQL with autocomplete and syntax highlighting.
- Open more than one query in tabs.
- Run the statement at the cursor, the selected text, all statements, or a `.sql` file.
- Save a query with a name (⌘S). Find saved queries in the Queries list or in the command palette (⌘K).

### Results and table data

- Scroll the results grid. The grid loads more rows when you come near the last row.
- Double-click a cell to edit it. Save applies all row changes in one transaction.
- Sort by a column. Filter table data by column, operator and value.
- Click the arrow in a foreign key cell to open the row that it refers to.
- PostGIS `geometry` and `geography` values show as WKT.
- Export a table as CSV, as DDL, or as SQL with data.

### Table structure

- Table Properties shows the columns, constraints, foreign keys, references, indexes and DDL of a table.
- Add, edit or drop columns, constraints, foreign keys and indexes. The dialog shows the SQL before it runs.
- The ER diagram shows the table and its related tables. You can move the boxes and zoom.

### AI assistant

- Ask questions about your data in a chat. The assistant can use up to 8 databases in one chat.
- The assistant reads the schema first. Then it runs read-only queries through a read guard.
- Before a write runs, you must approve it in the chat.
- The assistant makes bar, line, area, pie and scatter charts.
- Ask for a report, and the assistant writes one document with a summary, charts and tables.
- Export a reply as a PDF.
- With the "Schema only" privacy setting, the AI model gets the column names and the row count, but no row values.
- The chat history stays after a restart.

sandb supports these AI options:

| Option | Examples |
|--------|----------|
| OpenAI-compatible | Ollama, LM Studio, OpenRouter, OpenAI |
| Anthropic-compatible | Anthropic |
| ChatGPT plan (preview) | Eligible ChatGPT Plus / Pro accounts |

For API-key providers, set the provider, base URL, model and API key in Settings > AI.

For ChatGPT, choose **Settings > AI > ChatGPT plan**, then **Continue with ChatGPT**. Authorize sandb in your system browser and select an account model. No Codex installation is required. If sign-in succeeds without usage permission, choose **Enable ChatGPT plan usage** to request it explicitly.

Eligible requests use your ChatGPT plan or credits balance; sandb never silently falls back to paid API credentials. Availability depends on account, workspace, region and preview admission. **Manage usage** opens ChatGPT usage settings. **Sign out** clears local tokens and attempts remote revocation; sandb warns if revocation could not be confirmed.

The existing prompt, database tools, privacy settings and write safeguards are unchanged. OAuth credentials stay encrypted in backend storage, not browser storage. ChatGPT sign-in requires the native desktop app; the demo and dev bridge do not support it. Only one native sandb instance may use a data directory at a time.

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
