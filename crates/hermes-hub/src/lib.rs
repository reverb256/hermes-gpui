//! Single-owner Hermes hub.
//!
//! The hub owns pairing, the node registry, command routing, the audit index and
//! the secret-prompt broker. It never connects to a computer: nodes dial out and
//! keep that connection open, so NAT and changing addresses do not matter.
//!
//! Authentication is a bearer token per principal. Tokens are stored hashed in
//! SQLite; enrollment codes are single-use and short-lived. There is no tenant
//! column, no role table and no org model by design.

mod api;
mod auth;
mod db;
mod device_ws;
mod error;
mod node_ws;
mod util;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::routing::{delete, get, post};
use hermes_protocol::{
    AuditEntry, CommandRecord, HubToDevice, HubToNode, NodeSummary, SecretPrompt,
};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub use api::{
    EnrollBody, EnrollmentBody, EnrollmentResponse, SubmitCommandBody, SubmitCommandResponse,
};
pub use db::{Db, Enrollment, Principal, PrincipalKind, StoredPrompt};
pub use error::ApiError;
pub use util::now_ms;

use crate::auth::{bearer, constant_time_eq, hash_token};

/// Runtime configuration, read once at startup.
#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub db_path: PathBuf,
    pub admin_token: String,
    pub heartbeat_ms: u64,
    pub max_command_timeout_ms: u64,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("db_path", &self.db_path)
            .field("admin_token", &"<redacted>")
            .field("heartbeat_ms", &self.heartbeat_ms)
            .field("max_command_timeout_ms", &self.max_command_timeout_ms)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind = std::env::var("HERMES_HUB_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let db_path = std::env::var("HERMES_HUB_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/hermes.db"));
        let admin_token = std::env::var("HERMES_HUB_ADMIN_TOKEN")
            .context("HERMES_HUB_ADMIN_TOKEN is required")?;
        if admin_token.trim().len() < 32 {
            bail!("HERMES_HUB_ADMIN_TOKEN must be at least 32 characters");
        }
        let heartbeat_ms = env_u64("HERMES_HUB_HEARTBEAT_MS", 15_000)?;
        let max_command_timeout_ms = env_u64("HERMES_HUB_MAX_COMMAND_TIMEOUT_MS", 15 * 60 * 1000)?;
        if !(1_000..=120_000).contains(&heartbeat_ms) {
            bail!("HERMES_HUB_HEARTBEAT_MS must be 1000..=120000");
        }
        Ok(Self {
            bind,
            db_path,
            admin_token,
            heartbeat_ms,
            max_command_timeout_ms,
        })
    }

    /// Local-only configuration used by tests and embedded runs.
    pub fn for_test() -> Self {
        Self {
            bind: "127.0.0.1:0".into(),
            db_path: PathBuf::from(":memory:"),
            admin_token: "test-admin-token-that-is-long-enough".into(),
            heartbeat_ms: 15_000,
            max_command_timeout_ms: 60_000,
        }
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be a positive integer")),
        Err(_) => Ok(default),
    }
}

#[derive(Clone)]
struct NodeConnection {
    conn_id: String,
    tx: mpsc::UnboundedSender<HubToNode>,
    close: tokio::sync::watch::Sender<bool>,
}

#[derive(Clone)]
struct DeviceConnection {
    conn_id: String,
    tx: mpsc::UnboundedSender<HubToDevice>,
    close: tokio::sync::watch::Sender<bool>,
}

#[derive(Clone)]
struct PendingPrompt {
    node_id: String,
    challenge: String,
    expires_at_ms: u64,
}

/// Shared state for HTTP and WebSocket handlers.
pub struct HubState {
    pub config: Config,
    pub db: Db,
    pub started_at_ms: u64,
    nodes: RwLock<HashMap<String, NodeConnection>>,
    devices: RwLock<HashMap<String, DeviceConnection>>,
    prompts: RwLock<HashMap<String, PendingPrompt>>,
}

impl HubState {
    pub fn new(config: Config, db: Db) -> Arc<Self> {
        Arc::new(Self {
            config,
            db,
            started_at_ms: now_ms(),
            nodes: RwLock::new(HashMap::new()),
            devices: RwLock::new(HashMap::new()),
            prompts: RwLock::new(HashMap::new()),
        })
    }

    pub fn open(config: Config) -> Result<Arc<Self>> {
        let db = Db::open(&config.db_path)?;
        Ok(Self::new(config, db))
    }

    /// Build a hub backed by an in-memory database (tests, embedded runs).
    pub fn in_memory(config: Config) -> Arc<Self> {
        Self::new(config, Db::open_memory().expect("in-memory database"))
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/healthz", get(api::health))
            .route("/readyz", get(api::ready))
            .route("/api/v1/health", get(api::health))
            .route("/api/v1/enroll", post(api::enroll))
            .route("/api/v1/enrollments", post(api::create_enrollment))
            .route("/api/v1/nodes", get(api::nodes))
            .route("/api/v1/nodes/{id}", delete(api::revoke_node))
            .route("/api/v1/devices", get(api::devices))
            .route("/api/v1/devices/{id}", delete(api::revoke_device))
            .route("/api/v1/commands", post(api::submit_command))
            .route("/api/v1/commands/{id}", get(api::command))
            .route("/api/v1/commands/{id}/events", get(api::command_events))
            .route("/api/v1/audit", get(api::audit))
            .route("/api/v1/prompts", get(api::prompts))
            .route("/api/v1/prompts/{id}", delete(api::cancel_prompt))
            .route("/v1/node", get(node_ws::node_socket))
            .route("/v1/device", get(device_ws::device_socket))
            .with_state(self.clone())
    }

    fn admin(&self, headers: &axum::http::HeaderMap) -> Result<(), ApiError> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| bearer(Some(value)))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        if !constant_time_eq(token.as_bytes(), self.config.admin_token.as_bytes()) {
            return Err(ApiError::unauthorized("invalid admin token"));
        }
        Ok(())
    }

    fn principal(
        &self,
        headers: &axum::http::HeaderMap,
        kind: PrincipalKind,
    ) -> Result<Principal, ApiError> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| bearer(Some(value)))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        let principal = self
            .db
            .principal_by_token_hash(&hash_token(token))?
            .ok_or_else(|| ApiError::unauthorized("unknown or revoked token"))?;
        if principal.kind != kind {
            return Err(ApiError::forbidden(
                "token is for a different principal kind",
            ));
        }
        Ok(principal)
    }

    fn register_node(
        &self,
        node_id: &str,
        conn_id: &str,
        tx: mpsc::UnboundedSender<HubToNode>,
        close: tokio::sync::watch::Sender<bool>,
    ) {
        // Hold the registry lock across replacement so a command cannot be
        // routed to the old socket while its unfinished commands are failed.
        let mut nodes = self
            .nodes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let old = nodes.remove(node_id);
        if let Some(old) = old {
            let _ = old.tx.send(HubToNode::Error {
                code: "replaced".into(),
                message: "this node connected again; the older socket is closed".into(),
            });
            let _ = old.close.send(true);
            self.audit(
                "hub",
                "node.replaced",
                json!({ "node_id": node_id, "old_connection": old.conn_id }),
            );
            // Commands routed to the old socket will never produce a result.
            // Fail them visibly instead of leaving a queued command forever,
            // and fail closed on prompts that belonged to that connection.
            match self
                .db
                .fail_commands_for_node(node_id, "node reconnected", now_ms())
            {
                Ok(failed) => {
                    for command in failed {
                        self.broadcast_device(HubToDevice::CommandState { command });
                    }
                }
                Err(err) => warn!(error = %err, "failed to close commands for a replaced node"),
            }
            for prompt_id in self.take_prompts_for_node(node_id) {
                let _ = self
                    .db
                    .resolve_prompt(&prompt_id, "node_reconnected", now_ms());
                self.broadcast_device(HubToDevice::PromptClosed { prompt_id });
            }
        }
        nodes.insert(
            node_id.to_string(),
            NodeConnection {
                conn_id: conn_id.to_string(),
                tx,
                close,
            },
        );
    }

    /// Close a node socket and stop routing to it. The token is revoked
    /// separately, so a reconnect is refused before authentication.
    fn disconnect_node(&self, node_id: &str) {
        if let Some(connection) = self
            .nodes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(node_id)
        {
            let _ = connection.close.send(true);
        }
    }

    fn disconnect_device(&self, device_id: &str) {
        if let Some(connection) = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(device_id)
        {
            let _ = connection.close.send(true);
        }
    }

    /// Remove this connection only if it is still the current one. Returns
    /// false when a newer socket has already replaced it, so the old task does
    /// not mark the node offline or fail the new socket's commands.
    fn unregister_node(&self, node_id: &str, conn_id: &str) -> bool {
        let mut nodes = self
            .nodes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if nodes
            .get(node_id)
            .is_some_and(|connection| connection.conn_id == conn_id)
        {
            nodes.remove(node_id);
            true
        } else {
            false
        }
    }

    fn node_sender(&self, node_id: &str) -> Option<mpsc::UnboundedSender<HubToNode>> {
        self.nodes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(node_id)
            .map(|connection| connection.tx.clone())
    }

    fn is_node_online(&self, node_id: &str) -> bool {
        self.nodes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(node_id)
    }

    fn send_node(&self, node_id: &str, message: HubToNode) -> bool {
        match self.node_sender(node_id) {
            Some(tx) => tx.send(message).is_ok(),
            None => false,
        }
    }

    fn register_device(
        &self,
        device_id: &str,
        conn_id: &str,
        tx: mpsc::UnboundedSender<HubToDevice>,
        close: tokio::sync::watch::Sender<bool>,
    ) {
        let old = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                device_id.to_string(),
                DeviceConnection {
                    conn_id: conn_id.to_string(),
                    tx,
                    close,
                },
            );
        if let Some(old) = old {
            let _ = old.close.send(true);
        }
    }

    fn unregister_device(&self, device_id: &str, conn_id: &str) {
        let mut devices = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if devices
            .get(device_id)
            .is_some_and(|connection| connection.conn_id == conn_id)
        {
            devices.remove(device_id);
        }
    }

    fn device_sender(&self, device_id: &str) -> Option<mpsc::UnboundedSender<HubToDevice>> {
        self.devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(device_id)
            .map(|connection| connection.tx.clone())
    }

    fn send_device(&self, device_id: &str, message: HubToDevice) -> bool {
        match self.device_sender(device_id) {
            Some(tx) => tx.send(message).is_ok(),
            None => false,
        }
    }

    fn device_count(&self) -> usize {
        self.devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    fn connected_device_ids(&self) -> std::collections::HashSet<String> {
        self.devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    fn broadcast_device(&self, message: HubToDevice) {
        let senders: Vec<_> = self
            .devices
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|connection| connection.tx.clone())
            .collect();
        for sender in senders {
            let _ = sender.send(message.clone());
        }
    }

    fn broadcast_presence(&self, node: &NodeSummary) {
        self.broadcast_device(HubToDevice::NodePresence { node: node.clone() });
    }

    fn node_summary(&self, node_id: &str) -> Option<NodeSummary> {
        self.db.node(node_id).ok().flatten()
    }

    fn insert_prompt(&self, prompt: &SecretPrompt) {
        self.prompts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                prompt.id.clone(),
                PendingPrompt {
                    node_id: prompt.node_id.clone(),
                    challenge: prompt.challenge.clone(),
                    expires_at_ms: prompt.expires_at_ms,
                },
            );
    }

    #[cfg(test)]
    fn pending_prompt(&self, prompt_id: &str) -> Option<PendingPrompt> {
        self.prompts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(prompt_id)
            .cloned()
    }

    fn take_prompt(&self, prompt_id: &str) -> Option<PendingPrompt> {
        self.prompts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(prompt_id)
    }

    fn take_prompts_for_node(&self, node_id: &str) -> Vec<String> {
        let mut prompts = self
            .prompts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ids: Vec<String> = prompts
            .iter()
            .filter(|(_, prompt)| prompt.node_id == node_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            prompts.remove(id);
        }
        ids
    }

    pub fn audit(&self, actor: &str, kind: &str, detail: serde_json::Value) {
        if let Err(err) = self
            .db
            .audit(&util::new_id("audit"), now_ms(), actor, kind, &detail)
        {
            warn!(error = %err, "failed to write audit entry");
        }
    }

    pub fn command(&self, id: &str) -> Result<Option<CommandRecord>> {
        self.db.command(id)
    }

    pub fn audit_entries(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        self.db.audit_entries(limit)
    }

    /// Route a command created over HTTP or by a device.
    fn dispatch_command(
        self: &Arc<Self>,
        record: CommandRecord,
        request: hermes_protocol::CommandRequest,
    ) -> Result<(), ApiError> {
        if !self.is_node_online(&record.node_id) {
            let _ = self.db.finish_command(
                &record.id,
                hermes_protocol::CommandStatus::Failed,
                None,
                Some("node is offline"),
                now_ms(),
            );
            return Err(ApiError::conflict("node is offline"));
        }
        if self.send_node(&record.node_id, HubToNode::Command { command: request }) {
            self.broadcast_device(HubToDevice::CommandState {
                command: record.clone(),
            });
            self.audit(
                "hub",
                "command.queued",
                json!({
                    "command_id": record.id,
                    "node_id": record.node_id,
                    "argv": record.argv,
                }),
            );
            Ok(())
        } else {
            let failed = self.db.finish_command(
                &record.id,
                hermes_protocol::CommandStatus::Failed,
                None,
                Some("node connection closed before delivery"),
                now_ms(),
            )?;
            if let Some(failed) = failed {
                self.broadcast_device(HubToDevice::CommandState { command: failed });
            }
            Err(ApiError::conflict("node connection closed before delivery"))
        }
    }

    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        let address = listener.local_addr()?;
        info!(%address, "hermes-hub listening");
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("hub server failed")
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(error = %err, "failed to listen for Ctrl-C");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => warn!(error = %err, "failed to listen for SIGTERM"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_redacts_the_admin_token() {
        let config = Config::for_test();
        let debug = format!("{config:?}");
        assert!(!debug.contains(&config.admin_token));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn prompt_registry_is_scoped_to_a_node() {
        let state = HubState::in_memory(Config::for_test());
        let prompt = SecretPrompt {
            id: "p-1".into(),
            node_id: "n-1".into(),
            command_id: "c-1".into(),
            challenge: "x".repeat(43),
            requester: "node".into(),
            purpose: "sudo".into(),
            scope: "command".into(),
            env_var: "SUDO_PASSWORD".into(),
            created_at_ms: 1,
            expires_at_ms: 10_000,
        };
        state.insert_prompt(&prompt);
        assert!(state.pending_prompt("p-1").is_some());
        assert!(state.take_prompts_for_node("n-2").is_empty());
        assert_eq!(state.take_prompts_for_node("n-1"), vec!["p-1"]);
        assert!(state.pending_prompt("p-1").is_none());
    }
}
