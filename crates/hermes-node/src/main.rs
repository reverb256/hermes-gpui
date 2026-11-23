use anyhow::Result;
use hermes_node::{Agent, Config};
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // `tokio-tungstenite` can pull both rustls providers through the workspace
    // feature graph. Choose one explicitly before any TLS connection is built.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("hermes_node=info,warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("hermes-node {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let config = Config::from_env()?;
    if args.iter().any(|arg| arg == "--check") {
        // Print the effective local policy without the token or any credential.
        println!(
            "{}",
            json!({
                "node_id": config.node_id,
                "name": config.name,
                "hub_url": config.hub_url,
                "allowed_roots": config.allowed_roots,
                "allowed_commands": config.allowed_commands,
                "max_concurrency": config.max_concurrency,
            })
        );
        return Ok(());
    }

    let agent = Agent::new(config);
    tokio::select! {
        result = agent.run() => result,
        _ = shutdown_signal() => {
            // Docker and systemd stop the agent with SIGTERM. Ctrl-C is the
            // terminal equivalent. Exiting lets kill-on-drop reap children
            // instead of waiting out a container runtime's kill timeout.
            tracing::info!("shutdown signal received");
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = std::pin::pin!(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    #[cfg(unix)]
    let terminate = std::pin::pin!(async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => tracing::warn!(error = %err, "failed to listen for SIGTERM"),
        }
    });
    #[cfg(not(unix))]
    let terminate = std::pin::pin!(std::future::pending::<()>());
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
