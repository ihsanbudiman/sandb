# sandb test databases

## Interface regression tests

The UI suite uses the browser mock; it does not connect to a database or an AI provider.

```sh
pnpm test:unit
pnpm exec playwright install chromium
pnpm test:ui
```

Node.js 22.18+ is required for the TypeScript unit tests. Playwright starts Vite automatically (or reuses a running dev server locally).

## Database fixtures

Throwaway databases and a mock LLM for sandb tests. Compose project: `sandb-test`.

The host ports are keys in `.env.test` (bound to 127.0.0.1); compose, the scripts and the
integration tests read that one file, so a port changes in one place.

| Service  | Image          | Host port key  | Databases |
|----------|----------------|----------------|-----------|
| pg16     | postgres:16    | `PG16_PORT`     | `shop`, `shop_staging` (schemas `public`, `analytics`) |
| pg13     | postgres:13    | `PG13_PORT`     | same as pg16 |
| mysql    | mysql:8.4      | `MYSQL_PORT`    | `shop`, `analytics` |
| mariadb  | mariadb:11     | `MARIADB_PORT`  | `shop`, `analytics` |
| mongo-rs | mongo:8        | `MONGO_RS_PORT` | `shop_staging`, `analytics` (one-node replica set `rs0`, transactions work) |
| mongo-standalone | mongo:8 | `MONGO_STANDALONE_PORT` | same as mongo-rs (standalone, no transactions) |
| redis | redis:7.4 | `REDIS_PORT` (`46379`) | Databases `0` and `1` |
| redis-cluster | redis:7.4 | `REDIS_CLUSTER_PORT_1` to `REDIS_CLUSTER_PORT_3` (`46381` to `46383`) | Database `0` across three primary nodes |
| redis-sentinel | redis:7.4 | `REDIS_SENTINEL_PORT` (`46391`), `REDIS_SENTINEL_MASTER_PORT` (`46390`) | Database `0` on master `mymaster` |
| mock-llm | node:22-alpine | `MOCK_LLM_PORT` | OpenAI-compatible API at `/v1` |

The integration tests read the same keys through `src-tauri/tests/common/mod.rs`.

SQLite fixture: `sqlite/shop.db` (built by `pnpm db:sqlite`, ignored by git).

## Credentials (local test-only, see `.env.test`)

| User | Password | Rights |
|------|----------|--------|
| `postgres` / `root` | `sandb_test_root` | superuser (Mongo: `root`, `authSource=admin`) |
| `sandb_rw` | `sandb_rw_pw` | owner / ALL on `shop`, `analytics` (PG: also `shop_staging`; Mongo: `readWrite` on `shop_staging`, `analytics`) |
| `sandb_ro` | `sandb_ro_pw` | SELECT only (Mongo: `read` role) |
| `default` (Redis) | `sandb_test_root` | All Redis commands |
| `sandb_rw` (Redis) | `sandb_rw_pw` | All Redis commands |
| `sandb_ro` (Redis) | `sandb_ro_pw` | Read commands and selected status commands |

## Commands (from `sandb/`)

- `pnpm test`: svelte-check plus the Rust unit tests.
- `pnpm test:it`: every integration suite against this stack (run `pnpm db:up` first).
- `pnpm db:up`: start all services and wait until healthy (fails after 180 s and names the service).
- `pnpm db:down`: stop and remove the stack. Data is on tmpfs, so nothing is left.
- `pnpm db:reset`: down, then up with fresh seed data.
- `pnpm db:sqlite`: rebuild the SQLite fixture.
- `pnpm mock-llm`: run the mock LLM locally on 58080 without Docker (stop the container first).

## Mock LLM scenarios

The keyword in the last user message picks the reply: `list`, `describe`, `count`, `bypass` (`DELETE FROM orders`),
`write` (`propose_write`), `chart` (`render_chart`, orders by status), `error` (HTTP 500), `slow` (20 chunks, 300 ms apart); anything else streams an echo.
MongoDB scenarios (seed database `shop_staging`): `mongo read` (`run_mongo_read`, five paid orders), `mongo chart` (`render_mongo_chart`,
orders and revenue by status), `mongo write` (`propose_mongo_write`, `insertOne` into a scratch collection), `mongo out`
(`run_mongo_read` with an `$out` stage into a scratch collection, which the guard must refuse). The scratch collection is the
first `it_*` name in the message (default `it_mock`).
If the last message has role `tool`, the reply is text that quotes the first 200 characters of the tool result.
`mixed` is multi-step: `run_read_query` on source `db1` (`SELECT count(*) AS orders FROM orders`), after that tool result
`run_mongo_read` on source `db2` (`db.orders.countDocuments({})`), then text that quotes the first 200 characters of each tool result.
Redis scenarios: `redis read` reads `user:1`. `redis chart` charts the ten highest scores in `leaderboard`.
`redis write it_<hex>` proposes a write to a scratch key. `redis keys` proposes `KEYS *`, which the read guard refuses.
`redis settings` proposes `CONFIG GET databases`. The read guard refuses configuration reads before it contacts Redis.
`redis mixed` counts PostgreSQL orders on source `db1` and reads `greeting` from Redis on source `db2`.
Set `MOCK_LLM_KEY` to require `Authorization: Bearer <key>`.
`GET /_requests` returns the parsed bodies of the last 100 `POST /v1/chat/completions` requests (oldest first), so tests can check what was sent to the model.

## Isolation rules

- Only touch the `sandb-test` project: `docker compose -p sandb-test ...`. Never prune, never touch other containers.
- Ports bind to 127.0.0.1 only. `up.sh` refuses to start if a port is in use by something else.
- Tests use only these containers or temporary SQLite files.

## Notes

- MySQL 8.4 uses `caching_sha2_password`. Over plain TCP without TLS, a client must allow RSA key exchange
  (`--get-server-public-key`, or `allowPublicKeyRetrieval` in drivers) until the first TLS login caches the password.
- PostgreSQL 13 grants `CREATE ON SCHEMA public` to everyone by default. The seed revokes it, so `sandb_ro` is read-only on pg13 too.
- Mongo users live in `admin`, so connections need `authSource=admin`.
- Mongo seed (`seed/mongo/`): nested documents, arrays, missing fields, every BSON type in `shop_staging.type_zoo`,
  `products.price` with mixed types (Decimal128, Int32, String), the view `shop_staging.order_summary`, the time series
  collection `analytics.metrics`, and `shop_staging.big_docs` (100 documents of about 2 MB).
- `mongo-rs` listens on 57017 inside the container too, so the replica set member address `127.0.0.1:57017` works
  from the host without `directConnection`.
- Connect to Redis Sentinel without a password. Use a Redis user and password for `mymaster`.
