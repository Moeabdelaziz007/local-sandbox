use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast};

use crate::commands;
use crate::connection::Connection;
use crate::parser::{self, Frame};
use crate::persistence::AofSender;
use crate::stats::{ClientEntry, SharedStats};
use crate::store::Store;

type SharedStore = Arc<Mutex<Store>>;

/// Run the accept loop and expiry sweeper until `shutdown_tx` fires.
/// Pass `aof: None` to disable persistence (useful in tests).
pub async fn serve(
    listener: TcpListener,
    store: SharedStore,
    aof: Option<AofSender>,
    shutdown_tx: broadcast::Sender<()>,
    stats: SharedStats,
) {
    // Expiry sweeper
    {
        let store = Arc::clone(&store);
        let mut shutdown = shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = interval.tick() => store.lock().await.purge_expired(),
                    _ = shutdown.recv() => break,
                }
            }
        });
    }

    // Accept loop
    let mut shutdown = shutdown_tx.subscribe();
    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (socket, addr) = match accept {
                    Ok(a) => a,
                    Err(e) => { tracing::error!("accept error: {e}"); continue; }
                };
                tracing::debug!("accepted {addr}");
                let store = Arc::clone(&store);
                let aof = aof.clone();
                let stats = Arc::clone(&stats);
                tokio::spawn(handle_connection(socket, store, aof, stats));
            }
            _ = shutdown.recv() => break,
        }
    }
}

async fn handle_connection(
    socket: TcpStream,
    store: SharedStore,
    aof: Option<AofSender>,
    stats: SharedStats,
) {
    stats.connected_clients.fetch_add(1, Ordering::Relaxed);
    stats
        .total_connections_received
        .fetch_add(1, Ordering::Relaxed);

    // Capture addresses before the socket is moved into Connection.
    let peer_addr = socket
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let local_addr = socket
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();

    // Assign a unique monotonic client ID and register in the global registry.
    let client_id = stats.next_client_id.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let mut clients = stats.clients.lock().unwrap();
        clients.insert(
            client_id,
            ClientEntry {
                id: client_id,
                addr: peer_addr,
                laddr: local_addr,
                name: String::new(),
                connected_at: Instant::now(),
                last_cmd: String::new(),
                multi: -1,
            },
        );
    }

    // Connections start authenticated only if no password is required.
    let mut authenticated = stats.requirepass.is_none();

    // Transaction state.
    let mut in_multi = false;
    let mut tx_queue: Vec<parser::Command> = Vec::new();

    // Local copy of this connection's name (avoids locking the registry for every command).
    let mut client_name = String::new();

    let mut conn = Connection::new(socket);
    while let Ok(Some(frame)) = conn.read_frame().await {
        let cmd = match parser::Command::from_frame(frame) {
            Ok(c) => c,
            Err(e) => {
                conn.write_frame(&Frame::Error(format!("ERR {e}")));
                if conn.flush().await.is_err() {
                    break;
                }
                continue;
            }
        };
        stats
            .total_commands_processed
            .fetch_add(1, Ordering::Relaxed);

        // QUIT — always allowed; discards any open transaction.
        if cmd.name == "QUIT" {
            set_last_cmd(&stats, client_id, "quit");
            conn.write_frame(&Frame::Simple("OK".into()));
            let _ = conn.flush().await;
            break;
        }

        // AUTH — always executed immediately, even inside MULTI.
        if cmd.name == "AUTH" {
            set_last_cmd(&stats, client_id, "auth");
            let resp = handle_auth(&cmd, &stats, &mut authenticated);
            conn.write_frame(&resp);
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // Gate all other commands on authentication.
        if !authenticated {
            conn.write_frame(&Frame::Error("NOAUTH Authentication required.".into()));
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // MULTI — start a transaction.
        if cmd.name == "MULTI" {
            set_last_cmd(&stats, client_id, "multi");
            let resp = if in_multi {
                Frame::Error("ERR MULTI calls can not be nested".into())
            } else {
                in_multi = true;
                set_multi(&stats, client_id, 0);
                Frame::Simple("OK".into())
            };
            conn.write_frame(&resp);
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // DISCARD — abort the transaction.
        if cmd.name == "DISCARD" {
            set_last_cmd(&stats, client_id, "discard");
            let resp = if in_multi {
                in_multi = false;
                tx_queue.clear();
                set_multi(&stats, client_id, -1);
                Frame::Simple("OK".into())
            } else {
                Frame::Error("ERR DISCARD without MULTI".into())
            };
            conn.write_frame(&resp);
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // EXEC — execute all queued commands.
        if cmd.name == "EXEC" {
            set_last_cmd(&stats, client_id, "exec");
            if !in_multi {
                conn.write_frame(&Frame::Error("ERR EXEC without MULTI".into()));
                if conn.flush().await.is_err() {
                    break;
                }
                continue;
            }
            in_multi = false;
            set_multi(&stats, client_id, -1);
            let queue = std::mem::take(&mut tx_queue);
            let mut results = Vec::with_capacity(queue.len());
            for queued_cmd in queue {
                let resp = commands::dispatch(
                    queued_cmd,
                    Arc::clone(&store),
                    aof.clone(),
                    Arc::clone(&stats),
                )
                .await;
                results.push(resp);
            }
            conn.write_frame(&Frame::Array(results));
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // CLIENT — per-connection metadata commands.
        if cmd.name == "CLIENT" {
            let resp = handle_client_cmd(
                &cmd,
                &stats,
                client_id,
                &mut client_name,
                in_multi,
                tx_queue.len(),
            );
            // Record last_cmd with subcommand (e.g. "client|list").
            let subcmd_lower = cmd
                .args
                .first()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            set_last_cmd(&stats, client_id, &format!("client|{subcmd_lower}"));
            conn.write_frame(&resp);
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // Inside MULTI: queue the command and reply QUEUED.
        if in_multi {
            tx_queue.push(cmd);
            set_multi(&stats, client_id, tx_queue.len() as i64);
            conn.write_frame(&Frame::Simple("QUEUED".into()));
            if conn.flush().await.is_err() {
                break;
            }
            continue;
        }

        // Normal (non-transaction) execution.
        set_last_cmd(&stats, client_id, &cmd.name.to_lowercase());
        let resp =
            commands::dispatch(cmd, Arc::clone(&store), aof.clone(), Arc::clone(&stats)).await;
        conn.write_frame(&resp);
        if conn.flush().await.is_err() {
            break;
        }
    }

    // Deregister this connection from the global registry.
    stats.clients.lock().unwrap().remove(&client_id);
    stats.connected_clients.fetch_sub(1, Ordering::Relaxed);
}

// ── per-connection registry helpers ──────────────────────────────────────────

fn set_last_cmd(stats: &crate::stats::ServerStats, id: u64, cmd: &str) {
    if let Ok(mut clients) = stats.clients.lock()
        && let Some(e) = clients.get_mut(&id)
    {
        e.last_cmd = cmd.to_string();
    }
}

fn set_multi(stats: &crate::stats::ServerStats, id: u64, multi: i64) {
    if let Ok(mut clients) = stats.clients.lock()
        && let Some(e) = clients.get_mut(&id)
    {
        e.multi = multi;
    }
}

// ── CLIENT subcommand handler ─────────────────────────────────────────────────

fn handle_client_cmd(
    cmd: &parser::Command,
    stats: &crate::stats::ServerStats,
    client_id: u64,
    client_name: &mut String,
    in_multi: bool,
    tx_queue_len: usize,
) -> Frame {
    let subcmd = match cmd.args.first() {
        Some(b) => String::from_utf8_lossy(b).to_ascii_uppercase(),
        None => {
            return Frame::Error("ERR wrong number of arguments for 'client' command".into());
        }
    };

    match subcmd.as_str() {
        "SETNAME" => {
            if cmd.args.len() != 2 {
                return Frame::Error(
                    "ERR wrong number of arguments for 'client|setname' command".into(),
                );
            }
            let name = match std::str::from_utf8(&cmd.args[1]) {
                Ok(s) => s,
                Err(_) => {
                    return Frame::Error(
                        "ERR Client names cannot contain spaces, newlines or special characters."
                            .into(),
                    );
                }
            };
            // Name must not contain spaces or control characters.
            if name.bytes().any(|b| b <= b' ') {
                return Frame::Error(
                    "ERR Client names cannot contain spaces, newlines or special characters."
                        .into(),
                );
            }
            *client_name = name.to_string();
            if let Ok(mut clients) = stats.clients.lock()
                && let Some(e) = clients.get_mut(&client_id)
            {
                e.name = name.to_string();
            }
            Frame::Simple("OK".into())
        }

        "GETNAME" => {
            if client_name.is_empty() {
                Frame::Null
            } else {
                Frame::Bulk(Bytes::from(client_name.clone()))
            }
        }

        "ID" => Frame::Integer(client_id as i64),

        "LIST" => {
            let clients = match stats.clients.lock() {
                Ok(c) => c,
                Err(_) => return Frame::Error("ERR internal error".into()),
            };
            let now = Instant::now();
            // Collect and sort by ID for deterministic output.
            let mut entries: Vec<&ClientEntry> = clients.values().collect();
            entries.sort_by_key(|e| e.id);

            let mut output = String::new();
            for entry in entries {
                let age = now.duration_since(entry.connected_at).as_secs();
                // Reflect the current transaction depth for ourselves.
                let multi = if entry.id == client_id && in_multi {
                    tx_queue_len as i64
                } else {
                    entry.multi
                };
                let _ = writeln!(
                    output,
                    "id={id} addr={addr} laddr={laddr} fd=-1 name={name} age={age} \
                     idle=0 flags=N db=0 sub=0 psub=0 multi={multi} watch=0 qbuf=0 \
                     qbuf-free=0 argv-mem=0 multi-mem=0 tot-mem=0 rbs=0 rbp=0 oll=0 \
                     omem=0 events=r cmd={cmd} user=default resp=2",
                    id = entry.id,
                    addr = entry.addr,
                    laddr = entry.laddr,
                    name = entry.name,
                    cmd = entry.last_cmd,
                );
            }
            Frame::Bulk(Bytes::from(output))
        }

        _ => Frame::Error(format!(
            "ERR unknown subcommand '{}' for 'client' command",
            subcmd.to_ascii_lowercase()
        )),
    }
}

/// Handle the AUTH command, updating `authenticated` on success.
fn handle_auth(
    cmd: &parser::Command,
    stats: &crate::stats::ServerStats,
    authenticated: &mut bool,
) -> Frame {
    match &stats.requirepass {
        None => Frame::Error(
            "ERR Client sent AUTH, but no password is set. \
             Did you mean ACL SETUSER with >password?"
                .into(),
        ),
        Some(required) => {
            // AUTH password  OR  AUTH username password (username ignored)
            let given = match cmd.args.len() {
                1 => std::str::from_utf8(&cmd.args[0]).unwrap_or(""),
                2 => std::str::from_utf8(&cmd.args[1]).unwrap_or(""),
                _ => {
                    return Frame::Error("ERR wrong number of arguments for 'auth' command".into());
                }
            };
            if given == required.as_str() {
                *authenticated = true;
                Frame::Simple("OK".into())
            } else {
                *authenticated = false;
                Frame::Error("WRONGPASS invalid username-password pair or user is disabled.".into())
            }
        }
    }
}
