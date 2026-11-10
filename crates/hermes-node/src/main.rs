use anyhow::Result;
use hermes_node::{Agent, Config};
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
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
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal received");
            Ok(())
        }
    }
}
