MySQL and MariaDB performance tuning:
1. Plan: EXPLAIN SELECT .... The app refuses EXPLAIN ANALYZE, because it runs the statement, so you see estimates only.
2. Read each EXPLAIN row (one per table, in join order):
   - type: const, eq_ref and ref are good; range is fine; index is a full index scan; ALL is a full table scan.
   - key: the index used (NULL: none). possible_keys: the candidates. key_len: how many index columns are used.
   - rows times filtered: the rows the server expects to read.
   - Extra: "Using filesort" and "Using temporary" mean a sort or a temporary table; "Using index" means a covering index; "Using where" after a full scan means filtering late.
3. Indexes:
   - List them: SHOW INDEX FROM t, or information_schema.statistics. Cardinality shows selectivity.
   - Composite order (leftmost prefix rule): equality columns first, then the range column; columns after the first range column are not used for the search.
   - Covering index: add the selected columns so that Extra shows "Using index".
   - A function on the column (DATE(ts), LOWER(col)), a leading % in LIKE, an implicit type or collation conversion in a join or filter blocks the index. MySQL 8 has functional indexes: INDEX ((LOWER(email))).
   - InnoDB adds the primary key to every secondary index, so a long primary key makes all indexes bigger.
4. Table data: SHOW TABLE STATUS LIKE 't' (rows, data and index length); information_schema.tables for all tables. Old statistics give bad plans: ANALYZE TABLE t (propose it as a write).
5. Settings: SHOW VARIABLES LIKE 'innodb_buffer_pool_size', SHOW VARIABLES LIKE 'long_query_time'. The app hides performance_schema, sys and the process list; for the slowest queries, ask the user to read the slow query log or sys.statements_with_runtimes_in_95th_percentile.
6. Rewrites: filter on the raw column (ts >= '2026-01-01' AND ts < '2027-01-01'), EXISTS or a JOIN instead of IN (subquery) on old versions, no OR across different columns (use UNION ALL), keyset pagination instead of a big OFFSET, no SELECT *.
7. Fixes that write: show ALTER TABLE ... ADD INDEX ... in a code block, with ALGORITHM=INPLACE, LOCK=NONE where the version supports it. MySQL commits DDL at once. Propose it as a write only when the user asks, and warn about the time on big tables.
