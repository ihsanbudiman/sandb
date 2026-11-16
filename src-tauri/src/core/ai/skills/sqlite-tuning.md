SQLite performance tuning:
1. Plan: EXPLAIN QUERY PLAN SELECT ....
   - SCAN t: a full table scan. SEARCH t USING INDEX i (col=?): an index search. USING COVERING INDEX: no table lookup.
   - USE TEMP B-TREE FOR ORDER BY or GROUP BY: a sort that an index could avoid.
   - AUTOMATIC INDEX: SQLite builds a temporary index for each run; a real index is better.
2. Indexes:
   - List them: PRAGMA index_list('t'), then PRAGMA index_info('index_name'). Columns: PRAGMA table_info('t').
   - The app allows only these PRAGMAs: table_info, table_xinfo, index_list, index_info, foreign_key_list.
   - Column order: equality columns first, then one range column, then the ORDER BY column.
   - Covering index: add the selected columns to avoid the table lookup.
   - Partial index: CREATE INDEX ... WHERE deleted = 0. Expression index: CREATE INDEX ... (lower(email)).
   - Type affinity: comparing a TEXT column with a number, or the other way, can stop the index. Compare with the stored type (check typeof(col)).
   - Functions on the column (date(ts), lower(col)) and a leading % in LIKE block a plain index.
   - Foreign key columns have no index unless you create one; joins and ON DELETE checks need it.
3. Statistics: without ANALYZE, the planner guesses. Propose ANALYZE (a write) when the plans pick the wrong index.
4. Rewrites: filter on the raw column, EXISTS instead of IN on big subqueries, keyset pagination instead of a big OFFSET, no SELECT *, aggregate before a join.
5. Fixes that write: show CREATE INDEX and ANALYZE in a code block. SQLite locks the whole database while it writes. Propose them as a write only when the user asks.
