//! local-sandbox daemon entrypoint.

use anyhow::Context;
use local_sandbox_daemon::{boot_state, serve_with_shutdown};
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind daemon listener")?;
    tracing::info!(addr = %listener.local_addr()?, "local-sandbox daemon listening");
    let state = boot_state().await.context("boot daemon state")?;
    serve_with_shutdown(listener, state, shutdown_signal())
        .await
        .context("serve daemon")
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %error, "failed to listen for shutdown signal");
    }
}
