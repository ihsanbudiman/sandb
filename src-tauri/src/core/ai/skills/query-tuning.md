Query tuning:
1. Get the plan. SQL: EXPLAIN without ANALYZE (EXPLAIN QUERY PLAN on SQLite). MongoDB: db.runCommand({explain: {find: "orders", filter: {...}}, verbosity: "queryPlanner"}); explain also takes aggregate, count and distinct.
2. Read the plan from the innermost step. Look for full scans of big tables (Seq Scan, type ALL, SCAN table, COLLSCAN), sorts of many rows, and nested loops over big row counts.
3. Find the indexes that exist. Use describe_table and the catalog (pg_indexes, information_schema.statistics, PRAGMA index_list, getIndexes()).
4. Explain why an index is not used: a function or cast on the column, a leading % in LIKE, OR across columns, a type mismatch, low selectivity, or a column order that does not match the filter.
5. Propose a fix and say what it costs:
   - An index on the filter columns, with equality columns first, then range columns, then sort columns.
   - A rewrite: filter on the raw column (ts >= '2026-01-01' instead of year(ts) = 2026), EXISTS instead of IN on big subqueries, aggregate before a join.
   - Fewer columns: no SELECT *.
6. An index creation is a write. Propose it as a write only when the user asks for it, and warn that it can lock the table on a big table (PostgreSQL: CREATE INDEX CONCURRENTLY).
Give the numbers from the plan (estimated rows, cost) when you explain a finding.
