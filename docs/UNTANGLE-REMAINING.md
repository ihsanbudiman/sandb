# Untangle refactor: remaining work

The structural refactor stopped on request after fix wave 1. A second session then did item F14, ran the second re-audit, and started fix wave 2. A third session completed fix wave 2 and ran all checks. This file lists the work that is not done. It also lists the decisions to review.

## Current state

- Git: nothing is staged or committed. `src/lib/api/generated/` and the other new modules are still untracked.
- Rust: `cargo clippy --all-features --all-targets -- -D warnings` is clean.
- Unit tests: `cargo test --lib --bins --all-features` passes (lib 376, bridge 1).
- Integration tests: after fix wave 2, all 13 suites passed. Each suite ran alone. The total is 173 tests (see the table below).
- Frontend: `pnpm check` gives 0 errors and 0 warnings. `pnpm build` passes.
- Smoke tests: fix wave 2 passed in mock mode in a browser. Nobody did a bridge smoke test or started the desktop (Tauri) app.

### Integration test counts

The baseline before the refactor was 175 tests. (The decision log said 181. That number was wrong: the suite counts add up to 175.)

| Suite | Before | After | Reason |
|---|---|---|---|
| it_ai | 14 | 14 | |
| it_ai_mongo | 8 | 8 | |
| it_conn_ro | 4 | 4 | |
| it_connections | 9 | 9 | |
| it_mongo | 13 | 13 | |
| it_mongo_edit | 15 | 15 | |
| it_mongo_fault | 11 | 11 | |
| it_mongo_query | 12 | 12 | |
| it_mongo_struct | 12 | 11 | A pure serde shape test was removed. The ts-rs contract test now checks shapes. |
| it_mongo_write | 15 | 16 | One test now checks all write notices together. |
| it_query | 24 | 23 | A duplicate SQLite fixture test was removed. The stronger test stays. |
| it_readonly | 13 | 12 | Two prepared-statement tests were merged. |
| it_session | 25 | 25 | |

## Done in the second session

### F14: the backend owns the "Load more" page size

- `result::limit(requested, own)` decides the row limit. With no limit from the caller, the statement's own limit sizes the first page when it is larger than the default (200). Example: `SELECT … LIMIT 1000` shows 1000 rows at once.
- SQL: `sql::facts::row_limit` reads the literal `LIMIT` of the last statement (also `LIMIT n OFFSET m` and `LIMIT m, n`). MongoDB: the `.limit(n)` of the last `find`.
- `StatementResult.limit` (new wire field) tells the client the limit that the statement used. "Load more" asks for two times that value.
- `run.svelte.ts` no longer parses `LIMIT` or `.limit()`. It sends a limit only for a re-run ("Load more", sort, refresh).
- The mock uses the same rule and sends `limit`.
- Tests: unit tests for `limit` and `row_limit`.

### Second re-audit

Six read-only scouts audited the slices again. Their reports are `reaudit2-*.md` in the session folder (see "Where to find the details"). Summary:

| Slice | Fix items | Justified special cases |
|---|---|---|
| SQL (`core/db`, `sql`, `guard`) | 7 | 27 (+5 test-only doors) |
| Mongo | 3 | 21 |
| Core and AI | 4 | 16 |
| Frontend state | 9 | 26 |
| Components | 6 | 29 |
| Contract, mock, CI | 3 | 27 |

Bugs that the audit found:
- SQL DDL from the editor does not refresh the tree. SQL statements send no `read` flag, so `writeCheck.ts` ignores the `structure` flag.
- Switching the AI-write flag saved the connection. The save closed all tab sessions, so an open transaction rolled back.
- The `implicit_commit` warning shows for `SET @x`, `SET FOREIGN_KEY_CHECKS` and `USE`. These statements do not commit.
- After an approved MySQL write is stopped or fails, the message says "Nothing changed". An earlier DDL statement in the batch can already be committed.
- A Data view filter on a MongoDB uuid column sends a string, so it matches no document.
- The MongoDB AI read sends `maxTimeMS: 0` (no limit) when the timeout is 0.

## Do these first

1. **Commit the new files.** `src/lib/api/generated/` is untracked. The `wire::export_bindings` test compares that folder with the Rust types. On a clean checkout the test fails until you commit the folder. The other 54 new files are new modules from the refactor. Review them with `git status`.
2. **Start the desktop app once.** Run `pnpm tauri dev` and do these steps:
   - Open a connection and run a query.
   - Edit a cell and save it.
   - Export to a file (CSV, SQL, and documents for MongoDB).
   - Save a chat as PDF.
   - Close a window while a grid has unsaved edits.
   Nobody did these steps. The bridge cannot do file dialogs, quit prompts or PDF saves.
3. **Check the CI workflow.** `.github/workflows/ci.yml` is new. Nobody has run it on GitHub. The Docker VM on a CI runner can also run out of memory (see "Test environment").

## Fix wave 2: done

The third session read each `reaudit2-*.md` item and compared it with the code. Most items were already complete in the working tree. The session completed the open items and ran all checks.

### Items from the second session (checked again)

- Mongo 1–3, Core 1–4, Components 1 (Rust side), Contract 1 and Contract 3: no change. `it_session` now passes with the `update_connection` change.

### SQL (`reaudit2-DbSql.md`)

- Item 1: `writeCheck.fromInfo` reads `structure` from all statements. SQL DDL from the editor now refreshes the tree.
- Item 2: `db/write.rs` no longer says "Nothing changed" after a stop or a failure when an earlier MySQL statement can be committed. A MySQL statement that the parser cannot read (for example `REPAIR TABLE`) counts as a possible commit.
- Item 3: `StatementFacts::implicit_commit` in `sql/facts.rs` is the new fact. `SET @x`, `SET FOREIGN_KEY_CHECKS`, `SET @@global.autocommit` and `USE` do not set it. `SET @@autocommit` and `SET @@session.autocommit` set it. `guard` does not import `db` now, so the `guard ↔ db` cycle is gone. The approval card text is now about the batch.
- Items 4–7: `sqlite_export` quotes as SQLite. `StatementKind::is_ddl()` is the one DDL rule. `control_bound` is gone. The `ops.rs` doc tells about MySQL implicit commits.

### Frontend state (`reaudit2-AppLib.md`)

- Item 1: `mongo/value.ts` has the one mongosh literal table. A uuid filter now sends `UUID("…")`. A double filter sends `42.0`.
- Items 2–4: an approved write uses the `WriteInfo` of its preview and is not classified again. `afterRun` reads the collection from `WriteInfo`. When there is no `WriteInfo`, it refreshes the catalog. `writeCheck` has no `SQL_STRUCTURE` or `$out|$merge` regex. The mock sends `structure`.
- Items 5–9: `Dialect = SqlEngine`. `statementTextAt` is gone. `runPooled` is the one pooled-statement helper. `MONGO_VALID` uses the type of `MONGO_WRITABLE_TYPES`.
- Item 6: `ChatScope` and `ConnectionDialog` use `defaultDb(c)`.

### Components (`reaudit2-Components.md`)

- Item 1: `ChatComposer` turns writes off only on connections that have the flag on.
- Item 2: each filter operator in `tableActions.ts` owns its SQL and mongosh form. `TableFilterBar` only shows the table.
- Items 3–6: the `o:` key prefix is gone. `copyBlock` uses `cellText` from `results.ts`. The chip text says "Click to switch writes off." The dead resets in `ConnectionDialog` are gone.

### Mock (`reaudit2-FrontendContract.md` item 2, and one more fix)

- The mock says "The demo build cannot edit MongoDB collections".
- The mock now classifies an aggregate with `$out` or `$merge` as a write, and its AI guard refuses it. This agrees with the backend.

### Open points (small, not done)

- `App.svelte` `canUpdate` asks about pending transactions before each connection edit. A rename or an AI-write change no longer closes sessions, so this question is not necessary for these edits. The question is safe, so it stays.
- `SqlEditor.svelte` spells the SQLite default schema as `'main'`. `defaultDb` gives the default database, not the schema, so it stays.
- `MONGO_WRITABLE_TYPES` is a `string[]`. A compile-time check of `MONGO_VALID` needs a tuple from the generator.
- `cargo fmt`: decide if you want to format the whole tree (see Contract 1).

## Items the audits rejected (review if the code changes)

- MySQL type names have three tables: `db/reader.rs`, `db/value.rs` and `db/dump.rs`. Each one gives a different form (binary, text, SQL literal). Merge them when you add the next MySQL type.
- `guard_check` is a command with no caller in the frontend. Removing it touches about ten places.
- MongoDB `MONGO_VALID` (frontend) and the generated `MONGO_WRITABLE_TYPES` differ on purpose.
- `db/query.rs` `is_sortable` parses the SQL again. Passing the AST through would cost more.
- `db/conn.rs` `looks_like_read` is a heuristic. A replacement needs a new fact from `sql::facts`.
- The `ai ↔ state` module cycle stays. `AppState` owns the chat and approval registries. Breaking the cycle moves code but removes no copied rule.
- The `test-hooks` feature split did not happen. The test hooks stay behind the `it` feature.
- The mock does not refuse an engine change on a connection update. The UI cannot send that change.

## Behavior changes to review

These changes are deliberate, or they are bug fixes. Make sure that each one is what you want.

### Dialogs and AI writes

- **Dialogs.** Escape closes the top dialog. A backdrop click does not close a dialog. The command palette is the one exception: a backdrop click closes it.
- **AI writes.** The AI can propose a write only when all three are true: the connection can write, the stored "AI writes" flag is on, and the chat allows writes. The chat and Settings switches now save the flag to the connection.

### Engine behavior

- **MongoDB `$out` / `$merge`.** An aggregate with one of these stages now runs as a write. Before, it ran on the read path.
- **MySQL DDL in approved writes.** The approval now shows an `implicit_commit` warning. MySQL commits DDL at once, so the write is not one transaction.
- **Assistant on MongoDB.** `list_tables` and `describe_table` now work.

### Exports

- **Exports.** All file exports and `save_pdf` write a temporary file and then rename it. You can now cancel MongoDB exports.
- **PDF file name.** A bad PDF file name now causes an error. Before, the app used `report.pdf`.

### Bridge (development only)

- **Desktop-only commands.** They now answer `unsupported`.
- **`quit_app`.** It no longer closes sessions.

### Mock mode

- At start, the mock no longer runs a sample query or opens the first table.
- The scripted chat does not ask questions in the chat.
- The mock now refuses the same connections that the backend refuses.

## Test environment

- The Docker VM has 2 GB of memory. The `mongo-rs` container has no memory limit. The kernel killed it two times (OOM) when two Mongo suites ran at the same time. Run the Mongo suites one at a time, especially `it_mongo_struct` and `it_mongo_fault`. You can also give the VM more memory.
- When OrbStack starts, it also starts containers of other projects (`orbis-*`, `throtl-*`, `token-calculator-*`). They use VM memory. The second session stopped them again before it started the test stack. The test stack (`sandb-test`) is still running; stop it with `pnpm db:down`.
- The ports are now in `testdb/.env.test`. `compose.yaml` reads them from there. The running stack used these ports before the change and still uses them.
- The new scripts are `pnpm test` (check + unit tests) and `pnpm test:it` (integration tests, which need `pnpm db:up`).

## Where to find the details

The session reports have the full evidence for each wave:
- The decision log: problems and fixes.
- The `untangle-done-*.md` and `fixwave1-*.md` reports: one for each wave.
- The `reaudit-*.md` reports: the first re-audit findings.
- The `reaudit2-*.md` reports: the second re-audit findings, with `Fix`, `Keep` and `Count` for each slice.

First session folder: `~/.omp/agent/sessions/-Documents-project-sandb/2026-11-11T09-04-36-238Z_01a1c9da-198e-77f1-aa2c-12f300d4b85c/local/`. Second session: `local://reaudit2-*.md` of session `01a1ccaa`. These files are not in the repository. Copy them into `docs/` if you want to keep them.
