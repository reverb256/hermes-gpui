//! Node WebSocket endpoint at `/v1/node`.
//!
//! The node authenticates with its own bearer token, sends `hello` first and
//! then keeps the socket open. The hub routes commands over it and never dials
//! back, which is what makes NAT and laptops work without routes.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use hermes_protocol::{
    Capabilities, Frame, HubToNode, MAX_CHUNK_BYTES, MAX_FRAME_BYTES, NodeToHub, PromptAnswer,
    PromptDecision, SecretPrompt, StreamChannel,
};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::constant_time_eq;
use crate::{ApiError, HubState, Principal, PrincipalKind, now_ms};

pub async fn node_socket(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let principal = state.principal(&headers, PrincipalKind::Node)?;
    Ok(ws
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(err) = run_node(state, principal, socket).await {
                warn!(error = %err, "node connection ended with an error");
            }
        }))
}

async fn run_node(
    state: Arc<HubState>,
    principal: Principal,
    socket: WebSocket,
) -> anyhow::Result<()> {
    let (mut sink, mut stream) = socket.split();

    let first = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
    let hello = match first {
        Ok(Some(Ok(Message::Text(text)))) => match Frame::<NodeToHub>::decode(text.as_str()) {
            Ok(frame) => match frame.message {
                NodeToHub::Hello {
                    node_id,
                    agent_version,
                    host,
                    capabilities,
                    labels,
                } => {
                    if node_id != principal.id {
                        send_error(
                            &mut sink,
                            "identity_mismatch",
                            "hello node_id does not match the token",
                        )
                        .await;
                        return Ok(());
                    }
                    if host.hostname.trim().is_empty() || agent_version.trim().is_empty() {
                        send_error(
                            &mut sink,
                            "invalid_hello",
                            "hostname and agent_version are required",
                        )
                        .await;
                        return Ok(());
                    }
                    (host, capabilities, labels)
                }
                other => {
                    send_error(
                        &mut sink,
                        "hello_required",
                        &format!("first frame must be hello, got {other:?}"),
                    )
                    .await;
                    return Ok(());
                }
            },
            Err(err) => {
                send_error(&mut sink, "bad_frame", &err.to_string()).await;
                return Ok(());
            }
        },
        _ => {
            send_error(
                &mut sink,
                "hello_timeout",
                "no hello frame within 10 seconds",
            )
            .await;
            return Ok(());
        }
    };
    let (host, capabilities, labels) = hello;

    let name = host.hostname.clone();
    state.db.upsert_node(
        &principal.id,
        &name,
        &host,
        &capabilities,
        &labels,
        now_ms(),
    )?;
    let conn_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<HubToNode>();
    let (close_tx, mut close_rx) = watch::channel(false);
    state.register_node(&principal.id, &conn_id, tx.clone(), close_tx);
    if let Some(summary) = state.node_summary(&principal.id) {
        state.broadcast_presence(&summary);
    }
    state.audit(
        "node",
        "node.connected",
        json!({ "node_id": principal.id, "hostname": name, "agent_version": host.agent_version }),
    );

    let _ = tx.send(HubToNode::HelloAck {
        node_id: principal.id.clone(),
        heartbeat_ms: state.config.heartbeat_ms,
        max_command_timeout_ms: state.config.max_command_timeout_ms,
    });

    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match Frame::new(message).encode() {
                Ok(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => warn!(error = %err, "failed to encode a hub frame"),
            }
        }
        let _ = sink.close().await;
    });

    loop {
        tokio::select! {
            changed = close_rx.changed() => {
                let _ = changed;
                debug!(node_id = %principal.id, "hub closed the node socket");
                break;
            }
            incoming = stream.next() => {
                let Some(incoming) = incoming else { break };
                let text = match incoming {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => {
                            queue_error(&tx, "bad_frame", "binary frames must be UTF-8 JSON");
                            continue;
                        }
                    },
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(err) => {
                        debug!(error = %err, "node socket read failed");
                        break;
                    }
                };
                let frame = match Frame::<NodeToHub>::decode(&text) {
                    Ok(frame) => frame,
                    Err(err) => {
                        queue_error(&tx, "bad_frame", &err.to_string());
                        continue;
                    }
                };
                if let Err(err) = handle_node_message(
                    &state,
                    &principal,
                    &capabilities,
                    frame.message,
                ).await {
                    warn!(error = %err, "node message rejected");
                    queue_error(&tx, "rejected", &err.to_string());
                }
            }
        }
    }

    let was_current = state.unregister_node(&principal.id, &conn_id);
    drop(tx);
    let _ = writer.await;
    if !was_current {
        // A newer socket for this node has already replaced this one, or the
        // owner revoked it. Its presence and in-flight commands belong to the
        // replacement or to the revocation path; do not touch them here.
        return Ok(());
    }
    state.db.set_node_online(&principal.id, false, now_ms())?;
    let failed = state
        .db
        .fail_commands_for_node(&principal.id, "node disconnected", now_ms())?;
    for prompt_id in state.take_prompts_for_node(&principal.id) {
        let _ = state
            .db
            .resolve_prompt(&prompt_id, "node_disconnected", now_ms());
        state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed { prompt_id });
    }
    if let Some(summary) = state.node_summary(&principal.id) {
        state.broadcast_presence(&summary);
    }
    for command in failed {
        state.broadcast_device(hermes_protocol::HubToDevice::CommandState { command });
    }
    state.audit(
        "node",
        "node.disconnected",
        json!({ "node_id": principal.id }),
    );
    info!(node_id = %principal.id, "node disconnected");
    Ok(())
}

async fn handle_node_message(
    state: &Arc<HubState>,
    principal: &Principal,
    capabilities: &Capabilities,
    message: NodeToHub,
) -> anyhow::Result<()> {
    match message {
        NodeToHub::Hello { .. } => anyhow::bail!("duplicate hello"),
        NodeToHub::Heartbeat { .. } => {
            state.db.touch_node(&principal.id, now_ms())?;
        }
        NodeToHub::CommandStarted { command_id, .. } => {
            if state.db.command_node(&command_id)?.as_deref() != Some(principal.id.as_str()) {
                anyhow::bail!("command {command_id} does not belong to this node");
            }
            state.db.mark_command_running(&command_id, now_ms())?;
            if let Some(record) = state.db.command(&command_id)? {
                state.broadcast_device(hermes_protocol::HubToDevice::CommandState {
                    command: record,
                });
            }
        }
        NodeToHub::Stream {
            command_id,
            seq,
            channel,
            data_b64,
        } => {
            if state.db.command_node(&command_id)?.as_deref() != Some(principal.id.as_str()) {
                anyhow::bail!("command {command_id} does not belong to this node");
            }
            let decoded = STANDARD
                .decode(&data_b64)
                .map_err(|_| anyhow::anyhow!("stream chunk is not valid base64"))?;
            if decoded.len() > MAX_CHUNK_BYTES {
                anyhow::bail!("stream chunk exceeds {MAX_CHUNK_BYTES} bytes");
            }
            state.db.mark_command_running(&command_id, now_ms())?;
            // Terminal bytes are relayed live but never stored: a terminal can
            // echo a typed password, and the hub keeps no copy of either.
            if !matches!(channel, StreamChannel::Pty) {
                state
                    .db
                    .insert_command_event(&command_id, seq, channel, &data_b64, now_ms())?;
            }
            state.broadcast_device(hermes_protocol::HubToDevice::CommandStream {
                node_id: principal.id.clone(),
                command_id,
                seq,
                channel,
                data_b64,
            });
        }
        NodeToHub::CommandResult {
            command_id,
            status,
            exit_code,
            duration_ms,
            message,
        } => {
            if !status.is_terminal() {
                anyhow::bail!("command result must be terminal, got {}", status.as_str());
            }
            if state.db.command_node(&command_id)?.as_deref() != Some(principal.id.as_str()) {
                anyhow::bail!("command {command_id} does not belong to this node");
            }
            let record = state.db.finish_command(
                &command_id,
                status,
                exit_code,
                message.as_deref(),
                now_ms(),
            )?;
            if let Some(record) = record {
                state.broadcast_device(hermes_protocol::HubToDevice::CommandState {
                    command: record.clone(),
                });
                state.audit(
                    "node",
                    "command.finished",
                    json!({
                        "command_id": record.id,
                        "node_id": record.node_id,
                        "status": record.status.as_str(),
                        "exit_code": record.exit_code,
                        "duration_ms": duration_ms,
                    }),
                );
            }
        }
        NodeToHub::PromptRequest { prompt } => {
            handle_prompt_request(state, principal, capabilities, prompt).await?;
        }
        NodeToHub::PromptClosed { prompt_id } => {
            if state.take_prompt(&prompt_id).is_some() {
                let _ = state
                    .db
                    .resolve_prompt(&prompt_id, "closed_by_node", now_ms());
                state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed { prompt_id });
            }
        }
    }
    Ok(())
}

async fn handle_prompt_request(
    state: &Arc<HubState>,
    principal: &Principal,
    capabilities: &Capabilities,
    prompt: SecretPrompt,
) -> anyhow::Result<()> {
    if prompt.node_id != principal.id {
        anyhow::bail!("prompt node_id does not match the authenticated node");
    }
    if state.db.command_node(&prompt.command_id)?.as_deref() != Some(principal.id.as_str()) {
        anyhow::bail!("prompt command does not belong to this node");
    }
    prompt.validate().map_err(|err| anyhow::anyhow!(err))?;
    if prompt.is_expired(now_ms()) {
        anyhow::bail!("prompt already expired");
    }
    if !capabilities.secret_prompts {
        state.send_node(
            &principal.id,
            HubToNode::PromptAnswer {
                answer: PromptAnswer {
                    prompt_id: prompt.id.clone(),
                    challenge: prompt.challenge.clone(),
                    decision: PromptDecision::Cancel,
                },
            },
        );
        return Ok(());
    }

    state.db.insert_prompt(&prompt)?;
    state.insert_prompt(&prompt);
    state.audit(
        "node",
        "prompt.requested",
        json!({
            "prompt_id": prompt.id,
            "node_id": prompt.node_id,
            "command_id": prompt.command_id,
            "purpose": prompt.purpose,
            "scope": prompt.scope,
            "env_var": prompt.env_var,
        }),
    );

    // Fail closed: with no device online there is nobody to show a card to, so
    // the node gets an immediate cancellation instead of a hanging command.
    if state.device_count() == 0 {
        state.take_prompt(&prompt.id);
        let _ = state
            .db
            .resolve_prompt(&prompt.id, "no_device_online", now_ms());
        state.send_node(
            &principal.id,
            HubToNode::PromptAnswer {
                answer: PromptAnswer {
                    prompt_id: prompt.id.clone(),
                    challenge: prompt.challenge.clone(),
                    decision: PromptDecision::Cancel,
                },
            },
        );
        state.audit(
            "hub",
            "prompt.unavailable",
            json!({ "prompt_id": prompt.id, "reason": "no device online" }),
        );
        return Ok(());
    }

    state.broadcast_device(hermes_protocol::HubToDevice::PromptRequest {
        prompt: prompt.clone(),
    });

    let state_for_expiry = state.clone();
    let prompt_id = prompt.id.clone();
    let challenge = prompt.challenge.clone();
    let node_id = prompt.node_id.clone();
    let expires_at_ms = prompt.expires_at_ms;
    tokio::spawn(async move {
        let remaining = expires_at_ms.saturating_sub(now_ms());
        tokio::time::sleep(Duration::from_millis(remaining)).await;
        if state_for_expiry.take_prompt(&prompt_id).is_none() {
            return;
        }
        let _ = state_for_expiry
            .db
            .resolve_prompt(&prompt_id, "expired", now_ms());
        state_for_expiry.broadcast_device(hermes_protocol::HubToDevice::PromptClosed {
            prompt_id: prompt_id.clone(),
        });
        state_for_expiry.send_node(
            &node_id,
            HubToNode::PromptAnswer {
                answer: PromptAnswer {
                    prompt_id: prompt_id.clone(),
                    challenge,
                    decision: PromptDecision::Cancel,
                },
            },
        );
        state_for_expiry.audit("hub", "prompt.expired", json!({ "prompt_id": prompt_id }));
    });
    Ok(())
}

/// Verify an answer against the pending prompt, then forward it once.
pub(crate) fn accept_prompt_answer(
    state: &Arc<HubState>,
    answer: &PromptAnswer,
) -> Result<(), ApiError> {
    let pending = state
        .take_prompt(&answer.prompt_id)
        .ok_or_else(|| ApiError::not_found("prompt is not pending"))?;
    if now_ms() >= pending.expires_at_ms {
        let _ = state
            .db
            .resolve_prompt(&answer.prompt_id, "expired", now_ms());
        state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed {
            prompt_id: answer.prompt_id.clone(),
        });
        return Err(ApiError::conflict("prompt expired"));
    }
    if !constant_time_eq(answer.challenge.as_bytes(), pending.challenge.as_bytes()) {
        // Put nothing back: a wrong challenge burns the prompt rather than
        // letting an attacker keep guessing against a live challenge.
        let _ = state
            .db
            .resolve_prompt(&answer.prompt_id, "challenge_mismatch", now_ms());
        state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed {
            prompt_id: answer.prompt_id.clone(),
        });
        state.audit(
            "device",
            "prompt.rejected",
            json!({ "prompt_id": answer.prompt_id, "reason": "challenge mismatch" }),
        );
        return Err(ApiError::forbidden("prompt challenge did not match"));
    }
    let outcome = match &answer.decision {
        PromptDecision::Provide { .. } => "provided",
        PromptDecision::Cancel => "cancelled",
    };
    state
        .db
        .resolve_prompt(&answer.prompt_id, outcome, now_ms())?;
    state.broadcast_device(hermes_protocol::HubToDevice::PromptClosed {
        prompt_id: answer.prompt_id.clone(),
    });
    if !state.send_node(
        &pending.node_id,
        HubToNode::PromptAnswer {
            answer: answer.clone(),
        },
    ) {
        return Err(ApiError::conflict(
            "node disconnected while the prompt was open",
        ));
    }
    // The value is deliberately absent from this audit entry.
    state.audit(
        "device",
        "prompt.answered",
        json!({ "prompt_id": answer.prompt_id, "node_id": pending.node_id, "outcome": outcome }),
    );
    Ok(())
}

fn queue_error(tx: &mpsc::UnboundedSender<HubToNode>, code: &str, message: &str) {
    let _ = tx.send(HubToNode::Error {
        code: code.to_string(),
        message: message.to_string(),
    });
}

async fn send_error(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: &str,
    message: &str,
) {
    let frame = Frame::new(HubToNode::Error {
        code: code.to_string(),
        message: message.to_string(),
    });
    if let Ok(text) = frame.encode() {
        let _ = sink.send(Message::Text(text.into())).await;
    }
}
