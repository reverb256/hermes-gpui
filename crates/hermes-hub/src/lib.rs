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
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::routing::{delete, get, post};
use hermes_protocol::{
    AuditEntry, CommandRecord, CommandStatus, HubToDevice, HubToNode, NodeSummary, SecretPrompt,
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
    /// How long a device may drop its socket before the hub cancels the
    /// commands that device started. A reconnect inside the window keeps
    /// the PTY; cancel runs only once the window expires.
    pub disconnect_grace_ms: u64,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("db_path", &self.db_path)
            .field("admin_token", &"<redacted>")
            .field("heartbeat_ms", &self.heartbeat_ms)
            .field("max_command_timeout_ms", &self.max_command_timeout_ms)
            .field("disconnect_grace_ms", &self.disconnect_grace_ms)
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
        let disconnect_grace_ms = env_u64("HERMES_HUB_DISCONNECT_GRACE_MS", 30_000)?;
        if !(1_000..=120_000).contains(&heartbeat_ms) {
            bail!("HERMES_HUB_HEARTBEAT_MS must be 1000..=120000");
        }
        if disconnect_grace_ms > 600_000 {
            bail!("HERMES_HUB_DISCONNECT_GRACE_MS must be 0..=600000");
        }
        Ok(Self {
            bind,
            db_path,
            admin_token,
            heartbeat_ms,
            max_command_timeout_ms,
            disconnect_grace_ms,
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
            disconnect_grace_ms: 30_000,
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

/// One disconnect grace window, keyed to the device session.
struct DisconnectLease {
    generation: u64,
    command_ids: Vec<String>,
}

/// Shared state for HTTP and WebSocket handlers.
pub struct HubState {
    pub config: Config,
    pub db: Db,
    pub started_at_ms: u64,
    nodes: RwLock<HashMap<String, NodeConnection>>,
    devices: RwLock<HashMap<String, DeviceConnection>>,
    prompts: RwLock<HashMap<String, PendingPrompt>>,
    pending_disconnects: RwLock<HashMap<String, DisconnectLease>>,
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
            pending_disconnects: RwLock::new(HashMap::new()),
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
        self.clear_disconnect_lease(device_id);
        if let Some(connection) = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(device_id)
        {
            let _ = connection.close.send(true);
        }
    }

    /// Stop commands a device started. The node hangs up a PTY (SIGHUP, then
    /// kill) when it receives `cancel`. If the node is already gone, the
    /// record is closed here so it cannot stay `running` with no requester.
    /// Owner API commands have no requester and are left alone. A replacement
    /// socket is not a disconnect: callers must only use this when the device
    /// is actually gone.
    fn cancel_commands_for_device(&self, device_id: &str, reason: &'static str) {
        let active = match self.db.active_commands_for_requester(device_id) {
            Ok(active) => active,
            Err(err) => {
                warn!(error = %err, "failed to list commands for a device");
                return;
            }
        };
        for (command_id, node_id) in active {
            let delivered = self.send_node(
                &node_id,
                HubToNode::Cancel {
                    command_id: command_id.clone(),
                },
            );
            if !delivered {
                match self.db.finish_command(
                    &command_id,
                    CommandStatus::Cancelled,
                    None,
                    Some(reason),
                    now_ms(),
                ) {
                    Ok(Some(command)) => {
                        self.broadcast_device(HubToDevice::CommandState { command });
                    }
                    Ok(None) => {}
                    Err(err) => {
                        warn!(error = %err, "failed to cancel a command whose node is offline");
                    }
                }
            }
            self.audit(
                "hub",
                "command.cancel_requested",
                json!({
                    "device_id": device_id,
                    "command_id": command_id,
                    "reason": reason,
                    "delivered": delivered,
                }),
            );
        }
    }

    fn cancel_commands_by_id(&self, device_id: &str, command_ids: &[String], reason: &'static str) {
        let active: std::collections::HashSet<String> =
            match self.db.active_commands_for_requester(device_id) {
                Ok(active) => active.into_iter().map(|(id, _)| id).collect(),
                Err(err) => {
                    warn!(error = %err, "failed to list commands for a device");
                    return;
                }
            };
        let wanted: Vec<String> = command_ids
            .iter()
            .filter(|id| active.contains(*id))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Reuse the device-wide helper only after confirming the ids still
        // belong to this requester; a later command started on a new session
        // is not in `command_ids`.
        for command_id in wanted {
            let node_id = match self.db.command_node(&command_id) {
                Ok(Some(node_id)) => node_id,
                Ok(None) => continue,
                Err(err) => {
                    warn!(error = %err, "failed to look up a command during delayed cancel");
                    continue;
                }
            };
            let delivered = self.send_node(
                &node_id,
                HubToNode::Cancel {
                    command_id: command_id.clone(),
                },
            );
            if !delivered {
                match self.db.finish_command(
                    &command_id,
                    CommandStatus::Cancelled,
                    None,
                    Some(reason),
                    now_ms(),
                ) {
                    Ok(Some(command)) => {
                        self.broadcast_device(HubToDevice::CommandState { command });
                    }
                    Ok(None) => {}
                    Err(err) => {
                        warn!(error = %err, "failed to cancel a command whose node is offline");
                    }
                }
            }
            self.audit(
                "hub",
                "command.cancel_requested",
                json!({
                    "device_id": device_id,
                    "command_id": command_id,
                    "reason": reason,
                    "delivered": delivered,
                }),
            );
        }
    }

    fn clear_disconnect_lease(&self, device_id: &str) {
        self.pending_disconnects
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(device_id);
    }

    /// Start (or refresh) the reconnect grace window for this device. A
    /// later reconnect with the same device id drops the lease. Cancel is
    /// sent only if the window expires and the device has not reattached.
    fn schedule_disconnect_cancel(self: &Arc<Self>, device_id: &str) {
        let command_ids = match self.db.active_commands_for_requester(device_id) {
            Ok(active) => active.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            Err(err) => {
                warn!(error = %err, "failed to list commands for a disconnect grace window");
                return;
            }
        };
        let generation = {
            let mut pending = self
                .pending_disconnects
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let lease = pending
                .entry(device_id.to_string())
                .or_insert(DisconnectLease {
                    generation: 0,
                    command_ids: Vec::new(),
                });
            lease.generation = lease.generation.saturating_add(1);
            lease.command_ids = command_ids;
            lease.generation
        };
        let grace = Duration::from_millis(self.config.disconnect_grace_ms);
        if grace.is_zero() {
            self.finish_disconnect_cancel(device_id, generation);
            return;
        }
        let state = Arc::clone(self);
        let device_id = device_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            state.finish_disconnect_cancel(&device_id, generation);
        });
    }

    fn finish_disconnect_cancel(&self, device_id: &str, generation: u64) {
        let command_ids = {
            let devices = self
                .devices
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut pending = self
                .pending_disconnects
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if devices.contains_key(device_id) {
                if pending
                    .get(device_id)
                    .is_some_and(|lease| lease.generation == generation)
                {
                    pending.remove(device_id);
                }
                return;
            }
            match pending.get(device_id) {
                Some(lease) if lease.generation == generation => pending
                    .remove(device_id)
                    .map(|lease| lease.command_ids)
                    .unwrap_or_default(),
                _ => return,
            }
        };
        self.cancel_commands_by_id(device_id, &command_ids, "device disconnected");
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
        self.clear_disconnect_lease(device_id);
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

    /// Returns true when this socket was still the current one. A newer socket
    /// or an earlier revoke already owns the device, and must not have its
    /// commands cancelled by the socket that lost the race.
    fn unregister_device(&self, device_id: &str, conn_id: &str) -> bool {
        let mut devices = self
            .devices
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if devices
            .get(device_id)
            .is_some_and(|connection| connection.conn_id == conn_id)
        {
            devices.remove(device_id);
            true
        } else {
            false
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

    /// Send queued commands that never started, using the stored command id.
    /// Called when a node comes back after a hub restart. Mid-run commands
    /// were already failed at open; this path does not create a new record.
    fn redispatch_queued_for_node(&self, node_id: &str) {
        let queued = match self.db.queued_commands_for_node(node_id) {
            Ok(queued) => queued,
            Err(err) => {
                warn!(error = %err, node_id, "failed to list queued commands for redispatch");
                return;
            }
        };
        for (record, mut request) in queued {
            request.id = record.id.clone();
            if self.send_node(node_id, HubToNode::Command { command: request }) {
                self.broadcast_device(HubToDevice::CommandState {
                    command: record.clone(),
                });
                self.audit(
                    "hub",
                    "command.redispatched",
                    json!({
                        "command_id": record.id,
                        "node_id": record.node_id,
                        "argv": record.argv,
                    }),
                );
            } else {
                warn!(
                    command_id = %record.id,
                    node_id,
                    "queued command was not redispatched; the node socket closed"
                );
            }
        }
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

    fn test_request(id: &str, key: &str) -> hermes_protocol::CommandRequest {
        hermes_protocol::CommandRequest {
            id: id.into(),
            idempotency_key: key.into(),
            argv: vec!["sleep".into(), "60".into()],
            cwd: None,
            env: std::collections::BTreeMap::from([("HERMES_MARK".into(), id.into())]),
            timeout_ms: 60_000,
            secret: None,
            pty: Some(hermes_protocol::PtySpec { cols: 80, rows: 24 }),
        }
    }

    fn bind_node(state: &HubState) -> mpsc::UnboundedReceiver<HubToNode> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (close, _) = tokio::sync::watch::channel(false);
        state.register_node("n-1", "node-conn", tx, close);
        rx
    }

    fn bind_device(state: &HubState, device_id: &str, conn_id: &str) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (close, _) = tokio::sync::watch::channel(false);
        state.register_device(device_id, conn_id, tx, close);
    }

    fn grace_state(
        disconnect_grace_ms: u64,
    ) -> (Arc<HubState>, mpsc::UnboundedReceiver<HubToNode>) {
        let mut config = Config::for_test();
        config.disconnect_grace_ms = disconnect_grace_ms;
        let state = HubState::in_memory(config);
        let request = test_request("c-live", "idem-live");
        state
            .db
            .create_command("n-1", "c-live", &request, Some("device-1"), 10)
            .expect("insert");
        state
            .db
            .mark_command_running("c-live", 11)
            .expect("running");
        let node_rx = bind_node(&state);
        bind_device(&state, "device-1", "first");
        assert!(state.unregister_device("device-1", "first"));
        (state, node_rx)
    }

    #[tokio::test]
    async fn disconnect_grace_keeps_commands_if_the_device_reattaches() {
        // A short real window: paused tokio time plus `advance` can miss a
        // spawned `sleep` if the timer is not yet registered.
        let (state, mut node_rx) = grace_state(80);
        state.schedule_disconnect_cancel("device-1");

        tokio::time::sleep(Duration::from_millis(20)).await;
        bind_device(&state, "device-1", "second");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            node_rx.try_recv().is_err(),
            "a reconnect inside the grace window must not SIGHUP the PTY"
        );
        let live = state.db.command("c-live").expect("get").expect("exists");
        assert_eq!(live.status, CommandStatus::Running);
    }

    #[tokio::test]
    async fn disconnect_grace_cancels_only_after_the_window_expires() {
        let (state, mut node_rx) = grace_state(80);
        state.schedule_disconnect_cancel("device-1");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            node_rx.try_recv().is_err(),
            "cancel must wait until the grace window expires"
        );

        match tokio::time::timeout(Duration::from_millis(500), node_rx.recv()).await {
            Ok(Some(HubToNode::Cancel { command_id })) => assert_eq!(command_id, "c-live"),
            other => panic!("expected cancel after grace expiry, got {other:?}"),
        }
    }

    #[test]
    fn hub_restart_fails_running_and_redispatches_queued_by_command_id() {
        let dir = std::env::temp_dir().join(format!("hermes-hub-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("hub.db");
        let request = test_request("c-queued", "idem-queued");
        let running = test_request("c-run", "idem-run");
        let other = test_request("c-other", "idem-other");
        {
            let db = Db::open(&path).expect("db");
            db.create_command("n-1", "c-queued", &request, Some("device-1"), 10)
                .expect("queued");
            db.create_command("n-1", "c-run", &running, Some("device-1"), 10)
                .expect("running");
            db.mark_command_running("c-run", 11).expect("mark");
            db.create_command("n-2", "c-other", &other, None, 10)
                .expect("other node");
        }
        let mut config = Config::for_test();
        config.db_path = path;
        let state = HubState::open(config).expect("reopen");
        let failed = state.db.command("c-run").expect("get").expect("exists");
        assert_eq!(failed.status, CommandStatus::Failed);
        assert_eq!(failed.message.as_deref(), Some("hub restarted"));
        let queued = state.db.command("c-queued").expect("get").expect("exists");
        assert_eq!(queued.status, CommandStatus::Queued);
        assert_eq!(queued.id, "c-queued");

        let mut node_rx = bind_node(&state);
        state.redispatch_queued_for_node("n-1");
        match node_rx.try_recv() {
            Ok(HubToNode::Command { command }) => {
                assert_eq!(
                    command.id, "c-queued",
                    "redispatch must reuse the command id"
                );
                assert_eq!(command.idempotency_key, "idem-queued");
                assert_eq!(
                    command.env.get("HERMES_MARK").map(String::as_str),
                    Some("c-queued")
                );
                assert_eq!(
                    command.pty,
                    Some(hermes_protocol::PtySpec { cols: 80, rows: 24 })
                );
            }
            other => panic!("expected redispatched command, got {other:?}"),
        }
        assert!(
            node_rx.try_recv().is_err(),
            "a mid-run command and another node's queue must not be sent"
        );

        state.redispatch_queued_for_node("n-1");
        match node_rx.try_recv() {
            Ok(HubToNode::Command { command }) => {
                assert_eq!(command.id, "c-queued");
            }
            other => panic!("idempotent redispatch must reuse the same id, got {other:?}"),
        }
        let still = state.db.command("c-queued").expect("get").expect("exists");
        assert_eq!(still.status, CommandStatus::Queued);
        assert_eq!(still.id, "c-queued");
        std::fs::remove_dir_all(&dir).ok();
    }
}
