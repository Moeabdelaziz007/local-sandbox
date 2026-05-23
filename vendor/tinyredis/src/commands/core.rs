use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_u64, to_str};
use crate::{
    error::{Error, Result},
    parser::{Command, Frame},
    persistence::AofSender,
    store::{Store, Value},
};

type SharedStore = Arc<Mutex<Store>>;

// PING
pub fn ping(cmd: &Command) -> Result<Frame> {
    match cmd.args.first() {
        Some(msg) => Ok(Frame::Bulk(msg.clone())),
        None => Ok(Frame::Simple("PONG".into())),
    }
}
// ECHO
pub fn echo(cmd: &Command) -> Result<Frame> {
    let msg = cmd.arg(0, "ECHO")?;
    Ok(Frame::Bulk(msg.clone()))
}

// SET
pub async fn set(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "SET")?, "SET")?;
    let value = cmd.arg(1, "SET")?.clone();

    // Parse options: EX, PX, EXAT, PXAT, NX, XX
    let mut ttl: Option<Duration> = None;
    let mut nx = false;
    let mut xx = false;

    let mut i = 2;
    while i < cmd.args.len() {
        match cmd.args[i].to_ascii_uppercase().as_slice() {
            b"EX" => {
                i += 1;
                let n = parse_u64(cmd.arg(i, "SET")?)?;
                ttl = Some(Duration::from_secs(n));
            }
            b"PX" => {
                i += 1;
                let n = parse_u64(cmd.arg(i, "SET")?)?;
                ttl = Some(Duration::from_millis(n));
            }
            b"EXAT" => {
                i += 1;
                let epoch_secs = parse_u64(cmd.arg(i, "SET")?)?;
                let now = now_secs();
                ttl = Some(Duration::from_secs(epoch_secs.saturating_sub(now)));
            }
            b"PXAT" => {
                i += 1;
                let epoch_ms = parse_u64(cmd.arg(i, "SET")?)?;
                let now = now_ms();
                ttl = Some(Duration::from_millis(epoch_ms.saturating_sub(now)));
            }
            b"NX" => nx = true,
            b"XX" => xx = true,
            _ => return Err(Error::Syntax),
        }
        i += 1;
    }

    let mut store = store.lock().await;

    let exists = store.get(&key).is_some();
    if nx && exists {
        return Ok(Frame::Null);
    }
    if xx && !exists {
        return Ok(Frame::Null);
    }

    store.set(&key.clone(), Value::Str(value.clone()), ttl);
    drop(store);

    let mut aof_frames = vec![
        Frame::Bulk(Bytes::from_static(b"SET")),
        Frame::Bulk(Bytes::from(key)),
        Frame::Bulk(value),
    ];
    if let Some(d) = ttl {
        let epoch_ms = now_ms() + d.as_millis() as u64;
        aof_frames.push(Frame::Bulk(Bytes::from_static(b"PXAT")));
        aof_frames.push(Frame::Bulk(Bytes::from(epoch_ms.to_string())));
    }
    aof_log(aof, Frame::Array(aof_frames)).await;

    Ok(ok())
}

pub async fn setex(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = cmd.arg(0, "SETEX")?.clone();
    let seconds = cmd.arg(1, "SETEX")?.clone();
    let value = cmd.arg(2, "SETEX")?.clone();
    set(
        &Command {
            name: "SET".to_string(),
            args: vec![key, value, Bytes::from_static(b"EX"), seconds],
        },
        store,
        aof,
    )
    .await
}

pub async fn get(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "GET")?, "GET")?;
    let mut store = store.lock().await;
    match store.get(&key) {
        None => Ok(Frame::Null),
        Some(entry) => match &entry.value {
            Value::Str(b) => Ok(Frame::Bulk(b.clone())),
            _ => todo!(),
        },
    }
}

pub async fn del(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("DEL"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "DEL"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.del(&keys);

    if count > 0 {
        let mut frames = vec![Frame::Bulk(Bytes::from_static(b"DEL"))];
        frames.extend(
            keys.iter()
                .map(|k: &String| Frame::Bulk(Bytes::from(k.clone()))),
        );
        aof_log(aof, Frame::Array(frames)).await;
    }

    Ok(Frame::Integer(count as i64))
}

pub async fn exists(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("EXISTS"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "EXISTS"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.exists(&keys);
    Ok(Frame::Integer(count as i64))
}

pub async fn expire(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "EXPIRE")?, "EXPIRE")?;
    let secs = parse_u64(cmd.arg(1, "EXPIRE")?)?;
    let nx = cmd
        .args
        .get(2)
        .is_some_and(|arg| arg.eq_ignore_ascii_case(b"NX"));

    let set = {
        let mut store = store.lock().await;
        if nx && store.ttl(&key) != -1 {
            false
        } else {
            store.expire(&key, Duration::from_secs(secs))
        }
    };

    if set {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"EXPIREAT")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from((now_secs() + secs).to_string())),
            ]),
        )
        .await;
    }

    Ok(Frame::Integer(if set { 1 } else { 0 }))
}

pub async fn ttl(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "TTL")?, "TTL")?;
    Ok(Frame::Integer(store.lock().await.ttl(&key)))
}

pub async fn pttl(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "PTTL")?, "PTTL")?;
    Ok(Frame::Integer(store.lock().await.pttl(&key)))
}

pub async fn persist(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "PERSIST")?, "PERSIST")?;
    let removed = store.lock().await.persist(&key);
    if removed {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PERSIST")),
                Frame::Bulk(Bytes::from(key)),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(if removed { 1 } else { 0 }))
}

pub async fn pexpire(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "PEXPIRE")?, "PEXPIRE")?;
    let ms = parse_u64(cmd.arg(1, "PEXPIRE")?)?;
    let set = store.lock().await.expire(&key, Duration::from_millis(ms));
    if set {
        let epoch_ms = now_ms() + ms;
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(epoch_ms.to_string())),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(if set { 1 } else { 0 }))
}

pub async fn expireat(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "EXPIREAT")?, "EXPIREAT")?;
    let epoch_secs = parse_u64(cmd.arg(1, "EXPIREAT")?)?;
    let set = apply_expireat(store, &key, epoch_secs).await;
    if set {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"EXPIREAT")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(epoch_secs.to_string())),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(if set { 1 } else { 0 }))
}

pub async fn pexpireat(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "PEXPIREAT")?, "PEXPIREAT")?;
    let epoch_ms = parse_u64(cmd.arg(1, "PEXPIREAT")?)?;
    let set = apply_pexpireat(store, &key, epoch_ms).await;
    if set {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(epoch_ms.to_string())),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(if set { 1 } else { 0 }))
}

// Shared logic for EXPIREAT: past timestamps immediately delete the key.
async fn apply_expireat(store: &SharedStore, key: &str, epoch_secs: u64) -> bool {
    let now = now_secs();
    let mut s = store.lock().await;
    if epoch_secs > now {
        s.expire(key, Duration::from_secs(epoch_secs - now))
    } else {
        let existed = s.get(key).is_some();
        if existed {
            s.del(&[key.to_string()]);
        }
        existed
    }
}

async fn apply_pexpireat(store: &SharedStore, key: &str, epoch_ms: u64) -> bool {
    let now = now_ms();
    let mut s = store.lock().await;
    if epoch_ms > now {
        s.expire(key, Duration::from_millis(epoch_ms - now))
    } else {
        let existed = s.get(key).is_some();
        if existed {
            s.del(&[key.to_string()]);
        }
        existed
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn type_cmd(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "TYPE")?, "TYPE")?;
    Ok(Frame::Simple(store.lock().await.type_of(&key).into()))
}

// parse helpers

// Helpers
fn ok() -> Frame {
    Frame::Simple("OK".into())
}
