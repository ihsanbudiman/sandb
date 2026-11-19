# sandb: Redis support plan

Created: 2026-11-19.

## 1. Goal

Add Redis as a sixth engine. Connect to a server, find keys, run commands, edit values and export data. Use the AI chat with Redis, MongoDB and SQL sources in the same chat.

## 2. Decisions

| Topic | Decision |
|---|---|
| Driver | Use `redis` 1.8 with Tokio, rustls, WebPKI roots, Cluster, Sentinel and script support. Use `url` 2 for connection strings. Install the rustls `ring` provider before a connection opens. |
| Servers | Use RESP3. Refuse Redis earlier than 6. Support Valkey, standalone Redis, Cluster and Sentinel. Support TLS and standalone `redis://` and `rediss://` strings. |
| Databases | Use a decimal index. Use `0` if the input is empty. Cluster has only database `0`. |
| Browser | Keys take the place of tables. Key fields take the place of columns. Redis has no schema level. |
| Editor | Use `redis-cli` arguments. Put one command on each line. Quotes can span lines. Start a comment line with `#`. A semicolon does not end a command. |
| `SELECT` | Change the database for later commands in the same run. Do not send `SELECT` on a shared connection. |
| Unsupported commands | Refuse transactions, subscriptions, `MONITOR`, blocking commands and commands that change the connection state. Use Lua instead of a transaction. |
| Read-only | Refuse commands unless sandb knows they are reads or `SELECT`. Redis has no read-only session. Use a Redis ACL user without write permission for server-side protection. |
| Results | Read the full reply, sort it if requested, then show at most the selected row limit. The default is 200. Do not send MongoDB document fields. |
| Cancel | Stop waiting and discard the cached connection for that database. Try to stop a script or function on a new connection. Wait at most 2 seconds for that attempt. |
| Grid saves | Use one Lua script for each save. Check all old values before any change. A conflict changes nothing. |
| Release | Show Redis in the connection picker only in development until Phase 7. |

## 3. Assumptions

- Do not add a RESP2 fallback if a server or proxy refuses RESP3.
- Use `CLUSTER NODES` if the server does not know `CLUSTER SHARDS`.
- The Redis 7.4 test image does not include RedisJSON. Do not seed JSON module keys.
- Test a managed TLS service only with permission from its owner. Record if no managed TLS test runs.

## 4. How the code is shaped

Keep Redis operations in `src-tauri/src/core/redis/`. Route them in `core/ops.rs` and `catalog.rs::test_connection`. SQL and MongoDB modules do not import Redis code.

| Area | Redis path |
|---|---|
| Connection | Store one `RedisSettings` field with the deployment mode and Sentinel settings. Keep the password in the secret store. Cache at most eight database connections per saved connection. |
| Key browser | Use `SCAN`. In Cluster, scan each primary node. Show type and size. Show up to 1,000 keys by default. |
| Commands | Parse arguments as bytes. Match the command and its options against a table of reads, writes, server changes and unsupported commands. |
| Replies | Give known command replies named columns. Give other replies a scalar, row or JSON view. Keep binary values separate from text. |
| AI and MCP | Use Redis tools only on a Redis source. Route one read command per call. Refuse `KEYS` and tell the model to use `SCAN`. |
| Export | Write Redis commands in `.redis` files. Export a query reply as CSV. |

## 5. Phases

Each phase must leave the app ready to release. Keep features that are not ready out of the UI.

At each phase, check `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`. Include `dev-bridge` and `it` in Clippy checks. Run `pnpm gen:api`, `pnpm test`, `pnpm db:up`, `pnpm test:it` and `pnpm build`. Check the new UI through `pnpm bridge` and `pnpm dev:bridge` in a browser.

### Phase 0: Test stack and size check

Add the driver dependencies. Build the release binary before and after this change. Record the byte counts in section 8.

Run three Redis 7.4 test services: standalone on `46379`, Cluster on `46381` to `46383`, and Sentinel on `46391` with a master on `46390`. Keep Cluster node ports below `55536` because the bus ports are 10,000 higher.

Seed database `0` with strings, a binary value, a key with a TTL, hashes, a list, a set, a sorted set and a stream. Seed database `1` with `db1:only`. Seed Cluster with 3,000 keys on different slots. Add the test ACL users after the data. Check that all services are healthy and that `sandb_ro` gets `NOPERM` for `SET`.

**Built:** All three Redis services passed their health checks on macOS arm64. The seed checks found 3,000 Cluster keys and verified the standalone data sizes and binary value. `sandb_ro` received `NOPERM` for `SET`.

The dependency build passed. The frontend build and `svelte-check` also passed. The browser Test connection check reached SQLite 3.46.0 through the bridge.

The full `pnpm db:up` check did not pass. The 2,048 MiB Docker VM reported out-of-memory kills for MySQL and MongoDB. The three Redis services used about 12 MiB in total. Use the Redis services, `pg16` and `mock-llm` for focused Redis checks on this VM.

`pnpm test:it` passed its unit tests but failed in `it_conn_ro` because MySQL did not answer. The focused stack (`redis`, `redis-cluster`, `redis-sentinel`, `pg16` and `mock-llm`) passed its health checks.

### Phase 1: Engine and connection

Add the `redis` engine and route operations by family. Add Redis settings and keep old saved connections valid. Share the MongoDB parser error type without sharing the parsers.

Add the Redis URI parser, node list parser, client cache and probe. Normalize the database index and TLS mode. Check version, authentication and deployment errors in Test connection. Update the app dialog, generated API and demo. Check create, edit, duplicate and delete. Check that the connection file contains no password.

**Built:** The browser Test connection check passed on the local Standalone, Cluster and Sentinel services. It showed the authentication error for a wrong password. The demo saved a Redis URI connection. Four integration tests checked deployment errors, connection changes, secret storage and cache invalidation.

### Phase 2: Browse

List databases from `INFO keyspace`. List keys by pattern. Report the key type, size, length, encoding, expiry and memory use where permitted. Sample up to 100 hash field names. Get stream fields from recent entries. Show key lists and key properties in the tree. Check key counts and types on all three deployments.

**Built:** Four integration tests passed on the local Standalone, Cluster and Sentinel services. The Cluster scan found all 3,000 fixture keys. A key with binary bytes opened its own properties. The browser showed the 1,000-key limit and the `leaderboard` properties through the bridge. The demo also showed sampled fields, expiry and an escaped binary key name.

### Phase 3: Editor and command guards

Parse Redis command text and quote bytes with `redis-cli` escapes. Show line and column for invalid text. Add syntax highlighting, command completion and key completion. Do not format Redis text.

Check read-only rules before execution. Warn about flushes, server changes, scripts and unknown commands before production writes. Run each command on the correct database. Show keys and values, hash fields, list indexes, members and scores, and stream IDs in named columns. Report a scan cursor if more data is available.

Add View data, New key, Rename, Set TTL and Delete key. Route all writes through the editor guard. Test cancellation, cross-slot errors, `SELECT`, read-only rules and production confirmation.

**Built:** The bridge showed hash fields and read `db1:only` after `SELECT 1`. It refused `SET` on a read-only connection and `MULTI` in the editor. It warned before `FLUSHDB` on a production connection; the check cancelled the command. The demo showed the same hash columns and the `SELECT` notice.

`pnpm test` passed 432 Rust tests and the Svelte type check. All-target Clippy passed for default, `dev-bridge` and `it`. `pnpm gen:api` changed neither generated file. `pnpm build` and a direct check of the frontend Redis parser and demo backend passed.

`pnpm test:it` passed all 183 integration tests, including the existing SQL and MongoDB suites and all three Redis suites. The cancel tests cover both a request that has not started and a Redis script that is already running.

The format review compared 52 changed or new Rust files with `HEAD`. It fixed new hunks, including the Redis export arm in `commands.rs`. The full format gate still fails on pre-existing layout, for example in `query_registry.rs`.

The shared Redis Cluster client sets `.retries(0)`. It cannot tell if a write ran when the reply is lost, so it does not send the command again. The editor reports that the command may have run. The driver also returns slot redirects without replaying the request. Reload before you try a write again.

All 10 Redis integration tests passed after this change. This check used healthy local nodes. It did not drop a Cluster write reply.

### Phase 4: Grid edits

Allow edits for one supported command that reads one key. Check that the type still matches. Use the key, field, index or member to find each row. Keep the TTL of a string when it changes. Compare old values before the script applies new values. For a list, delete the selected indexes, not all equal values. Keep binary cells read-only. Test all edit types and verify that a conflict changes no values.

**Built:** The browser preview showed the planned `HSET` command. The bridge saved the edit, and a new query read the new value.

All six Redis grid integration tests passed. They checked conflicts, ACL refusals, list indexes, sorted set scores, TTL, read-only connections, and binary reads.

The script checks for conflicts before it writes. Redis 7 and later also check write permissions before the first write. Redis 6 refuses saves that need more than one native write. If the connection drops during a save, reload the result before you try again. Redis cannot undo a write that already ran.

### Phase 5: AI, MCP and skills

Add `run_redis_read`, `render_redis_chart` and `propose_redis_write`. Offer them only for Redis sources. Use Redis code blocks for previews. Keep the SQL and MongoDB prompt text the same in chats without Redis.

Add `redis` and `redis-tuning` skills. Add the Redis read tool to MCP. Use the mock LLM to check reads, charts, approval, guard errors and mixed chats. Check a mixed PostgreSQL and Redis chat with a real model only when its use is authorized.

**Built:** Five chat integration tests passed on the local fixtures. They checked Redis reads, charts, write approval, guard and ACL refusals, and mixed PostgreSQL and Redis sources.

The assistant and MCP unit tests passed. No real model was used.

Redis chat and MCP refuse `CONFIG GET`, `ACL LIST`, `CLIENT LIST` and `CLIENT INFO`. The editor and user-written query schedules can run them if the Redis account has permission. The read-only schedule guard still refuses writes. An approved Redis write shows a change count only when the command returns one.

### Phase 6: Scripts and export

Generate Redis scripts for strings, hashes, lists, sets, sorted sets and streams. Preserve the key bytes and TTL. Limit script previews to 10,000 elements. Write full exports in batches of 1,000 elements without reading the entire key into memory. Keep CSV as the only grid export for Redis. Test import, export, TTL and binary data.

**Built:** The integration tests restored all six Redis types on scratch keys. They checked binary bytes and the remaining TTL. They also verified CSV output and that a refused export left the destination file unchanged.

The large hash refused a preview. Its export contained all 100,000 fields in 100 `HSET` commands.

### Phase 7: Release checks

Use the local TCP proxy to test a broken connection during a command, a grid save and a chat. Check recovery on the next command. Verify the bridge and demo. Remove the development gate only after these checks. Measure the release binary and memory use. Update the README, changelog, What's New and this plan.

**Built:** The local TCP proxy cut the connection during a command, a grid save and a chat read. All three tests passed. The grid reported that the save could have run. The next operation worked in each test. No managed TLS service was tested.

Headless Helium showed the binary key script in the demo and in the app with the local bridge. The bridge also showed the Redis chat read from the local mock model. The browser demo does not edit live data. Full key export uses the desktop save dialog.

The production frontend build showed Redis in the connection picker. Headless Helium opened the Redis connection form.

The Redis notes are under `[Unreleased]` in `CHANGELOG.md`. What's New shows these notes when the release moves them under a version heading.

Clippy passed with all targets and features. `pnpm check` and `pnpm build` also passed. Headless Helium showed the refusal for `CONFIG GET` in the demo chat. The demo guard allowed an ordinary `GET`.

The workspace format check reported differences in 55 files. The format check passed for the Redis guard, command, chat test and export writer files.

The full integration suite did not pass. `pnpm test:it` stopped at `it_ai` after PostgreSQL and mock model requests failed with connection errors or timeouts.

All six `it_ai_redis` tests passed with a mock model process on the host. They checked settings refusal and confirmed that an approved hash update does not report a change count. The MCP guard, write count and generated API unit checks also passed.

`pnpm gen:api` passed. The full Rust unit run in `pnpm test` did not finish. The frontend check passed, but the command reached its time limit during Rust compilation.

The focused Redis schedule test passed on the standalone test service. The schedule read `CONFIG GET` with a read-only account and refused `SET`. Chat replay refused the same `CONFIG GET` command.

## 6. Risks

| Risk | Effect | Control |
|---|---|---|
| Redis has no read-only session | An app check alone does not give server-side protection | Refuse unknown commands in read-only mode. Recommend a Redis ACL user. |
| The server can finish a command after cancellation | A cancelled write can change data | Show a warning. Try to stop scripts and discard the cache entry. |
| A large reply uses memory before the row limit applies | High process memory use | Use range and scan commands for large keys. Measure the full reply path. |
| A Cluster command can reach more than one slot | The server can refuse the command | Show the server error. Use a common hash tag when keys must share a slot. |
| Other clients can change a key during a scan or export | The result can mix two versions of a key | Treat it as a live view, not a snapshot. |

## 7. Open questions

- Managed TLS verification needs an approved service.

## 8. Measurements

Use `cargo build --release --bin sandb` on macOS arm64 for binary size. Phase 0 adds the driver dependencies but does not call the driver. Phase 7 measures the code that uses the driver.

| Build | macOS arm64 release binary |
|---|---:|
| Phase 0, before the Redis dependencies | 45,131,920 bytes |
| Phase 0, with the Redis dependencies | 45,133,152 bytes (+1,232 bytes) |
| Phase 7, with Redis support | 47,389,568 bytes (+2,257,648 bytes, 5.00% above the Phase 0 baseline) |

The debug integration test had a peak RSS of 27.8 MiB while it exported `big:hash` and parsed the output. The total for the test process was below the 64 MiB growth limit. The bridge cannot write a key export to disk because that operation uses the desktop save dialog.

The bridge read the full 100,000-field reply from `HGETALL big:hash` but returned only the first 200 rows. The memory readings do not include a WebView.

| Bridge build | RSS after `PING` | RSS after `HGETALL big:hash` |
|---|---:|---:|
| Debug | 18.2 MiB | 43.9 MiB |
| Release | 12.1 MiB | 39.8 MiB |

## Appendix A. Verification by phase

Use the local `sandb-test` stack. Use `sandb_rw` and `sandb_rw_pw` for the editor checks. Use a test connection with `sandb_ro` and `sandb_ro_pw` to check server-side write refusal.

| Phase | Result to check |
|---|---|
| 0 | All services are healthy. Standalone has 1,000 user hashes and a 100,000-field hash. Cluster has 3,000 fixture keys. Redis refuses a read-only `SET` with `NOPERM`. |
| 1 | Test reports `Connected · Redis 7.4.x` for standalone, Cluster and Sentinel. A wrong password gives `auth_failed`. Standalone mode on a Cluster node tells the user to choose Cluster. A wrong Sentinel master name gives a separate error. |
| 2 | The tree shows `db0` and `db1`. `user:*` shows 1,000 keys and a limit notice. `leaderboard` has type `zset` and size 1,000. The app shows the expiry and opens keys with binary names. |
| 3 | `HGETALL user:1` shows three rows with `field` and `value` columns. `SELECT 1` followed by `GET db1:only` returns `yes`. A read-only connection refuses `SET`. `MULTI` tells the user to use Lua. Production `FLUSHDB` shows a warning before execution. After script cancellation, `PING` works. |
| 4 | Edit `user:2` in View data. The preview shows `HSET`. The edit changes only the selected field. A conflict leaves all fields unchanged. Deleting two list indexes does not delete other equal values. |
| 5 | The mock LLM can read and show a sorted-set chart. A proposed write does not run before approval. The guard refuses `KEYS *`. A mixed chat calls the correct tool for PostgreSQL and Redis. |
| 6 | Export `user:1` as `user_1.redis`. Import its commands on a scratch key and compare its fields. Check streams, TTL and binary values. Export the large hash and check that peak RSS grows by less than 64 MiB. |
| 7 | Cut the test connection during a command, edit and chat. The app shows an error and the next command works. Check the bridge, demo and release picker. |

## Appendix B. Command classification

Check the two-word name before the one-word name. Refuse an unknown command in read-only mode. In write mode, warn when a command is unknown or runs code that the app cannot classify.

- Treat standard string, hash, list, set, sorted set, stream, bit, Geo and supported module reads as reads. Treat `EVAL_RO`, `EVALSHA_RO` and `FCALL_RO` as reads. The server enforces this rule for these script commands.
- Treat `SORT`, `GEORADIUS` and `GEORADIUSBYMEMBER` as writes if they include `STORE` or `STOREDIST`. Refuse `XREAD` and `XREADGROUP` if they include `BLOCK`.
- Treat data changes, publish commands and script or function loads as writes. Treat `EVAL`, `EVALSHA` and `FCALL` as scripts. Treat `FLUSHDB` and `FLUSHALL` as destructive operations with their own warnings.
- Treat server configuration, client controls, replication controls, ACL changes, script resets and Cluster changes as server administration.
- Refuse commands that start or end a transaction, subscribe to messages, monitor traffic, wait for data or change the shared connection state. This includes `CLIENT REPLY`, `CLIENT TRACKING`, `AUTH`, `HELLO`, `QUIT` and `RESET`.
- Process `SELECT` in the app. Reject negative database indexes and refuse an index other than `0` on Cluster.
