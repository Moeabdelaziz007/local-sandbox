//! Daemon Redis-compatible key-value contracts.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use local_sandbox_core::{
    LocalEndpointProtocol, LocalEndpointPurpose, LocalEndpointVisibility, LocalHealthStatus,
    LocalKeyValueCapabilities, LocalProfile, LocalServiceProfile,
};
use local_sandbox_daemon::{boot_state_with_key_value_config, router_with_state, DaemonState};
use local_sandbox_services::KeyValueConfig;
use redis::Commands;
use std::{
    collections::HashMap,
    error::Error,
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

const RESPONSE_BODY_LIMIT: usize = 1024 * 1024;

#[tokio::test]
async fn daemon_profile_drives_embedded_redis_host_contract() -> Result<(), Box<dyn Error>> {
    let temp_root = unique_temp_root();
    let state = boot_state_for_contract(temp_root.as_path()).await?;
    let _cleanup = DaemonCleanup::new(state.clone(), temp_root);
    let profile = fetch_profile(state).await?;
    let service = key_value_service(&profile)?;

    assert_eq!(service.health.status, LocalHealthStatus::Ready);
    assert_profile_capabilities(service)?;

    let endpoint = host_redis_endpoint(service)?;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let client = redis::Client::open(endpoint).map_err(|error| error.to_string())?;
        let mut connection = client.get_connection().map_err(|error| error.to_string())?;
        prove_host_redis_contract(&mut connection).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| invalid_data(format!("redis contract task failed: {error}")))?
    .map_err(invalid_data)?;

    Ok(())
}

async fn boot_state_for_contract(temp_root: &Path) -> Result<DaemonState, Box<dyn Error>> {
    let mut last_error = None;
    for _ in 0..10 {
        let config = contract_config(temp_root)?;
        match boot_state_with_key_value_config(config).await {
            Ok(state)
                if state.key_value_manager().profile().health.status
                    == LocalHealthStatus::Ready =>
            {
                return Ok(state);
            }
            Ok(state) => {
                let profile = state.key_value_manager().profile();
                let message = profile
                    .health
                    .message
                    .unwrap_or_else(|| "key-value storage did not become ready".to_string());
                let _ = state.key_value_manager().shutdown();
                if !is_address_in_use_message(&message) {
                    return Err(invalid_data(message).into());
                }
                last_error = Some(message);
            }
            Err(error) if is_address_in_use_message(&error.to_string()) => {
                last_error = Some(error.to_string());
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(invalid_data(format!(
        "key-value startup did not get a free port after retries: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
    .into())
}

fn contract_config(temp_root: &Path) -> Result<KeyValueConfig, io::Error> {
    Ok(KeyValueConfig {
        data_dir: temp_root.join("key-value-data"),
        host: "127.0.0.1".to_string(),
        port: free_port()?,
        host_endpoint_host: "127.0.0.1".to_string(),
        guest_endpoint_host: "host.internal".to_string(),
        password: None,
        readiness_timeout: Duration::from_secs(30),
        ..KeyValueConfig::default()
    })
}

fn is_address_in_use_message(message: &str) -> bool {
    message.contains("Address already in use") || message.contains("address already in use")
}

fn prove_host_redis_contract(connection: &mut redis::Connection) -> Result<(), Box<dyn Error>> {
    let prefix = format!("local-sandbox-contract:{}", unique_suffix());

    let pong: String = redis::cmd("PING").query(connection)?;
    assert_eq!(pong, "PONG");

    let string_key = key(&prefix, "string");
    let set: String = redis::cmd("SET")
        .arg(&string_key)
        .arg("value")
        .query(connection)?;
    assert_eq!(set, "OK");
    let value: String = connection.get(&string_key)?;
    assert_eq!(value, "value");

    let setex_key = key(&prefix, "setex");
    let setex: String = redis::cmd("SETEX")
        .arg(&setex_key)
        .arg(30)
        .arg("value")
        .query(connection)?;
    assert_eq!(setex, "OK");
    let ttl: i64 = redis::cmd("TTL").arg(&setex_key).query(connection)?;
    assert!(ttl > 0);

    let expire_nx_key = key(&prefix, "expire-nx");
    redis::cmd("SET")
        .arg(&expire_nx_key)
        .arg("value")
        .query::<String>(connection)?;
    let first_expire: i64 = redis::cmd("EXPIRE")
        .arg(&expire_nx_key)
        .arg(30)
        .arg("NX")
        .query(connection)?;
    let second_expire: i64 = redis::cmd("EXPIRE")
        .arg(&expire_nx_key)
        .arg(30)
        .arg("NX")
        .query(connection)?;
    assert_eq!((first_expire, second_expire), (1, 0));

    let exat_key = key(&prefix, "set-exat");
    let exat = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 60;
    let set_exat: String = redis::cmd("SET")
        .arg(&exat_key)
        .arg("value")
        .arg("EXAT")
        .arg(exat)
        .query(connection)?;
    assert_eq!(set_exat, "OK");

    prove_hashes(connection, &prefix)?;
    prove_lists(connection, &prefix)?;
    prove_sets(connection, &prefix)?;
    prove_sorted_sets(connection, &prefix)?;
    prove_scan(connection, &prefix)?;
    prove_pipeline(connection, &prefix)?;
    prove_transactions(connection, &prefix)?;
    prove_targeted_scripts(connection, &prefix)?;

    Ok(())
}

fn prove_hashes(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let hash_key = key(prefix, "hash");
    let added: i64 = redis::cmd("HSET")
        .arg(&hash_key)
        .arg("a")
        .arg("1")
        .arg("b")
        .arg("2")
        .query(connection)?;
    assert_eq!(added, 2);
    let value: String = redis::cmd("HGET")
        .arg(&hash_key)
        .arg("a")
        .query(connection)?;
    assert_eq!(value, "1");
    let all: HashMap<String, String> = redis::cmd("HGETALL").arg(&hash_key).query(connection)?;
    assert_eq!(all.get("b").map(String::as_str), Some("2"));
    Ok(())
}

fn prove_lists(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let list_key = key(prefix, "list");
    let len: i64 = redis::cmd("RPUSH")
        .arg(&list_key)
        .arg("a")
        .arg("b")
        .arg("c")
        .query(connection)?;
    assert_eq!(len, 3);
    let trim: String = redis::cmd("LTRIM")
        .arg(&list_key)
        .arg(1)
        .arg(-1)
        .query(connection)?;
    assert_eq!(trim, "OK");
    let popped: String = redis::cmd("RPOP").arg(&list_key).query(connection)?;
    assert_eq!(popped, "c");
    let remaining: Vec<String> = redis::cmd("LRANGE")
        .arg(&list_key)
        .arg(0)
        .arg(-1)
        .query(connection)?;
    assert_eq!(remaining, vec!["b"]);
    Ok(())
}

fn prove_sets(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let set_key = key(prefix, "set");
    let added: i64 = redis::cmd("SADD")
        .arg(&set_key)
        .arg("a")
        .arg("b")
        .query(connection)?;
    assert_eq!(added, 2);
    let is_member: bool = redis::cmd("SISMEMBER")
        .arg(&set_key)
        .arg("a")
        .query(connection)?;
    assert!(is_member);
    let removed: i64 = redis::cmd("SREM")
        .arg(&set_key)
        .arg("b")
        .query(connection)?;
    assert_eq!(removed, 1);
    Ok(())
}

fn prove_sorted_sets(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let zset_key = key(prefix, "zset");
    let added: i64 = redis::cmd("ZADD")
        .arg(&zset_key)
        .arg(1)
        .arg("a")
        .arg(2)
        .arg("b")
        .arg(3)
        .arg("c")
        .query(connection)?;
    assert_eq!(added, 3);
    let removed: i64 = redis::cmd("ZREMRANGEBYRANK")
        .arg(&zset_key)
        .arg(0)
        .arg(0)
        .query(connection)?;
    assert_eq!(removed, 1);
    let remaining: Vec<String> = redis::cmd("ZRANGE")
        .arg(&zset_key)
        .arg(0)
        .arg(-1)
        .query(connection)?;
    assert_eq!(remaining, vec!["b", "c"]);
    Ok(())
}

fn prove_scan(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let scan_key = key(prefix, "scan");
    redis::cmd("SET")
        .arg(&scan_key)
        .arg("value")
        .query::<String>(connection)?;
    let (_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .arg(0)
        .arg("MATCH")
        .arg(format!("{prefix}:scan"))
        .query(connection)?;
    assert!(keys.iter().any(|key| key == &scan_key));
    Ok(())
}

fn prove_pipeline(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let pipeline_key = key(prefix, "pipeline");
    let values: Vec<String> = redis::pipe()
        .cmd("SET")
        .arg(&pipeline_key)
        .arg("value")
        .ignore()
        .cmd("GET")
        .arg(&pipeline_key)
        .query(connection)?;
    assert_eq!(values, vec!["value"]);
    Ok(())
}

fn prove_transactions(connection: &mut redis::Connection, prefix: &str) -> redis::RedisResult<()> {
    let tx_key = key(prefix, "transaction");
    let multi: String = redis::cmd("MULTI").query(connection)?;
    assert_eq!(multi, "OK");
    let queued_set: String = redis::cmd("SET")
        .arg(&tx_key)
        .arg("value")
        .query(connection)?;
    assert_eq!(queued_set, "QUEUED");
    let queued_get: String = redis::cmd("GET").arg(&tx_key).query(connection)?;
    assert_eq!(queued_get, "QUEUED");
    let results: Vec<String> = redis::cmd("EXEC").query(connection)?;
    assert_eq!(results, vec!["OK", "value"]);
    Ok(())
}

fn prove_targeted_scripts(
    connection: &mut redis::Connection,
    prefix: &str,
) -> redis::RedisResult<()> {
    let release_script = "\n      -- Check if the current UUID still holds the lock\n      if redis.call(\"get\", KEYS[1]) == ARGV[1] then\n        return redis.call(\"del\", KEYS[1])\n      else\n        return 0\n      end\n     ";
    let extend_script = "\n      -- Check if the current UUID still holds the lock\n      if redis.call(\"get\", KEYS[1]) ~= ARGV[1] then\n        return 0\n      end\n\n      -- Get the current TTL and extend it by the specified amount\n      local ttl = redis.call(\"ttl\", KEYS[1])\n      if ttl > 0 then\n        return redis.call(\"expire\", KEYS[1], ttl + ARGV[2])\n      else\n        return 0\n      end\n     ";

    let lock_key = key(prefix, "lock-release");
    redis::cmd("SET")
        .arg(&lock_key)
        .arg("token")
        .arg("EX")
        .arg(30)
        .query::<String>(connection)?;
    let deleted: i64 = redis::cmd("EVAL")
        .arg(release_script)
        .arg(1)
        .arg(&lock_key)
        .arg("token")
        .query(connection)?;
    assert_eq!(deleted, 1);

    let evalsha_key = key(prefix, "lock-evalsha-release");
    redis::cmd("SET")
        .arg(&evalsha_key)
        .arg("token")
        .arg("EX")
        .arg(30)
        .query::<String>(connection)?;
    let sha: String = redis::cmd("SCRIPT")
        .arg("LOAD")
        .arg(release_script)
        .query(connection)?;
    assert!(!sha.is_empty());
    let mismatched: i64 = redis::cmd("EVALSHA")
        .arg(&sha)
        .arg(1)
        .arg(&evalsha_key)
        .arg("wrong-token")
        .query(connection)?;
    assert_eq!(mismatched, 0);
    let still_locked: String = redis::cmd("GET").arg(&evalsha_key).query(connection)?;
    assert_eq!(still_locked, "token");
    let released: i64 = redis::cmd("EVALSHA")
        .arg(&sha)
        .arg(1)
        .arg(&evalsha_key)
        .arg("token")
        .query(connection)?;
    assert_eq!(released, 1);

    let extend_key = key(prefix, "lock-extend");
    redis::cmd("SET")
        .arg(&extend_key)
        .arg("token")
        .arg("EX")
        .arg(30)
        .query::<String>(connection)?;
    let before_extend: i64 = redis::cmd("TTL").arg(&extend_key).query(connection)?;
    let extended: i64 = redis::cmd("EVAL")
        .arg(extend_script)
        .arg(1)
        .arg(&extend_key)
        .arg("token")
        .arg(10)
        .query(connection)?;
    assert_eq!(extended, 1);
    let after_extend: i64 = redis::cmd("TTL").arg(&extend_key).query(connection)?;
    assert!(after_extend > before_extend);
    Ok(())
}

fn key_value_service(profile: &LocalProfile) -> Result<&LocalServiceProfile, io::Error> {
    profile
        .services
        .get("key_value")
        .ok_or_else(|| invalid_data("profile missing key_value service"))
}

fn host_redis_endpoint(service: &LocalServiceProfile) -> Result<String, io::Error> {
    service
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.visibility == LocalEndpointVisibility::Host
                && endpoint.purpose == LocalEndpointPurpose::KeyValue
                && endpoint.protocol == LocalEndpointProtocol::Redis
        })
        .and_then(|endpoint| endpoint.url.clone())
        .ok_or_else(|| invalid_data("key_value host Redis endpoint is missing"))
}

fn assert_profile_capabilities(service: &LocalServiceProfile) -> Result<(), io::Error> {
    let key_value = service
        .key_value
        .as_ref()
        .ok_or_else(|| invalid_data("key_value profile facts are missing"))?;
    assert_eq!(key_value.database, 0);
    assert_eq!(key_value.implementation, "tinyredis");

    let capabilities = key_value.capabilities;
    assert_supported_capabilities(capabilities);
    assert!(!capabilities.lua_scripts);

    Ok(())
}

fn assert_supported_capabilities(capabilities: LocalKeyValueCapabilities) {
    assert!(capabilities.ping);
    assert!(capabilities.strings);
    assert!(capabilities.expiry);
    assert!(capabilities.hashes);
    assert!(capabilities.lists);
    assert!(capabilities.sets);
    assert!(capabilities.sorted_sets);
    assert!(capabilities.scan);
    assert!(capabilities.pipeline);
    assert!(capabilities.transactions);
    assert!(capabilities.targeted_lock_scripts);
}

async fn fetch_profile(state: DaemonState) -> Result<LocalProfile, Box<dyn Error>> {
    let response = router_with_state(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/profile")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), RESPONSE_BODY_LIMIT).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

struct DaemonCleanup {
    state: DaemonState,
    temp_root: PathBuf,
}

impl DaemonCleanup {
    fn new(state: DaemonState, temp_root: PathBuf) -> Self {
        Self { state, temp_root }
    }
}

impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        let _ = self.state.key_value_manager().shutdown();
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

fn key(prefix: &str, name: &str) -> String {
    format!("{prefix}:{name}")
}

fn unique_temp_root() -> PathBuf {
    std::env::temp_dir().join(format!("local-sandbox-kv-contract-{}", unique_suffix()))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

fn free_port() -> Result<u16, io::Error> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
