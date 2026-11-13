#!/usr/bin/env bash
# Builds testdb/sqlite/shop.db (seeded). Only this file is replaced.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v sqlite3 >/dev/null || { echo "sqlite3 CLI not found" >&2; exit 1; }
mkdir -p sqlite
rm -f sqlite/shop.db
sqlite3 -bail sqlite/shop.db < seed/sqlite/shop.sql
echo "built $(pwd)/sqlite/shop.db"
