//! local-sandbox command-line interface.

use clap::{Parser, Subcommand};
use local_sandbox_core::{SandboxArchitecture, SandboxId, SandboxInfo, SandboxStatus};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "local-sandbox")]
#[command(about = "Manage local sandboxes")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print CLI and contract health.
    Health,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Health => {
            let now = unix_time_ms();
            let info = SandboxInfo {
                id: SandboxId::new(),
                status: SandboxStatus::Created,
                architecture: SandboxArchitecture::Arm64,
                routes: Vec::new(),
                created_at_unix_ms: now,
                expires_at_unix_ms: None,
            };
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
    }

    Ok(())
}

fn unix_time_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
