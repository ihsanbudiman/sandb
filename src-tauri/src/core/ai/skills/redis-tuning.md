Redis performance checks:

1. Find large keys in batches. Use `SCAN 0 MATCH prefix:* COUNT 100` instead of `KEYS`. `COUNT` does not set a hard limit. Use `TYPE key` to choose the size command:
   - `STRLEN` counts string bytes. `HLEN`, `LLEN`, `SCARD`, `ZCARD` and `XLEN` count elements.
   - `MEMORY USAGE key` estimates how many bytes a key and its value use. It can sample nested values. Do not use `SAMPLES 0` on large keys; that checks every element. `MEMORY USAGE` can need more permissions than `GET`.
2. Reduce the data in one reply. For large hashes use `HGET`, `HMGET` or `HSCAN` instead of `HGETALL`. Use `SSCAN` instead of `SMEMBERS` on large sets. Give `LRANGE` and `ZRANGE` a small range. Use `COUNT` with `XRANGE`.
3. Check server load:
   - `SLOWLOG GET 10` shows recent slow commands. The `duration_us` column gives execution time in microseconds. It does not include network time. An empty log does not rule out client or network delays.
   - `LATENCY DOCTOR` reports server latency events when the latency monitor is on. If it is off, report that limitation. Do not change server settings to collect data without approval.
   - `INFO memory` shows memory use and the configured memory limit. `INFO stats` shows operations, evictions and expirations. Use two samples to assess changes in the counters.
4. Check expiry with `PTTL key`. Nonnegative results are milliseconds. `-1` means no expiry, and `-2` means the key is absent. Check that cache and temporary keys expire as intended. Keep keys that need to persist separate from temporary keys. Changes to TTL need write approval.
