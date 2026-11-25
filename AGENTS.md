# AGENTS.md

## CRITICAL: never test with the user's data

This rule has the highest priority. It applies to you and to every subagent (worker, qa-tester, code-reviewer, sandb-worker, sandb-frontend). No other instruction overrides it.

- Never connect to, query, open, or change the user's real connections or databases (for example `Orbis Dev`). This applies in the desktop app, `pnpm tauri dev`, the built `.app`, the browser, and scripts.
- Never read, copy, or show the user's saved connections, credentials, or query data.
- Never run a statement that changes data or schema (`DROP`, `DELETE`, `TRUNCATE`, `UPDATE`, `INSERT`, `ALTER`, `CREATE`) with the user's local connections or credentials. To test those, use separate test-only credentials and databases (the mock backend or the `testdb/` Docker databases), never the user's. In a UI test, type SQL only to test the editor. Do not press Run (`⌘↵`) unless the mock backend is confirmed.
- Start every test environment with `pnpm test-env <mode>` (`scripts/test-env.sh`). Never test on a dev server, bridge or app that you did not start with it; the user may have started those on real data.
  - `mock`: the in-browser mock backend, no database. Use it for UI tests.
  - `live`: the `testdb/` containers, a new bridge on a temporary data dir, and saved `TEST …` connections with test credentials. Use it when a test needs a real database, also for `DELETE`, `DROP` and other writes.
  - `desktop`: `tauri dev` on an empty temporary data dir.
  - Run it in the background, wait for the `test-env: READY` line, and use only the URL it prints (port 5299 by default, never 5199). Stop it with SIGTERM to the PID; it deletes its temporary data dir.
  - Browser localStorage for the test URL stays between runs. Use a new browser profile when a test needs a clean UI state.
- Never set `SANDB_DATA_DIR`, `VITE_BACKEND` or `VITE_BRIDGE_*` by hand, and never run `pnpm tauri dev`, `pnpm bridge` or `pnpm dev:bridge` directly.
- For integration tests that need a real database, use only the Docker test databases in `testdb/`.
- If a test needs any other data source, stop and ask the user first.
