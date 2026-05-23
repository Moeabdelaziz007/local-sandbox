use std::sync::Arc;

use chrono::Local;

use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc};

use tinyredis::config::Config;
use tinyredis::error::Result;
use tinyredis::persistence::{self, AofEntry, AofWriter};
use tinyredis::server;
use tinyredis::stats::ServerStats;
use tinyredis::store::Store;

#[tokio::main]
async fn main() -> Result<()> {
    let pid = std::process::id();
    let version = env!("CARGO_PKG_VERSION");
    let sha = env!("GIT_SHA1");
    let dirty = env!("GIT_DIRTY");
    let bits = std::mem::size_of::<usize>() * 8;

    // ── Load config ──────────────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    let (cfg, config_file, config_note) = match args.get(1) {
        Some(path) => {
            let p = std::path::Path::new(path);
            let cfg = Config::from_file(p)?;
            let abs = p
                .canonicalize()
                .unwrap_or_else(|_| p.to_path_buf())
                .to_string_lossy()
                .into_owned();
            (
                cfg,
                abs.clone(),
                format!(
                    "Configuration loaded from file '{abs}' — using the specified configuration"
                ),
            )
        }
        None => (
            Config::default(),
            String::new(),
            "Warning: no config file specified, using the default config. \
             In order to specify a config file use tinyredis /path/to/tinyredis.conf"
                .to_string(),
        ),
    };

    // ── Config-role startup messages ─────────────────────────────────────────
    log_line('C', '*', "oO0OoO0OoO0Oo Redis is starting oO0OoO0OoO0Oo");
    log_line(
        'C',
        '*',
        &format!(
            "Redis version={version}, bits={bits}, commit={sha}, modified={dirty}, pid={pid}, just started"
        ),
    );
    log_line('C', '#', &config_note);

    // ── ASCII art logo ───────────────────────────────────────────────────────
    eprintln!("                _._");
    eprintln!("           _.-``__ ''-._");
    eprintln!("      _.-``    `.  `_.  ''-._           Redis Open Source");
    eprintln!("  .-`` .-```.  ```\\/    _.,_ ''-._      {version} ({sha}/{dirty}) {bits} bit");
    eprintln!(" (    '      ,       .-`  | `,    )     Running in standalone mode");
    eprintln!(" |`-._`-...-` __...-.``-._|'` _.-'|     Port: {}", cfg.port);
    eprintln!(" |    `-._   `._    /     _.-'    |     PID: {pid}");
    eprintln!("  `-._    `-._  `-./  _.-'    _.-'");
    eprintln!(" |`-._`-._    `-.__.-'    _.-'_.-'|");
    eprintln!(" |    `-._`-._        _.-'_.-'    |           https://redis.io");
    eprintln!("  `-._    `-._`-.__.-'_.-'    _.-'");
    eprintln!(" |`-._`-._    `-.__.-'    _.-'_.-'|");
    eprintln!(" |    `-._`-._        _.-'_.-'    |");
    eprintln!("  `-._    `-._`-.__.-'_.-'    _.-'");
    eprintln!("      `-._    `-.__.-'    _.-'");
    eprintln!("          `-._        _.-'");
    eprintln!("              `-.__.-'");
    eprintln!();

    // ── Bind and load AOF ────────────────────────────────────────────────────
    let listener = TcpListener::bind((cfg.bind.as_str(), cfg.port)).await?;

    let store = Arc::new(Mutex::new(Store::new()));
    store
        .lock()
        .await
        .configure_memory(cfg.maxmemory, &cfg.maxmemory_policy);

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Create AOF rewrite channel only when persistence is enabled.
    let (rewrite_tx, rewrite_rx) = if cfg.appendonly {
        let (tx, rx) = mpsc::channel::<()>(1);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let stats = Arc::new(ServerStats::from_config(&cfg, config_file, rewrite_tx));

    let aof_tx = if cfg.appendonly {
        persistence::replay(&cfg.appendfilename, &store).await?;
        let (tx, rx) = mpsc::channel::<AofEntry>(1024);
        let aof_writer = AofWriter::new(
            &cfg.appendfilename,
            rx,
            shutdown_tx.subscribe(),
            Arc::clone(&store),
            rewrite_rx.expect("rewrite_rx created with appendonly=true"),
            Arc::clone(&stats),
            cfg.appendfsync,
        )
        .await?;
        tokio::spawn(async move {
            if let Err(e) = aof_writer.run().await {
                tracing::error!("AOF writer error: {e}");
            }
        });
        Some(tx)
    } else {
        None
    };

    // ── Main-role ready messages ─────────────────────────────────────────────
    log_line('M', '*', "Server initialized");
    log_line('M', '*', "Ready to accept connections tcp");
    log_line(
        'M',
        '#',
        "WARNING: Redis does not require authentication. \
         Redis will accept connections from any local client.",
    );

    // ── Run until ctrl-c ─────────────────────────────────────────────────────
    tokio::select! {
        _ = server::serve(listener, store, aof_tx, shutdown_tx.clone(), stats) => {}
        _ = tokio::signal::ctrl_c() => {
            let unix_secs = Local::now().timestamp();
            eprintln!("\n{pid}:signal-handler ({unix_secs}) Received SIGINT scheduling shutdown...");
            log_line('M', '*', "User requested shutdown...");
            log_line('M', '*', "Saving the final RDB snapshot before exiting.");
            log_line('M', '*', "Done saving. Exiting now.");
            let _ = shutdown_tx.send(());
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Ok(())
}

/// Print a Redis-style log line to stderr.
/// Format: `{PID}:{role} {DD Mon YYYY HH:MM:SS.mmm} {level} {msg}`
fn log_line(role: char, level: char, msg: &str) {
    let pid = std::process::id();
    let ts = Local::now().format("%d %b %Y %H:%M:%S%.3f");
    eprintln!("{pid}:{role} {ts} {level} {msg}");
}
