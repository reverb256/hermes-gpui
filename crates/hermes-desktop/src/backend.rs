//! Tokio worker that owns the gateway connection and translates both wire
//! surfaces into UI events. The GPUI side never blocks on I/O.

use std::path::PathBuf;
use std::time::Duration;

use hermes_api::{
    ActiveProfile, EnvEntry, GatewayClient, GatewayEvent, HostConfig, Profile, ProfileDetail,
    RestClient, gateway,
};
use hermes_rpc::ConnectionState;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// `prompt.submit` is fire-and-forget; the backend answers only after the turn
/// settles, and a long tool chain may take minutes (official client: 30 min).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// `image.attach_bytes` caps the payload server-side; refuse locally first.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// An `approval` server request (`tui_gateway/methods_prompt.py`).
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// JSON-RPC id of the server request awaiting an answer.
    pub id: String,
    pub session_id: Option<String>,
    /// The approval queue's own id, keyed by `approval.respond`.
    pub request_id: Option<String>,
    pub command: String,
    pub description: String,
    pub allow_permanent: bool,
}

impl ApprovalRequest {
    fn from_request(id: String, params: &Value) -> Self {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            id,
            session_id: params
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            request_id: params
                .get("request_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            command: text("command"),
            description: text("description"),
            allow_permanent: params
                .get("allow_permanent")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

/// A `clarify` server request, flattened to its first question.
#[derive(Debug, Clone)]
pub struct ClarifyRequest {
    pub id: String,
    pub session_id: Option<String>,
    pub question: String,
    pub choices: Vec<String>,
}

impl ClarifyRequest {
    fn from_request(id: String, params: &Value) -> Option<Self> {
        let first = params.get("questions")?.as_array()?.first()?.clone();
        let question = first.get("question").and_then(Value::as_str)?.to_string();
        let choices = first
            .get("choices")
            .and_then(Value::as_array)
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id,
            session_id: params
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            question,
            choices,
        })
    }

    /// Wire shape `tools/clarify_tool.py` reads back: `{answers, outcome}`.
    fn answer(&self, answer: &str) -> Value {
        json!({
            "answers": { self.question.clone(): answer },
            "outcome": "submitted",
        })
    }
}

/// What the user picked on an approval card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Once,
    Always,
    Deny,
}

impl ApprovalChoice {
    /// The gateway's own vocabulary (`approval.respond`).
    fn as_wire(self) -> &'static str {
        match self {
            ApprovalChoice::Once => "once",
            ApprovalChoice::Always => "always",
            ApprovalChoice::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connection(ConnectionState),
    Profiles(Vec<Profile>),
    ActiveProfile(ActiveProfile),
    Routines(Vec<String>),
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
    Approval(ApprovalRequest),
    Clarify(ClarifyRequest),
    Attached {
        name: String,
    },
    /// `profiles.describe` answer for the inspector.
    ProfileDetail(ProfileDetail),
    /// The host `.env`, redacted server-side.
    Env(Vec<(String, EnvEntry)>),
    Avatar {
        profile: String,
        mime: String,
        bytes: Vec<u8>,
    },
    AvatarCleared {
        profile: String,
    },
    /// A line for the inspector's status strip, not the transcript.
    InspectorStatus(String),
}

#[derive(Debug)]
pub enum Command {
    Submit(String),
    Interrupt,
    NewSession,
    /// Retarget the next session at another profile.
    SwitchProfile(String),
    Refresh,
    Approve {
        request: ApprovalRequest,
        choice: ApprovalChoice,
    },
    Answer {
        request: ClarifyRequest,
        answer: String,
    },
    Decline {
        id: String,
        session_id: Option<String>,
    },
    Attach(PathBuf),
    /// Load a profile's connectors, secrets and avatar.
    Inspect {
        profile: String,
    },
    ConfigureProfile {
        profile: String,
        patch: Value,
    },
    AttachAvatar {
        profile: String,
        path: PathBuf,
    },
    ClearAvatar {
        profile: String,
    },
    SetSecret {
        profile: String,
        key: String,
        value: String,
    },
    DeleteSecret {
        profile: String,
        key: String,
    },
}

/// Handle used by the UI; the runtime lives as long as this value.
#[derive(Clone)]
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
    let mut host = host;
    let mut backoff = RECONNECT_MIN;

    loop {
        let (authed, notes) = hermes_api::authenticate(&host).await;
        for note in notes {
            let _ = events.send(BackendEvent::Error(format!("auth: {note}")));
        }
        host = authed;
        let rest = RestClient::new(&host);

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
        refresh(&client, &rest, &events).await;

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
                        Command::Refresh => refresh(&client, &rest, &events).await,
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
                        Command::Approve { request, choice } => {
                            let wire = choice.as_wire();
                            client.respond(&request.id, json!({ "choice": wire }));
                            // The queue that actually gates the tool is keyed by
                            // request_id; the frame above only unblocks the turn.
                            if let Some(request_id) = &request.request_id {
                                let _ = client
                                    .request_value_with_timeout(
                                        "approval.respond",
                                        json!({
                                            "session_id": session,
                                            "choice": wire,
                                            "request_id": request_id,
                                        }),
                                        Duration::from_secs(20),
                                    )
                                    .await;
                            }
                            let _ = events.send(BackendEvent::Notice(format!(
                                "approval: {wire}"
                            )));
                        }
                        Command::Answer { request, answer } => {
                            client.respond(&request.id, request.answer(&answer));
                        }
                        Command::Decline { id, session_id } => {
                            let _ = session_id;
                            client.respond(&id, json!({ "answers": {}, "outcome": "cancelled" }));
                        }
                        Command::Attach(path) => {
                            if session.is_none() {
                                match gateway::create_session(&client, session_profile.as_deref()).await {
                                    Ok(created) => {
                                        session = Some(created.session_id.clone());
                                        let _ = events.send(BackendEvent::SessionReady {
                                            session_id: created.session_id,
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
                            attach(&client, &events, &session_id, path).await;
                        }
                        Command::Inspect { profile } => {
                            match gateway::describe_profile(&client, &profile).await {
                                Ok(detail) => {
                                    tracing::debug!(
                                        profile = %profile,
                                        toolsets = detail.toolsets.len(),
                                        mcp = detail.mcp_servers.len(),
                                        "inspector: describe ok"
                                    );
                                    let _ = events.send(BackendEvent::ProfileDetail(detail));
                                }
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "connectors failed: {err}"
                                    )));
                                }
                            }
                            match rest.env_vars(Some(&profile)).await {
                                Ok(rows) => {
                                    tracing::debug!(
                                        rows = rows.len(),
                                        set = rows.iter().filter(|(_, entry)| entry.is_set).count(),
                                        "inspector: secrets loaded"
                                    );
                                    let _ = events.send(BackendEvent::Env(rows));
                                }
                                Err(err) => {
                                    tracing::warn!("inspector: secrets failed: {err}");
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "secrets failed: {err}"
                                    )));
                                }
                            }
                            match gateway::profile_asset(&client, &profile).await {
                                Ok(Some(asset)) => match base64_decode(&asset.data) {
                                    Some(bytes) => {
                                        let _ = events.send(BackendEvent::Avatar {
                                            profile,
                                            mime: asset.mime,
                                            bytes,
                                        });
                                    }
                                    None => {
                                        let _ = events.send(BackendEvent::InspectorStatus(
                                            "avatar could not be decoded".into(),
                                        ));
                                    }
                                },
                                Ok(None) => {}
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "avatar failed: {err}"
                                    )));
                                }
                            }
                        }
                        Command::ConfigureProfile { profile, patch } => {
                            match gateway::configure_profile(&client, &profile, patch).await {
                                Ok(_) => {
                                    let _ = events
                                        .send(BackendEvent::InspectorStatus("saved".into()));
                                    refresh(&client, &rest, &events).await;
                                }
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "save failed: {err}"
                                    )));
                                }
                            }
                        }
                        Command::AttachAvatar { profile, path } => {
                            attach_avatar(&client, &events, profile, path).await;
                        }
                        Command::ClearAvatar { profile } => {
                            match gateway::clear_profile_avatar(&client, &profile).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::AvatarCleared {
                                        profile: profile.clone(),
                                    });
                                    let _ = events
                                        .send(BackendEvent::InspectorStatus("avatar removed".into()));
                                    refresh(&client, &rest, &events).await;
                                }
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "remove failed: {err}"
                                    )));
                                }
                            }
                        }
                        Command::SetSecret { profile, key, value } => {
                            match rest.set_env(Some(&profile), &key, &value).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "saved {key}"
                                    )));
                                    if let Ok(rows) = rest.env_vars(Some(&profile)).await {
                                        let _ = events.send(BackendEvent::Env(rows));
                                    }
                                }
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "save {key} failed: {err}"
                                    )));
                                }
                            }
                        }
                        Command::DeleteSecret { profile, key } => {
                            match rest.delete_env(Some(&profile), &key).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "removed {key}"
                                    )));
                                    if let Ok(rows) = rest.env_vars(Some(&profile)).await {
                                        let _ = events.send(BackendEvent::Env(rows));
                                    }
                                }
                                Err(err) => {
                                    let _ = events.send(BackendEvent::InspectorStatus(format!(
                                        "remove {key} failed: {err}"
                                    )));
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
                                    let _ = events.send(BackendEvent::Error(err.to_string()));
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
                    route_request(&client, &events, request);
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

/// Surface a blocking server request as a card instead of declining it.
fn route_request(
    client: &GatewayClient,
    events: &mpsc::UnboundedSender<BackendEvent>,
    request: hermes_rpc::ServerRequest,
) {
    match request.method.as_str() {
        "approval" => {
            let _ = events.send(BackendEvent::Approval(ApprovalRequest::from_request(
                request.id,
                &request.params,
            )));
        }
        "clarify" => {
            match ClarifyRequest::from_request(request.id.clone(), &request.params) {
                Some(clarify) => {
                    let _ = events.send(BackendEvent::Clarify(clarify));
                }
                None => {
                    // A clarify with no questions is the backend's own cancel.
                    client.respond(
                        &request.id,
                        json!({ "answers": {}, "outcome": "cancelled" }),
                    );
                }
            }
        }
        other => {
            let _ = events.send(BackendEvent::Notice(format!(
                "{other} requested — no card for it yet"
            )));
            client.fail_request(
                &request.id,
                hermes_rpc::METHOD_NOT_FOUND,
                "no handler in hermes-gpui",
            );
        }
    }
}

async fn attach(
    client: &GatewayClient,
    events: &mpsc::UnboundedSender<BackendEvent>,
    session_id: &str,
    path: PathBuf,
) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".into());

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = events.send(BackendEvent::Error(format!("read {name} failed: {err}")));
            return;
        }
    };

    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let is_image = matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    );

    let outcome = if is_image {
        if bytes.len() > MAX_IMAGE_BYTES {
            let _ = events.send(BackendEvent::Error(format!(
                "{name} is larger than {} MB",
                MAX_IMAGE_BYTES / (1024 * 1024)
            )));
            return;
        }
        client
            .request_value_with_timeout(
                "image.attach_bytes",
                json!({
                    "session_id": session_id,
                    "filename": name,
                    "ext": extension,
                    "content_base64": hermes_rpc::base64_encode(&bytes),
                }),
                Duration::from_secs(60),
            )
            .await
    } else {
        // Data URL keeps the upload host-independent: the pod never sees the
        // client's filesystem, unlike a bare `path`.
        client
            .request_value_with_timeout(
                "file.attach",
                json!({
                    "session_id": session_id,
                    "name": name,
                    "data_url": format!(
                        "data:application/octet-stream;base64,{}",
                        hermes_rpc::base64_encode(&bytes)
                    ),
                }),
                Duration::from_secs(60),
            )
            .await
    };

    match outcome {
        Ok(_) => {
            let _ = events.send(BackendEvent::Attached { name });
        }
        Err(err) => {
            let _ = events.send(BackendEvent::Error(format!("attach {name}: {err}")));
        }
    }
}

async fn attach_avatar(
    client: &GatewayClient,
    events: &mpsc::UnboundedSender<BackendEvent>,
    profile: String,
    path: PathBuf,
) {
    /// `profiles.set_asset` refuses anything larger.
    const MAX_AVATAR_BYTES: usize = 2_000_000;

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = events.send(BackendEvent::InspectorStatus(format!(
                "read {} failed: {err}",
                path.display()
            )));
            return;
        }
    };
    let Some(mime) = image_mime(&bytes) else {
        let _ = events.send(BackendEvent::InspectorStatus(
            "avatar must be a PNG, JPEG or WebP image".into(),
        ));
        return;
    };
    if bytes.len() > MAX_AVATAR_BYTES {
        let _ = events.send(BackendEvent::InspectorStatus(format!(
            "avatar is larger than {} MB",
            MAX_AVATAR_BYTES / (1024 * 1024)
        )));
        return;
    }

    let data_url = crate::inspector::avatar_data_url(mime, &hermes_rpc::base64_encode(&bytes));
    match gateway::set_profile_avatar(client, &profile, &data_url).await {
        Ok(_) => {
            let _ = events.send(BackendEvent::Avatar {
                profile: profile.clone(),
                mime: mime.to_string(),
                bytes,
            });
            let _ = events.send(BackendEvent::InspectorStatus("avatar saved".into()));
        }
        Err(err) => {
            let _ = events.send(BackendEvent::InspectorStatus(format!(
                "avatar save failed: {err}"
            )));
        }
    }
}

/// Magic-byte sniffing: the gateway checks the same way server-side.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Standard base64 (padding optional). The workspace's encoder lives in
/// `hermes-rpc`; this is its decoder for avatar payloads.
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for byte in input.bytes() {
        if matches!(byte, b'=' | b'\n' | b'\r' | b' ' | b'\t') {
            continue;
        }
        acc = (acc << 6) | u32::from(value(byte)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

async fn refresh(
    client: &GatewayClient,
    rest: &RestClient,
    events: &mpsc::UnboundedSender<BackendEvent>,
) {
    // `profiles.list` carries the last-message preview and per-bot look that
    // the REST roster lacks; REST is the fallback when the method is missing.
    match gateway::list_profiles(client).await {
        Ok(profiles) if !profiles.is_empty() => {
            let _ = events.send(BackendEvent::Profiles(profiles));
        }
        _ => match rest.profiles().await {
            Ok(profiles) => {
                let _ = events.send(BackendEvent::Profiles(profiles));
            }
            Err(err) => {
                let _ = events.send(BackendEvent::Error(err.to_string()));
            }
        },
    }
    match rest.active_profile().await {
        Ok(active) => {
            let _ = events.send(BackendEvent::ActiveProfile(active));
        }
        Err(err) => {
            let _ = events.send(BackendEvent::Error(err.to_string()));
        }
    }
    if let Ok(jobs) = rest.cron_jobs().await {
        let mut names = Vec::new();
        if let Some(jobs) = jobs.get("jobs").and_then(Value::as_array) {
            for job in jobs {
                if let Some(name) = job.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
        let _ = events.send(BackendEvent::Routines(names));
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

/// Headless verification against a live host: REST, WS handshake, session
/// creation. Used by `hermes-desktop --probe`.
pub async fn probe(host: &HostConfig) -> Vec<String> {
    let mut report = Vec::new();
    let (host, notes) = hermes_api::authenticate(host).await;
    for note in &notes {
        report.push(format!("auth: {note}"));
    }
    if host.cookie.is_some() {
        report.push("auth: session cookie acquired".to_string());
    }
    if host.ticket.is_some() {
        report.push("auth: ws ticket minted".to_string());
    }
    let rest = RestClient::new(&host);

    match rest.health().await {
        Ok(health) => report.push(format!(
            "health: ok (version {})",
            health
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )),
        Err(err) => {
            report.push(format!("health: FAILED {err}"));
            return report;
        }
    }

    match rest.profiles().await {
        Ok(profiles) => {
            let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).take(6).collect();
            report.push(format!(
                "profiles: {} ({}{})",
                profiles.len(),
                names.join(", "),
                if profiles.len() > 6 { ", …" } else { "" }
            ));
        }
        Err(err) => report.push(format!("profiles: FAILED {err}")),
    }

    let client = match GatewayClient::connect(host.connection_config()).await {
        Ok(client) => {
            report.push("gateway ws: open".to_string());
            client
        }
        Err(err) => {
            report.push(format!("gateway ws: FAILED {err}"));
            return report;
        }
    };

    match gateway::create_session(&client, host.profile.as_deref()).await {
        Ok(created) => report.push(format!("session.create: {}", created.session_id)),
        Err(err) => report.push(format!("session.create: FAILED {err}")),
    }

    // The inspector's two data surfaces: one over the gateway, one over REST.
    let profile = host.profile.clone().unwrap_or_else(|| "default".into());
    match gateway::describe_profile(&client, &profile).await {
        Ok(detail) => report.push(format!(
            "describe {profile}: {} toolset(s), {} MCP server(s)",
            detail.toolsets.len(),
            detail.mcp_servers.len()
        )),
        Err(err) => report.push(format!("describe {profile}: FAILED {err}")),
    }
    match rest.env_vars(host.profile.as_deref()).await {
        Ok(rows) => report.push(format!(
            "secrets: {} catalogued, {} set",
            rows.len(),
            rows.iter().filter(|(_, entry)| entry.is_set).count()
        )),
        Err(err) => report.push(format!("secrets: FAILED {err}")),
    }

    client.close();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decoder_round_trips_the_encoder() {
        for payload in [
            b"".as_slice(),
            b"a".as_slice(),
            b"ab".as_slice(),
            b"avatar-bytes".as_slice(),
            &[0u8, 255, 128, 7, 9, 9, 42],
        ] {
            let encoded = hermes_rpc::base64_encode(payload);
            assert_eq!(base64_decode(&encoded).as_deref(), Some(payload));
        }
    }

    #[test]
    fn base64_decoder_rejects_non_alphabet_bytes() {
        assert_eq!(base64_decode("AAAA$AAA"), None);
        assert_eq!(
            base64_decode("QVZBVEFS").as_deref(),
            Some(b"AVATAR".as_slice())
        );
    }

    #[test]
    fn avatar_mime_is_sniffed_from_magic_bytes() {
        assert_eq!(
            image_mime(&[0x89, b'P', b'N', b'G', 0x0d]),
            Some("image/png")
        );
        assert_eq!(image_mime(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp.extend_from_slice(b"VP8 ");
        assert_eq!(image_mime(&webp), Some("image/webp"));
        assert_eq!(image_mime(b"GIF89a"), None);
    }
}
