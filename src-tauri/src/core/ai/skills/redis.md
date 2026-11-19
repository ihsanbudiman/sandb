Redis commands and data types:

1. Use one command in redis-cli syntax. Put quotes around arguments with spaces. Use the key name from the tools, including its quotes when shown.
2. Key names often have a prefix, for example `order:123`. Use `SCAN 0 MATCH order:* COUNT 100` to find keys with that prefix. `MATCH` uses a glob pattern, not a regular expression. Continue with the next cursor until it is `0`. An empty batch can have a nonzero cursor. `COUNT` is a hint, not a limit. Ignore duplicate keys and do not treat a partial scan as a total. On Redis Cluster, use `list_tables` for a bounded list across the primary nodes; a single `SCAN` does not cover the cluster.
3. Use `TYPE key` if the key type is not known. Read only the data you need:
   - String: `GET key` gives `key` and `value`. `GETRANGE key 0 99` reads at most 100 bytes.
   - Hash: `HGET key field` reads one field. `HMGET key field1 field2`, `HSCAN` and `HGETALL` give `field` and `value` rows. Use `HSCAN key 0 COUNT 100` for unknown fields on a large hash.
   - List: `LRANGE key 0 19` gives `index` and `value`. The stop index is inclusive.
   - Set: `SSCAN key 0 COUNT 100` and `SMEMBERS key` give `member` rows. Use `SISMEMBER key member` to check one member.
   - Sorted set: `ZRANGE key 0 9 REV WITHSCORES` gives the top ten `member` and `score` rows. Use those column names for a chart.
   - Stream: `XRANGE key - + COUNT 20` gives `id` and a column for each field. Use `XREVRANGE key + - COUNT 20` for the latest entries. `XREAD` gives `stream`, `id` and JSON `fields`; omit `BLOCK`.
4. The read tools refuse `KEYS`, blocking commands and writes. For a Lua read use `EVAL_RO`, for example `EVAL_RO 'return redis.call("HLEN", KEYS[1])' 1 user:1`. Supply each key after the key count and read it from `KEYS` in the script. Use `FCALL_RO` for a read-only function that is already on the server.
