#!/usr/bin/env bash
# Local CI: the same checks that .github/workflows/ci.yml runs, on this machine.
# Needs Docker for the integration suites (pnpm db:up).
# The lanes run at the same time: the test databases, the frontend checks and the Rust checks.
# Each step writes its own log. A failed step does not stop the other steps; at the end the
# script prints the log of every failed step.
set -euo pipefail
cd "$(dirname "$0")/.."

logs=$(mktemp -d)
start=$SECONDS

# run NAME CMD...: runs one step with its output in $logs/NAME.log and prints its time.
run() {
  local name=$1; shift
  local t=$SECONDS
  if "$@" >"$logs/$name.log" 2>&1; then
    printf '  ok    %-16s %4ds\n' "$name" $((SECONDS - t))
  else
    printf '  FAIL  %-16s %4ds\n' "$name" $((SECONDS - t))
    echo "$name" >>"$logs/failed"
    return 1
  fi
}

# The stack is throwaway (tmpfs). Stop it, and every step still running, also on a failure.
trap 'kill $(jobs -p) 2>/dev/null || true; pnpm db:down >/dev/null 2>&1 || true; rm -rf "$logs"' EXIT

# The databases start first and get ready while the code compiles.
run test-databases pnpm db:up &
db=$!

run install pnpm install --frozen-lockfile
run sqlite-fixture pnpm db:sqlite

(
  run frontend-check pnpm check || :
  run frontend-bundle pnpm test:bundle || :
  run frontend-unit pnpm test:unit || :
  run browser-deps pnpm exec playwright install chromium && run interface pnpm test:ui || :
) &
frontend=$!

# Every Rust test target builds once, with one feature set (--all-features). The test binaries
# then run directly, so a running suite does not hold Cargo's build lock for the next step.
build_tests() {
  cargo test --manifest-path src-tauri/Cargo.toml --all-features --no-run \
    --message-format=json-render-diagnostics >"$logs/tests.json"
  # kind: "test" for tests/*.rs, "unit" for the lib and bin targets (the lib has several crate types).
  jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable != null)
    | "\(if .target.kind == ["test"] then "test" else "unit" end) \(.target.name) \(.executable)"' \
    "$logs/tests.json" >"$logs/tests"
}

# run_tests INCLUDE [EXCLUDE]: runs every test binary whose "kind name" line matches the regex
# INCLUDE and not EXCLUDE, from the crate root as `cargo test` does. All of them run; it fails
# if one failed.
run_tests() {
  local status=0 kind name exe
  while read -r kind name exe; do
    printf '\n==> %s %s\n' "$kind" "$name"
    (cd src-tauri && "$exe") || status=1
  done < <(grep -E "$1" "$logs/tests" | grep -Ev "${2:-^$}")
  return $status
}

built=0
run rust-build build_tests && built=1 || :
dbok=0
wait $db && dbok=1 || :

rust=()
if [ $built -eq 1 ]; then
  run rust-unit run_tests '^unit ' & rust+=($!)
  if [ $dbok -eq 1 ]; then
    # it_mongo_fault runs its cases one at a time (one faulting proxy at a time), so it is
    # the longest suite; it runs beside the other suites, each on its own scratch data.
    run it-mongo-fault run_tests '^test it_mongo_fault ' & rust+=($!)
    run integration run_tests '^test it_' '^test it_mongo_fault ' & rust+=($!)
  fi
fi
run rust-lints cargo clippy --manifest-path src-tauri/Cargo.toml --all-features --all-targets -- -D warnings || :
run rust-doc cargo test --manifest-path src-tauri/Cargo.toml --all-features --doc || :

for pid in "${rust[@]}" "$frontend"; do wait "$pid" || :; done

if [ -s "$logs/failed" ]; then
  while IFS= read -r name; do
    printf '\n==> %s (log)\n' "$name"
    cat "$logs/$name.log"
  done <"$logs/failed"
  printf '\nCI FAILED in %ds: %s\n' $((SECONDS - start)) "$(tr '\n' ' ' <"$logs/failed")"
  exit 1
fi
# The totals show that the parallel lanes together ran every Rust test binary.
binaries=$(cat "$logs"/rust-unit.log "$logs"/it-*.log "$logs"/integration.log | grep -c '^==> ')
tests=$(cat "$logs"/rust-unit.log "$logs"/it-*.log "$logs"/integration.log \
  | grep -oE 'test result: ok\. [0-9]+ passed' | awk '{s += $4} END {print s + 0}')
printf '\nCI passed in %ds: %s Rust tests in %s of %s test binaries.\n' \
  $((SECONDS - start)) "$tests" "$binaries" "$(wc -l <"$logs/tests" | tr -d ' ')"
