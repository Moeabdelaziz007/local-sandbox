use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::commands::zset::parse_score;
use crate::config::FsyncPolicy;
use crate::error::Result;
use crate::parser::{Command, Frame};
use crate::stats::SharedStats;
use crate::store::{ListDirection, Store, Value, ZAddOpts};

pub type AofSender = mpsc::Sender<AofEntry>;

pub struct AofEntry {
    pub raw: bytes::Bytes, // pre-serialized RESP2
}

pub struct AofWriter {
    path: PathBuf,
    rx: mpsc::Receiver<AofEntry>,
    file: tokio::fs::File,
    shutdown: broadcast::Receiver<()>,
    store: Arc<Mutex<Store>>,
    rewrite_rx: mpsc::Receiver<()>,
    stats: SharedStats,
    fsync_policy: FsyncPolicy,
    /// Dirty flag for the `everysec` policy — set after each write, cleared after fsync.
    needs_fsync: bool,
}

impl AofWriter {
    pub async fn new(
        path: &Path,
        rx: mpsc::Receiver<AofEntry>,
        shutdown: broadcast::Receiver<()>,
        store: Arc<Mutex<Store>>,
        rewrite_rx: mpsc::Receiver<()>,
        stats: SharedStats,
        fsync_policy: FsyncPolicy,
    ) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        Ok(AofWriter {
            path: path.to_path_buf(),
            rx,
            file,
            shutdown,
            store,
            rewrite_rx,
            stats,
            fsync_policy,
            needs_fsync: false,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        let mut fsync_ticker = tokio::time::interval(std::time::Duration::from_secs(1));
        // The first tick fires immediately; skip it so we don't call sync_data at start.
        fsync_ticker.tick().await;

        loop {
            tokio::select! {
                Some(entry) = self.rx.recv() => {
                    self.file.write_all(&entry.raw).await?;
                    match self.fsync_policy {
                        FsyncPolicy::Always => {
                            self.file.sync_data().await?;
                        }
                        FsyncPolicy::EverySecond => {
                            self.needs_fsync = true;
                        }
                        FsyncPolicy::No => {}
                    }
                }
                _ = fsync_ticker.tick() => {
                    if self.fsync_policy == FsyncPolicy::EverySecond && self.needs_fsync {
                        self.file.sync_data().await?;
                        self.needs_fsync = false;
                    }
                }
                Some(()) = self.rewrite_rx.recv() => {
                    if let Err(e) = self.do_rewrite().await {
                        tracing::error!("BGREWRITEAOF failed: {e}");
                    }
                    self.stats.aof_rewrite_in_progress.store(false, Ordering::Relaxed);
                }
                _ = self.shutdown.recv() => {
                    // Drain any remaining entries before exiting.
                    while let Ok(entry) = self.rx.try_recv() {
                        self.file.write_all(&entry.raw).await?;
                    }
                    // Final fsync on shutdown regardless of policy.
                    self.file.sync_data().await?;
                    return Ok(());
                }
            }
        }
    }

    /// Compact the AOF: snapshot the store, append buffered entries, rename into place.
    async fn do_rewrite(&mut self) -> Result<()> {
        // 1. Snapshot current store state while holding the lock briefly.
        let snapshot = self.store.lock().await.snapshot_aof_bytes();

        // 2. Write snapshot + any entries buffered during snapshot to a temp file.
        let tmp = self.path.with_extension("aof.tmp");
        let mut tmp_file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .await?;
        tmp_file.write_all(&snapshot).await?;

        // Drain any entries that arrived while we were snapshotting.
        while let Ok(entry) = self.rx.try_recv() {
            tmp_file.write_all(&entry.raw).await?;
        }
        // Ensure the rewritten file is durable before we rename it into place.
        tmp_file.sync_data().await?;
        drop(tmp_file);

        // 3. Atomically replace the AOF file (POSIX rename is atomic).
        tokio::fs::rename(&tmp, &self.path).await?;

        // 4. Reopen self.file to point at the new (compacted) file.
        self.file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        tracing::info!("BGREWRITEAOF: rewrite completed successfully");
        Ok(())
    }
}

/// Replay the AOF file into the store at startup.
/// If the file does not exist, returns Ok(()) immediately.
pub async fn replay(path: &Path, store: &Arc<Mutex<Store>>) -> Result<()> {
    let contents = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut buf = BytesMut::from(&contents[..]);
    let mut store = store.lock().await;

    loop {
        if buf.is_empty() {
            break;
        }

        match Frame::parse(&buf)? {
            None => break, // incomplete trailing data — stop
            Some((frame, consumed)) => {
                // Advance past the consumed bytes.
                let _ = buf.split_to(consumed);

                let cmd = match Command::from_frame(frame) {
                    Ok(c) => c,
                    Err(_) => continue, // skip unrecognised frames defensively
                };

                apply_to_store(&mut store, cmd);
            }
        }
    }

    Ok(())
}

/// Apply a replayed command directly to the store (no AOF re-logging).
fn apply_to_store(store: &mut Store, cmd: Command) {
    match cmd.name.as_str() {
        "SET" => {
            if cmd.args.len() < 2 {
                return;
            }
            let key = match std::str::from_utf8(&cmd.args[0]) {
                Ok(s) => s.to_string(),
                Err(_) => return,
            };
            let value = Value::Str(cmd.args[1].clone());

            let ttl = match parse_ttl_from_args(&cmd.args[2..]) {
                ReplayTtl::None => None,
                ReplayTtl::Expired => return,
                ReplayTtl::Remaining(d) => Some(d),
            };
            store.set(&key, value, ttl);
        }
        "DEL" => {
            let keys: Vec<String> = cmd
                .args
                .iter()
                .filter_map(|b| std::str::from_utf8(b).ok().map(String::from))
                .collect();
            store.del(&keys);
        }
        "EXPIRE" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(secs_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(secs) = secs_str.parse::<u64>()
            {
                store.expire(key, std::time::Duration::from_secs(secs));
            }
        }
        "EXPIREAT" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(epoch_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(epoch_secs) = epoch_str.parse::<u64>()
            {
                match remaining_from_epoch_secs(epoch_secs) {
                    ReplayTtl::Remaining(d) => {
                        store.expire(key, d);
                    }
                    ReplayTtl::Expired => {
                        store.del(&[key.to_string()]);
                    }
                    ReplayTtl::None => {}
                }
            }
        }
        "PEXPIREAT" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(epoch_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(epoch_ms) = epoch_str.parse::<u64>()
            {
                match remaining_from_epoch_ms(epoch_ms) {
                    ReplayTtl::Remaining(d) => {
                        store.expire(key, d);
                    }
                    ReplayTtl::Expired => {
                        store.del(&[key.to_string()]);
                    }
                    ReplayTtl::None => {}
                }
            }
        }
        "PERSIST" => {
            if let Some(key_bytes) = cmd.args.first()
                && let Ok(key) = std::str::from_utf8(key_bytes)
            {
                store.persist(key);
            }
        }
        "APPEND" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let _ = store.append(key, &cmd.args[1]);
            }
        }
        "INCR" => {
            if let Some(key_bytes) = cmd.args.first()
                && let Ok(key) = std::str::from_utf8(key_bytes)
            {
                let _ = store.incr_by(key, 1);
            }
        }
        "INCRBY" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(delta_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(delta) = delta_str.parse::<i64>()
            {
                let _ = store.incr_by(key, delta);
            }
        }
        "MSET" => {
            let pairs: Vec<(String, bytes::Bytes)> = cmd
                .args
                .chunks_exact(2)
                .filter_map(|chunk| {
                    let key = std::str::from_utf8(&chunk[0]).ok()?.to_string();
                    Some((key, chunk[1].clone()))
                })
                .collect();
            store.mset(pairs);
        }
        "RENAME" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(newkey)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) {
                let _ = store.rename(key, newkey);
            }
        }
        "FLUSHDB" | "FLUSHALL" => {
            store.flushdb();
        }
        "LPUSH" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let values: Vec<bytes::Bytes> = cmd.args[1..].to_vec();
                let _ = store.lpush(key, &values);
            }
        }
        "RPUSH" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let values: Vec<bytes::Bytes> = cmd.args[1..].to_vec();
                let _ = store.rpush(key, &values);
            }
        }
        "LPOP" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(count_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(count) = count_str.parse::<usize>()
            {
                let _ = store.lpop(key, count);
            }
        }
        "RPOP" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let (Ok(key), Ok(count_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(count) = count_str.parse::<usize>()
            {
                let _ = store.rpop(key, count);
            }
        }
        "LSET" => {
            if cmd.args.len() < 3 {
                return;
            }
            if let (Ok(key), Ok(idx_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(idx) = idx_str.parse::<i64>()
            {
                let _ = store.lset(key, idx, cmd.args[2].clone());
            }
        }
        "LINSERT" => {
            if cmd.args.len() < 4 {
                return;
            }
            if let (Ok(key), Ok(where_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) {
                let before = where_str.eq_ignore_ascii_case("BEFORE");
                let pivot = cmd.args[2].clone();
                let element = cmd.args[3].clone();
                let _ = store.linsert(key, before, &pivot, element);
            }
        }
        "LREM" => {
            if cmd.args.len() < 3 {
                return;
            }
            if let (Ok(key), Ok(count_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
            ) && let Ok(count) = count_str.parse::<i64>()
            {
                let _ = store.lrem(key, count, &cmd.args[2]);
            }
        }
        "LTRIM" => {
            if cmd.args.len() < 3 {
                return;
            }
            if let (Ok(key), Ok(start_str), Ok(stop_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
                std::str::from_utf8(&cmd.args[2]),
            ) && let (Ok(start), Ok(stop)) = (start_str.parse::<i64>(), stop_str.parse::<i64>())
            {
                let _ = store.ltrim(key, start, stop);
            }
        }
        "LMOVE" => {
            if cmd.args.len() < 4 {
                return;
            }
            if let (Ok(src), Ok(dst), Ok(from_str), Ok(to_str)) = (
                std::str::from_utf8(&cmd.args[0]),
                std::str::from_utf8(&cmd.args[1]),
                std::str::from_utf8(&cmd.args[2]),
                std::str::from_utf8(&cmd.args[3]),
            ) {
                let wherefrom = if from_str.eq_ignore_ascii_case("LEFT") {
                    ListDirection::Left
                } else {
                    ListDirection::Right
                };
                let whereto = if to_str.eq_ignore_ascii_case("LEFT") {
                    ListDirection::Left
                } else {
                    ListDirection::Right
                };
                let _ = store.lmove(src, dst, wherefrom, whereto);
            }
        }
        "SADD" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let members: Vec<bytes::Bytes> = cmd.args[1..].to_vec();
                let _ = store.sadd(key, &members);
            }
        }
        "SREM" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let members: Vec<bytes::Bytes> = cmd.args[1..].to_vec();
                let _ = store.srem(key, &members);
            }
        }
        "ZADD" => {
            if cmd.args.len() < 3 || !(cmd.args.len() - 1).is_multiple_of(2) {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let mut entries: Vec<(f64, String)> = Vec::new();
                let mut i = 1;
                let mut valid = true;
                while i + 1 < cmd.args.len() {
                    match (
                        parse_score(&cmd.args[i]),
                        std::str::from_utf8(&cmd.args[i + 1]),
                    ) {
                        (Ok(score), Ok(member)) => {
                            entries.push((score, member.to_string()));
                            i += 2;
                        }
                        _ => {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid {
                    let _ = store.zadd(key, &entries, &ZAddOpts::default());
                }
            }
        }
        "ZREM" => {
            if cmd.args.len() < 2 {
                return;
            }
            if let Ok(key) = std::str::from_utf8(&cmd.args[0]) {
                let members: Vec<String> = cmd.args[1..]
                    .iter()
                    .filter_map(|b| std::str::from_utf8(b).ok().map(String::from))
                    .collect();
                let _ = store.zrem(key, &members);
            }
        }
        "HSET" => {
            // HSET key field value [field value ...]
            if cmd.args.len() < 3 || !(cmd.args.len() - 1).is_multiple_of(2) {
                return;
            }
            let key = match std::str::from_utf8(&cmd.args[0]) {
                Ok(s) => s,
                Err(_) => return,
            };
            let pairs: Vec<(String, bytes::Bytes)> = cmd.args[1..]
                .chunks_exact(2)
                .filter_map(|chunk| {
                    let field = std::str::from_utf8(&chunk[0]).ok()?.to_string();
                    Some((field, chunk[1].clone()))
                })
                .collect();
            let _ = store.hset(key, pairs);
        }
        "HDEL" => {
            if cmd.args.len() < 2 {
                return;
            }
            let key = match std::str::from_utf8(&cmd.args[0]) {
                Ok(s) => s,
                Err(_) => return,
            };
            let fields: Vec<String> = cmd.args[1..]
                .iter()
                .filter_map(|b| std::str::from_utf8(b).ok().map(String::from))
                .collect();
            let _ = store.hdel(key, &fields);
        }
        _ => {}
    }
}

enum ReplayTtl {
    None,
    Expired,
    Remaining(std::time::Duration),
}

fn parse_ttl_from_args(args: &[bytes::Bytes]) -> ReplayTtl {
    let mut i = 0;
    while i + 1 < args.len() {
        match args[i].to_ascii_uppercase().as_slice() {
            b"EX" => {
                if let Ok(s) = std::str::from_utf8(&args[i + 1])
                    && let Ok(n) = s.parse::<u64>()
                {
                    return ReplayTtl::Remaining(std::time::Duration::from_secs(n));
                }
            }
            b"PX" => {
                if let Ok(s) = std::str::from_utf8(&args[i + 1])
                    && let Ok(n) = s.parse::<u64>()
                {
                    return ReplayTtl::Remaining(std::time::Duration::from_millis(n));
                }
            }
            b"EXAT" => {
                if let Ok(s) = std::str::from_utf8(&args[i + 1])
                    && let Ok(epoch_secs) = s.parse::<u64>()
                {
                    return remaining_from_epoch_secs(epoch_secs);
                }
            }
            b"PXAT" => {
                if let Ok(s) = std::str::from_utf8(&args[i + 1])
                    && let Ok(epoch_ms) = s.parse::<u64>()
                {
                    return remaining_from_epoch_ms(epoch_ms);
                }
            }
            _ => {}
        }
        i += 1;
    }
    ReplayTtl::None
}

fn remaining_from_epoch_secs(epoch_secs: u64) -> ReplayTtl {
    remaining_from_epoch_secs_at(epoch_secs, now_epoch_secs())
}

fn remaining_from_epoch_secs_at(epoch_secs: u64, now_secs: u64) -> ReplayTtl {
    if epoch_secs > now_secs {
        ReplayTtl::Remaining(std::time::Duration::from_secs(epoch_secs - now_secs))
    } else {
        ReplayTtl::Expired
    }
}

fn remaining_from_epoch_ms(epoch_ms: u64) -> ReplayTtl {
    remaining_from_epoch_ms_at(epoch_ms, now_epoch_ms())
}

fn remaining_from_epoch_ms_at(epoch_ms: u64, now_ms: u64) -> ReplayTtl {
    if epoch_ms > now_ms {
        ReplayTtl::Remaining(std::time::Duration::from_millis(epoch_ms - now_ms))
    } else {
        ReplayTtl::Expired
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Frame;
    use bytes::Bytes;

    #[test]
    fn remaining_from_epoch_ms_at_returns_duration_for_future_epoch() {
        match remaining_from_epoch_ms_at(11_500, 1_000) {
            ReplayTtl::Remaining(duration) => {
                assert_eq!(duration, std::time::Duration::from_millis(10_500));
            }
            ReplayTtl::None | ReplayTtl::Expired => panic!("expected remaining TTL"),
        }
    }

    #[test]
    fn remaining_from_epoch_ms_at_expires_current_or_past_epoch() {
        assert!(matches!(
            remaining_from_epoch_ms_at(1_000, 1_000),
            ReplayTtl::Expired
        ));
        assert!(matches!(
            remaining_from_epoch_ms_at(999, 1_000),
            ReplayTtl::Expired
        ));
    }

    // Build a raw AOF buffer containing a single SET command with PXAT.
    fn aof_set_pxat(key: &str, val: &str, epoch_ms: u64) -> Bytes {
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::copy_from_slice(key.as_bytes())),
            Frame::Bulk(Bytes::copy_from_slice(val.as_bytes())),
            Frame::Bulk(Bytes::from_static(b"PXAT")),
            Frame::Bulk(Bytes::from(epoch_ms.to_string())),
        ])
        .serialize()
    }

    #[tokio::test]
    async fn replay_pxat_restores_remaining_ttl() {
        // Give the integration path a wide margin; exact time math is covered
        // by the deterministic `remaining_from_epoch_ms_at` tests above.
        let expire_at = now_epoch_ms() + 60 * 60 * 1000;
        let raw = aof_set_pxat("k", "v", expire_at);

        let store = Arc::new(Mutex::new(Store::new()));
        let tmp = tempfile(raw).await;
        replay(&tmp, &store).await.unwrap();
        std::fs::remove_file(&tmp).ok();

        let ttl = store.lock().await.ttl("k");
        assert!(ttl > 0, "expected positive ttl, got {ttl}");
    }

    #[tokio::test]
    async fn replay_pxat_skips_already_expired_key() {
        let expire_at = 1;
        let raw = aof_set_pxat("k", "v", expire_at);

        let store = Arc::new(Mutex::new(Store::new()));
        let tmp = tempfile(raw).await;
        replay(&tmp, &store).await.unwrap();
        std::fs::remove_file(&tmp).ok();

        let ttl = store.lock().await.ttl("k");
        assert_eq!(ttl, -2, "key should not have been restored");
    }

    #[tokio::test]
    async fn replay_no_ttl_restores_key_permanently() {
        let raw = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"k")),
            Frame::Bulk(Bytes::from_static(b"v")),
        ])
        .serialize();

        let store = Arc::new(Mutex::new(Store::new()));
        let tmp = tempfile(raw).await;
        replay(&tmp, &store).await.unwrap();
        std::fs::remove_file(&tmp).ok();

        let ttl = store.lock().await.ttl("k");
        assert_eq!(ttl, -1, "key with no TTL should be permanent");
    }

    async fn tempfile(content: Bytes) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::AsyncWriteExt;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tinyredis_test_{id}.aof"));
        let mut f = tokio::fs::File::create(&path).await.unwrap();
        f.write_all(&content).await.unwrap();
        path
    }
}
