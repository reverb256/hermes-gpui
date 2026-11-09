//! Tokio worker that owns the gateway connection and translates both wire
//! surfaces into UI events. The GPUI side never blocks on I/O.

use std::time::Duration;

use hermes_api::{
    ActiveProfile, GatewayClient, GatewayEvent, HostConfig, Profile, RestClient, gateway,
};
use hermes_rpc::ConnectionState;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// `prompt.submit` is fire-and-forget; the backend answers only after the turn
/// settles, and a long tool chain may take minutes (official client: 30 min).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connection(ConnectionState),
    Profiles(Vec<Profile>),
    ActiveProfile(ActiveProfile),
    SessionReady {
        session_id: String,
        profile: Option<String>,
    },
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    MessageStart,
    MessageComplete,
    ToolActivity(String),
    Notice(String),
    Error(String),
}

#[derive(Debug)]
pub enum Command {
    Submit(String),
    Interrupt,
    NewSession,
    /// Retarget the next session at another profile.
    SwitchProfile(String),
    Refresh,
}

/// Handle used by the UI; the runtime lives as long as this value.
pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
    _runtime: std::sync::Arc<tokio::runtime::Runtime>,
}

impl Backend {
    pub fn spawn(host: HostConfig) -> (Self, mpsc::UnboundedReceiver<BackendEvent>) {
        let runtime = std::sync::Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("hermes-io")
                .build()
                .expect("build tokio runtime"),
        );

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        runtime.spawn(worker(host, cmd_rx, event_tx));

        (
            Self {
                commands: cmd_tx,
                _runtime: runtime,
            },
            event_rx,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

async fn worker(
    host: HostConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<BackendEvent>,
) {
    let rest = RestClient::new(&host);
    let mut backoff = RECONNECT_MIN;

    loop {
        let _ = events.send(BackendEvent::Connection(ConnectionState::Connecting));

        let client = match GatewayClient::connect(host.connection_config()).await {
            Ok(client) => client,
            Err(err) => {
                let _ = events.send(BackendEvent::Error(format!(
                    "connect {} failed: {err}",
                    host.ws_url()
                )));
                let _ = events.send(BackendEvent::Connection(ConnectionState::Closed));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };

        backoff = RECONNECT_MIN;
        let _ = events.send(BackendEvent::Connection(ConnectionState::Open));
        refresh(&rest, &events).await;

        let mut event_rx = client.subscribe();
        let mut state_rx = client.state_changes();
        let mut request_rx = client.take_server_requests();
        let mut session: Option<String> = None;
        let mut session_profile = host.profile.clone();

        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { return };
                    match command {
                        Command::Refresh => refresh(&rest, &events).await,
                        Command::NewSession => {
                            session = None;
                            let _ = events.send(BackendEvent::MessageComplete);
                        }
                        Command::SwitchProfile(name) => {
                            session_profile = Some(name);
                            session = None;
                            let _ = events.send(BackendEvent::MessageComplete);
                        }
                        Command::Interrupt => {
                            if let Some(session_id) = &session {
                                if let Err(err) = gateway::interrupt(&client, session_id).await {
                                    let _ = events.send(BackendEvent::Error(err.to_string()));
                                }
                            }
                        }
                        Command::Submit(text) => {
                            if session.is_none() {
                                match gateway::create_session(&client, session_profile.as_deref()).await {
                                    Ok(created) => {
                                        session = Some(created.session_id.clone());
                                        let _ = events.send(BackendEvent::SessionReady {
                                            session_id: created.session_id.clone(),
                                            profile: session_profile.clone(),
                                        });
                                    }
                                    Err(err) => {
                                        let _ = events.send(BackendEvent::Error(format!(
                                            "session.create failed: {err}"
                                        )));
                                        continue;
                                    }
                                }
                            }
                            let Some(session_id) = session.clone() else { continue };
                            let submitter = client.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                if let Err(err) = submitter
                                    .request_value_with_timeout(
                                        "prompt.submit",
                                        json!({ "session_id": session_id, "text": text }),
                                        PROMPT_TIMEOUT,
                                    )
                                    .await
                                {
                                    let _ = events.send(BackendEvent::Error(body_of(&err)));
                                }
                            });
                        }
                    }
                }
                event = event_rx.recv() => {
                    let Ok(event) = event else { continue };
                    route_event(&events, event);
                }
                request = request_rx.recv() => {
                    let Some(request) = request else { continue };
                    // Approvals and clarify cards land here. Until the card UI
                    // exists, answer with the backend's own default so the
                    // agent turn is never wedged on a UI that cannot decide.
                    let _ = events.send(BackendEvent::Notice(format!(
                        "{} requested ({}) — auto-declined; approval UI not wired yet",
                        request.method, request.id
                    )));
                    client.fail_request(&request.id, hermes_rpc::SESSION_NOT_SHOWN, "no card UI");
                }
                changed = state_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let state = state_rx.borrow().clone();
                    let closed = state == ConnectionState::Closed;
                    let _ = events.send(BackendEvent::Connection(state));
                    if closed {
                        break;
                    }
                }
            }
        }

        let _ = events.send(BackendEvent::Error("gateway socket closed".into()));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn refresh(rest: &RestClient, events: &mpsc::UnboundedSender<BackendEvent>) {
    match rest.profiles().await {
        Ok(profiles) => {
            let _ = events.send(BackendEvent::Profiles(profiles));
        }
        Err(err) => {
            let _ = events.send(BackendEvent::Error(err.to_string()));
        }
    }
    match rest.active_profile().await {
        Ok(active) => {
            let _ = events.send(BackendEvent::ActiveProfile(active));
        }
        Err(err) => {
            let _ = events.send(BackendEvent::Error(err.to_string()));
        }
    }
}

fn route_event(events: &mpsc::UnboundedSender<BackendEvent>, event: GatewayEvent) {
    if let Some(text) = event.text_delta() {
        let _ = events.send(BackendEvent::Delta {
            text: text.to_string(),
        });
        return;
    }
    if let Some(text) = event.reasoning_delta() {
        let _ = events.send(BackendEvent::Reasoning {
            text: text.to_string(),
        });
        return;
    }

    match event.kind.as_str() {
        "message.start" => {
            let _ = events.send(BackendEvent::MessageStart);
        }
        "message.complete" => {
            let _ = events.send(BackendEvent::MessageComplete);
        }
        "tool.start" | "tool.complete" => {
            let name = event
                .payload
                .get("tool")
                .or_else(|| event.payload.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let _ = events.send(BackendEvent::ToolActivity(format!(
                "{} {name}",
                if event.kind == "tool.start" {
                    "→"
                } else {
                    "✓"
                }
            )));
        }
        "session.title" => {
            if let Some(title) = event.payload.get("title").and_then(Value::as_str) {
                let _ = events.send(BackendEvent::Notice(title.to_string()));
            }
        }
        "error" => {
            let message = event
                .payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("backend error");
            let _ = events.send(BackendEvent::Error(message.to_string()));
        }
        "notice" => {
            if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                let _ = events.send(BackendEvent::Notice(text.to_string()));
            }
        }
        _ => {}
    }
}

fn body_of(err: &hermes_rpc::RpcError) -> String {
    err.to_string()
}
