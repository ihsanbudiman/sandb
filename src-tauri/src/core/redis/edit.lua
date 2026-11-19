-- ARGV: type, list deletion marker, then records of (op, source, destination, old, value).
local key, kind, marker = KEYS[1], ARGV[1], ARGV[2]
if redis.call('TYPE', key).ok ~= kind then return {1} end

local writes, removals, seen = {}, {}, {}
local first_list_delete

-- Batch adjacent variadic commands without exceeding Lua's unpack stack limit. On Redis 6
-- a single native write is safe even when there is no way to preflight permissions.
local function queue(plan, name, option, ...)
    local args = plan[#plan]
    local count = select('#', ...)
    if name == 'LSET' or not args or args[1] ~= name or args.option ~= option or #args + count > 1024 then
        args = {name, key, option = option}
        if option then args[3] = option end
        plan[#plan + 1] = args
    end
    for i = 1, count do args[#args + 1] = select(i, ...) end
end

local function score_text(s)
    if s == 'inf' or s == '+inf' then return 'inf' end
    if s == '-inf' then return '-inf' end
    local n = tonumber(s)
    -- Both decimal encodings describe a double. Compare their canonical text, not the
    -- spelling returned by ZSCORE against the spelling round-tripped through JavaScript.
    if n == 0 then return '0' end
    return string.format('%.17g', n)
end

for pos = 3, #ARGV, 5 do
    local i = (pos - 3) / 5 + 1
    local op, src, dst, old, value = unpack(ARGV, pos, pos + 4)
    local insert, delete = op == 'i', op == 'd'
    local rename = op == 'r'
    if not insert then
        if seen[src] then return {i} end
        seen[src] = true
    end
    if rename or (insert and kind ~= 'list') then
        if seen[dst] then return {i} end
        seen[dst] = true
    end

    if kind == 'string' then
        if redis.call('GET', key) ~= old then return {i} end
        queue(writes, 'SET', false, value, 'KEEPTTL')
    elseif kind == 'hash' then
        if not insert and redis.call('HGET', key, src) ~= old then return {i} end
        if (insert or rename) and redis.call('HEXISTS', key, dst) ~= 0 then return {i} end
        if insert or rename then
            queue(writes, 'HSET', false, dst, value)
        elseif not delete then
            queue(writes, 'HSET', false, src, value)
        end
        if delete or rename then queue(removals, 'HDEL', false, src) end
    elseif kind == 'list' then
        if not insert and redis.call('LINDEX', key, src) ~= old then return {i} end
        if marker ~= '' and not delete and value == marker then return {i} end
        if insert then
            queue(writes, 'RPUSH', false, value)
        elseif delete then
            first_list_delete = first_list_delete or i
            queue(writes, 'LSET', false, src, marker)
        else
            queue(writes, 'LSET', false, src, value)
        end
    elseif kind == 'set' then
        if not insert and redis.call('SISMEMBER', key, src) ~= 1 then return {i} end
        if (insert or rename) and redis.call('SISMEMBER', key, dst) ~= 0 then return {i} end
        if insert or rename then queue(writes, 'SADD', false, dst) end
        if delete or rename then queue(removals, 'SREM', false, src) end
    elseif kind == 'zset' then
        if not insert then
            local score = redis.call('ZSCORE', key, src)
            if not score or score_text(score) ~= score_text(old) then return {i} end
        end
        if (insert or rename) and redis.call('ZSCORE', key, dst) then return {i} end
        if insert or rename then
            queue(writes, 'ZADD', 'NX', value, dst)
        elseif not delete then
            queue(writes, 'ZADD', 'XX', value, src)
        end
        if delete or rename then queue(removals, 'ZREM', false, src) end
    end
end

-- Additions precede removals so replacing the last field or member does not clear the TTL.
for _, args in ipairs(removals) do writes[#writes + 1] = args end
if first_list_delete then queue(writes, 'LREM', false, '0', marker) end

-- Redis scripts isolate writes; they do not roll back a command that has already run.
if redis.acl_check_cmd then
    for _, args in ipairs(writes) do
        if not redis.acl_check_cmd(unpack(args)) then
            return redis.error_reply('NOPERM Permission required for ' .. args[1] .. '. Nothing was saved.')
        end
    end
elseif #writes > 1 then
    return redis.error_reply('SANDBACL This save needs Redis 7 or later to check write permissions. Nothing was saved.')
end

if first_list_delete and redis.call('LPOS', key, marker) ~= false then return {first_list_delete} end
for i, args in ipairs(writes) do
    local result = redis.pcall(unpack(args))
    if type(result) == 'table' and result.err then
        if i == 1 then return result end
        return redis.error_reply('SANDBPARTIAL Some changes may have been saved. Reload the result. ' .. result.err)
    end
end
return {0}
