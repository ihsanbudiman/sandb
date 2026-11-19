//! Redis command effects used by editor safety checks and completion. Only explicit entries are
//! trusted as reads; a container command does not confer its subcommands' permissions.

use crate::core::error::AppError;

use super::{key::quote_arg, parse::Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
    Flush,
    Admin,
    Script,
    Select,
    Unknown,
    Unsupported(&'static str),
}

pub struct Spec {
    pub name: &'static str,
    pub kind: Kind,
    /// Argument index of the first key. Zero means there is no fixed key position.
    pub first_key: u8,
}

const TRANSACTIONS: &str = "Transactions are not supported. Use a Lua script (EVAL).";
const PUBSUB: &str = "Pub/Sub and MONITOR are not supported.";
const BLOCKING: &str = "Blocking commands are not supported.";

macro_rules! spec {
    ($name:literal, $kind:expr, $key:literal) => {
        Spec {
            name: $name,
            kind: $kind,
            first_key: $key,
        }
    };
}

macro_rules! connection {
    ($name:literal) => {
        spec!(
            $name,
            Unsupported(concat!(
                "sandb manages the connection; ",
                $name,
                " is not supported."
            )),
            0
        )
    };
}

use Kind::*;

// Keep this table sorted by name for the binary search in `lookup`.
pub(crate) const COMMANDS: &[Spec] = &[
    spec!("ACL CAT", Read, 0),
    spec!("ACL DELUSER", Admin, 0),
    spec!("ACL LIST", Read, 0),
    spec!("ACL LOAD", Admin, 0),
    spec!("ACL LOG", Admin, 0),
    spec!("ACL SAVE", Admin, 0),
    spec!("ACL SETUSER", Admin, 0),
    spec!("ACL USERS", Read, 0),
    spec!("ACL WHOAMI", Read, 0),
    spec!("APPEND", Write, 1),
    connection!("ASKING"),
    connection!("AUTH"),
    spec!("BF.ADD", Write, 1),
    spec!("BF.EXISTS", Read, 1),
    spec!("BF.INFO", Read, 1),
    spec!("BF.MADD", Write, 1),
    spec!("BF.MEXISTS", Read, 1),
    spec!("BGREWRITEAOF", Admin, 0),
    spec!("BGSAVE", Admin, 0),
    spec!("BITCOUNT", Read, 1),
    spec!("BITFIELD", Write, 1),
    spec!("BITFIELD_RO", Read, 1),
    spec!("BITOP", Write, 2),
    spec!("BITPOS", Read, 1),
    spec!("BLMOVE", Unsupported(BLOCKING), 0),
    spec!("BLMPOP", Unsupported(BLOCKING), 0),
    spec!("BLPOP", Unsupported(BLOCKING), 0),
    spec!("BRPOP", Unsupported(BLOCKING), 0),
    spec!("BRPOPLPUSH", Unsupported(BLOCKING), 0),
    spec!("BZMPOP", Unsupported(BLOCKING), 0),
    spec!("BZPOPMAX", Unsupported(BLOCKING), 0),
    spec!("BZPOPMIN", Unsupported(BLOCKING), 0),
    spec!("CF.ADD", Write, 1),
    spec!("CF.DEL", Write, 1),
    spec!("CF.EXISTS", Read, 1),
    spec!("CF.INFO", Read, 1),
    connection!("CLIENT CACHING"),
    spec!("CLIENT GETNAME", Read, 0),
    spec!("CLIENT ID", Read, 0),
    spec!("CLIENT INFO", Read, 0),
    spec!("CLIENT KILL", Admin, 0),
    spec!("CLIENT LIST", Read, 0),
    spec!("CLIENT NO-EVICT", Admin, 0),
    spec!("CLIENT NO-TOUCH", Admin, 0),
    spec!("CLIENT PAUSE", Admin, 0),
    connection!("CLIENT REPLY"),
    spec!("CLIENT SETINFO", Admin, 0),
    spec!("CLIENT SETNAME", Admin, 0),
    connection!("CLIENT TRACKING"),
    spec!("CLIENT UNPAUSE", Admin, 0),
    spec!("CLUSTER ADDSLOTS", Admin, 0),
    spec!("CLUSTER ADDSLOTSRANGE", Admin, 0),
    spec!("CLUSTER BUMPEPOCH", Admin, 0),
    spec!("CLUSTER COUNTKEYSINSLOT", Read, 0),
    spec!("CLUSTER DELSLOTS", Admin, 0),
    spec!("CLUSTER DELSLOTSRANGE", Admin, 0),
    spec!("CLUSTER FAILOVER", Admin, 0),
    spec!("CLUSTER FLUSHSLOTS", Admin, 0),
    spec!("CLUSTER FORGET", Admin, 0),
    spec!("CLUSTER GETKEYSINSLOT", Admin, 0),
    spec!("CLUSTER HELP", Admin, 0),
    spec!("CLUSTER INFO", Read, 0),
    spec!("CLUSTER KEYSLOT", Read, 2),
    spec!("CLUSTER LINKS", Admin, 0),
    spec!("CLUSTER MEET", Admin, 0),
    spec!("CLUSTER MYID", Read, 0),
    spec!("CLUSTER MYSHARDID", Admin, 0),
    spec!("CLUSTER NODES", Read, 0),
    spec!("CLUSTER REPLICATE", Admin, 0),
    spec!("CLUSTER RESET", Admin, 0),
    spec!("CLUSTER SAVECONFIG", Admin, 0),
    spec!("CLUSTER SET-CONFIG-EPOCH", Admin, 0),
    spec!("CLUSTER SETSLOT", Admin, 0),
    spec!("CLUSTER SHARDS", Read, 0),
    spec!("CLUSTER SLOTS", Read, 0),
    spec!("COMMAND COUNT", Read, 0),
    spec!("COMMAND DOCS", Read, 0),
    spec!("COMMAND GETKEYS", Read, 0),
    spec!("COMMAND INFO", Read, 0),
    spec!("COMMAND LIST", Read, 0),
    spec!("CONFIG GET", Read, 0),
    spec!("CONFIG RESETSTAT", Admin, 0),
    spec!("CONFIG REWRITE", Admin, 0),
    spec!("CONFIG SET", Admin, 0),
    spec!("COPY", Write, 1),
    spec!("DBSIZE", Read, 0),
    spec!("DEBUG", Admin, 0),
    spec!("DECR", Write, 1),
    spec!("DECRBY", Write, 1),
    spec!("DEL", Write, 1),
    spec!("DISCARD", Unsupported(TRANSACTIONS), 0),
    spec!("DUMP", Read, 1),
    spec!("ECHO", Read, 0),
    spec!("EVAL", Script, 3),
    spec!("EVALSHA", Script, 3),
    spec!("EVALSHA_RO", Read, 3),
    spec!("EVAL_RO", Read, 3),
    spec!("EXEC", Unsupported(TRANSACTIONS), 0),
    spec!("EXISTS", Read, 1),
    spec!("EXPIRE", Write, 1),
    spec!("EXPIREAT", Write, 1),
    spec!("EXPIRETIME", Read, 1),
    spec!("FAILOVER", Admin, 0),
    spec!("FCALL", Script, 3),
    spec!("FCALL_RO", Read, 3),
    spec!("FLUSHALL", Flush, 0),
    spec!("FLUSHDB", Flush, 0),
    spec!("FT.AGGREGATE", Read, 0),
    spec!("FT.ALTER", Write, 0),
    spec!("FT.CREATE", Write, 0),
    spec!("FT.DROPINDEX", Write, 0),
    spec!("FT.EXPLAIN", Read, 0),
    spec!("FT.INFO", Read, 0),
    spec!("FT.SEARCH", Read, 0),
    spec!("FT._LIST", Read, 0),
    spec!("FUNCTION DELETE", Admin, 0),
    spec!("FUNCTION DUMP", Read, 0),
    spec!("FUNCTION FLUSH", Admin, 0),
    spec!("FUNCTION KILL", Admin, 0),
    spec!("FUNCTION LIST", Read, 0),
    spec!("FUNCTION LOAD", Write, 0),
    spec!("FUNCTION RESTORE", Admin, 0),
    spec!("FUNCTION STATS", Read, 0),
    spec!("GEOADD", Write, 1),
    spec!("GEODIST", Read, 1),
    spec!("GEOHASH", Read, 1),
    spec!("GEOPOS", Read, 1),
    spec!("GEORADIUS", Read, 1),
    spec!("GEORADIUSBYMEMBER", Read, 1),
    spec!("GEORADIUSBYMEMBER_RO", Read, 1),
    spec!("GEORADIUS_RO", Read, 1),
    spec!("GEOSEARCH", Read, 1),
    spec!("GEOSEARCHSTORE", Write, 1),
    spec!("GET", Read, 1),
    spec!("GETBIT", Read, 1),
    spec!("GETDEL", Write, 1),
    spec!("GETEX", Write, 1),
    spec!("GETRANGE", Read, 1),
    spec!("GETSET", Write, 1),
    spec!("HDEL", Write, 1),
    connection!("HELLO"),
    spec!("HEXISTS", Read, 1),
    spec!("HEXPIRE", Write, 1),
    spec!("HEXPIREAT", Write, 1),
    spec!("HEXPIRETIME", Read, 1),
    spec!("HGET", Read, 1),
    spec!("HGETALL", Read, 1),
    spec!("HGETDEL", Write, 1),
    spec!("HGETEX", Write, 1),
    spec!("HINCRBY", Write, 1),
    spec!("HINCRBYFLOAT", Write, 1),
    spec!("HKEYS", Read, 1),
    spec!("HLEN", Read, 1),
    spec!("HMGET", Read, 1),
    spec!("HMSET", Write, 1),
    spec!("HPERSIST", Write, 1),
    spec!("HPEXPIRE", Write, 1),
    spec!("HPEXPIREAT", Write, 1),
    spec!("HPEXPIRETIME", Read, 1),
    spec!("HPTTL", Read, 1),
    spec!("HRANDFIELD", Read, 1),
    spec!("HSCAN", Read, 1),
    spec!("HSET", Write, 1),
    spec!("HSETEX", Write, 1),
    spec!("HSETNX", Write, 1),
    spec!("HSTRLEN", Read, 1),
    spec!("HTTL", Read, 1),
    spec!("HVALS", Read, 1),
    spec!("INCR", Write, 1),
    spec!("INCRBY", Write, 1),
    spec!("INCRBYFLOAT", Write, 1),
    spec!("INFO", Read, 0),
    spec!("JSON.ARRAPPEND", Write, 1),
    spec!("JSON.ARRINDEX", Read, 1),
    spec!("JSON.ARRINSERT", Write, 1),
    spec!("JSON.ARRLEN", Read, 1),
    spec!("JSON.ARRPOP", Write, 1),
    spec!("JSON.ARRTRIM", Write, 1),
    spec!("JSON.CLEAR", Write, 1),
    spec!("JSON.DEL", Write, 1),
    spec!("JSON.FORGET", Write, 1),
    spec!("JSON.GET", Read, 1),
    spec!("JSON.MERGE", Write, 1),
    spec!("JSON.MGET", Read, 1),
    spec!("JSON.MSET", Write, 1),
    spec!("JSON.NUMINCRBY", Write, 1),
    spec!("JSON.OBJKEYS", Read, 1),
    spec!("JSON.OBJLEN", Read, 1),
    spec!("JSON.SET", Write, 1),
    spec!("JSON.STRAPPEND", Write, 1),
    spec!("JSON.STRLEN", Read, 1),
    spec!("JSON.TOGGLE", Write, 1),
    spec!("JSON.TYPE", Read, 1),
    spec!("KEYS", Read, 0),
    spec!("LASTSAVE", Read, 0),
    spec!("LATENCY DOCTOR", Read, 0),
    spec!("LATENCY HISTORY", Read, 0),
    spec!("LATENCY LATEST", Read, 0),
    spec!("LATENCY RESET", Admin, 0),
    spec!("LCS", Read, 1),
    spec!("LINDEX", Read, 1),
    spec!("LINSERT", Write, 1),
    spec!("LLEN", Read, 1),
    spec!("LMOVE", Write, 1),
    spec!("LMPOP", Write, 2),
    spec!("LPOP", Write, 1),
    spec!("LPOS", Read, 1),
    spec!("LPUSH", Write, 1),
    spec!("LPUSHX", Write, 1),
    spec!("LRANGE", Read, 1),
    spec!("LREM", Write, 1),
    spec!("LSET", Write, 1),
    spec!("LTRIM", Write, 1),
    spec!("MEMORY DOCTOR", Read, 0),
    spec!("MEMORY PURGE", Admin, 0),
    spec!("MEMORY STATS", Read, 0),
    spec!("MEMORY USAGE", Read, 2),
    spec!("MGET", Read, 1),
    spec!("MODULE LIST", Read, 0),
    spec!("MODULE LOAD", Admin, 0),
    spec!("MODULE LOADEX", Admin, 0),
    spec!("MODULE UNLOAD", Admin, 0),
    spec!("MONITOR", Unsupported(PUBSUB), 0),
    spec!("MOVE", Write, 1),
    spec!("MSET", Write, 1),
    spec!("MSETNX", Write, 1),
    spec!("MULTI", Unsupported(TRANSACTIONS), 0),
    spec!("OBJECT ENCODING", Read, 2),
    spec!("OBJECT FREQ", Read, 2),
    spec!("OBJECT IDLETIME", Read, 2),
    spec!("OBJECT REFCOUNT", Read, 2),
    spec!("PERSIST", Write, 1),
    spec!("PEXPIRE", Write, 1),
    spec!("PEXPIREAT", Write, 1),
    spec!("PEXPIRETIME", Read, 1),
    spec!("PFADD", Write, 1),
    spec!("PFCOUNT", Read, 1),
    spec!("PFMERGE", Write, 1),
    spec!("PING", Read, 0),
    spec!("PSETEX", Write, 1),
    spec!("PSUBSCRIBE", Unsupported(PUBSUB), 0),
    connection!("PSYNC"),
    spec!("PTTL", Read, 1),
    spec!("PUBLISH", Write, 0),
    spec!("PUBSUB CHANNELS", Read, 0),
    spec!("PUBSUB NUMPAT", Read, 0),
    spec!("PUBSUB NUMSUB", Read, 0),
    spec!("PUBSUB SHARDCHANNELS", Read, 0),
    spec!("PUBSUB SHARDNUMSUB", Read, 0),
    spec!("PUNSUBSCRIBE", Unsupported(PUBSUB), 0),
    connection!("QUIT"),
    spec!("RANDOMKEY", Read, 0),
    connection!("READONLY"),
    connection!("READWRITE"),
    spec!("RENAME", Write, 1),
    spec!("RENAMENX", Write, 1),
    connection!("REPLCONF"),
    spec!("REPLICAOF", Admin, 0),
    connection!("RESET"),
    spec!("RESTORE", Write, 1),
    spec!("ROLE", Read, 0),
    spec!("RPOP", Write, 1),
    spec!("RPOPLPUSH", Write, 1),
    spec!("RPUSH", Write, 1),
    spec!("RPUSHX", Write, 1),
    spec!("SADD", Write, 1),
    spec!("SAVE", Admin, 0),
    spec!("SCAN", Read, 0),
    spec!("SCARD", Read, 1),
    connection!("SCRIPT DEBUG"),
    spec!("SCRIPT EXISTS", Read, 0),
    spec!("SCRIPT FLUSH", Admin, 0),
    spec!("SCRIPT KILL", Admin, 0),
    spec!("SCRIPT LOAD", Write, 0),
    spec!("SDIFF", Read, 1),
    spec!("SDIFFSTORE", Write, 1),
    spec!("SELECT", Select, 0),
    spec!("SET", Write, 1),
    spec!("SETBIT", Write, 1),
    spec!("SETEX", Write, 1),
    spec!("SETNX", Write, 1),
    spec!("SETRANGE", Write, 1),
    spec!("SHUTDOWN", Admin, 0),
    spec!("SINTER", Read, 1),
    spec!("SINTERCARD", Read, 2),
    spec!("SINTERSTORE", Write, 1),
    spec!("SISMEMBER", Read, 1),
    spec!("SLAVEOF", Admin, 0),
    spec!("SLOWLOG GET", Read, 0),
    spec!("SLOWLOG LEN", Read, 0),
    spec!("SLOWLOG RESET", Admin, 0),
    spec!("SMEMBERS", Read, 1),
    spec!("SMISMEMBER", Read, 1),
    spec!("SMOVE", Write, 1),
    spec!("SORT", Read, 1),
    spec!("SORT_RO", Read, 1),
    spec!("SPOP", Write, 1),
    spec!("SPUBLISH", Write, 0),
    spec!("SRANDMEMBER", Read, 1),
    spec!("SREM", Write, 1),
    spec!("SSCAN", Read, 1),
    spec!("SSUBSCRIBE", Unsupported(PUBSUB), 0),
    spec!("STRLEN", Read, 1),
    spec!("SUBSCRIBE", Unsupported(PUBSUB), 0),
    spec!("SUBSTR", Read, 1),
    spec!("SUNION", Read, 1),
    spec!("SUNIONSTORE", Write, 1),
    spec!("SUNSUBSCRIBE", Unsupported(PUBSUB), 0),
    spec!("SWAPDB", Admin, 0),
    connection!("SYNC"),
    spec!("TIME", Read, 0),
    spec!("TOUCH", Read, 1),
    spec!("TS.ADD", Write, 1),
    spec!("TS.ALTER", Write, 1),
    spec!("TS.CREATE", Write, 1),
    spec!("TS.DECRBY", Write, 1),
    spec!("TS.DEL", Write, 1),
    spec!("TS.GET", Read, 1),
    spec!("TS.INCRBY", Write, 1),
    spec!("TS.INFO", Read, 1),
    spec!("TS.MADD", Write, 1),
    spec!("TS.MGET", Read, 0),
    spec!("TS.MRANGE", Read, 0),
    spec!("TS.MREVRANGE", Read, 0),
    spec!("TS.QUERYINDEX", Read, 0),
    spec!("TS.RANGE", Read, 1),
    spec!("TS.REVRANGE", Read, 1),
    spec!("TTL", Read, 1),
    spec!("TYPE", Read, 1),
    spec!("UNLINK", Write, 1),
    spec!("UNSUBSCRIBE", Unsupported(PUBSUB), 0),
    spec!("UNWATCH", Unsupported(TRANSACTIONS), 0),
    spec!("WAIT", Unsupported(BLOCKING), 0),
    spec!("WAITAOF", Unsupported(BLOCKING), 0),
    spec!("WATCH", Unsupported(TRANSACTIONS), 0),
    spec!("XACK", Write, 1),
    spec!("XADD", Write, 1),
    spec!("XAUTOCLAIM", Write, 1),
    spec!("XCLAIM", Write, 1),
    spec!("XDEL", Write, 1),
    spec!("XGROUP CREATE", Write, 2),
    spec!("XGROUP CREATECONSUMER", Write, 2),
    spec!("XGROUP DELCONSUMER", Write, 2),
    spec!("XGROUP DESTROY", Write, 2),
    spec!("XGROUP SETID", Write, 2),
    spec!("XINFO CONSUMERS", Read, 2),
    spec!("XINFO GROUPS", Read, 2),
    spec!("XINFO STREAM", Read, 2),
    spec!("XLEN", Read, 1),
    spec!("XPENDING", Read, 1),
    spec!("XRANGE", Read, 1),
    spec!("XREAD", Read, 0),
    spec!("XREADGROUP", Write, 0),
    spec!("XREVRANGE", Read, 1),
    spec!("XSETID", Write, 1),
    spec!("XTRIM", Write, 1),
    spec!("ZADD", Write, 1),
    spec!("ZCARD", Read, 1),
    spec!("ZCOUNT", Read, 1),
    spec!("ZDIFF", Read, 2),
    spec!("ZDIFFSTORE", Write, 1),
    spec!("ZINCRBY", Write, 1),
    spec!("ZINTER", Read, 2),
    spec!("ZINTERCARD", Read, 2),
    spec!("ZINTERSTORE", Write, 1),
    spec!("ZLEXCOUNT", Read, 1),
    spec!("ZMPOP", Write, 2),
    spec!("ZMSCORE", Read, 1),
    spec!("ZPOPMAX", Write, 1),
    spec!("ZPOPMIN", Write, 1),
    spec!("ZRANDMEMBER", Read, 1),
    spec!("ZRANGE", Read, 1),
    spec!("ZRANGEBYLEX", Read, 1),
    spec!("ZRANGEBYSCORE", Read, 1),
    spec!("ZRANGESTORE", Write, 1),
    spec!("ZRANK", Read, 1),
    spec!("ZREM", Write, 1),
    spec!("ZREMRANGEBYLEX", Write, 1),
    spec!("ZREMRANGEBYRANK", Write, 1),
    spec!("ZREMRANGEBYSCORE", Write, 1),
    spec!("ZREVRANGE", Read, 1),
    spec!("ZREVRANGEBYLEX", Read, 1),
    spec!("ZREVRANGEBYSCORE", Read, 1),
    spec!("ZREVRANK", Read, 1),
    spec!("ZSCAN", Read, 1),
    spec!("ZSCORE", Read, 1),
    spec!("ZUNION", Read, 2),
    spec!("ZUNIONSTORE", Write, 1),
];

fn words(name: &str) -> (&str, Option<&str>) {
    match name.split_once(' ') {
        Some((base, sub)) => (base, Some(sub)),
        None => (name, None),
    }
}

fn lookup(stmt: &Statement) -> (Option<&'static Spec>, bool) {
    let Some(base) = stmt.args.first() else {
        return (None, false);
    };
    let start = COMMANDS.partition_point(|spec| {
        words(spec.name)
            .0
            .bytes()
            .cmp(base.iter().map(u8::to_ascii_uppercase))
            .is_lt()
    });
    let mut is_container = false;
    for spec in &COMMANDS[start..] {
        let (name, sub) = words(spec.name);
        if !base.eq_ignore_ascii_case(name.as_bytes()) {
            break;
        }
        match sub {
            None => return (Some(spec), false),
            Some(sub) => {
                is_container = true;
                if stmt
                    .args
                    .get(1)
                    .is_some_and(|arg| arg.eq_ignore_ascii_case(sub.as_bytes()))
                {
                    return (Some(spec), true);
                }
            }
        }
    }
    (None, is_container)
}

// Redis compares option names with `strcasecmp`, unlike the binary-safe command lookup. A
// NUL byte following the option name does not prevent STORE or BLOCK from taking effect.
fn option_is(arg: &[u8], name: &[u8]) -> bool {
    let Some(prefix) = arg.get(..name.len()) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(name) && matches!(arg.get(name.len()), None | Some(0))
}

pub fn classify(stmt: &Statement) -> Kind {
    let (Some(spec), _) = lookup(stmt) else {
        return Unknown;
    };
    match spec.name {
        "SORT" | "GEORADIUS" | "GEORADIUSBYMEMBER"
            if stmt.args[1..]
                .iter()
                .any(|arg| option_is(arg, b"STORE") || option_is(arg, b"STOREDIST")) =>
        {
            Write
        }
        "XREAD" | "XREADGROUP" if stmt.args[1..].iter().any(|arg| option_is(arg, b"BLOCK")) => {
            Unsupported(BLOCKING)
        }
        _ => spec.kind,
    }
}

#[cfg(test)]
pub fn names() -> Vec<&'static str> {
    let mut names: Vec<_> = COMMANDS.iter().map(|spec| words(spec.name).0).collect();
    names.dedup();
    names
}

pub fn is_script(stmt: &Statement) -> bool {
    lookup(stmt).0.is_some_and(|spec| {
        matches!(
            spec.name,
            "EVAL" | "EVALSHA" | "EVAL_RO" | "EVALSHA_RO" | "FCALL" | "FCALL_RO"
        )
    })
}

pub fn select_database(stmt: &Statement) -> Result<&str, AppError> {
    let database = match stmt.args.as_slice() {
        [name, index] if name.eq_ignore_ascii_case(b"SELECT") => decimal(index),
        _ => None,
    };
    database
        .ok_or_else(|| AppError::invalid("SELECT needs one database number, for example SELECT 0"))
}

fn decimal(bytes: &[u8]) -> Option<&str> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    text.parse::<u32>().ok()?;
    Some(text)
}

pub(super) fn first_key(stmt: &Statement) -> Option<&[u8]> {
    let spec = lookup(stmt).0?;
    let index = match spec.name {
        "EVAL" | "EVALSHA" | "EVAL_RO" | "EVALSHA_RO" | "FCALL" | "FCALL_RO" => {
            if decimal(stmt.args.get(2)?)?.bytes().all(|b| b == b'0') {
                return None;
            }
            spec.first_key.into()
        }
        "XREAD" | "XREADGROUP" => stream_key_index(stmt)?,
        _ => usize::from(spec.first_key),
    };
    if index == 0 {
        return None;
    }
    stmt.args.get(index).map(Vec::as_slice)
}

fn stream_key_index(stmt: &Statement) -> Option<usize> {
    let mut args = stmt.args.iter().enumerate().skip(1);
    while let Some((i, option)) = args.next() {
        if option_is(option, b"STREAMS") {
            return Some(i + 1);
        }
        if option_is(option, b"GROUP") {
            args.nth(1)?;
        } else if option_is(option, b"COUNT") || option_is(option, b"BLOCK") {
            args.next()?;
        }
    }
    None
}

pub(super) fn known_name(stmt: &Statement) -> Option<&'static str> {
    lookup(stmt).0.map(|spec| spec.name)
}

pub(super) fn name(stmt: &Statement) -> String {
    let (spec, is_container) = lookup(stmt);
    if let Some(spec) = spec {
        return spec.name.to_owned();
    }
    let mut args = stmt
        .args
        .iter()
        .map(|arg| quote_arg(&arg.to_ascii_uppercase()));
    let mut name = args.next().unwrap_or_default();
    if is_container && let Some(sub) = args.next() {
        name.push(' ');
        name.push_str(&sub);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::redis::parse::parse;

    fn statement(text: &str) -> Statement {
        parse(text).unwrap().remove(0)
    }

    #[test]
    fn every_spec_matches_and_autocomplete_names_are_sorted_and_unique() {
        assert!(COMMANDS.windows(2).all(|pair| pair[0].name < pair[1].name));
        for spec in COMMANDS {
            let input = spec.name.to_ascii_lowercase();
            assert_eq!(classify(&statement(&input)), spec.kind, "{input}");
        }
        let names = names();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        for name in ["CONFIG", "EVAL_RO", "XREADGROUP", "JSON.GET"] {
            assert!(names.contains(&name));
        }
    }

    #[test]
    fn containers_do_not_inherit_kinds() {
        for text in [
            "ACL",
            "ACL FUTURE",
            "CLIENT FUTURE",
            "CLUSTER FUTURE",
            "COMMAND FUTURE",
            "CONFIG FUTURE",
            "SCRIPT FUTURE",
            "XGROUP FUTURE",
            r#""CONFIG GET""#,
        ] {
            assert_eq!(classify(&statement(text)), Unknown, "{text}");
        }
    }

    #[test]
    fn options_change_the_kind_even_with_binary_suffixes() {
        for name in ["SORT", "GEORADIUS", "GEORADIUSBYMEMBER"] {
            assert_eq!(classify(&statement(&format!("{name} k"))), Read);
            for option in ["StOrE", "sToReDiSt", r#""STORE\x00tail""#] {
                assert_eq!(
                    classify(&statement(&format!("{name} k {option} dest"))),
                    Write
                );
            }
        }
        for name in ["XREAD", "XREADGROUP"] {
            for option in ["bLoCk", r#""BLOCK\x00tail""#] {
                assert_eq!(
                    classify(&statement(&format!("{name} {option} 0 STREAMS k 0"))),
                    Unsupported(BLOCKING)
                );
            }
        }
        assert_eq!(classify(&statement("XREAD STREAMS k 0")), Read);
        assert_eq!(
            classify(&statement("XREADGROUP GROUP g c STREAMS k >")),
            Write
        );
    }

    #[test]
    fn script_kind_and_cancellation_are_separate() {
        for name in ["eval", "evalsha", "fcall"] {
            let write = statement(name);
            assert!(is_script(&write));
            assert_eq!(classify(&write), Script);
            let read = statement(&format!("{name}_ro"));
            assert!(is_script(&read));
            assert_eq!(classify(&read), Read);
        }
        assert!(!is_script(&statement("SCRIPT LOAD 'return 1'")));
    }

    #[test]
    fn select_accepts_exactly_one_nonnegative_decimal_u32() {
        assert_eq!(select_database(&statement("select 0")).unwrap(), "0");
        assert_eq!(
            select_database(&statement("SELECT \"001\"")).unwrap(),
            "001"
        );
        for text in [
            "SELECT",
            "SELECT 1 2",
            "SELECT -1",
            "SELECT +1",
            "SELECT 1.0",
            "SELECT 4294967296",
            "SELECT ''",
            r#"SELECT "\x003""#,
        ] {
            assert!(select_database(&statement(text)).is_err(), "{text}");
        }
    }

    #[test]
    fn first_key_skips_command_specific_nonkey_arguments() {
        for text in [
            "DEL key other",
            "BITOP AND key a b",
            "XGROUP CREATE key g 0",
            "XREADGROUP COUNT 5 GROUP STREAMS c STREAMS key >",
            "EVAL 'return 1' 1 key",
            "LMPOP 2 key other LEFT",
        ] {
            assert_eq!(
                first_key(&statement(text)),
                Some(b"key".as_slice()),
                "{text}"
            );
        }
        for text in [
            "EVAL 'return 1' 0 notakey",
            "PUBLISH channel value",
            "CONFIG SET key value",
        ] {
            assert_eq!(first_key(&statement(text)), None, "{text}");
        }
    }
}
