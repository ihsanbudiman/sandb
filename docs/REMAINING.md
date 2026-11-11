# sandb: work that is left

Updated: 2026-11-03. Project root: `/Users/bvt-ihsanbudiman/Documents/project/sandb/`.

Related documents:
- Base spec: `~/.factory/specs/2026-11-02-light-db-client-with-byok-ai-chat-and-read-only-safeguards.md`
- Build plan (phases 0 to 7): `~/.factory/specs/2026-11-02-dbchat-desktop-build-plan.md`
- Phase 3b brief and backend API contract: `~/.factory/specs/2026-11-03-sandb-phase-3b-brief.md`

## 1. Status

| Phase | Scope | Built | Reviewed | QA |
|---|---|---|---|---|
| 0 | Test databases, seeds, mock LLM, scripts | Yes | Not needed | PASS |
| 1 | Tauri scaffold, connections, keychain, dev bridge | Yes | Yes | PASS (2 x P3) |
| 2 | Schema tree, editor runs, paging, cancel, CSV | Yes | Yes (2 rounds) | PASS |
| 3 | AI read guard and read-only executor | Yes | APPROVE (6 rounds) | PASS (BACKEND) |
| 3b | Read-only connection, Production manual commit, editor target | Yes | Yes (1 round; 7 P1, 4 P2, 6 P3 all fixed) | PASS (bridge, final code) |
| 4 | AI service (streaming client, tool loop) | Yes | Yes (1 round; 1 P1, 4 P3 all fixed) | PASS (bridge smoke, final code) |
| 5 | Chat panel, write preview, safeguard toggle, audit log | Partly: Settings save and Test, live chat, Stop | No | Partly (bridge smoke) |
| 6 | Edit mode in the result grid | No | No | No |
| 7 | Hardening, release build, final review and QA | No | No | No |

Current gate results (2026-11-04, after phase 4 review fixes):
- `cargo fmt --check`: pass. `cargo clippy --all-targets -- -D warnings`: pass (also with `--features dev-bridge` and `--features it`).
- `cargo test --lib`: 176 passed.
- `cargo test --features it -- --test-threads=4` (with `SANDB_MOCK_LLM_URL` set to a mock that has the `/_requests` endpoint): it_ai 12, it_connections 9, it_query 24, it_readonly 13, it_conn_ro 4, it_session 22. All pass.
- `pnpm check`: 0 errors, 0 warnings. `pnpm build`: pass.

Phase 4 notes:
- Code: `src-tauri/src/core/ai/` (`settings.rs`, `client.rs`, `tools.rs`, `prompt.rs`, `session.rs`). Commands: `get_ai_settings`, `save_ai_settings`, `set_ai_key`, `test_ai`, `list_ai_models`, `chat_send`, `chat_cancel` (Tauri and bridge; the bridge streams `chat_send` as SSE).
- `test_ai` calls `GET /models` and fails with `model_not_found` if the model is not in the list (`name:latest` also matches, for Ollama).
- `list_ai_models` calls `GET /models` for the given (maybe unsaved) settings and key, and returns the ids sorted, without duplicates. The UI model picker (`ModelPicker.svelte`, in Settings and below the chat message box) uses it. The model is one setting for all chats.
- `propose_write` never runs SQL itself. It refuses with `write_blocked` when the chat's write mode is off or the connection is read-only. Otherwise it sends `write_preview`, and the tool loop waits (`core/ai/approval.rs`, 10-minute timeout that denies) for `approve_write(chatId, id)` or `deny_write(chatId, id)`. An approved write runs through `core/db/write.rs`: it checks the saved connection again (`writes_allowed`), refuses transaction-control statements, and runs all statements in one transaction that commits at once on a connection that leaves the pool. `allowWrites` comes from the chat panel toggle, which is UI-only.
- Privacy `schema`: the model gets column names, the row count and the truncated flag. Database error text is replaced by the SQLSTATE or error number.
- Stop drops the running AI read. The database query stops at its own time limit.
- The `sandb-test` mock-llm container runs the old `server.mjs` until it restarts. `it_ai` needs the new `GET /_requests` endpoint.

Phase 3b semantics after the review fixes:
- A tab session exists only while its transaction holds uncommitted work. A run of plain reads uses the pool. Commit, Rollback, a typed COMMIT and a MySQL implicit commit at the end of a run all close the session, so `tab_status` returns `null` then.
- Statements that can end a transaction are probed; the next write opens a new transaction. Nothing autocommits silently.
- The MySQL implicit-commit notice adds ", including the uncommitted changes made before it" when work was pending.
- The connection dialog and delete wait for the three-way prompt before the backend closes sessions, and the backend closes sessions before pools (the old order deadlocked).
- QA notes: `.shots/qa/phase3b/`. QA ran on the final code, on the live stack through the bridge. Checked: the badges; the read-only refusal; a read on Production opens no transaction; the pending indicator; Commit and Rollback; a second session cannot see the write before Commit and sees it after; a truncated read over the row limit keeps the pending write, and Commit persists it; the three-way prompt on tab close, database switch, connection save (the old deadlock) and connection delete; the aborted message; the target dropdowns. Not checked: the quit prompt in a real Tauri window (⌘Q is a custom menu item, phase 7), the Tauri CSV save dialog, and a prompt that an environment change in the connection dialog triggers (it uses the same `canupdate` path as the tested connection save).

The workflow for each phase is the same: `sandb-worker` builds, `code-reviewer` reviews, `sandb-worker` fixes P0/P1, and `qa-tester` tests. QA runs last, on the final code.

## 2. Phase 3b: done

Backend and frontend are built, reviewed and QA'd on the bridge. Sections 2.2 to 2.7 below are kept as a record of what was built. Open items for later phases:
- The ⌘Q prompt is a custom menu item; Dock Quit and logout/shutdown still exit without it (tao limitation).
- Dropped futures mid-run (a bridge client disconnect) leave an in-use slot; Tauri commands are not affected.
- The quit prompt from the real Tauri window is still untested (phase 7).

### 2.1 Backend (done, for reference)
- Commands (Tauri and bridge): `commit_transaction`, `rollback_transaction`, `tab_status`, `close_tab_session`, `quit_app`. `run_query` and `export_csv` accept an optional `tabId`. `list_databases` exists.
- `TabStatus` (camelCase): `{ open, txOpen, pending, aborted }`. `tab_status` returns `null` when the tab has no session.
- A run uses the tab session only when the connection is Production, writable, and the request has a `tabId`. All other runs use autocommit through the pool.
- Read-only connection: the database blocks writes on every engine. The editor pre-check refuses writes and session-mode changes with error `connection_read_only` (HTTP 403).
- Quit: Rust stops the window close when a tab has a pending transaction, and sends the event `sandb://quit-requested`.
- Main files: `src-tauri/src/core/conn_ro.rs`, `src-tauri/src/core/db/session.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/bin/bridge.rs`, `src-tauri/src/lib.rs`.

### 2.2 Frontend prerequisite: real editor tabs
The editor has no tab model now. In `src/App.svelte:273-277`, "Query 1" is static text and the `+` button does nothing. The connection is global (`db.activeId` in `src/lib/state.svelte.ts`), and the database is per connection (`activeDb` in `App.svelte:67`). Per-tab sessions need per-tab state, so this work comes first:
1. Add a tab store (for example `src/lib/tabs.svelte.ts`). Each tab has: `id` (UUID, sent as `tabId`), `title`, `connId`, `database`, `query`, `result`, `opened`, `status: TabStatus | null`.
2. Move `query`, `result`, `resultConn`, `opened` and `activeDb` from `App.svelte` into the active tab.
3. Make the tab strip work: new tab, select tab, close tab, rename on double-click (optional).
4. Keep the existing behaviour for one tab: open a table from the tree, run, Load more, cancel, export.
5. Decide how `db.activeId` works with tabs. Recommended: the active tab owns the connection, and `active()` reads it from the active tab. The target bar's connection dropdown then switches the active tab's connection.

### 2.3 API layer (`src/lib/api/`)
1. `types.ts`:
   - add `readOnly: boolean` to `Connection` and `ConnectionInput`;
   - add `tabId?: string` to `RunRequest`;
   - add `TabStatus`;
   - add `connection_read_only` to `ErrorCode`;
   - add the new methods to `Backend`: `commitTransaction(tabId)`, `rollbackTransaction(tabId)`, `tabStatus(tabId)`, `closeTabSession(tabId)` and `quitApp()`.
2. `exportCsv` needs an optional `tabId` argument.
3. `tauri.ts` and `http.ts`: one call for each new command. Check the exact argument names in `commands.rs` and the body shapes in `bridge.rs`.
4. `mock.ts`: simulate the Production tab session. A write run on a Production connection sets `pending`. Commit or rollback clears it. A read-only connection refuses writes with `connection_read_only`.
5. Add `readOnly: false` to the `placeholder` connection in `state.svelte.ts`, and to the mock seed connections.

### 2.4 Feature 1 UI: read-only connection
1. `ConnectionDialog.svelte`:
   - Add a read-only toggle next to Environment, with the hint "Blocks all writes, including your own queries".
   - Fix the phase 1 P3: at a window height of 650 px, the Environment section is below the fold. Make the dialog body scroll, or keep the section visible.
2. A "Read-only" badge in `ConnectionTree.svelte`, in the connection dropdown and in the editor toolbar.
3. Add `canWrite(conn)` in TS (for example in `state.svelte.ts`). Phases 5 and 6 use it to disable AI writes and edit-mode Save.
4. On a read-only connection, hide or disable the existing write UI. That is the AI write toggle in `SettingsPage.svelte` and `SafeguardDialog.svelte`, and the edit toggle in `ResultsGrid.svelte`.
5. Show the `connection_read_only` error message clearly in the result area.

### 2.5 Feature 2 UI: manual commit on Production
1. Send the active tab's `tabId` with every `runQuery`, Load more and `exportCsv`.
2. Put Commit and Rollback buttons next to Run (`App.svelte:282-297`). Show them only for Production connections that are not read-only. Disable them when `status.pending` is false.
3. Pending indicator: an amber dot and the text "Transaction open".
4. Refresh `tab_status` after each run, commit and rollback.
5. Show the MySQL notice "MySQL committed this DDL immediately". Show the aborted message "Transaction aborted, roll back to continue.", and keep Rollback available when `status.aborted` is true.
6. Make a new `PendingTxDialog.svelte` with three choices: Commit, Rollback and Cancel. Cancel keeps everything as it was. Use it when a tab with a pending transaction is about to:
   - close;
   - switch its connection or database;
   - lose its connection, because the connection is deleted (`DeleteConnectionDialog.svelte`; one decision for all affected tabs);
   - lose Production, because the connection's environment changes to another value (`ConnectionDialog.svelte`);
   - be closed by quitting the app.
7. Quit:
   - In the Tauri transport only, listen to `sandb://quit-requested` with `listen` from `@tauri-apps/api/event`. After the user decides, commit or roll back each pending tab, then call `quitApp()`.
   - In bridge and mock mode, use `beforeunload` as a best effort.
8. When a tab closes with no pending transaction, call `closeTabSession(tabId)`.
9. There is no timer and no idle timeout that discards work.

### 2.6 Feature 3 UI: editor target dropdowns
1. Add a target bar in the editor toolbar. It shows the tab's connection (engine badge, name, environment dot, read-only badge) and database.
2. A connection dropdown changes the tab's `connId`. A database dropdown changes the tab's `database`. The list comes from `listDatabases(connId)`. SQLite has only `main`, so its dropdown is not shown.
3. A switch goes through `PendingTxDialog` when the tab has a pending transaction. It also goes through the existing `confirmDiscard()` for unsaved edits.
4. Opening a table from the tree still sets the tab's connection and database.

### 2.7 Phase 3b review and QA
1. Gates: `pnpm check`, `pnpm build`, and all Rust gates if Rust changed.
2. `code-reviewer` on the full phase 3b (backend and frontend). Focus areas:
   - Session leaks: a tab session that never closes, or a connection that never returns.
   - A Production write that gets committed without the user's decision.
   - The read-only pre-check: is there a way around it?
   - The quit flow.
3. Fix P0/P1, then review again if the fixes are large.
4. `qa-tester` in FULLSTACK mode in Helium. Check:
   - the badges;
   - the read-only toggle, and that a write is refused;
   - Commit and Rollback;
   - that another session cannot see the write before Commit;
   - the three-way prompt on tab close, switch, delete and environment change;
   - the dropdowns.

   Save screenshots and video in `.shots/qa/phase3b/`.

## 3. Phase 4: AI service (backend)
Goal: an OpenAI-compatible streaming client and a tool loop in Rust. No UI in this phase.

1. Settings: base URL, model, privacy mode (`results` or `schema`), max rows (default 500), time limit (default 15 s). Store them in `settings.json`. Store the API key in the secret store (the OS credential store in the app, memory in the bridge and the tests). Never write the key to a log.
2. `core/ai/client.rs`: `POST {baseUrl}/chat/completions` with `stream: true`.
   - Parse SSE `data:` lines and handle `[DONE]`.
   - Collect partial tool calls by index.
   - Handle HTTP errors, malformed JSON, a stream that stops early, and a timeout. Give each a clear error code.
3. `core/ai/tools.rs`: 4 tools with JSON schemas:
   - `list_tables`, `describe_table`: use the existing introspection.
   - `run_read_query`: always goes through `run_ai_read` (phase 3 guard and executor).
   - `propose_write`: does not run SQL. It returns a pending approval (see phase 5). When safeguards are ON, or the connection is read-only (`writes_allowed` is false), it refuses at once.
4. `core/ai/session.rs`: the tool loop.
   - Set a maximum number of steps, for example 8.
   - A cancel token stops the HTTP stream and any running query.
   - One chat session belongs to one connection.
5. `core/ai/prompt.rs`: the system prompt. It contains the engine, the database, the schema summary, and the rules (read-only by default, one statement, no guessing of values).
6. Privacy mode `schema`: query rows never go to the model. A tool result then holds only the column names and the row count.
7. Streaming to the UI: a Tauri `Channel<ChatEvent>` in the app, and SSE in the bridge, with the same event enum. Events: `text`, `tool_call`, `tool_result`, `write_preview`, `error`, `done`.
8. Commands: `get_ai_settings`, `save_ai_settings`, `set_ai_key`, `test_ai` (calls `GET /models`), `chat_send`, `chat_cancel`.
9. Tests against `mock-llm` (port 58080). Scenarios are keyed by a keyword in the last user message: `list`, `describe`, `count`, `bypass`, `write`, `error`, `slow`. Test each scenario. Also test:
   - an HTTP 500;
   - a malformed stream;
   - cancel during `slow`;
   - that `schema` privacy never sends rows: the mock records the request body, and the test checks it.
10. Review with a security focus (prompt injection from data in rows, key handling), then QA BACKEND.

## 4. Phase 5: chat panel, write preview, audit log
The UI exists as a prototype on mock data: `ChatPanel.svelte` (with the inline approval and question cards and the chat history), `chat.svelte.ts`, `SafeguardDialog.svelte`, `AuditPage.svelte`, `audit.svelte.ts` and the AI part of `SettingsPage.svelte`. This phase connects them to the real backend.

1. Settings page: save the AI settings and the key through phase 4 commands, and add a "Test" button.
2. Chat panel:
   - Show streamed text and tool calls (the SQL and the row count).
   - Add a Stop button.
   - Each SQL block has "Insert into editor". It inserts into the active tab.
3. Write approval:
   - `propose_write` sends `write_preview`. The approval card in `ChatPanel.svelte` shows the SQL, the statement type (`classify_write`), and red warnings for `UPDATE`/`DELETE` without `WHERE`, `DROP` and `TRUNCATE`.
   - Approve runs the SQL in one transaction. Deny writes nothing.
   - Commands: `approve_write(id)`, `deny_write(id)`.
   - A pending approval uses a `oneshot` in the state, with a timeout that denies.
4. Safeguard toggle for each connection (default ON):
   - Turning it OFF asks for confirmation (`SafeguardDialog.svelte`).
   - A change from staging to production while AI write is ON asks for confirmation again (approved default).
   - The toggle is disabled on read-only connections (`canWrite`).
5. Deleting a connection cancels its AI stream and its pending write previews (approved default).
6. AI writes on Production: decide how they work with the phase 3b manual commit (see section 8, question 1).
7. Audit log:
   - Store it in `audit.db` (SQLite) in the data folder.
   - Record every AI statement: time, connection, SQL, result, approval (approved, denied, rejected by the guard).
   - Show it on `AuditPage.svelte` with filters.
   - Do not store row data or the API key.
8. QA FULLSTACK in Helium with `mock-llm`. Check:
   - a read question and answer;
   - a blocked bypass;
   - a write with safeguards ON is rejected;
   - with safeguards OFF, the preview shows, and approve runs the write;
   - deny writes nothing;
   - every case appears in the audit log.

## 5. Phase 6: edit mode integration checks
Table Data views and simple query results support double-click editing. Live saves use primary keys, bound values and one transaction. Sorting runs before the row limit.

1. Run integration saves on PostgreSQL, MySQL, MariaDB and SQLite. Include a forced unique violation that rolls back all changes.
2. Run QA FULLSTACK for table Data views and query results.

## 6. Phase 7: hardening and release
1. `cargo clippy -D warnings` on the release features. Check that the CSP and capabilities are the minimum (`src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`).
2. `pnpm tauri build`. The `.app`/`.dmg` should be about 15 MB. Reduce the JS chunk if possible (the build warns about a chunk larger than 500 kB).
3. Check that the release binary does not contain the dev bridge: run `cargo tree -e features` and a `strings` check on the binary. The `run_read_only_unchecked`, `run_editor_unchecked` and `server_kill` test hooks are compiled only with `cfg(test)` or `feature = "it"`. Confirm that they are not in the release binary.
4. Test the parts that QA could not test through the bridge:
   - the Tauri native CSV save dialog;
   - the OS credential store (Keychain, Credential Manager, Secret Service): save, read and delete a password, and the move of an old `credentials.enc` into the store on Windows and Linux;
   - the quit prompt from the real window.

   Screenshots of the Tauri window need the macOS Screen Recording permission.
5. A final full `code-reviewer` run and a full `qa-tester` run.
6. A final report to the user.

## 7. Known issues and small gaps

| Severity | Item | Where |
|---|---|---|
| P3 | At 650 px window height, the Environment section of the connection dialog is below the fold. | `ConnectionDialog.svelte` (phase 1 QA) |
| P3 | When a single-value function in FROM has an alias, the guard uses the wrong default column name. So `SELECT g.g FROM generate_series(1,3) g` is refused for the AI and is not a plain read in the editor. It gives extra rejections only, not a safety gap. | `src-tauri/src/core/sql/facts.rs:555-570` |
| P3 | MySQL `DESCRIBE` shows an empty BINARY cell as `{"$bytes":"","len":0}` instead of an empty value. Display only. | `src-tauri/src/core/db/query.rs` (`mysql_cell_binary`) |
| Accepted | PG `string_agg` over a very large `generate_series` is allowed. The 16 MiB result limit and the 15 s time limit stop it. | `src-tauri/src/core/guard/classify.rs` |
| Not tested | Layer-2 `read_only_violation` from end to end through the bridge. Layer 1 stops all writes first. The `it` tests cover layer 2. | `tests/it_readonly.rs` |
| Not tested | The Tauri native save dialog, the password file in the real app, and the real quit flow. | Phase 7 |
| Info | `.factory/` in the project root holds the `sandb-worker` droid. It is there on purpose. | |

## 8. Open questions for the user
1. **AI writes on Production with manual commit.** Decided for now: an approved AI write runs in its own transaction and commits at once (`core/db/write.rs`), because the user already approved it in the preview. Change this only if the user asks for the tab transaction instead.
2. **Edit-mode Save on Production with manual commit.** Should Save run inside the tab transaction, so that it is pending until Commit? Or should it commit at once after the extra confirmation? Recommendation: inside the tab transaction, so that one Commit button controls all changes on Production.
3. **Tab persistence.** Should tabs and their SQL text be kept after the app restarts? Open transactions cannot be kept. Recommendation: keep the text, not the sessions.

## 9. How to resume
1. Start the test databases: `pnpm db:up` (compose project `sandb-test`, ports 55416, 55413, 53384, 53311, mock LLM 58080).
2. Start the dev bridge with a new token: `SANDB_BRIDGE_TOKEN=<token> pnpm bridge` (port 58123).
3. Start Vite in bridge mode: `VITE_BACKEND=bridge VITE_BRIDGE_TOKEN=<token> pnpm dev` (port 5199).
4. Restart the bridge after each Rust change, because it runs a compiled binary.
5. Next task: section 4 (phase 5). Settings save and Test, the live chat, Stop and write approval (`approve_write`, `deny_write`) are done. The saved safeguard toggle, cancel on connection delete, the real row estimate and red warnings from `classify_write` in the approval card, and `audit.db` are left.
6. To keep each worker request small, give each worker one section of this file, not the full phase.
