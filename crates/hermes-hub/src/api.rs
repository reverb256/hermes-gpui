//! Owner HTTP API. Every route except health and enrollment requires the admin
//! bearer token; nodes and devices use their own tokens only on WebSockets.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use hermes_protocol::{CommandRequest, PromptAnswer, PromptDecision, SecretSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{hash_token, random_challenge, random_token};
use crate::{
    ApiError, HubState, PrincipalKind, now_ms,
    util::{clamp_limit, new_id},
};

#[derive(Debug, Deserialize)]
pub struct EnrollmentBody {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct EnrollmentResponse {
    /// Returned exactly once. Only its hash is stored.
    pub code: String,
    pub kind: String,
    pub name: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct EnrollBody {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub id: String,
    /// Returned exactly once. Only its hash is stored.
    pub token: String,
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SubmitCommandBody {
    pub node_id: String,
    pub idempotency_key: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub secret: Option<SecretSpec>,
    /// When set, the command runs on a pseudo-terminal with no timeout.
    #[serde(default)]
    pub pty: Option<hermes_protocol::PtySpec>,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitCommandResponse {
    pub command: hermes_protocol::CommandRecord,
    /// False when the idempotency key already had a command.
    pub created: bool,
}

#[derive(Debug, Serialize)]
pub struct NodesResponse {
    pub nodes: Vec<hermes_protocol::NodeSummary>,
}

#[derive(Debug, Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub online: bool,
}

#[derive(Debug, Serialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceSummary>,
}

#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub entries: Vec<hermes_protocol::AuditEntry>,
}

#[derive(Debug, Serialize)]
pub struct EventsResponse {
    pub events: Vec<hermes_protocol::CommandEvent>,
}

#[derive(Debug, Serialize)]
pub struct PromptsResponse {
    pub prompts: Vec<hermes_protocol::SecretPrompt>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub after: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "hermes-hub",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": hermes_protocol::PROTOCOL_VERSION,
    }))
}

pub async fn ready(
    State(state): State<Arc<HubState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.db.healthy() {
        return Err(ApiError::internal("database is not answering"));
    }
    Ok(Json(json!({ "status": "ready" })))
}

/// Mint a one-time enrollment code. The code is shown once and never stored.
pub async fn create_enrollment(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Json(body): Json<EnrollmentBody>,
) -> Result<Json<EnrollmentResponse>, ApiError> {
    state.admin(&headers)?;
    let kind = PrincipalKind::parse(body.kind.trim())
        .ok_or_else(|| ApiError::bad_request("kind must be node or device"))?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::bad_request("name must be 1..=64 characters"));
    }
    let ttl_ms = body
        .ttl_ms
        .unwrap_or(10 * 60 * 1000)
        .clamp(60_000, 24 * 60 * 60 * 1000);
    let expires_at_ms = now_ms() + ttl_ms;
    let code = random_challenge();
    state
        .db
        .create_enrollment(&hash_token(&code), kind, name, expires_at_ms)?;
    state.audit(
        "owner",
        "enrollment.created",
        json!({ "kind": kind.as_str(), "name": name, "expires_at_ms": expires_at_ms }),
    );
    Ok(Json(EnrollmentResponse {
        code,
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        expires_at_ms,
    }))
}

/// Exchange a single-use code for a long-lived principal token.
pub async fn enroll(
    State(state): State<Arc<HubState>>,
    Json(body): Json<EnrollBody>,
) -> Result<Json<EnrollResponse>, ApiError> {
    let enrollment = state
        .db
        .consume_enrollment(&hash_token(body.code.trim()), now_ms())?
        .ok_or_else(|| ApiError::bad_request("enrollment code is invalid, used or expired"))?;
    let id = new_id(enrollment.kind.as_str());
    let token = random_token();
    state.db.create_principal(
        &id,
        enrollment.kind,
        &enrollment.name,
        &hash_token(&token),
        now_ms(),
    )?;
    state.audit(
        "owner",
        "principal.enrolled",
        json!({ "id": id, "kind": enrollment.kind.as_str(), "name": enrollment.name }),
    );
    Ok(Json(EnrollResponse {
        id,
        token,
        kind: enrollment.kind.as_str().to_string(),
        name: enrollment.name,
    }))
}

pub async fn nodes(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
) -> Result<Json<NodesResponse>, ApiError> {
    state.admin(&headers)?;
    Ok(Json(NodesResponse {
        nodes: state.db.nodes()?,
    }))
}

pub async fn devices(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
) -> Result<Json<DevicesResponse>, ApiError> {
    state.admin(&headers)?;
    let online: std::collections::HashSet<String> = state.connected_device_ids();
    let devices = state
        .db
        .principals(PrincipalKind::Device)?
        .into_iter()
        .map(|principal| DeviceSummary {
            online: online.contains(&principal.id),
            id: principal.id,
            name: principal.name,
        })
        .collect();
    Ok(Json(DevicesResponse { devices }))
}

pub async fn revoke_node(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.admin(&headers)?;
    let principal = state
        .db
        .principal(&id)?
        .ok_or_else(|| ApiError::not_found("node not found"))?;
    if principal.kind != PrincipalKind::Node {
        return Err(ApiError::not_found("node not found"));
    }
    state.db.revoke_principal(&id, now_ms())?;
    state.disconnect_node(&id);
    state.db.set_node_online(&id, false, now_ms())?;
    let failed = state
        .db
        .fail_commands_for_node(&id, "node revoked", now_ms())?;
    for prompt_id in state.take_prompts_for_node(&id) {
        let _ = state
            .db
            .resolve_prompt(&prompt_id, "node_revoked", now_ms());
        state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed { prompt_id });
    }
    if let Some(node) = state.node_summary(&id) {
        state.broadcast_presence(&node);
    }
    for command in failed {
        state.broadcast_device(hermes_protocol::HubToDevice::CommandState { command });
    }
    state.audit(
        "owner",
        "node.revoked",
        json!({ "node_id": id, "name": principal.name }),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke_device(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.admin(&headers)?;
    let principal = state
        .db
        .principal(&id)?
        .ok_or_else(|| ApiError::not_found("device not found"))?;
    if principal.kind != PrincipalKind::Device {
        return Err(ApiError::not_found("device not found"));
    }
    state.db.revoke_principal(&id, now_ms())?;
    state.disconnect_device(&id);
    state.audit(
        "owner",
        "device.revoked",
        json!({ "device_id": id, "name": principal.name }),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn submit_command(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Json(body): Json<SubmitCommandBody>,
) -> Result<Json<SubmitCommandResponse>, ApiError> {
    state.admin(&headers)?;
    if body.node_id.trim().is_empty() {
        return Err(ApiError::bad_request("node_id is required"));
    }
    // A terminal stays open until it exits or is cancelled, so it carries no
    // wall-clock timeout; `CommandRequest::validate` enforces the pairing.
    let default_timeout = if body.pty.is_some() { 0 } else { 30_000 };
    let timeout_ms = body.timeout_ms.unwrap_or(default_timeout);
    if body.pty.is_none() && (timeout_ms == 0 || timeout_ms > state.config.max_command_timeout_ms) {
        return Err(ApiError::bad_request(format!(
            "timeout_ms must be 1..={}",
            state.config.max_command_timeout_ms
        )));
    }
    let request = CommandRequest {
        id: body
            .id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| new_id("cmd")),
        idempotency_key: body.idempotency_key,
        argv: body.argv,
        cwd: body.cwd,
        env: body.env,
        timeout_ms,
        secret: body.secret,
        pty: body.pty,
    };
    request.validate()?;
    match state.db.principal(&body.node_id)? {
        Some(principal) if principal.kind == PrincipalKind::Node => {}
        _ => return Err(ApiError::not_found("node not found")),
    }
    // Insert before checking presence so a retry of an accepted command can
    // still return the first record after the node goes offline.
    let (record, created) =
        state
            .db
            .create_command(&body.node_id, &request.id, &request, now_ms())?;
    if created {
        state.dispatch_command(record.clone(), request)?;
    }
    state.audit(
        "owner",
        "command.requested",
        json!({ "command_id": record.id, "node_id": record.node_id, "created": created }),
    );
    Ok(Json(SubmitCommandResponse {
        command: record,
        created,
    }))
}

pub async fn command(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<hermes_protocol::CommandRecord>, ApiError> {
    state.admin(&headers)?;
    state
        .db
        .command(&id)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("command not found"))
}

pub async fn command_events(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    state.admin(&headers)?;
    if state.db.command(&id)?.is_none() {
        return Err(ApiError::not_found("command not found"));
    }
    let events = state
        .db
        .command_events(
            &id,
            query.after.unwrap_or(0),
            clamp_limit(query.limit, 500, 2_000),
        )?
        .into_iter()
        .map(|stored| stored.event)
        .collect();
    Ok(Json(EventsResponse { events }))
}

pub async fn audit(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    state.admin(&headers)?;
    Ok(Json(AuditResponse {
        entries: state.db.audit_entries(clamp_limit(query.limit, 100, 500))?,
    }))
}

pub async fn prompts(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
) -> Result<Json<PromptsResponse>, ApiError> {
    state.admin(&headers)?;
    Ok(Json(PromptsResponse {
        prompts: state.db.pending_prompts()?,
    }))
}

/// Cancel a prompt from the owner API. The pending answer is dropped, never
/// stored, and the node receives an explicit cancellation.
pub async fn cancel_prompt(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.admin(&headers)?;
    let pending = state
        .take_prompt(&id)
        .ok_or_else(|| ApiError::not_found("prompt not found"))?;
    if !state
        .db
        .resolve_prompt(&id, "cancelled_by_owner", now_ms())?
    {
        return Err(ApiError::conflict("prompt was already resolved"));
    }
    state.send_node(
        &pending.node_id,
        hermes_protocol::HubToNode::PromptAnswer {
            answer: PromptAnswer {
                prompt_id: id.clone(),
                challenge: pending.challenge,
                decision: PromptDecision::Cancel,
            },
        },
    );
    state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed {
        prompt_id: id.clone(),
    });
    state.audit("owner", "prompt.cancelled", json!({ "prompt_id": id }));
    Ok(StatusCode::NO_CONTENT)
}
