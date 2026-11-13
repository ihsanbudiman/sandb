#!/usr/bin/env bash
# Starts the sandb-test stack and waits until it is healthy.
set -euo pipefail
cd "$(dirname "$0")/.."

# The host ports live in .env.test, the same file compose and the tests read.
set -a
# shellcheck disable=SC1091
. ./.env.test
set +a

running=$(docker compose -p sandb-test -f compose.yaml --env-file .env.test ps -q 2>/dev/null || true)
if [ -z "$running" ]; then
  for p in "$PG16_PORT" "$PG13_PORT" "$MYSQL_PORT" "$MARIADB_PORT" "$MONGO_RS_PORT" "$MONGO_STANDALONE_PORT" "$MOCK_LLM_PORT"; do
    if lsof -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then
      echo "port $p is already in use by another process; refusing to start" >&2
      lsof -iTCP:"$p" -sTCP:LISTEN >&2 || true
      exit 1
    fi
  done
fi

docker compose -p sandb-test -f compose.yaml --env-file .env.test up -d
exec ./scripts/wait.sh
