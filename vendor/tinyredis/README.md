# tinyredis

A Redis-compatible server written in Rust. It uses RESP2, persists writes to an append-only file, and accepts connections from any standard Redis client.

## Building

```
cargo build --release
```

Requires Rust 2024 edition (stable ≥ 1.85).

## Running

```
tinyredis                        # defaults
tinyredis /path/to/tinyredis.conf
```

## Configuration

All directives are optional. Unknown directives are silently ignored.

```
bind 127.0.0.1
port 6379

appendonly yes
appendfilename "tinyredis.aof"
appendfsync everysec     # always | everysec | no

maxmemory 0              # bytes; 0 = no limit; supports kb/mb/gb suffixes
maxmemory-policy noeviction

requirepass ""           # empty = no authentication
```

`appendfsync` controls how often the AOF file is flushed to disk:

| value | behaviour |
|---|---|
| `always` | sync after every write |
| `everysec` | sync once per second in the background (default) |
| `no` | let the OS decide |

`maxmemory-policy` controls what happens when used memory exceeds `maxmemory`:

| policy | behaviour |
|---|---|
| `noeviction` | return an error on writes (default) |
| `allkeys-lru` | evict any key by approximate LRU |
| `volatile-lru` | evict a TTL key by approximate LRU |
| `allkeys-random` | evict any key at random |
| `volatile-random` | evict a random TTL key |
| `volatile-ttl` | evict the TTL key with the shortest remaining lifetime |

## Supported commands

### Strings
`SET` `GET` `DEL` `EXISTS` `TYPE` `EXPIRE` `EXPIREAT` `PEXPIRE` `PEXPIREAT` `TTL` `PTTL` `PERSIST` `STRLEN` `APPEND` `GETDEL` `GETSET` `INCR` `INCRBY` `DECR` `DECRBY` `INCRBYFLOAT` `MGET` `MSET`

`SET` supports `EX`, `PX`, `EXAT`, `PXAT`, `NX`, `XX`, `GET`, `KEEPTTL`.

### Hashes
`HSET` `HMSET` `HGET` `HMGET` `HDEL` `HEXISTS` `HLEN` `HKEYS` `HVALS` `HGETALL` `HINCRBY` `HINCRBYFLOAT` `HSETNX`

### Lists
`LPUSH` `RPUSH` `LPOP` `RPOP` `LLEN` `LRANGE` `LINDEX` `LSET` `LINSERT` `LREM` `LTRIM` `LMOVE`

### Sets
`SADD` `SREM` `SISMEMBER` `SMISMEMBER` `SMEMBERS` `SCARD` `SRANDMEMBER` `SPOP` `SUNION` `SINTER` `SDIFF` `SUNIONSTORE` `SINTERSTORE` `SDIFFSTORE` `SMOVE`

### Sorted sets
`ZADD` `ZREM` `ZSCORE` `ZINCRBY` `ZRANK` `ZREVRANK` `ZCARD` `ZCOUNT` `ZRANGE` `ZRANGEBYSCORE` `ZREVRANGEBYSCORE` `ZRANGEBYLEX` `ZPOPMIN` `ZPOPMAX` `ZRANDMEMBER` `ZUNIONSTORE` `ZINTERSTORE`

`ZADD` supports `NX`, `XX`, `GT`, `LT`, `CH`, `INCR`. `ZRANGE` supports the unified `BYSCORE`/`BYLEX`/`REV`/`LIMIT` syntax from Redis 6.2.

### Keyspace
`KEYS` `SCAN` `DBSIZE` `RENAME` `FLUSHDB` `FLUSHALL`

### Transactions
`MULTI` `EXEC` `DISCARD`

Commands queued inside `MULTI` are executed atomically on `EXEC`. Per-command errors do not abort the transaction.

### Connection
`PING` `ECHO` `QUIT` `AUTH` `CLIENT ID` `CLIENT GETNAME` `CLIENT SETNAME` `CLIENT LIST`

### Server
`INFO` `BGREWRITEAOF`

`BGREWRITEAOF` rewrites the AOF to a compact snapshot of the current keyspace, then atomically replaces the existing file.

## Persistence

On startup, tinyredis replays the AOF file to restore state. Every mutating command is appended to the file in RESP2 format. Commands with redundant representations are canonicalized before logging (e.g. `ZINCRBY` is logged as `ZADD`, `ZPOPMIN`/`ZPOPMAX` as `ZREM`).

`BGREWRITEAOF` compacts the AOF by serializing the live keyspace into a minimal set of commands (`SET`, `RPUSH`, `HSET`, `SADD`, `ZADD`, with `PEXPIREAT` where applicable), writing to a temporary file, then renaming it into place.

## Testing

```
cargo test
```

Integration tests use the `redis` crate to connect to a live server instance spun up per test. AOF is disabled in tests.

## Architecture

```
src/
  main.rs          entry point, config loading, AOF setup
  server.rs        accept loop, per-connection handler, MULTI/CLIENT state
  connection.rs    buffered RESP2 reader/writer
  parser.rs        RESP2 frame parsing and serialization
  store.rs         in-memory key-value store, eviction, snapshot
  persistence.rs   AOF writer, fsync policy, BGREWRITEAOF
  config.rs        config file parser
  stats.rs         server stats, client registry
  commands/
    core.rs        SET, GET, DEL, EXPIRE, TTL, TYPE, ...
    extra.rs       INCR, APPEND, MSET, GETDEL, ...
    hash.rs        H* commands
    list.rs        L* commands
    set.rs         S* commands
    zset.rs        Z* commands
    keyspace.rs    KEYS, SCAN, RENAME, FLUSH*, BGREWRITEAOF
    info.rs        INFO
```

Values are stored as `Bytes` (zero-copy clones). Lists use `VecDeque`, sorted sets use a dual-index of `HashMap<String, f64>` and `BTreeMap<(OrderedFloat<f64>, String), ()>` for O(log N) insert and O(N) range queries. The LRU clock is a separate `HashMap<String, u64>` to avoid touching the entry struct.
