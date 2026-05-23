use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_i64, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::{ListDirection, Store};

type SharedStore = Arc<Mutex<Store>>;

pub async fn lpush(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("LPUSH"));
    }
    let key = to_str(cmd.arg(0, "LPUSH")?, "LPUSH")?;
    let values: Vec<Bytes> = cmd.args[1..].to_vec();

    let len = store.lock().await.lpush(&key, &values)?;

    aof_log(aof, build_frame("LPUSH", &key, &values)).await;
    Ok(Frame::Integer(len as i64))
}

pub async fn rpush(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("RPUSH"));
    }
    let key = to_str(cmd.arg(0, "RPUSH")?, "RPUSH")?;
    let values: Vec<Bytes> = cmd.args[1..].to_vec();

    let len = store.lock().await.rpush(&key, &values)?;

    aof_log(aof, build_frame("RPUSH", &key, &values)).await;
    Ok(Frame::Integer(len as i64))
}

pub async fn lpop(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("LPOP"));
    }
    let key = to_str(cmd.arg(0, "LPOP")?, "LPOP")?;
    let with_count = cmd.args.len() > 1;
    let count = if with_count {
        parse_i64(cmd.arg(1, "LPOP")?)
            .map_err(|_| Error::Protocol("value is not an integer or out of range".into()))
            .and_then(|n| {
                if n < 0 {
                    Err(Error::Protocol(
                        "value is not an integer or out of range".into(),
                    ))
                } else {
                    Ok(n as usize)
                }
            })?
    } else {
        1
    };

    let result = store.lock().await.lpop(&key, count)?;

    match result {
        None => Ok(Frame::Null),
        Some(mut items) => {
            aof_log(
                aof,
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"LPOP")),
                    Frame::Bulk(Bytes::from(key)),
                    Frame::Integer(items.len() as i64),
                ]),
            )
            .await;
            if with_count {
                Ok(Frame::Array(items.into_iter().map(Frame::Bulk).collect()))
            } else {
                Ok(Frame::Bulk(items.remove(0)))
            }
        }
    }
}

pub async fn rpop(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("RPOP"));
    }
    let key = to_str(cmd.arg(0, "RPOP")?, "RPOP")?;
    let with_count = cmd.args.len() > 1;
    let count = if with_count {
        parse_i64(cmd.arg(1, "RPOP")?)
            .map_err(|_| Error::Protocol("value is not an integer or out of range".into()))
            .and_then(|n| {
                if n < 0 {
                    Err(Error::Protocol(
                        "value is not an integer or out of range".into(),
                    ))
                } else {
                    Ok(n as usize)
                }
            })?
    } else {
        1
    };

    let result = store.lock().await.rpop(&key, count)?;

    match result {
        None => Ok(Frame::Null),
        Some(mut items) => {
            aof_log(
                aof,
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"RPOP")),
                    Frame::Bulk(Bytes::from(key)),
                    Frame::Integer(items.len() as i64),
                ]),
            )
            .await;
            if with_count {
                Ok(Frame::Array(items.into_iter().map(Frame::Bulk).collect()))
            } else {
                Ok(Frame::Bulk(items.remove(0)))
            }
        }
    }
}

pub async fn llen(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "LLEN")?, "LLEN")?;
    let len = store.lock().await.llen(&key)?;
    Ok(Frame::Integer(len as i64))
}

pub async fn lrange(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("LRANGE"));
    }
    let key = to_str(cmd.arg(0, "LRANGE")?, "LRANGE")?;
    let start = parse_i64(cmd.arg(1, "LRANGE")?)?;
    let stop = parse_i64(cmd.arg(2, "LRANGE")?)?;

    let items = store.lock().await.lrange(&key, start, stop)?;
    Ok(Frame::Array(items.into_iter().map(Frame::Bulk).collect()))
}

pub async fn lindex(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("LINDEX"));
    }
    let key = to_str(cmd.arg(0, "LINDEX")?, "LINDEX")?;
    let index = parse_i64(cmd.arg(1, "LINDEX")?)?;

    match store.lock().await.lindex(&key, index)? {
        None => Ok(Frame::Null),
        Some(v) => Ok(Frame::Bulk(v)),
    }
}

pub async fn lset(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("LSET"));
    }
    let key = to_str(cmd.arg(0, "LSET")?, "LSET")?;
    let index = parse_i64(cmd.arg(1, "LSET")?)?;
    let value = cmd.arg(2, "LSET")?.clone();

    store.lock().await.lset(&key, index, value.clone())?;

    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"LSET")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Integer(index),
            Frame::Bulk(value),
        ]),
    )
    .await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn linsert(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 4 {
        return Err(Error::WrongArity("LINSERT"));
    }
    let key = to_str(cmd.arg(0, "LINSERT")?, "LINSERT")?;
    let where_str = to_str(cmd.arg(1, "LINSERT")?, "LINSERT")?.to_uppercase();
    let before = match where_str.as_str() {
        "BEFORE" => true,
        "AFTER" => false,
        _ => return Err(Error::Protocol("syntax error".into())),
    };
    let pivot = cmd.arg(2, "LINSERT")?.clone();
    let element = cmd.arg(3, "LINSERT")?.clone();

    let result = store
        .lock()
        .await
        .linsert(&key, before, &pivot, element.clone())?;

    if result > 0 {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"LINSERT")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(where_str)),
                Frame::Bulk(pivot),
                Frame::Bulk(element),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(result))
}

pub async fn lrem(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("LREM"));
    }
    let key = to_str(cmd.arg(0, "LREM")?, "LREM")?;
    let count = parse_i64(cmd.arg(1, "LREM")?)?;
    let element = cmd.arg(2, "LREM")?.clone();

    let removed = store.lock().await.lrem(&key, count, &element)?;

    if removed > 0 {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"LREM")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Integer(count),
                Frame::Bulk(element),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(removed as i64))
}

pub async fn ltrim(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("LTRIM"));
    }
    let key = to_str(cmd.arg(0, "LTRIM")?, "LTRIM")?;
    let start = parse_i64(cmd.arg(1, "LTRIM")?)?;
    let stop = parse_i64(cmd.arg(2, "LTRIM")?)?;

    store.lock().await.ltrim(&key, start, stop)?;

    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"LTRIM")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Integer(start),
            Frame::Integer(stop),
        ]),
    )
    .await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn lmove(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 4 {
        return Err(Error::WrongArity("LMOVE"));
    }
    let src = to_str(cmd.arg(0, "LMOVE")?, "LMOVE")?;
    let dst = to_str(cmd.arg(1, "LMOVE")?, "LMOVE")?;
    let wherefrom = parse_direction(cmd.arg(2, "LMOVE")?)?;
    let whereto = parse_direction(cmd.arg(3, "LMOVE")?)?;

    let result = store.lock().await.lmove(&src, &dst, wherefrom, whereto)?;

    match result {
        None => Ok(Frame::Null),
        Some(element) => {
            aof_log(
                aof,
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"LMOVE")),
                    Frame::Bulk(Bytes::from(src)),
                    Frame::Bulk(Bytes::from(dst)),
                    Frame::Bulk(Bytes::from(direction_str(wherefrom))),
                    Frame::Bulk(Bytes::from(direction_str(whereto))),
                ]),
            )
            .await;
            Ok(Frame::Bulk(element))
        }
    }
}

fn parse_direction(b: &Bytes) -> Result<ListDirection> {
    match b.to_ascii_uppercase().as_slice() {
        b"LEFT" => Ok(ListDirection::Left),
        b"RIGHT" => Ok(ListDirection::Right),
        _ => Err(Error::Protocol("syntax error".into())),
    }
}

fn direction_str(d: ListDirection) -> &'static str {
    match d {
        ListDirection::Left => "LEFT",
        ListDirection::Right => "RIGHT",
    }
}

fn build_frame(cmd_name: &'static str, key: &str, values: &[Bytes]) -> Frame {
    let mut parts = vec![
        Frame::Bulk(Bytes::from_static(cmd_name.as_bytes())),
        Frame::Bulk(Bytes::from(key.to_string())),
    ];
    parts.extend(values.iter().cloned().map(Frame::Bulk));
    Frame::Array(parts)
}
