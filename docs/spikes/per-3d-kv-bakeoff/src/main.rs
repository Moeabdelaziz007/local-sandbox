use anyhow::{anyhow, Context, Result};
use redis::{Commands, Connection};
use std::{
    collections::{BTreeSet, HashMap},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tinyredis::{
    config::Config as TinyConfig, server as tiny_server, stats::ServerStats, store::Store,
};
use tokio::{
    net::TcpStream,
    process::{Child, Command},
    sync::{broadcast, Mutex},
    task::JoinHandle,
    time::timeout,
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut all_rows = Vec::new();

    for candidate in candidates_from_env() {
        println!("candidate={} starting", candidate.name());
        let server = match RunningServer::start(candidate).await {
            Ok(server) => server,
            Err(error) => {
                all_rows.push(Row::fail(
                    candidate.name(),
                    "lifecycle",
                    "start",
                    error.context("candidate failed to start"),
                ));
                continue;
            }
        };
        let url = server.url();
        println!("candidate={} endpoint={url}", candidate.name());

        let mut rows = run_matrix(candidate.name(), &url)?;
        all_rows.append(&mut rows);

        let stopped = server.shutdown().await;
        println!(
            "candidate={} shutdown={}",
            candidate.name(),
            match &stopped {
                Ok(()) => "clean".to_string(),
                Err(error) => format!("failed:{error:#}"),
            }
        );
        match stopped {
            Ok(()) => all_rows.push(Row::pass(
                candidate.name(),
                "lifecycle",
                "shutdown",
                "endpoint stopped cleanly".to_string(),
            )),
            Err(error) => {
                all_rows.push(Row::fail(candidate.name(), "lifecycle", "shutdown", error))
            }
        }
    }

    println!("candidate\tfamily\tcommand\tstatus\tdetail");
    for row in all_rows {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            row.candidate, row.family, row.command, row.status, row.detail
        );
    }

    Ok(())
}

fn candidates_from_env() -> Vec<Candidate> {
    let selected = std::env::var("KV_BAKEOFF_CANDIDATES").unwrap_or_else(|_| "all".to_string());
    selected
        .split(',')
        .filter_map(|name| match name.trim().to_ascii_lowercase().as_str() {
            "all" => Some(vec![Candidate::TinyRedis, Candidate::FeoxServer]),
            "tinyredis" | "tiny" | "tinyredis-1.0.0" => Some(vec![Candidate::TinyRedis]),
            "feox" | "feox-server" | "feox-server-0.7.0" => Some(vec![Candidate::FeoxServer]),
            "" => Some(vec![]),
            unknown => {
                eprintln!("ignoring unknown KV_BAKEOFF_CANDIDATES entry {unknown:?}");
                None
            }
        })
        .flatten()
        .collect()
}

fn run_matrix(candidate: &'static str, url: &str) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    check(
        candidate,
        &mut rows,
        url,
        "readiness",
        "PING",
        |connection| {
            let mut connection = connection;
            let response: String = redis::cmd("PING").query(&mut connection)?;
            ensure_eq(response, "PONG", "PING response")
        },
    );

    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "SET",
        |connection| {
            let mut connection = connection;
            let response: String = redis::cmd("SET")
                .arg(key(candidate, "string:set"))
                .arg("value")
                .query(&mut connection)?;
            ensure_eq(response, "OK", "SET response")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "GET",
        |connection| {
            let value: String = connection.get(key(candidate, "string:set"))?;
            ensure_eq(value, "value", "GET value")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "MGET",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:mget:a"))
                .arg("a")
                .query::<String>(&mut connection)?;
            redis::cmd("SET")
                .arg(key(candidate, "string:mget:b"))
                .arg("b")
                .query::<String>(&mut connection)?;
            let values: Vec<Option<String>> = redis::cmd("MGET")
                .arg(key(candidate, "string:mget:a"))
                .arg(key(candidate, "string:mget:b"))
                .arg(key(candidate, "string:mget:missing"))
                .query(&mut connection)?;
            ensure_eq(
                values,
                vec![Some("a".into()), Some("b".into()), None],
                "MGET values",
            )
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "DEL",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:del"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let removed: i64 = redis::cmd("DEL")
                .arg(key(candidate, "string:del"))
                .query(&mut connection)?;
            ensure_eq(removed, 1, "DEL count")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "EXISTS",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:exists"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let count: i64 = redis::cmd("EXISTS")
                .arg(key(candidate, "string:exists"))
                .arg(key(candidate, "string:exists:missing"))
                .query(&mut connection)?;
            ensure_eq(count, 1, "EXISTS count")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "SETEX",
        |connection| {
            let mut connection = connection;
            let response: String = redis::cmd("SETEX")
                .arg(key(candidate, "string:setex"))
                .arg(30)
                .arg("value")
                .query(&mut connection)?;
            ensure_eq(response, "OK", "SETEX response")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "SET NX EX",
        |connection| {
            let mut connection = connection;
            let first: Option<String> = redis::cmd("SET")
                .arg(key(candidate, "string:nxex"))
                .arg("first")
                .arg("NX")
                .arg("EX")
                .arg(30)
                .query(&mut connection)?;
            let second: Option<String> = redis::cmd("SET")
                .arg(key(candidate, "string:nxex"))
                .arg("second")
                .arg("NX")
                .arg("EX")
                .arg(30)
                .query(&mut connection)?;
            ensure_eq(
                (first, second),
                (Some("OK".into()), None),
                "SET NX EX tuple",
            )
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "SET NX PX",
        |connection| {
            let mut connection = connection;
            let first: Option<String> = redis::cmd("SET")
                .arg(key(candidate, "string:nxpx"))
                .arg("first")
                .arg("NX")
                .arg("PX")
                .arg(30_000)
                .query(&mut connection)?;
            ensure_eq(first, Some("OK".into()), "SET NX PX response")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "TTL -2/-1/N",
        |connection| {
            let mut connection = connection;
            let missing: i64 = redis::cmd("TTL")
                .arg(key(candidate, "string:ttl:missing"))
                .query(&mut connection)?;
            redis::cmd("SET")
                .arg(key(candidate, "string:ttl:forever"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let forever: i64 = redis::cmd("TTL")
                .arg(key(candidate, "string:ttl:forever"))
                .query(&mut connection)?;
            redis::cmd("SETEX")
                .arg(key(candidate, "string:ttl:expiring"))
                .arg(30)
                .arg("value")
                .query::<String>(&mut connection)?;
            let expiring: i64 = redis::cmd("TTL")
                .arg(key(candidate, "string:ttl:expiring"))
                .query(&mut connection)?;
            anyhow::ensure!(
                missing == -2 && forever == -1 && expiring > 0,
                "TTL values were {missing}/{forever}/{expiring}"
            );
            Ok(format!("TTL={missing}/{forever}/{expiring}"))
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "EXPIRE NX",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:expire-nx"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let first: i64 = redis::cmd("EXPIRE")
                .arg(key(candidate, "string:expire-nx"))
                .arg(30)
                .arg("NX")
                .query(&mut connection)?;
            let second: i64 = redis::cmd("EXPIRE")
                .arg(key(candidate, "string:expire-nx"))
                .arg(60)
                .arg("NX")
                .query(&mut connection)?;
            ensure_eq((first, second), (1, 0), "EXPIRE NX results")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "EXPIREAT",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:expireat"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let changed: i64 = redis::cmd("EXPIREAT")
                .arg(key(candidate, "string:expireat"))
                .arg(epoch_seconds_plus(60))
                .query(&mut connection)?;
            ensure_eq(changed, 1, "EXPIREAT result")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "SET EXAT",
        |connection| {
            let mut connection = connection;
            let response: String = redis::cmd("SET")
                .arg(key(candidate, "string:exat"))
                .arg("value")
                .arg("EXAT")
                .arg(epoch_seconds_plus(60))
                .query(&mut connection)?;
            ensure_eq(response, "OK", "SET EXAT response")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "strings/expiry",
        "TYPE",
        |connection| {
            let mut connection = connection;
            redis::cmd("SET")
                .arg(key(candidate, "string:type"))
                .arg("value")
                .query::<String>(&mut connection)?;
            let key_type: String = redis::cmd("TYPE")
                .arg(key(candidate, "string:type"))
                .query(&mut connection)?;
            ensure_eq(key_type, "string", "TYPE result")
        },
    );

    check(candidate, &mut rows, url, "hashes", "HSET", |connection| {
        let mut connection = connection;
        let count: i64 = redis::cmd("HSET")
            .arg(key(candidate, "hash:hset"))
            .arg("field")
            .arg("value")
            .query(&mut connection)?;
        ensure_eq(count, 1, "HSET count")
    });
    check(candidate, &mut rows, url, "hashes", "HMSET", |connection| {
        let mut connection = connection;
        let response: String = redis::cmd("HMSET")
            .arg(key(candidate, "hash:hmset"))
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .query(&mut connection)?;
        ensure_eq(response, "OK", "HMSET response")
    });
    check(candidate, &mut rows, url, "hashes", "HGET", |connection| {
        let mut connection = connection;
        redis::cmd("HSET")
            .arg(key(candidate, "hash:hget"))
            .arg("field")
            .arg("value")
            .query::<i64>(&mut connection)?;
        let value: String = redis::cmd("HGET")
            .arg(key(candidate, "hash:hget"))
            .arg("field")
            .query(&mut connection)?;
        ensure_eq(value, "value", "HGET value")
    });
    check(candidate, &mut rows, url, "hashes", "HMGET", |connection| {
        let mut connection = connection;
        redis::cmd("HMSET")
            .arg(key(candidate, "hash:hmget"))
            .arg("a")
            .arg("1")
            .arg("b")
            .arg("2")
            .query::<String>(&mut connection)?;
        let values: Vec<Option<String>> = redis::cmd("HMGET")
            .arg(key(candidate, "hash:hmget"))
            .arg("a")
            .arg("b")
            .arg("missing")
            .query(&mut connection)?;
        ensure_eq(
            values,
            vec![Some("1".into()), Some("2".into()), None],
            "HMGET values",
        )
    });
    check(
        candidate,
        &mut rows,
        url,
        "hashes",
        "HGETALL",
        |connection| {
            let mut connection = connection;
            redis::cmd("HMSET")
                .arg(key(candidate, "hash:hgetall"))
                .arg("a")
                .arg("1")
                .arg("b")
                .arg("2")
                .query::<String>(&mut connection)?;
            let values: HashMap<String, String> = redis::cmd("HGETALL")
                .arg(key(candidate, "hash:hgetall"))
                .query(&mut connection)?;
            ensure_eq(values.len(), 2, "HGETALL field count")
        },
    );
    check(candidate, &mut rows, url, "hashes", "HDEL", |connection| {
        let mut connection = connection;
        redis::cmd("HSET")
            .arg(key(candidate, "hash:hdel"))
            .arg("field")
            .arg("value")
            .query::<i64>(&mut connection)?;
        let count: i64 = redis::cmd("HDEL")
            .arg(key(candidate, "hash:hdel"))
            .arg("field")
            .query(&mut connection)?;
        ensure_eq(count, 1, "HDEL count")
    });

    check(candidate, &mut rows, url, "lists", "LPUSH", |connection| {
        let mut connection = connection;
        let len: i64 = redis::cmd("LPUSH")
            .arg(key(candidate, "list:lpush"))
            .arg("a")
            .arg("b")
            .query(&mut connection)?;
        ensure_eq(len, 2, "LPUSH length")
    });
    check(candidate, &mut rows, url, "lists", "RPUSH", |connection| {
        let mut connection = connection;
        let len: i64 = redis::cmd("RPUSH")
            .arg(key(candidate, "list:rpush"))
            .arg("a")
            .arg("b")
            .query(&mut connection)?;
        ensure_eq(len, 2, "RPUSH length")
    });
    check(candidate, &mut rows, url, "lists", "LLEN", |connection| {
        let mut connection = connection;
        redis::cmd("RPUSH")
            .arg(key(candidate, "list:llen"))
            .arg("a")
            .arg("b")
            .query::<i64>(&mut connection)?;
        let len: i64 = redis::cmd("LLEN")
            .arg(key(candidate, "list:llen"))
            .query(&mut connection)?;
        ensure_eq(len, 2, "LLEN")
    });
    check(candidate, &mut rows, url, "lists", "LRANGE", |connection| {
        let mut connection = connection;
        redis::cmd("RPUSH")
            .arg(key(candidate, "list:lrange"))
            .arg("a")
            .arg("b")
            .query::<i64>(&mut connection)?;
        let values: Vec<String> = redis::cmd("LRANGE")
            .arg(key(candidate, "list:lrange"))
            .arg(0)
            .arg(-1)
            .query(&mut connection)?;
        ensure_eq(
            values,
            vec!["a".to_string(), "b".to_string()],
            "LRANGE values",
        )
    });
    check(candidate, &mut rows, url, "lists", "RPOP", |connection| {
        let mut connection = connection;
        redis::cmd("RPUSH")
            .arg(key(candidate, "list:rpop"))
            .arg("a")
            .arg("b")
            .query::<i64>(&mut connection)?;
        let value: String = redis::cmd("RPOP")
            .arg(key(candidate, "list:rpop"))
            .query(&mut connection)?;
        ensure_eq(value, "b", "RPOP value")
    });
    check(candidate, &mut rows, url, "lists", "LREM", |connection| {
        let mut connection = connection;
        redis::cmd("RPUSH")
            .arg(key(candidate, "list:lrem"))
            .arg("a")
            .arg("b")
            .arg("a")
            .query::<i64>(&mut connection)?;
        let count: i64 = redis::cmd("LREM")
            .arg(key(candidate, "list:lrem"))
            .arg(0)
            .arg("a")
            .query(&mut connection)?;
        ensure_eq(count, 2, "LREM count")
    });
    check(candidate, &mut rows, url, "lists", "LTRIM", |connection| {
        let mut connection = connection;
        redis::cmd("RPUSH")
            .arg(key(candidate, "list:ltrim"))
            .arg("a")
            .arg("b")
            .arg("c")
            .query::<i64>(&mut connection)?;
        redis::cmd("LTRIM")
            .arg(key(candidate, "list:ltrim"))
            .arg(1)
            .arg(2)
            .query::<String>(&mut connection)?;
        let values: Vec<String> = redis::cmd("LRANGE")
            .arg(key(candidate, "list:ltrim"))
            .arg(0)
            .arg(-1)
            .query(&mut connection)?;
        ensure_eq(
            values,
            vec!["b".to_string(), "c".to_string()],
            "LTRIM values",
        )
    });
    check(
        candidate,
        &mut rows,
        url,
        "lists",
        "concurrent RPOP uniqueness",
        |_connection| concurrent_rpop_unique(candidate, url),
    );

    check(
        candidate,
        &mut rows,
        url,
        "sets",
        "SADD duplicate count",
        |connection| {
            let mut connection = connection;
            let first: i64 = redis::cmd("SADD")
                .arg(key(candidate, "set:sadd"))
                .arg("a")
                .arg("a")
                .arg("b")
                .query(&mut connection)?;
            ensure_eq(first, 2, "SADD duplicate count")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "sets",
        "SMEMBERS",
        |connection| {
            let mut connection = connection;
            redis::cmd("SADD")
                .arg(key(candidate, "set:smembers"))
                .arg("a")
                .arg("b")
                .query::<i64>(&mut connection)?;
            let values: BTreeSet<String> = redis::cmd("SMEMBERS")
                .arg(key(candidate, "set:smembers"))
                .query::<Vec<String>>(&mut connection)?
                .into_iter()
                .collect();
            ensure_eq(
                values,
                BTreeSet::from(["a".to_string(), "b".to_string()]),
                "SMEMBERS values",
            )
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "sets",
        "SISMEMBER",
        |connection| {
            let mut connection = connection;
            redis::cmd("SADD")
                .arg(key(candidate, "set:sismember"))
                .arg("a")
                .query::<i64>(&mut connection)?;
            let exists: i64 = redis::cmd("SISMEMBER")
                .arg(key(candidate, "set:sismember"))
                .arg("a")
                .query(&mut connection)?;
            ensure_eq(exists, 1, "SISMEMBER result")
        },
    );
    check(candidate, &mut rows, url, "sets", "SREM", |connection| {
        let mut connection = connection;
        redis::cmd("SADD")
            .arg(key(candidate, "set:srem"))
            .arg("a")
            .query::<i64>(&mut connection)?;
        let removed: i64 = redis::cmd("SREM")
            .arg(key(candidate, "set:srem"))
            .arg("a")
            .query(&mut connection)?;
        ensure_eq(removed, 1, "SREM count")
    });

    check(candidate, &mut rows, url, "zsets", "ZADD", |connection| {
        let mut connection = connection;
        let added: i64 = redis::cmd("ZADD")
            .arg(key(candidate, "zset:zadd"))
            .arg(1)
            .arg("a")
            .query(&mut connection)?;
        ensure_eq(added, 1, "ZADD count")
    });
    check(candidate, &mut rows, url, "zsets", "ZSCORE", |connection| {
        let mut connection = connection;
        redis::cmd("ZADD")
            .arg(key(candidate, "zset:zscore"))
            .arg(2)
            .arg("a")
            .query::<i64>(&mut connection)?;
        let score: String = redis::cmd("ZSCORE")
            .arg(key(candidate, "zset:zscore"))
            .arg("a")
            .query(&mut connection)?;
        ensure_eq(score, "2", "ZSCORE value")
    });
    check(
        candidate,
        &mut rows,
        url,
        "zsets",
        "ZINCRBY",
        |connection| {
            let mut connection = connection;
            redis::cmd("ZADD")
                .arg(key(candidate, "zset:zincrby"))
                .arg(2)
                .arg("a")
                .query::<i64>(&mut connection)?;
            let score: String = redis::cmd("ZINCRBY")
                .arg(key(candidate, "zset:zincrby"))
                .arg(3)
                .arg("a")
                .query(&mut connection)?;
            ensure_eq(score, "5", "ZINCRBY value")
        },
    );
    check(candidate, &mut rows, url, "zsets", "ZCARD", |connection| {
        let mut connection = connection;
        redis::cmd("ZADD")
            .arg(key(candidate, "zset:zcard"))
            .arg(1)
            .arg("a")
            .arg(2)
            .arg("b")
            .query::<i64>(&mut connection)?;
        let count: i64 = redis::cmd("ZCARD")
            .arg(key(candidate, "zset:zcard"))
            .query(&mut connection)?;
        ensure_eq(count, 2, "ZCARD count")
    });
    check(candidate, &mut rows, url, "zsets", "ZRANGE", |connection| {
        let mut connection = connection;
        redis::cmd("ZADD")
            .arg(key(candidate, "zset:zrange"))
            .arg(2)
            .arg("b")
            .arg(1)
            .arg("a")
            .query::<i64>(&mut connection)?;
        let values: Vec<String> = redis::cmd("ZRANGE")
            .arg(key(candidate, "zset:zrange"))
            .arg(0)
            .arg(-1)
            .query(&mut connection)?;
        ensure_eq(
            values,
            vec!["a".to_string(), "b".to_string()],
            "ZRANGE values",
        )
    });
    check(candidate, &mut rows, url, "zsets", "ZREM", |connection| {
        let mut connection = connection;
        redis::cmd("ZADD")
            .arg(key(candidate, "zset:zrem"))
            .arg(1)
            .arg("a")
            .query::<i64>(&mut connection)?;
        let removed: i64 = redis::cmd("ZREM")
            .arg(key(candidate, "zset:zrem"))
            .arg("a")
            .query(&mut connection)?;
        ensure_eq(removed, 1, "ZREM count")
    });
    check(
        candidate,
        &mut rows,
        url,
        "zsets",
        "ZREMRANGEBYRANK",
        |connection| {
            let mut connection = connection;
            redis::cmd("ZADD")
                .arg(key(candidate, "zset:zremrangebyrank"))
                .arg(1)
                .arg("a")
                .arg(2)
                .arg("b")
                .arg(3)
                .arg("c")
                .query::<i64>(&mut connection)?;
            let removed: i64 = redis::cmd("ZREMRANGEBYRANK")
                .arg(key(candidate, "zset:zremrangebyrank"))
                .arg(0)
                .arg(0)
                .query(&mut connection)?;
            ensure_eq(removed, 1, "ZREMRANGEBYRANK count")
        },
    );

    check(
        candidate,
        &mut rows,
        url,
        "scan/type",
        "SCAN MATCH COUNT",
        |connection| scan_mixed_static(candidate, connection),
    );

    check(
        candidate,
        &mut rows,
        url,
        "pipelines",
        "INCR + EXPIRE NX",
        |connection| {
            let mut connection = connection;
            let key_name = key(candidate, "pipeline:incr-expire-nx");
            let (count, expire): (i64, i64) = redis::pipe()
                .cmd("INCR")
                .arg(&key_name)
                .cmd("EXPIRE")
                .arg(&key_name)
                .arg(30)
                .arg("NX")
                .query(&mut connection)?;
            ensure_eq((count, expire), (1, 1), "pipeline INCR/EXPIRE")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "pipelines",
        "HSET/HDEL/EXPIREAT/SET EXAT",
        |connection| {
            let mut connection = connection;
            let hash_key = key(candidate, "pipeline:hash-expiry");
            let string_key = key(candidate, "pipeline:string-exat");
            let _: redis::Value = redis::pipe()
                .cmd("HSET")
                .arg(&hash_key)
                .arg("field")
                .arg("value")
                .cmd("HDEL")
                .arg(&hash_key)
                .arg("field")
                .cmd("EXPIREAT")
                .arg(&hash_key)
                .arg(epoch_seconds_plus(60))
                .cmd("SET")
                .arg(&string_key)
                .arg("value")
                .arg("EXAT")
                .arg(epoch_seconds_plus(60))
                .query(&mut connection)?;
            Ok("pipeline executed".to_string())
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "pipelines",
        "ZADD/ZREMRANGEBYRANK/ZRANGE",
        |connection| {
            let mut connection = connection;
            let zset_key = key(candidate, "pipeline:zset");
            let _: redis::Value = redis::pipe()
                .cmd("ZADD")
                .arg(&zset_key)
                .arg(1)
                .arg("a")
                .arg(2)
                .arg("b")
                .cmd("ZREMRANGEBYRANK")
                .arg(&zset_key)
                .arg(0)
                .arg(0)
                .cmd("ZRANGE")
                .arg(&zset_key)
                .arg(0)
                .arg(-1)
                .query(&mut connection)?;
            Ok("pipeline executed".to_string())
        },
    );

    check(
        candidate,
        &mut rows,
        url,
        "scripts/transactions",
        "EVAL compare-and-delete",
        |connection| {
            let mut connection = connection;
            let lock_key = key(candidate, "script:eval-lock");
            redis::cmd("SET")
                .arg(&lock_key)
                .arg("token")
                .query::<String>(&mut connection)?;
            let deleted: i64 = redis::cmd("EVAL")
            .arg("if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end")
            .arg(1)
            .arg(&lock_key)
            .arg("token")
            .query(&mut connection)?;
            ensure_eq(deleted, 1, "EVAL delete result")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "scripts/transactions",
        "SCRIPT LOAD",
        |connection| {
            let mut connection = connection;
            let sha: String = redis::cmd("SCRIPT")
                .arg("LOAD")
                .arg("return 7")
                .query(&mut connection)?;
            anyhow::ensure!(!sha.is_empty(), "empty script SHA");
            Ok(format!("sha={sha}"))
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "scripts/transactions",
        "EVALSHA",
        |connection| {
            let mut connection = connection;
            let sha: String = redis::cmd("SCRIPT")
                .arg("LOAD")
                .arg("return 7")
                .query(&mut connection)?;
            let value: i64 = redis::cmd("EVALSHA")
                .arg(sha)
                .arg(0)
                .query(&mut connection)?;
            ensure_eq(value, 7, "EVALSHA result")
        },
    );
    check(
        candidate,
        &mut rows,
        url,
        "scripts/transactions",
        "MULTI/EXEC",
        |connection| {
            let mut connection = connection;
            let key_name = key(candidate, "transaction:multi-exec");
            let (first, second): (i64, i64) = redis::pipe()
                .atomic()
                .cmd("INCR")
                .arg(&key_name)
                .cmd("INCR")
                .arg(&key_name)
                .query(&mut connection)?;
            ensure_eq((first, second), (1, 2), "MULTI/EXEC results")
        },
    );

    Ok(rows)
}

fn concurrent_rpop_unique(candidate: &str, url: &str) -> Result<String> {
    let list_key = key(candidate, "list:concurrent-rpop");
    let mut setup = redis_connection(url)?;
    for value in 0..20 {
        redis::cmd("LPUSH")
            .arg(&list_key)
            .arg(format!("item-{value}"))
            .query::<i64>(&mut setup)?;
    }

    let mut handles = Vec::new();
    for _ in 0..20 {
        let thread_url = url.to_string();
        let thread_key = list_key.clone();
        handles.push(thread::spawn(move || -> Result<Option<String>> {
            let mut connection = redis_connection(&thread_url)?;
            let value: Option<String> =
                redis::cmd("RPOP").arg(thread_key).query(&mut connection)?;
            Ok(value)
        }));
    }

    let mut values = BTreeSet::new();
    for handle in handles {
        let value = handle
            .join()
            .map_err(|_| anyhow!("RPOP worker panicked"))??;
        let value = value.ok_or_else(|| anyhow!("RPOP returned nil during concurrent pop"))?;
        anyhow::ensure!(values.insert(value.clone()), "duplicate RPOP value {value}");
    }
    anyhow::ensure!(
        values.len() == 20,
        "expected 20 unique RPOP values, got {}",
        values.len()
    );
    Ok("20 unique pops".to_string())
}

fn scan_mixed_static(candidate: &str, connection: &mut Connection) -> Result<String> {
    let prefix = key(candidate, "scan:mixed");
    let expected = BTreeSet::from([
        format!("{prefix}:hash"),
        format!("{prefix}:list"),
        format!("{prefix}:set"),
        format!("{prefix}:string"),
        format!("{prefix}:zset"),
    ]);

    redis::cmd("SET")
        .arg(format!("{prefix}:string"))
        .arg("value")
        .query::<String>(connection)?;
    redis::cmd("HSET")
        .arg(format!("{prefix}:hash"))
        .arg("field")
        .arg("value")
        .query::<i64>(connection)?;
    redis::cmd("RPUSH")
        .arg(format!("{prefix}:list"))
        .arg("value")
        .query::<i64>(connection)?;
    redis::cmd("SADD")
        .arg(format!("{prefix}:set"))
        .arg("value")
        .query::<i64>(connection)?;
    redis::cmd("ZADD")
        .arg(format!("{prefix}:zset"))
        .arg(1)
        .arg("value")
        .query::<i64>(connection)?;

    let mut cursor = 0_u64;
    let mut seen = BTreeSet::new();
    let mut iterations = 0;
    loop {
        iterations += 1;
        anyhow::ensure!(iterations < 20, "SCAN did not converge");
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("{prefix}:*"))
            .arg("COUNT")
            .arg(2)
            .query(connection)?;
        for found in keys {
            anyhow::ensure!(seen.insert(found.clone()), "SCAN duplicate key {found}");
        }
        cursor = next_cursor;
        if cursor == 0 {
            break;
        }
    }

    ensure_eq(seen, expected, "SCAN key set")
}

fn check<F>(
    candidate: &'static str,
    rows: &mut Vec<Row>,
    url: &str,
    family: &'static str,
    command: &'static str,
    f: F,
) where
    F: FnOnce(&mut Connection) -> Result<String>,
{
    let result = redis_connection(url).and_then(|mut connection| f(&mut connection));
    match result {
        Ok(detail) => rows.push(Row::pass(candidate, family, command, detail)),
        Err(error) => rows.push(Row::fail(candidate, family, command, error)),
    }
}

fn redis_connection(url: &str) -> Result<Connection> {
    let client =
        redis::Client::open(url).with_context(|| format!("create Redis client for {url}"))?;
    client
        .get_connection_with_timeout(Duration::from_secs(2))
        .with_context(|| format!("connect Redis client to {url}"))
}

fn ensure_eq<T, U>(actual: T, expected: U, label: &str) -> Result<String>
where
    T: std::fmt::Debug + PartialEq<U>,
    U: std::fmt::Debug,
{
    anyhow::ensure!(
        actual == expected,
        "{label}: expected {expected:?}, got {actual:?}"
    );
    Ok(format!("{label}={actual:?}"))
}

fn key(candidate: &str, suffix: &str) -> String {
    format!("per:3d:kv:{}:{suffix}", candidate.replace('-', "_"))
}

fn epoch_seconds_plus(seconds: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
        + seconds
}

#[derive(Clone, Copy)]
enum Candidate {
    TinyRedis,
    FeoxServer,
}

impl Candidate {
    fn name(self) -> &'static str {
        match self {
            Candidate::TinyRedis => "tinyredis-1.0.0",
            Candidate::FeoxServer => "feox-server-0.7.0",
        }
    }
}

enum RunningServer {
    Tiny {
        addr: SocketAddr,
        shutdown_tx: broadcast::Sender<()>,
        task: JoinHandle<()>,
    },
    Feox {
        addr: SocketAddr,
        child: Child,
    },
}

impl RunningServer {
    async fn start(candidate: Candidate) -> Result<Self> {
        match candidate {
            Candidate::TinyRedis => Self::start_tiny().await,
            Candidate::FeoxServer => Self::start_feox().await,
        }
    }

    fn url(&self) -> String {
        format!("redis://{}", self.addr())
    }

    fn addr(&self) -> SocketAddr {
        match self {
            RunningServer::Tiny { addr, .. } | RunningServer::Feox { addr, .. } => *addr,
        }
    }

    async fn start_tiny() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("bind tinyredis listener")?;
        let addr = listener
            .local_addr()
            .context("read tinyredis listener address")?;
        let mut config = TinyConfig::default();
        config.port = addr.port();
        config.appendonly = false;
        let stats = Arc::new(ServerStats::from_config(&config, String::new(), None));
        let store = Arc::new(Mutex::new(Store::new()));
        let (shutdown_tx, _) = broadcast::channel(1);
        let task = tokio::spawn(tiny_server::serve(
            listener,
            store,
            None,
            shutdown_tx.clone(),
            stats,
        ));
        wait_for_tcp(addr).await?;
        Ok(RunningServer::Tiny {
            addr,
            shutdown_tx,
            task,
        })
    }

    async fn start_feox() -> Result<Self> {
        let binary = build_candidate_binary("feox-server-0.7.0", "feox-server").await?;
        let addr = free_tcp_addr()?;
        let child = Command::new(binary)
            .arg("--bind")
            .arg(addr.ip().to_string())
            .arg("--port")
            .arg(addr.port().to_string())
            .arg("--threads")
            .arg("1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn feox-server binary")?;
        wait_for_tcp(addr).await?;
        Ok(RunningServer::Feox { addr, child })
    }

    async fn shutdown(self) -> Result<()> {
        let addr = self.addr();
        match self {
            RunningServer::Tiny {
                shutdown_tx, task, ..
            } => {
                let _ = shutdown_tx.send(());
                timeout(Duration::from_secs(5), task)
                    .await
                    .context("tinyredis task did not stop")?
                    .context("tinyredis task panicked")?;
            }
            RunningServer::Feox { mut child, .. } => {
                let pid = child.id().ok_or_else(|| anyhow!("feox child had no pid"))?;
                let status = Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .status()
                    .await
                    .context("send TERM to feox child")?;
                anyhow::ensure!(status.success(), "kill -TERM failed with {status}");
                let exit = timeout(Duration::from_secs(5), child.wait())
                    .await
                    .context("feox child did not exit after TERM")?
                    .context("wait for feox child")?;
                anyhow::ensure!(exit.success(), "feox child exited with {exit}");
            }
        }
        assert_stopped(addr).await
    }
}

async fn build_candidate_binary(crate_name: &str, bin_name: &str) -> Result<PathBuf> {
    let source = registry_crate_dir(crate_name)?;
    let manifest = source.join("Cargo.toml");
    let target_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("candidate-bins")
        .join(crate_name);
    let output = Command::new("cargo")
        .arg("+1.95.0")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--bin")
        .arg(bin_name)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()
        .await
        .with_context(|| format!("build {crate_name} binary {bin_name}"))?;
    anyhow::ensure!(
        output.status.success(),
        "build {crate_name} binary {bin_name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let binary = target_dir.join("debug").join(bin_name);
    anyhow::ensure!(
        binary.exists(),
        "built binary not found at {}",
        binary.display()
    );
    Ok(binary)
}

fn registry_crate_dir(crate_name: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is required to locate cargo registry")?;
    let registry_src = Path::new(&home).join(".cargo/registry/src");
    for registry in std::fs::read_dir(&registry_src)
        .with_context(|| format!("read {}", registry_src.display()))?
    {
        let candidate = registry?.path().join(crate_name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "crate source {crate_name} not found under {}",
        registry_src.display()
    ))
}

async fn wait_for_tcp(addr: SocketAddr) -> Result<()> {
    timeout(Duration::from_secs(10), async {
        loop {
            if TcpStream::connect(addr).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .with_context(|| format!("server did not accept TCP at {addr}"))
}

async fn assert_stopped(addr: SocketAddr) -> Result<()> {
    match timeout(Duration::from_secs(1), TcpStream::connect(addr)).await {
        Ok(Ok(_)) => Err(anyhow!("server still accepts TCP at {addr} after shutdown")),
        Ok(Err(_)) | Err(_) => Ok(()),
    }
}

fn free_tcp_addr() -> Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("reserve free TCP port")?;
    listener.local_addr().context("read reserved TCP port")
}

struct Row {
    candidate: &'static str,
    family: &'static str,
    command: &'static str,
    status: &'static str,
    detail: String,
}

impl Row {
    fn pass(
        candidate: &'static str,
        family: &'static str,
        command: &'static str,
        detail: String,
    ) -> Self {
        Self {
            candidate,
            family,
            command,
            status: "pass",
            detail: sanitize(detail),
        }
    }

    fn fail(
        candidate: &'static str,
        family: &'static str,
        command: &'static str,
        error: anyhow::Error,
    ) -> Self {
        Self {
            candidate,
            family,
            command,
            status: "fail",
            detail: sanitize(format!("{error:#}")),
        }
    }
}

fn sanitize(value: String) -> String {
    value.replace('\t', " ").replace('\n', " ")
}
