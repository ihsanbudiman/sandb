# sandb MCP server

## Context

Make sandb an MCP server, so that external agents (Claude Code, Cursor, Claude Desktop) can read the databases that sandb manages. The user decided three things:
- **Transport:** the desktop app runs a Streamable HTTP MCP server on `127.0.0.1` and checks a bearer token. You turn it on in Settings. The app must be open.
- **Connection access:** MCP settings keep an allowlist (`connections: [id…]`) of the saved connections that MCP clients can use. The list is empty by default. You edit it in Settings > MCP. The connection dialog and `Connection` do not change.
- **Writes:** none. MCP gives only schema tools and read tools. All reads go through the read guard and the read-only session that the AI chat uses.

A client can see only the allowlisted connections. It picks a connection with the `connection` argument on each tool call, and it can pick a database with `database`. The client gets the ids from `list_connections`. The server keeps no per-session connection state.

## Approach

### 1. Dependencies (`src-tauri/Cargo.toml`)
- Make `axum` non-optional: change it to `axum = "0.8"` and remove `"dep:axum"` from the `dev-bridge` feature. `tower-http` and `tempfile` stay in `dev-bridge`.
- Add `rmcp = { version = "3.11", default-features = false, features = ["server", "transport-streamable-http-server"] }`. rmcp 3.11.0 has MSRV 1.88, and the crate has `rust-version = "1.90"`. Do not enable `macros`, because the tool list is dynamic.

### 2. Shared token comparison
- Move `fn ct_eq(a: &[u8], b: &[u8]) -> bool` from `src-tauri/src/bin/bridge.rs` to `src-tauri/src/core/secrets.rs` as `pub fn ct_eq`. Keep the body the same. In `bridge.rs`, delete the local copy and `use sandb_lib::core::secrets::ct_eq`.
- In `secrets.rs`, add `pub const MCP_TOKEN_ACCOUNT: &str = "mcp:token";` next to `AI_KEY_ACCOUNT`. Do not add it to the keychain migration, because that migration is only for releases before 0.8.

### 3. Tool definitions for clients outside the chat (`core/ai/tools.rs`)
- Move the description `match` from `definitions()` (lines ~238-247, the mixed SQL + MongoDB suffix) to `fn description(t: &Tool, engines: Engines) -> String`. Make `definitions()` call it. The output must stay the same, and the existing tests prove this (`sql_only_chats_get_the_sql_tools_unchanged`, `mixed_chats_split_the_source_enum_per_engine`).
- Add:
  ```rust
  /// One tool's description and parameters for a client outside the chat (the MCP server):
  /// worded for SQL and MongoDB both, without `source`. `None` for a name not in the table.
  pub fn definition(name: &str) -> Option<(String, Value)> {
      tool_named(name).map(|t| {
          let (_, params) = (t.build)(t, true);
          (description(t, Engines { sql: true, mongo: true }), params)
      })
  }
  ```

### 4. Core MCP module (`src-tauri/src/core/mcp/`, no Tauri types)
Register `pub mod mcp;` in `core/mod.rs`. Use three files.

**`core/mcp/settings.rs`**
```rust
const SECTION: &str = "mcp";
pub const DEFAULT_MCP_PORT: u16 = 58124;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpSettings { pub enabled: bool, pub port: u16, pub connections: Vec<String> }
// Default: enabled false, port DEFAULT_MCP_PORT, connections empty.

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    #[serde(flatten)] pub settings: McpSettings,
    pub token: String,
    /// `http://127.0.0.1:{port}/mcp`, also when stopped, for the config snippets.
    pub url: String,
    pub running: bool,
    /// The last start failure (for example, a port in use); `None` while running or off.
    pub error: Option<String>,
}
```
- `pub fn load(state: &AppState) -> Result<McpSettings, AppError>` returns `state.store.load_section(SECTION)`.
- `fn normalized(mut self, state: &AppState) -> Result<Self, AppError>`: a port below 1024 gives `AppError::invalid("The port must be between 1024 and 65535")`. For `connections`, keep the first instance of each id. Remove ids that are not in `state.list_connections()`.
- `pub(crate) fn save(state: &AppState, s: &McpSettings) -> Result<(), AppError>` calls `state.store.save_section(SECTION, s)`. It is `pub(crate)` because the tests in `core/mcp/tools.rs` call it.
- `pub fn token(state) -> Result<String, AppError>`: read `MCP_TOKEN_ACCOUNT`. If the entry is missing or empty, make a token, set it, and return it.
- `fn new_token() -> String` returns `format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())`. This gives 64 hex chars and 244 random bits, and uses only `uuid`, which is already a dependency.

**`core/mcp/tools.rs`**: the tool list and the calls. Use the chat's tool code again.
- `pub struct McpTool { pub name: &'static str, pub description: String, pub schema: Value }`
- `pub struct McpCall { pub text: String, pub is_error: bool }`
- `const DB_TOOLS: [&str; 4] = ["list_tables", "describe_table", "run_read_query", "run_mongo_read"];`
- `pub fn list() -> Vec<McpTool>`, in this order:
  1. `list_connections`: "List the database connections that sandb shares with MCP clients: id, name, engine, environment, default database and read-only flag. Call this first: every other tool takes a connection id from this list." Schema `{"type":"object","properties":{}}`.
  2. `list_databases`: "List the databases on the server of one connection." Schema with `connection` only.
  3. Each name in `DB_TOOLS`: take `(description, params)` from `ai::tools::definition(name).expect("the chat table has every MCP database tool")`. Add `properties.connection = {"type":"string","description":"Connection id from list_connections."}`. Add `properties.database = {"type":"string","description":"Database name. Default: the connection's own database. Required for a MongoDB connection without one; see list_databases."}`. Push `"connection"` to `required`, and make the array if it is missing.
- `pub async fn call(state: &AppState, name: &str, mut args: Map<String, Value>) -> McpCall`:
  1. Load `settings::load(state)`. If the load fails, return an error result.
  2. `list_connections`: return `state.list_connections()` filtered to `settings.connections`, in that order, as a JSON array of `{id, name, engine, env, database, readOnly, version}`. Use the serde (camelCase) names of `Connection`. Do not send `user`, `host`, `port`, `file` or `uri`, because a client does not need connection details to use the tools.
  3. Any other name: read `connection` as a string. If it is missing, return the error `The tool needs "connection", an id from list_connections`. If the id is not in the allowlist, or `state.connection(id)` fails, return the same `not_found` message for both: `Connection {id} is not shared with MCP clients. Use an id from list_connections`. This way, the server does not tell a client which ids exist but are not shared.
  4. `list_databases`: `ops::list_databases(state, id)` → `{"databases": [...]}`.
  5. `DB_TOOLS`: `conn = state.connection(id)?`. `database = connections::target_database(&conn, args.get("database").and_then(Value::as_str))`. For a MongoDB connection with an empty database, return the error `Pass "database" for {conn.name}: the connection has no default database. Call list_databases`. Build `Source { id: "db1".into(), conn, database, schema: None, allow_writes: false }`. Load `ai::settings::load(state)?` so that MCP uses the same privacy, max rows and time limit as the chat. Remove the `connection`, `database` and `source` keys from `args`. Then call `ai::tools::execute(&ToolContext { state, sources: &[source], settings: &ai }, "mcp", name, &Value::Object(args).to_string())`. `text = out.content`. `is_error = !out.events.iter().any(|e| matches!(e, ChatEvent::ToolResult { ok: true, .. }))`. Engine mismatch, guard refusal, privacy hiding and the 32 KiB cap all come from `execute`.
  6. An unknown name returns the error `Unknown tool {name}`.
  - Error text before `execute`: `json!({"error": {"code": e.code, "message": e.message}}).to_string()` with `is_error: true`.

**`core/mcp/server.rs`**: the HTTP server and its life cycle.
- ```rust
  /// The app state for the life of the server task. The desktop app passes its `AppHandle`;
  /// tests pass `Arc<AppState>`.
  pub trait StateHandle: Clone + Send + Sync + 'static { fn state(&self) -> &AppState; }
  impl StateHandle for Arc<AppState> { fn state(&self) -> &AppState { self } }
  ```
- `#[derive(Default)] pub struct McpServer { slot: tokio::sync::Mutex<Slot> }`, with `#[derive(Default)] struct Slot { running: Option<Running>, error: Option<String> }` and `struct Running { addr: SocketAddr, token: String, stop: CancellationToken, task: JoinHandle<()> }`. Add the field `pub(in crate::core) mcp: McpServer` to `AppState` in `core/state.rs` and initialize it with `McpServer::default()`.
- `#[derive(Clone)] struct Handler<H> { handle: H }` with `impl<H: StateHandle> rmcp::ServerHandler`. The rmcp 3.11 names below come from its source:
  - `get_info(&self) -> ServerConfig`: `ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new("sandb", env!("CARGO_PKG_VERSION"))).with_instructions("Read-only access to the databases that the sandb user shares. Call list_connections first; pass its id as connection on every other tool.")`.
  - `async fn list_tools(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData>`: `ListToolsResult::with_all_items(tools::list().into_iter().map(|t| Tool::new(t.name, t.description, Arc::new(t.schema.as_object().cloned().unwrap_or_default())).with_annotations(ToolAnnotations::new().read_only(true))).collect())`.
  - `async fn call_tool(&self, req: CallToolRequestParams, _: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData>`: `let out = tools::call(self.handle.state(), &req.name, req.arguments.unwrap_or_default()).await;`. Return `Ok(CallToolResult::success/error(vec![ContentBlock::text(out.text)]).into())`. Tool failures are always `Ok(CallToolResult::error)`, never `Err`, so that the client sees the message.
- `async fn start<H: StateHandle>(handle: H, port: u16, token: String) -> Result<Running, AppError>`:
  - `TcpListener::bind((Ipv4Addr::LOCALHOST, port))`. If the bind fails: `AppError::invalid(format!("Could not listen on 127.0.0.1:{port}: {e}"))`. `addr = listener.local_addr()?`. Port 0 is for tests.
  - `stop = CancellationToken::new()`.
  - `StreamableHttpService::new(move || Ok(Handler { handle: handle.clone() }), LocalSessionManager::default().into(), StreamableHttpServerConfig::default().enforce_origin_validation().with_cancellation_token(stop.child_token()))`. The default `allowed_hosts` (`localhost`, `127.0.0.1`, `::1`) blocks DNS rebinding. `enforce_origin_validation()` with an empty list refuses all browser requests that have an `Origin` header.
  - `Router::new().nest_service("/mcp", service).layer(middleware::from_fn_with_state(Arc::<str>::from(token.as_str()), auth))`. `auth` is the same as the bridge's `auth`: read `Authorization: Bearer …` and compare with `secrets::ct_eq`. If the check fails, return `(StatusCode::UNAUTHORIZED, Json(AppError::new(ErrorCode::Unauthorized, "Missing or wrong sandb MCP token"))).into_response()`.
  - Spawn `axum::serve(listener, router).with_graceful_shutdown(stop.clone().cancelled_owned())` with `tokio::spawn`. If the server fails, it writes `eprintln!("sandb: MCP server stopped: {e}")`.
- `async fn stop(r: Running)`: `r.stop.cancel()`, then `let _ = tokio::time::timeout(Duration::from_secs(3), r.task).await;`. The timeout ensures that a client that does not close its stream does not stop a restart.
- `pub async fn status(state: &AppState) -> Result<McpStatus, AppError>`: lock the slot. Load settings and the token. `running = slot.running.is_some()`, `error = slot.error.clone()`, `url = format!("http://127.0.0.1:{}/mcp", settings.port)`.
- `pub async fn apply<H: StateHandle>(handle: &H) -> Result<McpStatus, AppError>`: lock the slot for the full operation, so that two applies cannot overlap.
  - Load the settings and the token.
  - If `enabled`, and the server runs with the same port and the same token, do nothing. Allowlist changes need no restart, because `tools::call` reads the settings on each call.
  - Otherwise, stop the old server. If `enabled`, call `start`. On success, set `running` and clear `error`. On failure, set `error = Some(e.message)`. Do not return `Err`, so that the saved settings stay and the UI shows the failure.
  - Return the status. To prevent a deadlock, build the status from the held slot. Do not call `status()`.
- `pub async fn save<H: StateHandle>(handle: &H, s: McpSettings) -> Result<McpStatus, AppError>`: `normalized` → `settings::save` → `apply`.
- `pub async fn new_token<H: StateHandle>(handle: &H) -> Result<McpStatus, AppError>`: set `MCP_TOKEN_ACCOUNT` to `new_token()`, then call `apply`. The token is different, so the server restarts, and the old token stops working.
- Re-export from `core/mcp/mod.rs`: `pub use server::{StateHandle, apply, new_token, save, status}; pub use settings::{McpSettings, McpStatus, DEFAULT_MCP_PORT};`.

### 5. Desktop wiring (`src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`)
- `lib.rs`: `impl core::mcp::StateHandle for tauri::AppHandle { fn state(&self) -> &AppState { Manager::state::<AppState>(self).inner() } }`. In `.setup`, after `app.manage(state)`, add:
  ```rust
  let handle = app.handle().clone();
  tauri::async_runtime::spawn(async move {
      match core::mcp::apply(&handle).await {
          Ok(s) => if let Some(e) = s.error { eprintln!("sandb: MCP server: {e}") },
          Err(e) => eprintln!("sandb: MCP server: {e}"),
      }
  });
  ```
  Call `tauri::async_runtime::spawn` because the setup closure has no Tokio context.
- `commands.rs`, in the `async` block: `get_mcp_status GetMcpStatusArgs {} -> McpStatus => async |st: &AppState, _| { mcp::status(st).await };`. The bridge serves it, and it reports `running: false`.
- `commands.rs`, in the `native` list (doc comment: "Restarts the MCP server, which only the desktop app runs"): `save_mcp_settings` and `new_mcp_token`. Write them by hand next to `quit_app`:
  ```rust
  #[tauri::command]
  pub async fn save_mcp_settings(app: AppHandle, settings: McpSettings) -> Res<McpStatus> { mcp::save(&app, settings).await }
  #[tauri::command]
  pub async fn new_mcp_token(app: AppHandle) -> Res<McpStatus> { mcp::new_token(&app).await }
  ```
- `src-tauri/src/wire.rs`: add `decl::<McpSettings>(cfg)` and `decl::<McpStatus>(cfg)` in alphabetical order in `types()`. Add `("DEFAULT_MCP_PORT", DEFAULT_MCP_PORT.to_string())` to `constants()`. Run `cargo test export_bindings` once to make `src/lib/api/generated/wire.ts` and `constants.ts` again. The first run fails, because it writes new files. The second run passes.

### 6. Frontend (`src/lib/api/*`, `src/lib/state.svelte.ts`, `SettingsPage.svelte`)
- `api/types.ts`: add `mcp: boolean` to `Capabilities`, with the doc "The desktop app runs the MCP server.". Add these `Backend` methods: `getMcpStatus(): Promise<McpStatus>; saveMcpSettings(settings: McpSettings): Promise<McpStatus>; newMcpToken(): Promise<McpStatus>;`. Import the types from `generated/wire`, as the other types do.
- `api/index.ts`: set `mcp: true` in `desktop`, and `mcp: false` in `bridge` and `demo`.
- `api/backend.ts`: `getMcpStatus: () => call('get_mcp_status')`, `saveMcpSettings: (settings) => call('save_mcp_settings', { settings })`, `newMcpToken: () => call('new_mcp_token')`. The mock needs no handler, because the section stays hidden without `capabilities.mcp`.
- `state.svelte.ts` line 9: add `'mcp'` to the `settingsSection` union.
- `SettingsPage.svelte`:
  - In `sections`, after `safeguards`, add `{ id: 'mcp', l: 'MCP', i: Plug }`, but only when `api.capabilities.mcp`. Use the same conditional spread as `backdrops`. Import `Plug` and `Copy` from `@lucide/svelte`.
  - Local state: `let mcp = $state<McpStatus | null>(null); let mcpError = $state<ApiError | null>(null);`. Use an `$effect` that loads `api.getMcpStatus()` when `ui.settingsSection === 'mcp'` and `mcp === null`.
  - `async function saveMcp(patch: Partial<McpSettings>)`: send `{ enabled, port, connections }` from `mcp` with the patch to `api.saveMcpSettings`. Then set `mcp` to the result. Catch the error into `mcpError` with `ApiError.from`.
  - Section 1, "MCP server": copy the layout of the Safeguards `<section>` (the `grid … sm:grid-cols-[180px_1fr]` pattern and the `role="switch"` toggle markup at lines 255-264).
    - Left column text: "Let AI tools such as Claude Code and Cursor read the connections that you choose. Writes are never allowed."
    - Right column: an Enabled switch, and a port `<input type="number" min="1024" max="65535">` that saves on `change`.
    - The status line: "Running at {url}" (emerald, `Check` icon), "Off", or `mcp.error` (red). Then `mcpError?.message`.
  - Section 2, "Shared connections": copy the Safeguards connection list (`{#each db.connections as c (c.id)}`, with engine badge, name and env tag), with a checkbox for each row. `checked={mcp.connections.includes(c.id)}`. A change saves `connections` with the id added or removed. Keep `db.connections` order. If there are no connections, show "Add a connection first."
  - Section 3, "Connect a client": show the token masked (`••••` + last 4 characters), an Eye/EyeOff reveal button, a Copy button (`navigator.clipboard.writeText`, as in `AuditPage.svelte:53`), and a "New token" button that calls `api.newMcpToken()`. Add the title "Clients that use the old token stop working". Under them, show two `<pre>` snippets, each with a Copy button. Build them from `mcp.url` and `mcp.token`:
    - Claude Code: `claude mcp add --transport http sandb {url} --header "Authorization: Bearer {token}"`
    - JSON (Cursor and other clients): `{"mcpServers":{"sandb":{"url":"{url}","headers":{"Authorization":"Bearer {token}"}}}}`. Format it with `JSON.stringify(…, null, 2)`.
    - One `text-faint` line: "Claude Desktop: use npx mcp-remote {url} --header \"Authorization: Bearer …\"".

## Critical files & anchors
- `src-tauri/src/core/ai/tools.rs`: `definitions()` (description `match`, ~L238), `execute()` (~L599), `ToolContext::source()` (~L516). With one source and no `source` key, it picks that source.
- `src-tauri/src/core/ai/session.rs` `converse()` ~L220-242: the model for how a `ChatSource` becomes a `Source`, which `core/mcp/tools.rs::call` does again for one connection.
- `src-tauri/src/bin/bridge.rs` `auth()` / `ct_eq()`: the auth middleware pattern to copy. `ct_eq` moves to `core/secrets.rs`.
- `src-tauri/src/commands.rs` `command_table!` `native` list ~L305: the place for the two commands that restart the server.
- `src/lib/components/SettingsPage.svelte` ~L234-268: the Safeguards connection list, the layout to copy for "Shared connections".

## Verification
Run all commands from `src-tauri/`, except where the step names another directory.

1. Unit tests in `core/mcp/tools.rs` (`#[cfg(test)]`). First, move `sqlite_conn` from `core/ai/session.rs` (~L448) to `core/ai/test_support.rs` as `pub(crate) async fn sqlite_conn`. Then change all the callers in `session.rs` to use it (`grep -n "sqlite_conn(" src/core/ai/session.rs`: L512, 513, 845, 890). Then add these tests:
   - `only_shared_connections_are_visible`: make connections `a` and `b` (3 rows each), and save `McpSettings { enabled: false, port: DEFAULT_MCP_PORT, connections: vec![a] }` with `settings::save`. `call("list_connections")` must return exactly one entry, with `id == a`. `call("run_read_query", {connection: b, sql: "SELECT 1"})` must return `is_error` with the text "not shared".
   - `reads_use_the_guard`: `{connection: a, sql: "SELECT count(*) AS n FROM t"}` must return `!is_error`, and the text must contain `3`. `{connection: a, sql: "DELETE FROM t"}` must return `is_error` with `guard_rejected` in the text. After that, `count(*)` must still return 3.
   - `tool_list_is_read_only`: the names from `list()` must equal `["list_connections","list_databases","list_tables","describe_table","run_read_query","run_mongo_read"]`. Each tool except `list_connections` must have `"connection"` in `schema.required`.
   - Command: `cargo test --lib mcp`. Then run `cargo test --lib` to prove that the moved helper and the `definitions()` change keep the old tests green.
2. Server test in `core/mcp/server.rs`: call `start(Arc::new(state), 0, "t0k".into())`. Use `reqwest` (already a dependency) to POST to `http://{addr}/mcp` with `Accept: application/json, text/event-stream` and the body `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}`.
   - With no `Authorization`, the response must be 401.
   - With `Authorization: Bearer t0k`, the response must be 200, and the body must contain `"serverInfo"` and `"sandb"`.
   - Then call `stop(running)` and check that a new bind to the same `addr` succeeds.
3. Bindings: run `cargo test export_bindings` (twice, see step 5 of the Approach), then `cd .. && pnpm check`. The expected result is 0 errors.
4. Lint gate: run `cargo clippy --all-targets -- -D warnings` and `cargo clippy --all-targets --features dev-bridge -- -D warnings`. These prove that the bridge still builds with the shared `ct_eq` and the non-optional axum.
5. End-to-end (manual, desktop):
   - Run `SANDB_DATA_DIR=/tmp/sandb-mcp pnpm tauri dev` from the repo root.
   - Add a SQLite connection to `testdb/` or to any `.db` file. In Settings > MCP, enable the server, select the connection, and copy the Claude Code command.
   - Expected: the status shows "Running at http://127.0.0.1:58124/mcp".
   - Run the copied `claude mcp add …`, then `claude mcp list`. Expected: `sandb` shows as connected.
   - Ask Claude Code "list the tables in my sandb connection". Expected: it calls `list_connections`, then `list_tables`, and shows the tables.
   - Ask it to delete a row. Expected: the read guard refuses the query (`guard_rejected`), and no MCP tool can write.
   - Clear the checkbox and call again. Expected: "not shared".
   - Click "New token". Expected: the old `claude` config gets 401.

## Assumptions & contingencies
- MCP uses the AI settings for privacy ("Schema only" also hides row values from MCP clients), max rows and time limit. If MCP should have its own limits, add fields to `McpSettings` later. The current design does not need them.
- MCP reads do not show in the Audit page, because that log is frontend-only (`src/lib/audit.svelte.ts`).
- The default port is 58124. A bind failure is shown as `McpStatus.error` and does not stop the app.
- If an rmcp 3.11 name in step 4 does not compile, check the source of the pinned version in `~/.cargo/registry/src/*/rmcp-3.11.*/src/handler/server.rs` and `src/model.rs`. The names above come from the rmcp 3.11.0 source. If rmcp cannot be resolved together with the current `tokio-util`/`axum` versions, pin the newest rmcp 3.x that resolves. Do not write the protocol by hand.
