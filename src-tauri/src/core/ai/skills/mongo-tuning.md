MongoDB performance tuning:
1. Plan: db.runCommand({explain: {find: "orders", filter: {...}, sort: {...}}, verbosity: "queryPlanner"}). For a pipeline: {explain: {aggregate: "orders", pipeline: [...], cursor: {}}, verbosity: "queryPlanner"}. Explain also takes count and distinct.
   - verbosity "executionStats" runs the query to give real numbers. Use it only after queryPlanner, and only on a filter that is not too slow.
2. Read queryPlanner.winningPlan from the innermost stage:
   - COLLSCAN: a full collection scan. IXSCAN then FETCH: an index search. IXSCAN without FETCH: a covered query (the projection excludes _id or the index holds it).
   - SORT stage: an in-memory sort that an index could avoid (the limit is 100 MB without allowDiskUse).
   - rejectedPlans: other indexes the planner tried.
   - executionStats: compare totalKeysExamined and totalDocsExamined with nReturned. A good query examines about as many as it returns.
3. Indexes:
   - List them: db.orders.getIndexes(). Usage since the last restart: db.orders.aggregate([{$indexStats: {}}]) (accesses.ops = 0 means not used).
   - ESR rule for compound indexes: Equality fields first, then Sort fields, then Range fields.
   - Index prefixes: {a: 1, b: 1} also serves queries on a alone; a separate {a: 1} index is redundant.
   - Multikey indexes on arrays: at most one array field per compound index.
   - Partial index for a hot subset: {partialFilterExpression: {status: "pending"}}. TTL index for data that expires.
   - $regex without a ^ anchor, $ne, $nin and $not cannot use an index well. Case-insensitive search needs a collation index, not /x/i.
4. Pipelines: $match and $sort at the start can use an index; after $group, $unwind or $project they cannot. Put $limit right after $sort. Filter before $lookup, and index the foreign field of the $lookup.
5. Sizes: db.runCommand({collStats: "orders"}) gives count, size, avgObjSize, totalIndexSize and indexSizes. db.runCommand({dbStats: 1}) for the database. Large documents and unbounded arrays make every read slower.
6. Server: db.runCommand({serverStatus: 1}) and db.runCommand({currentOp: 1}) show load and running operations. The profiler setting is a write; for the slowest queries, ask the user to read system.profile or the slow query log.
7. Fixes that write: show db.orders.createIndex({...}) in a code block. An index build on a big collection takes time and uses memory. Propose it as a write only when the user asks.
