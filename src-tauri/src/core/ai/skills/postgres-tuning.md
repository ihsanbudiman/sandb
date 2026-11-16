PostgreSQL performance tuning:
1. Plan: EXPLAIN (FORMAT TEXT) SELECT ... or EXPLAIN (COSTS, VERBOSE) SELECT .... The app refuses EXPLAIN ANALYZE, because it runs the statement, so you see estimates, not real times. Say so when you explain a plan.
2. Read the plan from the innermost node:
   - Seq Scan on a big table with a selective filter: an index is missing or not usable.
   - Index Scan or Index Only Scan: good. Bitmap Heap Scan: good for medium selectivity.
   - Nested Loop with a big outer row count, Hash Join on huge inputs, Sort or HashAggregate on many rows: look at the join columns and the filters before the join.
   - Rows estimate far from the table size you expect: statistics may be old (see step 4).
3. Indexes:
   - List them: SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 't'.
   - Usage: SELECT relname, indexrelname, idx_scan, pg_size_pretty(pg_relation_size(indexrelid)) FROM pg_stat_user_indexes ORDER BY idx_scan. idx_scan = 0 on an old index means it is not used (the counter restarts with a stats reset).
   - B-tree column order: equality columns first, then the range column, then the ORDER BY column.
   - Partial index for a hot subset: CREATE INDEX ... WHERE status = 'pending'.
   - Expression index when the query filters on an expression: CREATE INDEX ... (lower(email)).
   - Covering index for index-only scans: CREATE INDEX ... (customer_id) INCLUDE (total).
   - GIN for jsonb @>, arrays and full text; BRIN for very big tables in time order.
   - A function or cast on the column (date(ts), col::text), a leading % in LIKE, or a type mismatch blocks a plain index.
4. Table health: SELECT relname, n_live_tup, n_dead_tup, last_autovacuum, last_autoanalyze, seq_scan, idx_scan FROM pg_stat_user_tables ORDER BY n_dead_tup DESC. Many dead rows: vacuum is behind. Old last_autoanalyze: run ANALYZE (a write, so propose it).
5. Sizes: pg_size_pretty(pg_total_relation_size('t')), pg_table_size, pg_indexes_size.
6. Settings: SHOW work_mem, SHOW shared_buffers, SHOW random_page_cost. The app hides pg_settings and pg_stat_statements; for the slowest queries, ask the user to read pg_stat_statements themselves.
7. Rewrites: filter on the raw column (ts >= '2026-01-01' AND ts < '2027-01-01'), EXISTS instead of IN or DISTINCT over a join, aggregate before you join, keyset pagination (WHERE id > last ORDER BY id LIMIT n) instead of a big OFFSET, no SELECT *.
8. Fixes that write: show CREATE INDEX CONCURRENTLY (it does not block writes but cannot run in a transaction), ANALYZE and VACUUM in a code block. Propose them as a write only when the user asks, and warn about the time and the lock on big tables.
