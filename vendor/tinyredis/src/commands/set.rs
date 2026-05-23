use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_i64, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::Store;

type SharedStore = Arc<Mutex<Store>>;

pub async fn sadd(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SADD"));
    }
    let key = to_str(cmd.arg(0, "SADD")?, "SADD")?;
    let members: Vec<Bytes> = cmd.args[1..].to_vec();

    let added = store.lock().await.sadd(&key, &members)?;

    let mut parts = vec![
        Frame::Bulk(Bytes::from_static(b"SADD")),
        Frame::Bulk(Bytes::from(key)),
    ];
    parts.extend(members.into_iter().map(Frame::Bulk));
    aof_log(aof, Frame::Array(parts)).await;

    Ok(Frame::Integer(added as i64))
}

pub async fn srem(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SREM"));
    }
    let key = to_str(cmd.arg(0, "SREM")?, "SREM")?;
    let members: Vec<Bytes> = cmd.args[1..].to_vec();

    let removed = store.lock().await.srem(&key, &members)?;

    if removed > 0 {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"SREM")),
            Frame::Bulk(Bytes::from(key)),
        ];
        parts.extend(members.into_iter().map(Frame::Bulk));
        aof_log(aof, Frame::Array(parts)).await;
    }
    Ok(Frame::Integer(removed as i64))
}

pub async fn sismember(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SISMEMBER"));
    }
    let key = to_str(cmd.arg(0, "SISMEMBER")?, "SISMEMBER")?;
    let member = cmd.arg(1, "SISMEMBER")?.clone();
    Ok(Frame::Integer(
        store.lock().await.sismember(&key, &member)? as i64
    ))
}

pub async fn smismember(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SMISMEMBER"));
    }
    let key = to_str(cmd.arg(0, "SMISMEMBER")?, "SMISMEMBER")?;
    let members: Vec<Bytes> = cmd.args[1..].to_vec();
    let results = store.lock().await.smismember(&key, &members)?;
    Ok(Frame::Array(
        results
            .into_iter()
            .map(|b| Frame::Integer(b as i64))
            .collect(),
    ))
}

pub async fn smembers(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "SMEMBERS")?, "SMEMBERS")?;
    let members = store.lock().await.smembers(&key)?;
    Ok(Frame::Array(members.into_iter().map(Frame::Bulk).collect()))
}

pub async fn scard(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "SCARD")?, "SCARD")?;
    Ok(Frame::Integer(store.lock().await.scard(&key)? as i64))
}

pub async fn srandmember(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("SRANDMEMBER"));
    }
    let key = to_str(cmd.arg(0, "SRANDMEMBER")?, "SRANDMEMBER")?;

    if cmd.args.len() == 1 {
        // No count — return single element or nil
        let mut items = store.lock().await.srandmember(&key, 1)?;
        return match items.pop() {
            None => Ok(Frame::Null),
            Some(m) => Ok(Frame::Bulk(m)),
        };
    }

    let count = parse_i64(cmd.arg(1, "SRANDMEMBER")?)?;
    let items = store.lock().await.srandmember(&key, count)?;
    Ok(Frame::Array(items.into_iter().map(Frame::Bulk).collect()))
}

pub async fn spop(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("SPOP"));
    }
    let key = to_str(cmd.arg(0, "SPOP")?, "SPOP")?;
    let with_count = cmd.args.len() > 1;
    let count = if with_count {
        parse_i64(cmd.arg(1, "SPOP")?)? as usize
    } else {
        1
    };

    let items = store.lock().await.spop(&key, count)?;

    if !items.is_empty() {
        // AOF: log as SREM key member [...]
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"SREM")),
            Frame::Bulk(Bytes::from(key)),
        ];
        parts.extend(items.iter().cloned().map(Frame::Bulk));
        aof_log(aof, Frame::Array(parts)).await;
    }

    if with_count {
        Ok(Frame::Array(items.into_iter().map(Frame::Bulk).collect()))
    } else {
        match items.into_iter().next() {
            None => Ok(Frame::Null),
            Some(m) => Ok(Frame::Bulk(m)),
        }
    }
}

pub async fn sunion(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("SUNION"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "SUNION"))
        .collect::<Result<_>>()?;
    let members = store.lock().await.sunion(&keys)?;
    Ok(Frame::Array(members.into_iter().map(Frame::Bulk).collect()))
}

pub async fn sinter(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("SINTER"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "SINTER"))
        .collect::<Result<_>>()?;
    let members = store.lock().await.sinter(&keys)?;
    Ok(Frame::Array(members.into_iter().map(Frame::Bulk).collect()))
}

pub async fn sdiff(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("SDIFF"));
    }
    let keys: Vec<String> = cmd
        .args
        .iter()
        .map(|b| to_str(b, "SDIFF"))
        .collect::<Result<_>>()?;
    let members = store.lock().await.sdiff(&keys)?;
    Ok(Frame::Array(members.into_iter().map(Frame::Bulk).collect()))
}

pub async fn sunionstore(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SUNIONSTORE"));
    }
    let dest = to_str(cmd.arg(0, "SUNIONSTORE")?, "SUNIONSTORE")?;
    let keys: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "SUNIONSTORE"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.sunionstore(&dest, &keys)?;
    aof_log_store_result(aof, &dest, store, count).await;
    Ok(Frame::Integer(count as i64))
}

pub async fn sinterstore(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SINTERSTORE"));
    }
    let dest = to_str(cmd.arg(0, "SINTERSTORE")?, "SINTERSTORE")?;
    let keys: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "SINTERSTORE"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.sinterstore(&dest, &keys)?;
    aof_log_store_result(aof, &dest, store, count).await;
    Ok(Frame::Integer(count as i64))
}

pub async fn sdiffstore(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("SDIFFSTORE"));
    }
    let dest = to_str(cmd.arg(0, "SDIFFSTORE")?, "SDIFFSTORE")?;
    let keys: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "SDIFFSTORE"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.sdiffstore(&dest, &keys)?;
    aof_log_store_result(aof, &dest, store, count).await;
    Ok(Frame::Integer(count as i64))
}

pub async fn smove(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("SMOVE"));
    }
    let src = to_str(cmd.arg(0, "SMOVE")?, "SMOVE")?;
    let dst = to_str(cmd.arg(1, "SMOVE")?, "SMOVE")?;
    let member = cmd.arg(2, "SMOVE")?.clone();

    let moved = store.lock().await.smove(&src, &dst, &member)?;
    if moved {
        // Log as SREM src member + SADD dst member
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SREM")),
                Frame::Bulk(Bytes::from(src)),
                Frame::Bulk(member.clone()),
            ]),
        )
        .await;
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SADD")),
                Frame::Bulk(Bytes::from(dst)),
                Frame::Bulk(member),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(moved as i64))
}

/// AOF for SUNIONSTORE/SINTERSTORE/SDIFFSTORE: DEL dest + SADD dest members.
async fn aof_log_store_result(
    aof: &Option<AofSender>,
    dest: &str,
    store: &SharedStore,
    count: usize,
) {
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"DEL")),
            Frame::Bulk(Bytes::from(dest.to_string())),
        ]),
    )
    .await;
    if count == 0 {
        return;
    }
    let members = store.lock().await.smembers(dest).unwrap_or_default();
    if !members.is_empty() {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"SADD")),
            Frame::Bulk(Bytes::from(dest.to_string())),
        ];
        parts.extend(members.into_iter().map(Frame::Bulk));
        aof_log(aof, Frame::Array(parts)).await;
    }
}
