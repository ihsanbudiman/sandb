#!/usr/bin/env bash
# Starts an isolated sandb test environment. It never uses the user's saved connections,
# credentials or app data: every mode gets a fresh temporary data dir and its own ports, and the
# only databases it connects to are the throwaway sandb-test containers (testdb/).
#
#   scripts/test-env.sh mock      Vite with the in-browser mock backend. No database at all.
#   scripts/test-env.sh live      Test containers + dev bridge + Vite, with test connections saved.
#   scripts/test-env.sh desktop   `tauri dev` on an empty temporary data dir (add test connections by hand).
#
# It runs in the foreground until Ctrl+C (or SIGTERM), or until one of its processes stops. Then it
# stops what it started and deletes the temporary data dir. The test containers keep running;
# `pnpm db:down` stops them. Browser localStorage (saved queries, chats, layout) for the test URL
# stays between runs: use a new browser profile when a test needs a clean UI state.
set -euo pipefail
[ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 3 ]; } \
  || { echo "test-env: needs bash 4.3 or later (brew install bash)" >&2; exit 1; }
# Each background job gets its own process group, so cleanup stops its whole tree. Foreground
# mode keeps one group instead: Playwright SIGKILLs that whole group, supervisor and server alike.
[ "${SANDB_TEST_FOREGROUND:-}" = 1 ] || set -m
cd "$(dirname "$0")/.."

mode=${1:-}
case "$mode" in mock | live | desktop) ;; *)
  sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
  ;;
esac

die() { echo "test-env: $*" >&2; exit 1; }

# A data dir chosen by the caller could be the real one: only this script picks it.
[ -z "${SANDB_DATA_DIR:-}" ] || die "SANDB_DATA_DIR is set ($SANDB_DATA_DIR); unset it, this script makes its own"

dev_port=${SANDB_TEST_DEV_PORT:-5299}
bridge_port=${SANDB_TEST_BRIDGE_PORT:-58223}
# Own ports, so a dev server or bridge the user started (maybe on real data) is never reused.
[ "$dev_port" != 5199 ] || die "port 5199 is the normal dev port; pick another SANDB_TEST_DEV_PORT"
for p in "$dev_port" $([ "$mode" = live ] && echo "$bridge_port"); do
  if lsof -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then die "port $p is already in use; stop that process or pick another port"; fi
done

tmp_base=${TMPDIR:-/tmp}
tmp_base=${tmp_base%/}
tmp=""
data=""
pids=()

# A job's whole process tree. pnpm may put its server child in a new process group, so group
# cleanup alone would leave that server behind after the test run.
job_tree_pids() {
  ps -eo pid=,ppid= | awk -v root="$1" '
    { pid[NR] = $1; parent[NR] = $2 }
    END {
      inside[root] = 1
      print root
      changed = 1
      while (changed) {
        changed = 0
        for (i = 1; i <= NR; i++) {
          if (!inside[pid[i]] && inside[parent[i]]) {
            inside[pid[i]] = 1
            print pid[i]
            changed = 1
          }
        }
      }
    }'
}

cleanup() {
  trap - EXIT INT TERM
  for pid in ${pids[@]+"${pids[@]}"}; do
    tree=$(job_tree_pids "$pid")
    kill -TERM $tree 2>/dev/null || true
    # A stopped job only gets the TERM once it runs again.
    kill -CONT $tree 2>/dev/null || true
  done
  wait 2>/dev/null || true
  # A custom data dir has its own credential store service (src-tauri/src/lib.rs); remove its items.
  if [ "$mode" = desktop ] && [ -n "$data" ]; then
    for account in vault:key ai:api_key mcp:token; do
      security delete-generic-password -s "dev.sandb.app ($data)" -a "$account" >/dev/null 2>&1 || true
    done
  fi
  case "$tmp" in "$tmp_base"/sandb-test-env.*) rm -rf -- "$tmp" ;; esac
  echo "test-env: stopped${tmp:+, removed $tmp}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 0' TERM

tmp=$(mktemp -d "$tmp_base/sandb-test-env.XXXXXX")
data="$tmp/data"
mkdir -p "$data"
real_data="$HOME/Library/Application Support/dev.sandb.app"
case "$(cd "$data" && pwd -P)/" in "$(cd "$real_data" 2>/dev/null && pwd -P || echo /nonexistent)"/*) die "data dir resolves inside the real app data" ;; esac

# Waits until a descendant of job `pid` listens on `port`. A listener from any other process fails,
# so READY never points at a server this script did not start. pnpm may put its server child in a
# new process group, so the owner is checked by ancestry rather than by the job's process group.
owned_by_job() {
  local owner=$1 pid=$2 parent
  for _ in $(seq 1 32); do
    [ "$owner" = "$pid" ] && return 0
    parent=$(ps -o ppid= -p "$owner" 2>/dev/null | tr -d ' ')
    [ -n "$parent" ] && [ "$parent" -ge 1 ] 2>/dev/null || return 1
    owner=$parent
  done
  return 1
}

wait_port() {
  local port=$1 name=$2 pid=$3 owner
  for _ in $(seq 1 600); do
    kill -0 "$pid" 2>/dev/null || die "$name exited; see $tmp/$name.log"
    owner=$(lsof -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -1 || true)
    if [ -n "$owner" ]; then
      owned_by_job "$owner" "$pid" || die "port $port is held by another process ($owner)"
      return 0
    fi
    sleep 0.5
  done
  die "$name did not listen on port $port; see $tmp/$name.log"
}

# Vite reads `.env.local` too; the process env wins over it, so these values always apply.
# Background jobs read /dev/null: under job control a read from the terminal would stop them.
start_vite() {
  SANDB_DEV_PORT=$dev_port "$@" node node_modules/vite/bin/vite.js </dev/null >"$tmp/vite.log" 2>&1 &
  pids+=($!)
  wait_port "$dev_port" vite $!
}

case "$mode" in
mock)
  # Playwright SIGKILLs the web-server process group, so a supervisor never gets to clean up.
  # Foreground mock mode replaces this script with Vite after the same port and environment checks.
  if [ "${SANDB_TEST_FOREGROUND:-}" = 1 ]; then
    SANDB_DEV_PORT="$dev_port" VITE_BACKEND=mock VITE_BRIDGE_TOKEN= node node_modules/vite/bin/vite.js </dev/null >"$tmp/vite.log" 2>&1 &
    vite=$!
    wait_port "$dev_port" vite "$vite"
    echo "test-env: READY mode=mock url=http://localhost:$dev_port logs=$tmp"
    wait "$vite"
    exit $?
  fi
  start_vite env VITE_BACKEND=mock VITE_BRIDGE_TOKEN=
  ;;

live)
  set -a
  # shellcheck disable=SC1091
  . testdb/.env.test
  set +a
  echo "test-env: starting the sandb-test containers (pnpm db:up)"
  pnpm db:up </dev/null >"$tmp/db-up.log" 2>&1 || die "pnpm db:up failed; see $tmp/db-up.log"
  [ -f testdb/sqlite/shop.db ] || pnpm db:sqlite </dev/null >/dev/null
  # A copy, so a test that writes does not change the fixture.
  cp testdb/sqlite/shop.db "$tmp/shop.db"

  echo "test-env: building the dev bridge"
  cargo build --manifest-path src-tauri/Cargo.toml --features dev-bridge --bin bridge </dev/null >"$tmp/build.log" 2>&1 \
    || die "bridge build failed; see $tmp/build.log"
  token=$(openssl rand -hex 24)
  SANDB_DATA_DIR="$data" SANDB_BRIDGE_TOKEN="$token" SANDB_BRIDGE_PORT="$bridge_port" SANDB_DEV_PORT="$dev_port" \
    src-tauri/target/debug/bridge </dev/null >"$tmp/bridge.log" 2>&1 &
  pids+=($!)
  wait_port "$bridge_port" bridge $!

  # The bridge's data dir is new, so these are the only connections it has.
  add() {
    local input=$1 password=$2
    case "$(jq -r '.host' <<<"$input")" in 127.0.0.1 | "") ;; *) die "refusing a test connection that is not on 127.0.0.1" ;; esac
    curl -fsS -X POST "http://127.0.0.1:$bridge_port/api/create_connection" \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
      -d "$(jq -nc --argjson input "$input" --arg p "$password" '{input: $input, password: (if $p == "" then null else $p end)}')" >/dev/null \
      || die "could not save test connection $(jq -r .name <<<"$input")"
  }
  sql() { jq -nc --arg n "$1" --arg e "$2" --argjson port "$3" --arg u "$4" --argjson ro "$5" \
    '{name: $n, engine: $e, env: "local", host: "127.0.0.1", port: $port, database: "shop", user: $u, ssl: "disable", readOnly: $ro}'; }
  add "$(sql 'TEST pg16 shop' pg "$PG16_PORT" "$SANDB_RW_USER" false)" "$SANDB_RW_PASSWORD"
  add "$(sql 'TEST pg16 shop (read-only)' pg "$PG16_PORT" "$SANDB_RO_USER" true)" "$SANDB_RO_PASSWORD"
  add "$(sql 'TEST mysql shop' mysql "$MYSQL_PORT" "$SANDB_RW_USER" false)" "$SANDB_RW_PASSWORD"
  add "$(sql 'TEST mariadb shop' mysql "$MARIADB_PORT" "$SANDB_RW_USER" false)" "$SANDB_RW_PASSWORD"
  add "$(jq -nc --arg f "$tmp/shop.db" '{name: "TEST sqlite shop", engine: "sqlite", host: "", file: $f}')" ""
  add "$(jq -nc --argjson port "$MONGO_RS_PORT" --arg u "$SANDB_RW_USER" \
    '{name: "TEST mongo shop_staging", engine: "mongo", env: "local", host: "127.0.0.1", port: $port, database: "shop_staging", user: $u, ssl: "disable", authSource: "admin"}')" "$SANDB_RW_PASSWORD"
  add "$(jq -nc --argjson port "$REDIS_PORT" --arg u "$SANDB_RW_USER" \
    '{name: "TEST redis", engine: "redis", env: "local", host: "127.0.0.1", port: $port, database: "0", user: $u, ssl: "disable", redis: {mode: "standalone"}}')" "$SANDB_RW_PASSWORD"

  start_vite env VITE_BACKEND=bridge VITE_BRIDGE_URL="http://127.0.0.1:$bridge_port" VITE_BRIDGE_TOKEN="$token"
  ;;

desktop)
  config=$(jq -nc --arg url "http://localhost:$dev_port" --arg cmd "SANDB_DEV_PORT=$dev_port VITE_BACKEND=desktop VITE_BRIDGE_TOKEN= pnpm dev" \
    '{build: {devUrl: $url, beforeDevCommand: $cmd}}')
  SANDB_DATA_DIR="$data" pnpm tauri dev --config "$config" </dev/null >"$tmp/tauri.log" 2>&1 &
  pids+=($!)
  tauri_pid=$!
  wait_port "$dev_port" tauri "$tauri_pid"
  # Vite is up before the app compiles; READY waits for the app itself.
  started=0
  for _ in $(seq 1 1200); do
    if grep -q 'target/debug/sandb`' "$tmp/tauri.log"; then started=1; break; fi
    kill -0 "$tauri_pid" 2>/dev/null || die "tauri dev exited; see $tmp/tauri.log"
    sleep 0.5
  done
  [ "$started" = 1 ] || die "the app did not start; see $tmp/tauri.log"
  echo "test-env: add connections by hand with the test credentials in testdb/README.md"
  ;;
esac

echo "test-env: READY mode=$mode url=http://localhost:$dev_port data=$data logs=$tmp"
# Stops everything as soon as one process stops, so a half-dead environment never stays up.
wait -n || true
echo "test-env: a process stopped; see the logs in $tmp" >&2
exit 1
