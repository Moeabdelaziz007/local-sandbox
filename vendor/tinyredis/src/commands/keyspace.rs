use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::commands::utils::{aof_log, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::stats::SharedStats;
use crate::store::Store;
use bytes::Bytes;
use tokio::sync::Mutex;

type SharedStore = Arc<Mutex<Store>>;

pub async fn keys(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let pattern = to_str(cmd.arg(0, "KEYS")?, "KEYS")?;
    let mut keys = store.lock().await.keys(&pattern);
    keys.sort(); // deterministic order
    Ok(Frame::Array(
        keys.into_iter()
            .map(|k| Frame::Bulk(Bytes::from(k)))
            .collect(),
    ))
}

/// SCAN cursor [MATCH pattern] [COUNT count]
/// Always completes in one call (returns cursor "0").
pub async fn scan(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    // cursor argument is accepted but ignored — we always do a full scan
    let _ = cmd.arg(0, "SCAN")?;

    let mut pattern = "*".to_string();
    let mut i = 1;
    while i < cmd.args.len() {
        match cmd.args[i].to_ascii_uppercase().as_slice() {
            b"MATCH" => {
                i += 1;
                pattern = to_str(cmd.arg(i, "SCAN")?, "SCAN")?;
            }
            b"COUNT" => {
                i += 1; // accept and ignore
            }
            _ => return Err(Error::Syntax),
        }
        i += 1;
    }

    let mut keys = store.lock().await.keys(&pattern);
    keys.sort();
    let elements: Vec<Frame> = keys
        .into_iter()
        .map(|k| Frame::Bulk(Bytes::from(k)))
        .collect();
    Ok(Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"0")),
        Frame::Array(elements),
    ]))
}

pub async fn dbsize(store: &SharedStore) -> Result<Frame> {
    Ok(Frame::Integer(store.lock().await.dbsize() as i64))
}

pub async fn rename(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "RENAME")?, "RENAME")?;
    let newkey = to_str(cmd.arg(1, "RENAME")?, "RENAME")?;
    store.lock().await.rename(&key, &newkey)?;
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"RENAME")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from(newkey)),
        ]),
    )
    .await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn flushdb(store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    store.lock().await.flushdb();
    aof_log(
        aof,
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"FLUSHDB"))]),
    )
    .await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn flushall(store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    store.lock().await.flushdb();
    aof_log(
        aof,
        Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"FLUSHALL"))]),
    )
    .await;
    Ok(Frame::Simple("OK".into()))
}

pub async fn bgrewriteaof(stats: &SharedStats) -> Result<Frame> {
    match &stats.rewrite_tx {
        None => Ok(Frame::Error(
            "ERR AOF is not enabled or rewrite channel unavailable".into(),
        )),
        Some(tx) => {
            if stats
                .aof_rewrite_in_progress
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
            {
                return Ok(Frame::Error(
                    "ERR Background append only file rewriting already in progress".into(),
                ));
            }
            // Best-effort send; if the channel is full the rewrite is already queued.
            if tx.try_send(()).is_err() {
                stats
                    .aof_rewrite_in_progress
                    .store(false, Ordering::Relaxed);
                return Ok(Frame::Error(
                    "ERR Background rewrite already scheduled".into(),
                ));
            }
            Ok(Frame::Simple(
                "Background append only file rewriting started".into(),
            ))
        }
    }
}
