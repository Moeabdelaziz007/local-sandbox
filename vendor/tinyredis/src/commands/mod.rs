use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::Error;
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::stats::SharedStats;
use crate::store::Store;

mod core;
mod extra;
pub mod hash;
mod info;
mod keyspace;
pub mod list;
mod script;
pub mod set;
pub mod utils;
pub mod zset;

/// Commands that add data and must trigger eviction checks.
const WRITE_CMDS: &[&str] = &[
    "SET",
    "MSET",
    "MSETNX",
    "GETSET",
    "APPEND",
    "INCR",
    "INCRBY",
    "DECR",
    "DECRBY",
    "INCRBYFLOAT",
    "LPUSH",
    "RPUSH",
    "LINSERT",
    "LSET",
    "SADD",
    "SUNIONSTORE",
    "SINTERSTORE",
    "SDIFFSTORE",
    "ZADD",
    "ZINCRBY",
    "ZUNIONSTORE",
    "ZINTERSTORE",
    "HSET",
    "HMSET",
    "HSETNX",
    "HINCRBY",
    "HINCRBYFLOAT",
];

pub async fn dispatch(
    cmd: Command,
    store: Arc<Mutex<Store>>,
    aof: Option<AofSender>,
    stats: SharedStats,
) -> Frame {
    // Evict before write commands when maxmemory is configured.
    if stats.maxmemory > 0
        && WRITE_CMDS.contains(&cmd.name.as_str())
        && let Err(e) = store.lock().await.evict_if_needed()
    {
        return Frame::Error(format!("ERR {e}"));
    }

    let result = match cmd.name.as_str() {
        "PING" => core::ping(&cmd),
        "ECHO" => core::echo(&cmd),
        "SET" => core::set(&cmd, &store, &aof).await,
        "SETEX" => core::setex(&cmd, &store, &aof).await,
        "GET" => core::get(&cmd, &store).await,
        "DEL" => core::del(&cmd, &store, &aof).await,
        "EXISTS" => core::exists(&cmd, &store).await,
        "EXPIRE" => core::expire(&cmd, &store, &aof).await,
        "EXPIREAT" => core::expireat(&cmd, &store, &aof).await,
        "PEXPIRE" => core::pexpire(&cmd, &store, &aof).await,
        "PEXPIREAT" => core::pexpireat(&cmd, &store, &aof).await,
        "TTL" => core::ttl(&cmd, &store).await,
        "PTTL" => core::pttl(&cmd, &store).await,
        "PERSIST" => core::persist(&cmd, &store, &aof).await,
        "TYPE" => core::type_cmd(&cmd, &store).await,
        "STRLEN" => extra::strlen(&cmd, &store).await,
        "GETDEL" => extra::getdel(&cmd, &store, &aof).await,
        "GETSET" => extra::getset(&cmd, &store, &aof).await,
        "APPEND" => extra::append(&cmd, &store, &aof).await,
        "INCR" => extra::incr(&cmd, &store, &aof).await,
        "INCRBY" => extra::incrby(&cmd, &store, &aof).await,
        "DECR" => extra::decr(&cmd, &store, &aof).await,
        "DECRBY" => extra::decrby(&cmd, &store, &aof).await,
        "INCRBYFLOAT" => extra::incrbyfloat(&cmd, &store, &aof).await,
        "MGET" => extra::mget(&cmd, &store).await,
        "MSET" => extra::mset(&cmd, &store, &aof).await,
        "KEYS" => keyspace::keys(&cmd, &store).await,
        "SCAN" => keyspace::scan(&cmd, &store).await,
        "DBSIZE" => keyspace::dbsize(&store).await,
        "RENAME" => keyspace::rename(&cmd, &store, &aof).await,
        "FLUSHDB" => keyspace::flushdb(&store, &aof).await,
        "FLUSHALL" => keyspace::flushall(&store, &aof).await,
        "INFO" => info::info(&cmd, &store, &stats).await,
        "EVAL" => script::eval(&cmd, &store, &aof).await,
        "EVALSHA" => script::evalsha(&cmd, &store, &aof).await,
        "SCRIPT" => script::script(&cmd, &store).await,
        "BGREWRITEAOF" => keyspace::bgrewriteaof(&stats).await,
        "HSET" => hash::hset(&cmd, &store, &aof).await,
        "HMSET" => hash::hmset(&cmd, &store, &aof).await,
        "HGET" => hash::hget(&cmd, &store).await,
        "HMGET" => hash::hmget(&cmd, &store).await,
        "HDEL" => hash::hdel(&cmd, &store, &aof).await,
        "HEXISTS" => hash::hexists(&cmd, &store).await,
        "HLEN" => hash::hlen(&cmd, &store).await,
        "HKEYS" => hash::hkeys(&cmd, &store).await,
        "HVALS" => hash::hvals(&cmd, &store).await,
        "HGETALL" => hash::hgetall(&cmd, &store).await,
        "HINCRBY" => hash::hincrby(&cmd, &store, &aof).await,
        "HINCRBYFLOAT" => hash::hincrbyfloat(&cmd, &store, &aof).await,
        "HSETNX" => hash::hsetnx(&cmd, &store, &aof).await,
        "LPUSH" => list::lpush(&cmd, &store, &aof).await,
        "RPUSH" => list::rpush(&cmd, &store, &aof).await,
        "LPOP" => list::lpop(&cmd, &store, &aof).await,
        "RPOP" => list::rpop(&cmd, &store, &aof).await,
        "LLEN" => list::llen(&cmd, &store).await,
        "LRANGE" => list::lrange(&cmd, &store).await,
        "LINDEX" => list::lindex(&cmd, &store).await,
        "LSET" => list::lset(&cmd, &store, &aof).await,
        "LINSERT" => list::linsert(&cmd, &store, &aof).await,
        "LREM" => list::lrem(&cmd, &store, &aof).await,
        "LTRIM" => list::ltrim(&cmd, &store, &aof).await,
        "LMOVE" => list::lmove(&cmd, &store, &aof).await,
        "SADD" => set::sadd(&cmd, &store, &aof).await,
        "SREM" => set::srem(&cmd, &store, &aof).await,
        "SISMEMBER" => set::sismember(&cmd, &store).await,
        "SMISMEMBER" => set::smismember(&cmd, &store).await,
        "SMEMBERS" => set::smembers(&cmd, &store).await,
        "SCARD" => set::scard(&cmd, &store).await,
        "SRANDMEMBER" => set::srandmember(&cmd, &store).await,
        "SPOP" => set::spop(&cmd, &store, &aof).await,
        "SUNION" => set::sunion(&cmd, &store).await,
        "SINTER" => set::sinter(&cmd, &store).await,
        "SDIFF" => set::sdiff(&cmd, &store).await,
        "SUNIONSTORE" => set::sunionstore(&cmd, &store, &aof).await,
        "SINTERSTORE" => set::sinterstore(&cmd, &store, &aof).await,
        "SDIFFSTORE" => set::sdiffstore(&cmd, &store, &aof).await,
        "SMOVE" => set::smove(&cmd, &store, &aof).await,
        "ZADD" => zset::zadd(&cmd, &store, &aof).await,
        "ZREM" => zset::zrem(&cmd, &store, &aof).await,
        "ZREMRANGEBYRANK" => zset::zremrangebyrank(&cmd, &store, &aof).await,
        "ZSCORE" => zset::zscore(&cmd, &store).await,
        "ZINCRBY" => zset::zincrby(&cmd, &store, &aof).await,
        "ZRANK" => zset::zrank(&cmd, &store).await,
        "ZREVRANK" => zset::zrevrank(&cmd, &store).await,
        "ZCARD" => zset::zcard(&cmd, &store).await,
        "ZCOUNT" => zset::zcount(&cmd, &store).await,
        "ZRANGE" => zset::zrange(&cmd, &store).await,
        "ZRANGEBYSCORE" => zset::zrangebyscore(&cmd, &store).await,
        "ZREVRANGEBYSCORE" => zset::zrevrangebyscore(&cmd, &store).await,
        "ZRANGEBYLEX" => zset::zrangebylex(&cmd, &store).await,
        "ZPOPMIN" => zset::zpopmin(&cmd, &store, &aof).await,
        "ZPOPMAX" => zset::zpopmax(&cmd, &store, &aof).await,
        "ZRANDMEMBER" => zset::zrandmember(&cmd, &store).await,
        "ZUNIONSTORE" => zset::zunionstore(&cmd, &store, &aof).await,
        "ZINTERSTORE" => zset::zinterstore(&cmd, &store, &aof).await,
        _ => Err(Error::Protocol(format!("unknown command '{}'", cmd.name))),
    };

    result.unwrap_or_else(|e| Frame::Error(format!("ERR {e}")))
}
