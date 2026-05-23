use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use crate::config::{Config, FsyncPolicy};

/// Per-connection metadata visible in `CLIENT LIST`.
pub struct ClientEntry {
    pub id: u64,
    /// Remote address (e.g. `127.0.0.1:52164`).
    pub addr: String,
    /// Local listener address (e.g. `127.0.0.1:6379`).
    pub laddr: String,
    /// Name set by `CLIENT SETNAME` (empty = unset).
    pub name: String,
    pub connected_at: Instant,
    /// Lowercase name of the last executed command (e.g. `get`, `client|list`).
    pub last_cmd: String,
    /// -1 = not in MULTI; ≥0 = number of queued commands.
    pub multi: i64,
}

/// Registry of all currently connected clients, keyed by client ID.
/// Uses a plain `std::sync::Mutex` because all critical sections are short
/// and never span an `.await` point.
pub type ClientRegistry = Arc<Mutex<HashMap<u64, ClientEntry>>>;

pub struct ServerStats {
    pub started_at: Instant,
    pub bind: String,
    pub port: u16,
    pub hz: u16,
    pub run_id: String,
    pub aof_enabled: bool,
    pub maxmemory: u64,
    pub maxmemory_policy: String,
    pub config_file: String,
    pub requirepass: Option<String>,
    pub appendfsync: FsyncPolicy,
    pub connected_clients: AtomicI64,
    pub total_connections_received: AtomicU64,
    pub total_commands_processed: AtomicU64,
    /// Sender used by BGREWRITEAOF to trigger a rewrite. `None` when AOF is disabled.
    pub rewrite_tx: Option<mpsc::Sender<()>>,
    /// Set to `true` while an AOF rewrite is in progress.
    pub aof_rewrite_in_progress: Arc<AtomicBool>,
    /// Monotonically-increasing counter for assigning client IDs.
    pub next_client_id: AtomicU64,
    /// Live client metadata for `CLIENT LIST`.
    pub clients: ClientRegistry,
}

impl ServerStats {
    /// Build stats from a loaded `Config` and the resolved config file path
    /// (empty string when using defaults).
    /// Pass `rewrite_tx: None` when AOF is disabled or in tests.
    pub fn from_config(
        cfg: &Config,
        config_file: String,
        rewrite_tx: Option<mpsc::Sender<()>>,
    ) -> Self {
        ServerStats {
            started_at: Instant::now(),
            bind: cfg.bind.clone(),
            port: cfg.port,
            hz: 10,
            run_id: generate_run_id(),
            aof_enabled: cfg.appendonly,
            maxmemory: cfg.maxmemory,
            maxmemory_policy: cfg.maxmemory_policy.clone(),
            config_file,
            requirepass: cfg.requirepass.clone(),
            appendfsync: cfg.appendfsync,
            connected_clients: AtomicI64::new(0),
            total_connections_received: AtomicU64::new(0),
            total_commands_processed: AtomicU64::new(0),
            rewrite_tx,
            aof_rewrite_in_progress: Arc::new(AtomicBool::new(false)),
            next_client_id: AtomicU64::new(0),
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

/// Generate a Redis-style 40 hex-char run_id from current time + PID.
fn generate_run_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u64;
    let a = t as u64;
    let b = (t >> 64) as u64 ^ pid.wrapping_mul(0x9e3779b97f4a7c15);
    let c = pid ^ a.wrapping_mul(6364136223846793005);
    format!("{:016x}{:016x}{:08x}", a, b, c as u32)
}

pub type SharedStats = Arc<ServerStats>;
