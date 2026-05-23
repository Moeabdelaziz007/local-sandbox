use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::error::Result;
use crate::parser::{Command, Frame};
use crate::stats::SharedStats;
use crate::store::Store;

type SharedStore = Arc<Mutex<Store>>;

pub async fn info(cmd: &Command, store: &SharedStore, stats: &SharedStats) -> Result<Frame> {
    let section = cmd
        .args
        .first()
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.to_lowercase());

    let (total_keys, expires, used_memory) = {
        let mut s = store.lock().await;
        let (total_keys, expires) = s.keyspace_info();
        let used_memory = s.rough_memory_usage();
        (total_keys, expires, used_memory)
    };

    let uptime_secs = stats.uptime_secs();
    let uptime_days = uptime_secs / 86400;
    let connected = stats.connected_clients.load(Ordering::Relaxed);
    let total_conn = stats.total_connections_received.load(Ordering::Relaxed);
    let total_cmds = stats.total_commands_processed.load(Ordering::Relaxed);

    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let server_time_usec = unix_secs * 1_000_000
        + SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_micros() as u64;
    // LRU clock: seconds since epoch, bounded to 24 bits (wraps every ~194 days)
    let lru_clock = unix_secs & 0x00FF_FFFF;

    let arch_bits = std::mem::size_of::<usize>() * 8;
    let pid = std::process::id();
    let executable = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default();

    let server = format!(
        "# Server\r\n\
         redis_version:{}\r\n\
         redis_git_sha1:{}\r\n\
         redis_git_dirty:{}\r\n\
         redis_build_id:0\r\n\
         redis_mode:standalone\r\n\
         os:{}\r\n\
         arch_bits:{}\r\n\
         process_id:{}\r\n\
         process_supervised:no\r\n\
         run_id:{}\r\n\
         tcp_port:{}\r\n\
         server_time_usec:{}\r\n\
         uptime_in_seconds:{}\r\n\
         uptime_in_days:{}\r\n\
         hz:{}\r\n\
         configured_hz:{}\r\n\
         lru_clock:{}\r\n\
         executable:{}\r\n\
         config_file:{}\r\n\
         io_threads_active:0\r\n\
         listener0:name=tcp,bind={},bind=-::*,port={}",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_SHA1"),
        env!("GIT_DIRTY"),
        os_string(),
        arch_bits,
        pid,
        stats.run_id,
        stats.port,
        server_time_usec,
        uptime_secs,
        uptime_days,
        stats.hz,
        stats.hz,
        lru_clock,
        executable,
        stats.config_file,
        stats.bind,
        stats.port,
    );
    let clients = format!("# Clients\r\nconnected_clients:{}", connected);
    let persistence = format!(
        "# Persistence\r\n\
         loading:0\r\n\
         rdb_changes_since_last_save:0\r\n\
         rdb_bgsave_in_progress:0\r\n\
         rdb_last_bgsave_status:ok\r\n\
         aof_enabled:{}\r\n\
         aof_appendfsync:{}\r\n\
         aof_rewrite_in_progress:{}\r\n\
         aof_last_bgrewrite_status:ok\r\n\
         aof_last_write_status:ok",
        stats.aof_enabled as u8,
        stats.appendfsync.as_str(),
        stats.aof_rewrite_in_progress.load(Ordering::Relaxed) as u8,
    );
    let stats_sec = format!(
        "# Stats\r\ntotal_connections_received:{}\r\ntotal_commands_processed:{}",
        total_conn, total_cmds
    );
    let replication = format!(
        "# Replication\r\n\
         role:master\r\n\
         connected_slaves:0\r\n\
         master_replid:{}\r\n\
         master_repl_offset:0\r\n\
         repl_backlog_active:0\r\n\
         repl_backlog_size:1048576",
        stats.run_id,
    );
    let cpu = "# CPU\r\n\
               used_cpu_sys:0.000000\r\n\
               used_cpu_user:0.000000\r\n\
               used_cpu_sys_children:0.000000\r\n\
               used_cpu_user_children:0.000000"
        .to_string();
    let memory = format!(
        "# Memory\r\n\
         used_memory:{used_memory}\r\n\
         used_memory_human:{}\r\n\
         maxmemory:{}\r\n\
         maxmemory_human:{}\r\n\
         maxmemory_policy:{}",
        format_memory(used_memory),
        stats.maxmemory,
        format_memory(stats.maxmemory),
        stats.maxmemory_policy,
    );
    let cluster = "# Cluster\r\ncluster_enabled:0".to_string();
    let keyspace = if total_keys > 0 {
        format!(
            "# Keyspace\r\ndb0:keys={},expires={},avg_ttl=0",
            total_keys, expires
        )
    } else {
        "# Keyspace".to_string()
    };

    let body = match section.as_deref() {
        Some("server") => server,
        Some("clients") => clients,
        Some("memory") => memory,
        Some("persistence") => persistence,
        Some("stats") => stats_sec,
        Some("replication") => replication,
        Some("cpu") => cpu,
        Some("cluster") => cluster,
        Some("keyspace") => keyspace,
        _ => [
            &server,
            &clients,
            &memory,
            &persistence,
            &stats_sec,
            &replication,
            &cpu,
            &cluster,
            &keyspace,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\r\n\r\n"),
    };

    Ok(Frame::Bulk(Bytes::from(body + "\r\n")))
}

/// Format a byte count as a human-readable string (e.g. `0B`, `100.00M`, `1.00G`).
fn format_memory(bytes: u64) -> String {
    if bytes == 0 {
        return "0B".to_string();
    }
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    const KB: u64 = 1024;
    if bytes >= GB {
        format!("{:.2}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2}K", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}B")
    }
}

/// Returns the OS name and kernel version + arch, matching Redis's `uname -srm` format.
/// Falls back to compile-time constants if `uname` is unavailable.
fn os_string() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "i386",
        other => other,
    };

    // Try `uname -r` for the kernel version (matches what Redis reports)
    let version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let os_name = match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "windows" => return format!("Windows {}", arch),
        other => other,
    };

    if version.is_empty() {
        format!("{} {}", os_name, arch)
    } else {
        format!("{} {} {}", os_name, version, arch)
    }
}
