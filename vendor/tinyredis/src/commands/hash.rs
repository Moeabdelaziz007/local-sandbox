use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_i64, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::Store;

type SharedStore = Arc<Mutex<Store>>;

pub async fn hset(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    // HSET key field value [field value ...]
    if cmd.args.len() < 3 || !(cmd.args.len() - 1).is_multiple_of(2) {
        return Err(Error::WrongArity("HSET"));
    }
    let key = to_str(cmd.arg(0, "HSET")?, "HSET")?;
    let pairs = parse_field_value_pairs(&cmd.args[1..], "HSET")?;

    let added = store.lock().await.hset(&key, pairs.clone())?;

    aof_log(aof, hset_frame(&key, &pairs)).await;
    Ok(Frame::Integer(added as i64))
}

pub async fn hmset(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    // HMSET is a deprecated alias for HSET that always returns OK
    if cmd.args.len() < 3 || !(cmd.args.len() - 1).is_multiple_of(2) {
        return Err(Error::WrongArity("HMSET"));
    }
    let key = to_str(cmd.arg(0, "HMSET")?, "HMSET")?;
    let pairs = parse_field_value_pairs(&cmd.args[1..], "HMSET")?;

    store.lock().await.hset(&key, pairs.clone())?;

    aof_log(aof, hset_frame(&key, &pairs)).await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn hget(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HGET")?, "HGET")?;
    let field = to_str(cmd.arg(1, "HGET")?, "HGET")?;
    Ok(match store.lock().await.hget(&key, &field)? {
        Some(b) => Frame::Bulk(b),
        None => Frame::Null,
    })
}

pub async fn hmget(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("HMGET"));
    }
    let key = to_str(cmd.arg(0, "HMGET")?, "HMGET")?;
    let fields: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "HMGET"))
        .collect::<Result<_>>()?;

    let vals = store.lock().await.hmget(&key, &fields)?;
    let frames = vals
        .into_iter()
        .map(|opt| match opt {
            Some(b) => Frame::Bulk(b),
            None => Frame::Null,
        })
        .collect();
    Ok(Frame::Array(frames))
}

pub async fn hdel(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("HDEL"));
    }
    let key = to_str(cmd.arg(0, "HDEL")?, "HDEL")?;
    let fields: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "HDEL"))
        .collect::<Result<_>>()?;

    let removed = store.lock().await.hdel(&key, &fields)?;
    if removed > 0 {
        let mut frames = vec![
            Frame::Bulk(Bytes::from_static(b"HDEL")),
            Frame::Bulk(Bytes::from(key)),
        ];
        for f in &fields {
            frames.push(Frame::Bulk(Bytes::from(f.clone())));
        }
        aof_log(aof, Frame::Array(frames)).await;
    }
    Ok(Frame::Integer(removed as i64))
}

pub async fn hexists(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HEXISTS")?, "HEXISTS")?;
    let field = to_str(cmd.arg(1, "HEXISTS")?, "HEXISTS")?;
    Ok(Frame::Integer(
        store.lock().await.hexists(&key, &field)? as i64
    ))
}

pub async fn hlen(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HLEN")?, "HLEN")?;
    Ok(Frame::Integer(store.lock().await.hlen(&key)? as i64))
}

pub async fn hkeys(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HKEYS")?, "HKEYS")?;
    let keys = store.lock().await.hkeys(&key)?;
    Ok(Frame::Array(
        keys.into_iter()
            .map(|k| Frame::Bulk(Bytes::from(k)))
            .collect(),
    ))
}

pub async fn hvals(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HVALS")?, "HVALS")?;
    let vals = store.lock().await.hvals(&key)?;
    Ok(Frame::Array(vals.into_iter().map(Frame::Bulk).collect()))
}

pub async fn hgetall(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HGETALL")?, "HGETALL")?;
    let pairs = store.lock().await.hgetall(&key)?;
    let frames = pairs
        .into_iter()
        .flat_map(|(k, v)| [Frame::Bulk(Bytes::from(k)), Frame::Bulk(v)])
        .collect();
    Ok(Frame::Array(frames))
}

pub async fn hincrby(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HINCRBY")?, "HINCRBY")?;
    let field = to_str(cmd.arg(1, "HINCRBY")?, "HINCRBY")?;
    let delta = parse_i64(cmd.arg(2, "HINCRBY")?)?;

    let result = store.lock().await.hincrby(&key, &field, delta)?;
    // Canonicalize to HSET for idempotent replay
    aof_log(
        aof,
        hset_frame(&key, &[(field, Bytes::from(result.to_string()))]),
    )
    .await;
    Ok(Frame::Integer(result))
}

pub async fn hincrbyfloat(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HINCRBYFLOAT")?, "HINCRBYFLOAT")?;
    let field = to_str(cmd.arg(1, "HINCRBYFLOAT")?, "HINCRBYFLOAT")?;
    let delta = parse_decimal(cmd.arg(2, "HINCRBYFLOAT")?)?;

    let result = store.lock().await.hincrbyfloat(&key, &field, delta)?;
    aof_log(
        aof,
        hset_frame(&key, &[(field, Bytes::from(result.clone()))]),
    )
    .await;
    Ok(Frame::Bulk(Bytes::from(result)))
}

pub async fn hsetnx(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "HSETNX")?, "HSETNX")?;
    let field = to_str(cmd.arg(1, "HSETNX")?, "HSETNX")?;
    let value = cmd.arg(2, "HSETNX")?.clone();

    let set = store.lock().await.hsetnx(&key, &field, value.clone())?;
    if set {
        aof_log(aof, hset_frame(&key, &[(field, value)])).await;
    }
    Ok(Frame::Integer(set as i64))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn parse_field_value_pairs(args: &[Bytes], cmd: &'static str) -> Result<Vec<(String, Bytes)>> {
    args.chunks_exact(2)
        .map(|chunk| Ok((to_str(&chunk[0], cmd)?, chunk[1].clone())))
        .collect()
}

fn hset_frame(key: &str, pairs: &[(String, Bytes)]) -> Frame {
    let mut frames = vec![
        Frame::Bulk(Bytes::from_static(b"HSET")),
        Frame::Bulk(Bytes::from(key.to_string())),
    ];
    for (field, value) in pairs {
        frames.push(Frame::Bulk(Bytes::from(field.clone())));
        frames.push(Frame::Bulk(value.clone()));
    }
    Frame::Array(frames)
}

fn parse_decimal(b: &Bytes) -> Result<rust_decimal::Decimal> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::NotInteger)
}
