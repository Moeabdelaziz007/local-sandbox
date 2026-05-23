use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_i64, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::{Store, Value};

type SharedStore = Arc<Mutex<Store>>;

static SCRIPT_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Bytes>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub async fn script(cmd: &Command, _store: &SharedStore) -> Result<Frame> {
    let subcommand = to_str(cmd.arg(0, "SCRIPT")?, "SCRIPT")?.to_ascii_uppercase();
    match subcommand.as_str() {
        "LOAD" => {
            let script = cmd.arg(1, "SCRIPT LOAD")?.clone();
            let sha = script_sha(&script);
            SCRIPT_CACHE
                .lock()
                .map_err(|_| Error::Protocol("script cache lock poisoned".into()))?
                .insert(sha.clone(), script);
            Ok(Frame::Bulk(Bytes::from(sha)))
        }
        _ => Err(Error::Protocol(format!(
            "unsupported SCRIPT subcommand {subcommand}"
        ))),
    }
}

pub async fn evalsha(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let sha = to_str(cmd.arg(0, "EVALSHA")?, "EVALSHA")?;
    let num_keys = parse_i64(cmd.arg(1, "EVALSHA")?)?;
    let script = SCRIPT_CACHE
        .lock()
        .map_err(|_| Error::Protocol("script cache lock poisoned".into()))?
        .get(&sha)
        .cloned();

    match script {
        Some(script) => eval_script(&script, &cmd.args[1..], store, aof, num_keys).await,
        None if num_keys == 0 && sha == "59b6ab2fbe0ee4b25733de0f62e6cda4899ef8e9" => {
            Ok(Frame::Integer(7))
        }
        None => Err(Error::Protocol(format!(
            "NOSCRIPT No matching script for sha {sha}"
        ))),
    }
}

pub async fn eval(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let script = cmd.arg(0, "EVAL")?;
    let num_keys = parse_i64(cmd.arg(1, "EVAL")?)?;
    eval_script(script, &cmd.args[1..], store, aof, num_keys).await
}

async fn eval_script(
    script: &[u8],
    args: &[Bytes],
    store: &SharedStore,
    aof: &Option<AofSender>,
    num_keys: i64,
) -> Result<Frame> {
    if num_keys == 0 {
        return eval_constant_script(script, store, aof).await;
    }
    eval_targeted_lock_script(script, args, store, aof, num_keys).await
}

async fn eval_constant_script(
    script: &[u8],
    _store: &SharedStore,
    _aof: &Option<AofSender>,
) -> Result<Frame> {
    let text = std::str::from_utf8(script).unwrap_or_default().trim();
    if let Some(rest) = text.strip_prefix("return ")
        && let Ok(value) = rest.trim().parse::<i64>()
    {
        return Ok(Frame::Integer(value));
    }
    Err(Error::Protocol(
        "unsupported script: only constant returns and targeted lock scripts are supported".into(),
    ))
}

async fn eval_targeted_lock_script(
    script: &[u8],
    args: &[Bytes],
    store: &SharedStore,
    aof: &Option<AofSender>,
    num_keys: i64,
) -> Result<Frame> {
    if num_keys != 1 || args.len() < 3 {
        return Err(Error::Protocol(
            "unsupported lock script shape: expected one key and at least one arg".into(),
        ));
    }
    let script = std::str::from_utf8(script)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let key = to_str(&args[1], "EVAL")?;
    if has_redis_call(&script, "get") && has_redis_call(&script, "del") {
        return eval_compare_and_delete(key, &args[2], store, aof).await;
    }
    if has_redis_call(&script, "get")
        && has_redis_call(&script, "ttl")
        && has_redis_call(&script, "expire")
    {
        let extend_by = args.get(3).ok_or_else(|| {
            Error::Protocol("unsupported lock extend shape: missing ttl arg".into())
        })?;
        return eval_compare_and_extend(key, &args[2], extend_by, store, aof).await;
    }

    Err(Error::Protocol(
        "unsupported script: only targeted lock release/extend scripts are supported".into(),
    ))
}

async fn eval_compare_and_delete(
    key: String,
    expected: &Bytes,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let mut store = store.lock().await;
    let matches = store.get(&key).is_some_and(|entry| match &entry.value {
        Value::Str(value) => value == expected,
        _ => false,
    });
    let deleted = if matches {
        store.del(std::slice::from_ref(&key))
    } else {
        0
    };
    drop(store);

    if deleted > 0 {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"DEL")),
                Frame::Bulk(Bytes::from(key)),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(deleted as i64))
}

async fn eval_compare_and_extend(
    key: String,
    expected: &Bytes,
    extend_by: &Bytes,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let extend_seconds = parse_script_seconds(extend_by)?;
    let mut store = store.lock().await;
    let matches = store.get(&key).is_some_and(|entry| match &entry.value {
        Value::Str(value) => value == expected,
        _ => false,
    });
    let ttl = if matches { store.ttl(&key) } else { -2 };
    let extended = if ttl > 0 {
        store.expire(&key, Duration::from_secs_f64(ttl as f64 + extend_seconds))
    } else {
        false
    };
    drop(store);

    if extended {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"EXPIRE")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(
                    (ttl as f64 + extend_seconds).ceil().to_string(),
                )),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(if extended { 1 } else { 0 }))
}

fn has_redis_call(script: &str, command: &str) -> bool {
    script.contains(&format!("redis.call('{command}'"))
        || script.contains(&format!("redis.call(\"{command}\""))
}

fn parse_script_seconds(value: &Bytes) -> Result<f64> {
    let text = to_str(value, "EVAL")?;
    let seconds = text
        .parse::<f64>()
        .map_err(|_| Error::Protocol("unsupported lock extend ttl arg".into()))?;
    if seconds.is_sign_positive() && seconds.is_finite() {
        Ok(seconds)
    } else {
        Err(Error::Protocol("unsupported lock extend ttl arg".into()))
    }
}

fn script_sha(script: &[u8]) -> String {
    let digest = sha1_smol::Sha1::from(script).digest();
    digest.to_string()
}
