#!/usr/bin/env bash
# Blocks until every sandb-test service is healthy. Exits non-zero on timeout or a dead service.
set -euo pipefail
cd "$(dirname "$0")/.."

TIMEOUT="${TIMEOUT:-180}"
SERVICES=(pg16 pg13 mysql mariadb mongo-rs mongo-standalone mock-llm)
dc() { docker compose -p sandb-test -f compose.yaml --env-file .env.test "$@"; }

start=$(date +%s)
while true; do
  bad=()
  for s in "${SERVICES[@]}"; do
    id=$(dc ps -q "$s" 2>/dev/null || true)
    if [ -z "$id" ]; then bad+=("$s:missing"); continue; fi
    state=$(docker inspect -f '{{.State.Status}}/{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id")
    case "$state" in
      running/healthy) ;;
      exited/*|dead/*) echo "service $s is not running ($state)" >&2; dc logs --tail 40 "$s" >&2; exit 1 ;;
      *) bad+=("$s:$state") ;;
    esac
  done
  if [ ${#bad[@]} -eq 0 ]; then echo "all services healthy: ${SERVICES[*]}"; exit 0; fi
  if [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ]; then
    echo "timeout after ${TIMEOUT}s; not healthy: ${bad[*]}" >&2
    for b in "${bad[@]}"; do dc logs --tail 40 "${b%%:*}" >&2 || true; done
    exit 1
  fi
  sleep 2
done
