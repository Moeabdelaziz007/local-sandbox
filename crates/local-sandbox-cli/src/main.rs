//! local-sandbox command-line interface.

use clap::{Parser, Subcommand};
use local_sandbox_core::{SandboxId, SandboxInfo, SandboxStatus};

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
            let info = SandboxInfo {
                id: SandboxId::new(),
                status: SandboxStatus::Created,
            };
            println!("{}", serde_json::to_string_pretty(&info)?);
        }
    }

    Ok(())
}
