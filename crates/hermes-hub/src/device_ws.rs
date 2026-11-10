//! Device WebSocket endpoint at `/v1/device`.
//!
//! This is the transport the GPUI client uses: it receives the node list,
//! submits commands, watches their streams and answers secret prompt cards.
//! A device token is scoped to one device and can be revoked independently.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use hermes_protocol::{DeviceToHub, Frame, HubToDevice, MAX_FRAME_BYTES};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{ApiError, HubState, Principal, PrincipalKind, now_ms};

pub async fn device_socket(
    State(state): State<Arc<HubState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let principal = state.principal(&headers, PrincipalKind::Device)?;
    Ok(ws
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(err) = run_device(state, principal, socket).await {
                warn!(error = %err, "device connection ended with an error");
            }
        }))
}

async fn run_device(
    state: Arc<HubState>,
    principal: Principal,
    socket: WebSocket,
) -> anyhow::Result<()> {
    let (mut sink, mut stream) = socket.split();
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
    let device_name = match first {
        Ok(Some(Ok(Message::Text(text)))) => match Frame::<DeviceToHub>::decode(text.as_str()) {
            Ok(frame) => match frame.message {
                DeviceToHub::Hello { device_id, name } => {
                    if device_id != principal.id {
                        send_error(
                            &mut sink,
                            "identity_mismatch",
                            "hello device_id does not match the token",
                        )
                        .await;
                        return Ok(());
                    }
                    name
                }
                _ => {
                    send_error(&mut sink, "hello_required", "first frame must be hello").await;
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

    let conn_id = Uuid::new_v4().to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<HubToDevice>();
    let (close_tx, mut close_rx) = watch::channel(false);
    state.register_device(&principal.id, &conn_id, tx.clone(), close_tx);
    state.audit(
        "device",
        "device.connected",
        json!({ "device_id": principal.id, "name": device_name }),
    );

    let _ = tx.send(HubToDevice::HelloAck {
        device_id: principal.id.clone(),
        heartbeat_ms: state.config.heartbeat_ms,
    });
    let _ = tx.send(HubToDevice::NodeList {
        nodes: state.db.nodes()?,
    });
    for prompt in state.db.pending_prompts()? {
        let _ = tx.send(HubToDevice::PromptRequest { prompt });
    }

    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match Frame::new(message).encode() {
                Ok(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => warn!(error = %err, "failed to encode a device frame"),
            }
        }
        let _ = sink.close().await;
    });

    loop {
        tokio::select! {
            changed = close_rx.changed() => {
                let _ = changed;
                debug!(device_id = %principal.id, "hub closed the device socket");
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
                        debug!(error = %err, "device socket read failed");
                        break;
                    }
                };
                let frame = match Frame::<DeviceToHub>::decode(&text) {
                    Ok(frame) => frame,
                    Err(err) => {
                        queue_error(&tx, "bad_frame", &err.to_string());
                        continue;
                    }
                };
                if let Err(err) = handle_device_message(&state, &principal, frame.message) {
                    warn!(error = %err, "device message rejected");
                    queue_error(&tx, "rejected", err.message());
                }
            }
        }
    }

    state.unregister_device(&principal.id, &conn_id);
    drop(tx);
    let _ = writer.await;
    state.audit(
        "device",
        "device.disconnected",
        json!({ "device_id": principal.id }),
    );
    info!(device_id = %principal.id, "device disconnected");
    Ok(())
}

fn handle_device_message(
    state: &Arc<HubState>,
    principal: &Principal,
    message: DeviceToHub,
) -> Result<(), ApiError> {
    match message {
        DeviceToHub::Hello { .. } => Err(ApiError::bad_request("duplicate hello")),
        DeviceToHub::Ping { at_ms } => {
            let _ = state.send_device(
                &principal.id,
                HubToDevice::Pong {
                    at_ms,
                    server_at_ms: now_ms(),
                },
            );
            Ok(())
        }
        DeviceToHub::SubmitCommand { node_id, command } => {
            command.validate()?;
            if command.timeout_ms > state.config.max_command_timeout_ms {
                return Err(ApiError::bad_request(format!(
                    "timeout_ms must be <= {}",
                    state.config.max_command_timeout_ms
                )));
            }
            match state.db.principal(&node_id)? {
                Some(owner) if owner.kind == PrincipalKind::Node => {}
                _ => return Err(ApiError::not_found("node not found")),
            }
            // Insert before presence is checked so an idempotent retry still
            // finds the first record if the node has gone offline.
            let (record, created) =
                state
                    .db
                    .create_command(&node_id, &command.id, &command, now_ms())?;
            if created {
                state.dispatch_command(record.clone(), command)?;
            } else {
                let _ = state.send_device(
                    &principal.id,
                    HubToDevice::CommandState {
                        command: record.clone(),
                    },
                );
            }
            state.audit(
                "device",
                "command.requested",
                json!({
                    "device_id": principal.id,
                    "command_id": record.id,
                    "node_id": node_id,
                    "created": created,
                }),
            );
            Ok(())
        }
        DeviceToHub::CancelCommand { command_id } => {
            let record = state
                .db
                .command(&command_id)?
                .ok_or_else(|| ApiError::not_found("command not found"))?;
            if record.status.is_terminal() {
                return Err(ApiError::conflict("command already finished"));
            }
            if !state.send_node(
                &record.node_id,
                hermes_protocol::HubToNode::Cancel {
                    command_id: command_id.clone(),
                },
            ) {
                return Err(ApiError::conflict("node is offline"));
            }
            state.audit(
                "device",
                "command.cancel_requested",
                json!({ "device_id": principal.id, "command_id": command_id }),
            );
            Ok(())
        }
        DeviceToHub::PromptAnswer { answer } => {
            crate::node_ws::accept_prompt_answer(state, &answer)
        }
    }
}

fn queue_error(tx: &mpsc::UnboundedSender<HubToDevice>, code: &str, message: &str) {
    let _ = tx.send(HubToDevice::Error {
        code: code.to_string(),
        message: message.to_string(),
    });
}

async fn send_error(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: &str,
    message: &str,
) {
    let frame = Frame::new(HubToDevice::Error {
        code: code.to_string(),
        message: message.to_string(),
    });
    if let Ok(text) = frame.encode() {
        let _ = sink.send(Message::Text(text.into())).await;
    }
}
