use std::sync::Arc;

use crate::commands::utils;
use crate::commands::utils::to_str;
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::Store;
use bytes::Bytes;
use tokio::sync::Mutex;

type SharedStore = Arc<Mutex<Store>>;

async fn aof_log(aof: &Option<AofSender>, frame: Frame) {
    if let Some(tx) = aof {
        let _ = tx
            .send(crate::persistence::AofEntry {
                raw: frame.serialize(),
            })
            .await;
    }
}

pub async fn append(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "APPEND")?, "APPEND")?;
    let extra = cmd.arg(1, "APPEND")?.clone();

    let len = store.lock().await.append(&key, &extra)?;

    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"APPEND")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(extra),
        ]),
    )
    .await;

    Ok(Frame::Integer(len as i64))
}

pub async fn incr(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "INCR")?, "INCR")?;
    let result = store.lock().await.incr_by(&key, 1)?;

    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"INCR")),
            Frame::Bulk(Bytes::from(key)),
        ]),
    )
    .await;

    Ok(Frame::Integer(result))
}

pub async fn incrby(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "INCRBY")?, "INCRBY")?;
    let delta = utils::parse_i64(cmd.arg(1, "INCRBY")?)?;

    let result = store.lock().await.incr_by(&key, delta)?;

    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"INCRBY")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from(delta.to_string())),
        ]),
    )
    .await;

    Ok(Frame::Integer(result))
}

pub async fn mget(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("MGET"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "MGET"))
        .collect::<Result<_>>()?;

    let values = store.lock().await.mget(&keys);

    let frames = values
        .into_iter()
        .map(|opt| match opt {
            Some(b) => Frame::Bulk(b),
            None => Frame::Null,
        })
        .collect();

    Ok(Frame::Array(frames))
}

pub async fn mset(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() || !cmd.args.len().is_multiple_of(2) {
        return Err(Error::WrongArity("MSET"));
    }

    let pairs: Vec<(String, Bytes)> = cmd
        .args
        .chunks_exact(2)
        .map(|chunk| Ok((to_str(&chunk[0], "MSET")?, chunk[1].clone())))
        .collect::<Result<_>>()?;

    store.lock().await.mset(pairs.clone());

    let mut aof_frames = vec![Frame::Bulk(Bytes::from_static(b"MSET"))];
    for (k, v) in &pairs {
        aof_frames.push(Frame::Bulk(Bytes::from(k.clone())));
        aof_frames.push(Frame::Bulk(v.clone()));
    }
    aof_log(aof, Frame::Array(aof_frames)).await;

    Ok(Frame::Simple("OK".into()))
}

pub async fn strlen(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "STRLEN")?, "STRLEN")?;
    Ok(Frame::Integer(store.lock().await.strlen(&key)? as i64))
}

pub async fn getdel(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "GETDEL")?, "GETDEL")?;
    let val = store.lock().await.getdel(&key)?;
    if val.is_some() {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"DEL")),
                Frame::Bulk(Bytes::from(key)),
            ]),
        )
        .await;
    }
    Ok(match val {
        Some(b) => Frame::Bulk(b),
        None => Frame::Null,
    })
}

pub async fn getset(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "GETSET")?, "GETSET")?;
    let value = cmd.arg(1, "GETSET")?.clone();
    let old = store.lock().await.getset(&key, value.clone())?;
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(value),
        ]),
    )
    .await;
    Ok(match old {
        Some(b) => Frame::Bulk(b),
        None => Frame::Null,
    })
}

pub async fn decr(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "DECR")?, "DECR")?;
    let result = store.lock().await.incr_by(&key, -1)?;
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"INCRBY")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from_static(b"-1")),
        ]),
    )
    .await;
    Ok(Frame::Integer(result))
}

pub async fn decrby(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "DECRBY")?, "DECRBY")?;
    let delta = utils::parse_i64(cmd.arg(1, "DECRBY")?)?;
    let result = store.lock().await.incr_by(&key, -delta)?;
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"INCRBY")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from((-delta).to_string())),
        ]),
    )
    .await;
    Ok(Frame::Integer(result))
}

pub async fn incrbyfloat(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "INCRBYFLOAT")?, "INCRBYFLOAT")?;
    let delta = parse_decimal(cmd.arg(1, "INCRBYFLOAT")?)?;
    let result = store.lock().await.incr_by_float(&key, delta)?;
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from(result.clone())),
        ]),
    )
    .await;
    Ok(Frame::Bulk(Bytes::from(result)))
}

fn parse_decimal(b: &Bytes) -> Result<rust_decimal::Decimal> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::NotInteger)
}
