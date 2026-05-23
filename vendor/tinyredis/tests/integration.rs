use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use tinyredis::config::Config;
use tinyredis::server;
use tinyredis::stats::ServerStats;
use tinyredis::store::Store;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};

struct TestServer {
    port: u16,
    shutdown_tx: broadcast::Sender<()>,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store = Arc::new(Mutex::new(Store::new()));
        let cfg = Config {
            port,
            appendonly: false,
            ..Config::default()
        };
        let stats = Arc::new(ServerStats::from_config(&cfg, String::new(), None));
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        tokio::spawn(server::serve(
            listener,
            store,
            None,
            shutdown_tx.clone(),
            stats,
        ));

        TestServer { port, shutdown_tx }
    }

    async fn client(&self) -> redis::aio::MultiplexedConnection {
        let client = redis::Client::open(format!("redis://127.0.0.1:{}/", self.port)).unwrap();
        client.get_multiplexed_async_connection().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
    }
}

// ── PING ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_ping() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let pong: String = redis::cmd("PING").query_async(&mut con).await.unwrap();
    assert_eq!(pong, "PONG");
}

#[tokio::test]
async fn test_ping_with_message() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let reply: Vec<u8> = redis::cmd("PING")
        .arg("hello")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(reply, b"hello");
}

// ── ECHO ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_echo() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let reply: Vec<u8> = redis::cmd("ECHO")
        .arg("world")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(reply, b"world");
}

// ── SET / GET ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_set_get() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let val: String = con.get("k").await.unwrap();
    assert_eq!(val, "v");
}

#[tokio::test]
async fn test_get_missing_returns_nil() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let val: Option<String> = con.get("no_such_key").await.unwrap();
    assert!(val.is_none());
}

#[tokio::test]
async fn test_set_ex_ttl() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("SET")
        .arg("temp")
        .arg("val")
        .arg("EX")
        .arg(10)
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let ttl: i64 = redis::cmd("TTL")
        .arg("temp")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(ttl > 0 && ttl <= 10, "expected ttl in (0,10], got {ttl}");
}

#[tokio::test]
async fn test_set_px_ttl() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("SET")
        .arg("temp_px")
        .arg("val")
        .arg("PX")
        .arg(5000)
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let ttl: i64 = redis::cmd("TTL")
        .arg("temp_px")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(ttl > 0 && ttl <= 5, "expected ttl in (0,5], got {ttl}");
}

#[tokio::test]
async fn test_key_expires() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("SET")
        .arg("expkey")
        .arg("val")
        .arg("PX")
        .arg(100)
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let val: Option<String> = con.get("expkey").await.unwrap();
    assert!(val.is_none(), "key should have expired");
}

#[tokio::test]
async fn test_set_nx() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("nx_key", "original").await.unwrap();
    let result: Option<String> = redis::cmd("SET")
        .arg("nx_key")
        .arg("new")
        .arg("NX")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(result.is_none(), "SET NX should return nil when key exists");
    let val: String = con.get("nx_key").await.unwrap();
    assert_eq!(val, "original");
}

#[tokio::test]
async fn test_set_xx() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: Option<String> = redis::cmd("SET")
        .arg("xx_missing")
        .arg("v")
        .arg("XX")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "SET XX should return nil when key is absent"
    );
    let val: Option<String> = con.get("xx_missing").await.unwrap();
    assert!(val.is_none());
}

// ── DEL ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_del() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("d1", "v").await.unwrap();
    con.set::<_, _, ()>("d2", "v").await.unwrap();
    let n: i64 = con.del(&["d1", "d2", "d3"]).await.unwrap();
    assert_eq!(n, 2);
    let v: Option<String> = con.get("d1").await.unwrap();
    assert!(v.is_none());
}

// ── EXISTS ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_exists() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("e1", "v").await.unwrap();
    let n: i64 = con.exists(&["e1", "no_such"]).await.unwrap();
    assert_eq!(n, 1);
}

// ── EXPIRE / TTL ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_expire_and_ttl() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("etkey", "v").await.unwrap();
    let ttl_before: i64 = redis::cmd("TTL")
        .arg("etkey")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(ttl_before, -1, "no expiry set yet");
    let set: i64 = con.expire("etkey", 10).await.unwrap();
    assert_eq!(set, 1);
    let ttl_after: i64 = redis::cmd("TTL")
        .arg("etkey")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(ttl_after > 0 && ttl_after <= 10);
}

#[tokio::test]
async fn test_ttl_missing_key() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let ttl: i64 = redis::cmd("TTL")
        .arg("ghost")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(ttl, -2);
}

// ── PERSIST / PEXPIRE / PTTL / EXPIREAT / PEXPIREAT ──────────────────────────

#[tokio::test]
async fn test_persist() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("SET")
        .arg("k")
        .arg("v")
        .arg("EX")
        .arg(10)
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let removed: i64 = redis::cmd("PERSIST")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    let ttl: i64 = redis::cmd("TTL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(ttl, -1);
}

#[tokio::test]
async fn test_persist_on_key_without_ttl_returns_zero() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let removed: i64 = redis::cmd("PERSIST")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(removed, 0);
}

#[tokio::test]
async fn test_pexpire_and_pttl() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let set: i64 = redis::cmd("PEXPIRE")
        .arg("k")
        .arg(5000)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(set, 1);
    let pttl: i64 = redis::cmd("PTTL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(pttl > 0 && pttl <= 5000, "got {pttl}");
}

#[tokio::test]
async fn test_pttl_no_expiry_returns_minus_one() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let pttl: i64 = redis::cmd("PTTL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(pttl, -1);
}

#[tokio::test]
async fn test_pttl_missing_key_returns_minus_two() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let pttl: i64 = redis::cmd("PTTL")
        .arg("ghost")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(pttl, -2);
}

#[tokio::test]
async fn test_expireat() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 10;
    let set: i64 = redis::cmd("EXPIREAT")
        .arg("k")
        .arg(epoch)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(set, 1);
    let ttl: i64 = redis::cmd("TTL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(ttl > 0 && ttl <= 10, "got {ttl}");
}

#[tokio::test]
async fn test_expireat_past_deletes_key() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let past_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 1;
    let set: i64 = redis::cmd("EXPIREAT")
        .arg("k")
        .arg(past_epoch)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(set, 1);
    let val: Option<String> = con.get("k").await.unwrap();
    assert!(val.is_none());
}

#[tokio::test]
async fn test_pexpireat() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 5000;
    let set: i64 = redis::cmd("PEXPIREAT")
        .arg("k")
        .arg(epoch_ms)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(set, 1);
    let pttl: i64 = redis::cmd("PTTL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(pttl > 0 && pttl <= 5000, "got {pttl}");
}

// ── TYPE ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_type_string() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("tkey", "v").await.unwrap();
    let t: String = redis::cmd("TYPE")
        .arg("tkey")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(t, "string");
}

#[tokio::test]
async fn test_type_none() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let t: String = redis::cmd("TYPE")
        .arg("ghost")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(t, "none");
}

// ── KEYS / SCAN / DBSIZE / RENAME / FLUSHDB / FLUSHALL ───────────────────────

#[tokio::test]
async fn test_keys_star() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("a", "1").await.unwrap();
    con.set::<_, _, ()>("b", "2").await.unwrap();
    let mut keys: Vec<String> = redis::cmd("KEYS")
        .arg("*")
        .query_async(&mut con)
        .await
        .unwrap();
    keys.sort();
    assert_eq!(keys, vec!["a", "b"]);
}

#[tokio::test]
async fn test_keys_pattern() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("foo:1", "v").await.unwrap();
    con.set::<_, _, ()>("foo:2", "v").await.unwrap();
    con.set::<_, _, ()>("bar:1", "v").await.unwrap();
    let mut keys: Vec<String> = redis::cmd("KEYS")
        .arg("foo:*")
        .query_async(&mut con)
        .await
        .unwrap();
    keys.sort();
    assert_eq!(keys, vec!["foo:1", "foo:2"]);
}

#[tokio::test]
async fn test_scan_returns_all_keys() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("x", "1").await.unwrap();
    con.set::<_, _, ()>("y", "2").await.unwrap();
    let result: (String, Vec<String>) = redis::cmd("SCAN")
        .arg(0)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(result.0, "0"); // cursor 0 means done
    let mut keys = result.1;
    keys.sort();
    assert_eq!(keys, vec!["x", "y"]);
}

#[tokio::test]
async fn test_scan_with_match() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("ns:a", "1").await.unwrap();
    con.set::<_, _, ()>("ns:b", "2").await.unwrap();
    con.set::<_, _, ()>("other", "3").await.unwrap();
    let result: (String, Vec<String>) = redis::cmd("SCAN")
        .arg(0)
        .arg("MATCH")
        .arg("ns:*")
        .query_async(&mut con)
        .await
        .unwrap();
    let mut keys = result.1;
    keys.sort();
    assert_eq!(keys, vec!["ns:a", "ns:b"]);
}

#[tokio::test]
async fn test_dbsize() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("a", "1").await.unwrap();
    con.set::<_, _, ()>("b", "2").await.unwrap();
    let n: i64 = redis::cmd("DBSIZE").query_async(&mut con).await.unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn test_rename() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("src", "hello").await.unwrap();
    redis::cmd("RENAME")
        .arg("src")
        .arg("dst")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let src: Option<String> = con.get("src").await.unwrap();
    assert!(src.is_none());
    let dst: String = con.get("dst").await.unwrap();
    assert_eq!(dst, "hello");
}

#[tokio::test]
async fn test_rename_missing_key_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let err = redis::cmd("RENAME")
        .arg("ghost")
        .arg("dst")
        .query_async::<()>(&mut con)
        .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn test_flushdb() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("a", "1").await.unwrap();
    con.set::<_, _, ()>("b", "2").await.unwrap();
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let n: i64 = redis::cmd("DBSIZE").query_async(&mut con).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn test_flushall() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("a", "1").await.unwrap();
    redis::cmd("FLUSHALL")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let n: i64 = redis::cmd("DBSIZE").query_async(&mut con).await.unwrap();
    assert_eq!(n, 0);
}

// ── APPEND ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_append() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let n1: i64 = redis::cmd("APPEND")
        .arg("akey")
        .arg("Hello")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n1, 5);
    let n2: i64 = redis::cmd("APPEND")
        .arg("akey")
        .arg(", World")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n2, 12);
    let val: String = con.get("akey").await.unwrap();
    assert_eq!(val, "Hello, World");
}

// ── STRLEN / GETDEL / GETSET ──────────────────────────────────────────────────

#[tokio::test]
async fn test_strlen() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "hello").await.unwrap();
    let n: i64 = redis::cmd("STRLEN")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 5);
}

#[tokio::test]
async fn test_strlen_missing_returns_zero() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let n: i64 = redis::cmd("STRLEN")
        .arg("ghost")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn test_getdel() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "v").await.unwrap();
    let val: String = redis::cmd("GETDEL")
        .arg("k")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(val, "v");
    let after: Option<String> = con.get("k").await.unwrap();
    assert!(after.is_none());
}

#[tokio::test]
async fn test_getdel_missing_returns_nil() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let val: Option<String> = redis::cmd("GETDEL")
        .arg("ghost")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(val.is_none());
}

#[tokio::test]
async fn test_getset() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("k", "old").await.unwrap();
    let old: String = redis::cmd("GETSET")
        .arg("k")
        .arg("new")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(old, "old");
    let cur: String = con.get("k").await.unwrap();
    assert_eq!(cur, "new");
}

#[tokio::test]
async fn test_getset_absent_returns_nil() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let old: Option<String> = redis::cmd("GETSET")
        .arg("k")
        .arg("v")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(old.is_none());
}

// ── INCR / INCRBY / DECR / DECRBY / INCRBYFLOAT ──────────────────────────────

#[tokio::test]
async fn test_incr() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let n: i64 = redis::cmd("INCR")
        .arg("counter")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let n2: i64 = redis::cmd("INCR")
        .arg("counter")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n2, 2);
}

#[tokio::test]
async fn test_incrby() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("num", "10").await.unwrap();
    let n: i64 = redis::cmd("INCRBY")
        .arg("num")
        .arg(5)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 15);
}

#[tokio::test]
async fn test_incr_non_integer_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("notnum", "abc").await.unwrap();
    let err = redis::cmd("INCR")
        .arg("notnum")
        .query_async::<i64>(&mut con)
        .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn test_decr() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("counter", "5").await.unwrap();
    let n: i64 = redis::cmd("DECR")
        .arg("counter")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 4);
}

#[tokio::test]
async fn test_decrby() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("num", "10").await.unwrap();
    let n: i64 = redis::cmd("DECRBY")
        .arg("num")
        .arg(3)
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(n, 7);
}

#[tokio::test]
async fn test_incrbyfloat() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    con.set::<_, _, ()>("f", "10.5").await.unwrap();
    let result: String = redis::cmd("INCRBYFLOAT")
        .arg("f")
        .arg("0.1")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(result, "10.6");
}

#[tokio::test]
async fn test_incrbyfloat_creates_key() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: String = redis::cmd("INCRBYFLOAT")
        .arg("newf")
        .arg("3.14")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(result, "3.14");
}

// ── MGET / MSET ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_mset_mget() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("MSET")
        .arg("mk1")
        .arg("v1")
        .arg("mk2")
        .arg("v2")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let vals: Vec<Option<String>> = redis::cmd("MGET")
        .arg("mk1")
        .arg("mk2")
        .arg("no_such")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(vals, vec![Some("v1".into()), Some("v2".into()), None]);
}

// ── MULTI / EXEC / DISCARD ────────────────────────────────────────────────────

#[tokio::test]
async fn test_multi_exec_basic() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    // A simple transaction: SET then GET
    let results: (String, String) = redis::pipe()
        .atomic()
        .set("txkey", "txval")
        .get("txkey")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(results, ("OK".to_string(), "txval".to_string()));
}

#[tokio::test]
async fn test_multi_exec_multiple_sets() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    redis::pipe()
        .atomic()
        .set("a", "1")
        .set("b", "2")
        .set("c", "3")
        .query_async::<()>(&mut con)
        .await
        .unwrap();

    let vals: Vec<String> = redis::cmd("MGET")
        .arg("a")
        .arg("b")
        .arg("c")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(vals, vec!["1", "2", "3"]);
}

#[tokio::test]
async fn test_multi_discard() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    // Seed a value
    redis::cmd("SET")
        .arg("dk")
        .arg("original")
        .query_async::<()>(&mut con)
        .await
        .unwrap();

    // Start MULTI, queue a SET, then DISCARD
    redis::cmd("MULTI")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let queued: String = redis::cmd("SET")
        .arg("dk")
        .arg("changed")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(queued, "QUEUED");
    redis::cmd("DISCARD")
        .query_async::<()>(&mut con)
        .await
        .unwrap();

    // Value should be unchanged
    let val: String = redis::cmd("GET")
        .arg("dk")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(val, "original");
}

#[tokio::test]
async fn test_exec_without_multi_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: redis::RedisResult<()> = redis::cmd("EXEC").query_async(&mut con).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_discard_without_multi_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: redis::RedisResult<()> = redis::cmd("DISCARD").query_async(&mut con).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_nested_multi_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    redis::cmd("MULTI")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let result: redis::RedisResult<()> = redis::cmd("MULTI").query_async(&mut con).await;
    assert!(result.is_err());
    // Clean up
    redis::cmd("DISCARD")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_multi_exec_error_in_queue_continues() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    // SET a string, then inside a transaction try to INCR it (would error),
    // plus a valid SET. Both commands execute; INCR returns an error, SET succeeds.
    redis::cmd("SET")
        .arg("not_int")
        .arg("abc")
        .query_async::<()>(&mut con)
        .await
        .unwrap();

    redis::cmd("MULTI")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    redis::cmd("INCR")
        .arg("not_int")
        .query_async::<()>(&mut con)
        .await
        .unwrap(); // queued
    redis::cmd("SET")
        .arg("ok_key")
        .arg("ok_val")
        .query_async::<()>(&mut con)
        .await
        .unwrap(); // queued

    // EXEC returns array; we use raw command to inspect
    // The INCR will return an error element; SET will return OK
    // Use pipeline exec_atomic equivalent via raw command
    let _: redis::RedisResult<()> = redis::cmd("EXEC").query_async(&mut con).await;

    // ok_key should have been set despite the INCR error
    let val: String = redis::cmd("GET")
        .arg("ok_key")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(val, "ok_val");
}

// ── CLIENT LIST / CLIENT SETNAME ──────────────────────────────────────────────

#[tokio::test]
async fn test_client_id_returns_integer() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let id: i64 = redis::cmd("CLIENT")
        .arg("ID")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(id >= 1, "client id should be positive, got {id}");
}

#[tokio::test]
async fn test_client_setname_and_getname() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    // Set a name and read it back.
    redis::cmd("CLIENT")
        .arg("SETNAME")
        .arg("myconn")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let name: String = redis::cmd("CLIENT")
        .arg("GETNAME")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(name, "myconn");
}

#[tokio::test]
async fn test_client_getname_empty_before_setname() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    // Before SETNAME the result is nil (null bulk).
    let name: Option<String> = redis::cmd("CLIENT")
        .arg("GETNAME")
        .query_async(&mut con)
        .await
        .unwrap();
    assert_eq!(name, None);
}

#[tokio::test]
async fn test_client_setname_rejects_spaces() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: redis::RedisResult<()> = redis::cmd("CLIENT")
        .arg("SETNAME")
        .arg("bad name")
        .query_async(&mut con)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_client_list_contains_current_connection() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;

    redis::cmd("CLIENT")
        .arg("SETNAME")
        .arg("listtest")
        .query_async::<()>(&mut con)
        .await
        .unwrap();
    let list: String = redis::cmd("CLIENT")
        .arg("LIST")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(list.contains("name=listtest"), "CLIENT LIST output: {list}");
    assert!(list.contains("id="), "CLIENT LIST should contain id=");
    assert!(list.contains("addr="), "CLIENT LIST should contain addr=");
}

#[tokio::test]
async fn test_client_list_unknown_subcommand_errors() {
    let srv = TestServer::start().await;
    let mut con = srv.client().await;
    let result: redis::RedisResult<()> = redis::cmd("CLIENT")
        .arg("NOSUCHCMD")
        .query_async(&mut con)
        .await;
    assert!(result.is_err());
}
