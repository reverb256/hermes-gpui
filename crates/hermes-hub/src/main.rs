use std::net::SocketAddr;

use anyhow::Context;
use hermes_hub::{Config, HubState};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("hermes_hub=info,warn")),
        )
        .init();

    let config = Config::from_env()?;
    let address: SocketAddr = config
        .bind
        .parse()
        .with_context(|| format!("invalid HERMES_HUB_BIND {:?}", config.bind))?;
    let state = HubState::open(config)?;
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("bind {address}"))?;
    state.serve(listener).await
}
