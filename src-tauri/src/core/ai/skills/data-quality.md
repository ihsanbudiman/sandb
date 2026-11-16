Data quality check of a table or collection. Run few queries; aggregate on the server.
1. Size: the row count, and the newest and oldest timestamp.
2. Missing values: for each important column, the count and percentage of NULL (MongoDB: missing or null) and of empty text.
3. Duplicates: group by the natural key (for example email, or order number) and count the groups with more than one row. Show at most 10 examples.
4. Orphans: rows whose foreign key value has no parent row (LEFT JOIN parent WHERE parent.id IS NULL; MongoDB: $lookup then match an empty array).
5. Ranges: min, max and average of numeric columns. Flag negative amounts, dates in the future or before the year 2000, and values far outside the usual range.
6. Categories: the distinct values and counts of status-like columns. Flag spelling variants ('Paid', 'paid ', 'PAID').
7. Types: on MongoDB and SQLite, fields with more than one type.
Report each finding with its count and percentage, and order them by impact. Say which checks found no problem. Do not fix data; propose a write only when the user asks.
