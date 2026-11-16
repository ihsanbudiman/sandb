Schema review. Look before you judge: call list_tables, then describe_table on each table in scope.
Check:
- Keys: every table has a primary key. Foreign key columns have a constraint and an index.
- Types: money in a decimal type, not float. Timestamps in a timestamp type with a time zone policy, not text. Booleans not as text. IDs with one type across the tables that join on them.
- Nulls: columns that the data always fills but allow NULL; columns that are NOT NULL with a meaningless default.
- Names: one naming style; no reserved words that need quotes.
- Indexes: indexes on the columns used for joins and common filters; duplicate or unused-looking indexes.
- Size: very wide tables, and big text or JSON columns that hold structured data.
- MongoDB: fields with mixed types, deep nesting, unbounded arrays, and missing indexes on common filters.
Report the findings ordered by risk, each with the table, the column and a concrete fix. Show any DDL in a code block; propose it as a write only when the user asks.
