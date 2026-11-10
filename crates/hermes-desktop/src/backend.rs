//! Tokio worker that owns the gateway connection and translates both wire
//! surfaces into UI events. The GPUI side never blocks on I/O.

use std::path::PathBuf;
use std::time::Duration;

use hermes_api::{
    ActiveProfile, AuxiliaryTask, CronDeliveryTarget, CronJob, EnvEntry, GatewayClient,
    GatewayEvent, HostConfig, McpCatalog, McpServer, MemoryStatus, MoaConfig, ModelInfo,
    ModelOptions, OAuthPoll, OAuthProvider, OAuthStart, PortalStatus, Profile, ProfileDetail,
    RestClient, SessionSummary, Skill, SkillHubPreview, SkillHubResult, SkillHubScan,
    UsageAnalytics, gateway,
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
    Spoken,
    SpeakFailed(String),
    Media {
        path: String,
        mime: String,
        bytes: Vec<u8>,
    },
    MediaFailed {
        path: String,
        message: String,
    },
    /// `profiles.describe` answer for the inspector.
    ProfileDetail(ProfileDetail),
    /// The host `.env`, redacted server-side.
    Env(Vec<(String, EnvEntry)>),
    /// Usage rollups plus the model and portal facts the page shows beside them.
    Usage {
        analytics: UsageAnalytics,
        model: Option<ModelInfo>,
        portal: Option<PortalStatus>,
    },
    /// Analytics refused (a host with an unreadable `state.db`, for example).
    UsageFailed(String),
    /// `GET /api/model/options` for the picker.
    Models(ModelOptions),
    ModelsFailed(String),
    ModelStatus(String),
    /// The model after a successful switch, so the usage page stays honest.
    ModelInfo(ModelInfo),
    Auxiliary(Vec<AuxiliaryTask>),
    AuxiliaryFailed(String),
    AuxiliarySaved {
        task: String,
        provider: String,
        model: String,
    },
    ReasoningEffort(String),
    ReasoningFailed(String),
    Moa(MoaConfig),
    MoaSaved(MoaConfig),
    MoaFailed(String),
    Cron(Vec<CronJob>, Vec<CronDeliveryTarget>),
    CronFailed(String),
    CronSaved(String),
    CronRuns {
        id: String,
        runs: Vec<SessionSummary>,
    },
    OAuth(Vec<OAuthProvider>),
    OAuthStarted {
        provider: String,
        start: OAuthStart,
    },
    OAuthPolled {
        provider: String,
        poll: OAuthPoll,
    },
    OAuthSaved(String),
    OAuthFailed(String),
    Sessions(Vec<SessionSummary>, u64),
    SessionsFailed(String),
    SessionMessages {
        id: String,
        messages: Vec<serde_json::Value>,
    },
    SessionResumed {
        stored_id: String,
        session_id: String,
        messages: Vec<serde_json::Value>,
    },
    SessionDeleted(String),
    SessionUpdated(String),
    Skills(Vec<Skill>),
    SkillsFailed(String),
    SkillToggled {
        name: String,
        enabled: bool,
    },
    HubSearched(Vec<SkillHubResult>),
    HubPreviewed {
        preview: SkillHubPreview,
        scan: Option<SkillHubScan>,
    },
    HubInstalled {
        identifier: String,
    },
    HubFailed(String),
    Mcp(Vec<McpServer>),
    McpFailed(String),
    McpOAuth(hermes_api::McpOAuthFlow),
    McpOAuthFailed(String),
    McpOAuthCancelled(String),
    KanbanBoards(hermes_api::KanbanBoards),
    Kanban {
        board: Option<String>,
        data: hermes_api::KanbanBoard,
    },
    KanbanCreated,
    KanbanBoardSaved {
        slug: String,
        created: bool,
    },
    KanbanBoardSaveFailed(String),
    KanbanFailed(String),
    McpToggled {
        name: String,
        enabled: bool,
    },
    McpTested {
        name: String,
        result: serde_json::Value,
    },
    McpCatalog(McpCatalog),
    McpCatalogFailed(String),
    MemoryConfig(hermes_api::MemoryProviderConfig),
    MemoryConfigFailed(String),
    MemoryConfigSaved(String),
    MemoryReset(Vec<String>),
    McpCreated(String),
    McpDeleted(String),
    McpEdited(String),
    McpConfig {
        name: String,
        config: serde_json::Value,
    },
    McpInstalled {
        name: String,
        background: bool,
    },
    Memory(MemoryStatus),
    MemoryFailed(String),
    MemoryProviderSet(String),
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
    LoadMedia(String),
    Speak {
        text: String,
    },
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
    LoadUsage {
        days: u32,
    },
    LoadModels,
    SetModel {
        provider: String,
        model: String,
    },
    LoadAuxiliary,
    SetAuxModel {
        task: String,
        provider: String,
        model: String,
    },
    ResetAuxModels,
    LoadMoa,
    SaveMoa(MoaConfig),
    LoadCron,
    CreateCron {
        body: Value,
    },
    CronAction {
        id: String,
        action: String,
    },
    DeleteCron {
        id: String,
    },
    LoadCronRuns {
        id: String,
    },
    LoadOAuth,
    StartOAuth {
        provider: String,
    },
    SubmitOAuth {
        provider: String,
        session_id: String,
        code: String,
    },
    PollOAuth {
        provider: String,
        session_id: String,
    },
    DisconnectOAuth {
        provider: String,
    },
    SetReasoning {
        level: String,
    },
    LoadSessions,
    ExportSession {
        id: String,
        path: PathBuf,
    },
    ResumeSession {
        id: String,
    },
    DeleteSession {
        id: String,
    },
    UpdateSession {
        id: String,
        patch: Value,
    },
    LoadSkills,
    SetSkill {
        name: String,
        enabled: bool,
    },
    SearchSkillHub {
        query: String,
    },
    PreviewSkillHub {
        identifier: String,
    },
    InstallSkillHub {
        identifier: String,
    },
    LoadMcp,
    LoadKanbanBoards,
    CreateKanbanBoard {
        slug: String,
    },
    RenameKanbanBoard {
        slug: String,
        name: String,
    },
    LoadKanban {
        board: Option<String>,
    },
    CreateKanbanTask {
        title: String,
        board: Option<String>,
    },
    UpdateKanbanTask {
        id: String,
        patch: Value,
        board: Option<String>,
        editing: bool,
    },
    DeleteKanbanTask {
        id: String,
        board: Option<String>,
    },
    StartMcpOAuth {
        name: String,
        profile: Option<String>,
    },
    PollMcpOAuth {
        id: String,
    },
    CancelMcpOAuth {
        id: String,
    },
    SetMcp {
        name: String,
        enabled: bool,
    },
    TestMcp {
        name: String,
    },
    CreateMcp {
        body: Value,
    },
    LoadMcpConfig {
        name: String,
    },
    SaveMcp {
        old_name: String,
        name: String,
        config: Value,
    },
    DeleteMcp {
        name: String,
    },
    LoadMcpCatalog,
    InstallMcp {
        name: String,
        env: Value,
        enable: bool,
    },
    LoadMemory,
    SetMemoryProvider {
        provider: String,
    },
    LoadMemoryConfig {
        provider: String,
    },
    UpdateMemoryConfig {
        provider: String,
        values: Value,
    },
    ResetMemory {
        target: String,
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

/// Do not queue a click made while disconnected: replaying a mutation after
/// reconnect could unexpectedly modify credentials, jobs or board state.
fn offline_event(command: Command) -> BackendEvent {
    let message = "Gateway offline. Reconnect and try again.".to_string();
    match command {
        Command::LoadSessions => BackendEvent::SessionsFailed(message),
        Command::LoadMcp => BackendEvent::McpFailed(message),
        Command::LoadMcpCatalog => BackendEvent::McpCatalogFailed(message),
        Command::CreateKanbanBoard { .. } | Command::RenameKanbanBoard { .. } => {
            BackendEvent::KanbanBoardSaveFailed(message)
        }
        Command::LoadKanbanBoards
        | Command::LoadKanban { .. }
        | Command::CreateKanbanTask { .. }
        | Command::UpdateKanbanTask { .. }
        | Command::DeleteKanbanTask { .. } => BackendEvent::KanbanFailed(message),
        Command::StartMcpOAuth { .. }
        | Command::PollMcpOAuth { .. }
        | Command::CancelMcpOAuth { .. } => BackendEvent::McpOAuthFailed(message),
        Command::LoadOAuth
        | Command::StartOAuth { .. }
        | Command::PollOAuth { .. }
        | Command::SubmitOAuth { .. }
        | Command::DisconnectOAuth { .. } => BackendEvent::OAuthFailed(message),
        Command::LoadCron
        | Command::CreateCron { .. }
        | Command::CronAction { .. }
        | Command::DeleteCron { .. }
        | Command::LoadCronRuns { .. } => BackendEvent::CronFailed(message),
        Command::LoadMoa | Command::SaveMoa(_) => BackendEvent::MoaFailed(message),
        Command::LoadSkills => BackendEvent::SkillsFailed(message),
        Command::LoadModels => BackendEvent::ModelsFailed(message),
        Command::LoadAuxiliary => BackendEvent::AuxiliaryFailed(message),
        Command::LoadUsage { .. } => BackendEvent::UsageFailed(message),
        Command::LoadMemory => BackendEvent::MemoryFailed(message),
        Command::LoadMemoryConfig { .. } => BackendEvent::MemoryConfigFailed(message),
        _ => BackendEvent::Error(message),
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
                let retry = tokio::time::sleep(backoff);
                tokio::pin!(retry);
                loop {
                    tokio::select! {
                        _ = &mut retry => break,
                        command = commands.recv() => {
                            let Some(command) = command else { return };
                            let _ = events.send(offline_event(command));
                        }
                    }
                }
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
                            if let Some(session_id) = &session
                                && let Err(err) = gateway::interrupt(&client, session_id).await {
                                    let _ = events.send(BackendEvent::Error(err.to_string()));
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
                        Command::Speak { text } => {
                            let Some(config) = crate::voice::TtsConfig::from_env() else {
                                let _ = events.send(BackendEvent::SpeakFailed(
                                    "set HERMES_TTS_URL to the voice-models service".into(),
                                ));
                                continue;
                            };
                            match crate::voice::synthesize(&config, &text).await {
                                Ok(bytes) => {
                                    let path = crate::voice::temp_wav_path();
                                    if let Err(err) = tokio::fs::write(&path, &bytes).await {
                                        let _ = events.send(BackendEvent::SpeakFailed(format!(
                                            "write tts wav: {err}"
                                        )));
                                        continue;
                                    }
                                    let Some(player) = crate::voice::player() else {
                                        let _ = tokio::fs::remove_file(&path).await;
                                        let _ = events.send(BackendEvent::SpeakFailed(
                                            "no audio player (install pw-play, paplay or aplay)"
                                                .into(),
                                        ));
                                        continue;
                                    };
                                    let played = tokio::process::Command::new(player)
                                        .arg(&path)
                                        .status()
                                        .await;
                                    let _ = tokio::fs::remove_file(&path).await;
                                    match played {
                                        Ok(status) if status.success() => {
                                            let _ = events.send(BackendEvent::Spoken);
                                        }
                                        Ok(status) => {
                                            let _ = events.send(BackendEvent::SpeakFailed(format!(
                                                "{player} exited with {status}"
                                            )));
                                        }
                                        Err(err) => {
                                            let _ = events.send(BackendEvent::SpeakFailed(format!(
                                                "{player}: {err}"
                                            )));
                                        }
                                    }
                                }
                                Err(message) => {
                                    let _ = events.send(BackendEvent::SpeakFailed(message));
                                }
                            }
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
                        Command::LoadMemory => match rest.memory_status().await {
                            Ok(status) => {
                                let _ = events.send(BackendEvent::Memory(status));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::MemoryFailed(message));
                            }
                        },
                        Command::SetMemoryProvider { provider } => {
                            match rest.set_memory_provider(&provider).await {
                                Ok(_) => {
                                    let _ = events
                                        .send(BackendEvent::MemoryProviderSet(provider.clone()));
                                    if let Ok(status) = rest.memory_status().await {
                                        let _ = events.send(BackendEvent::Memory(status));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::MemoryFailed(format!(
                                        "{provider}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::LoadMemoryConfig { provider } => {
                            match rest.memory_config(&provider).await {
                                Ok(config) => {
                                    let _ = events.send(BackendEvent::MemoryConfig(config));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::MemoryConfigFailed(format!(
                                        "{provider}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::UpdateMemoryConfig { provider, values } => {
                            match rest.set_memory_config(&provider, values).await {
                                Ok(_) => {
                                    let _ = events
                                        .send(BackendEvent::MemoryConfigSaved(provider.clone()));
                                    if let Ok(status) = rest.memory_status().await {
                                        let _ = events.send(BackendEvent::Memory(status));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::MemoryConfigFailed(format!(
                                        "{provider}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::ResetMemory { target } => {
                            match rest.reset_memory(&target).await {
                                Ok(result) => {
                                    let deleted = result
                                        .get("deleted")
                                        .and_then(Value::as_array)
                                        .map(|rows| {
                                            rows.iter()
                                                .filter_map(Value::as_str)
                                                .map(str::to_string)
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();
                                    let _ = events.send(BackendEvent::MemoryReset(deleted));
                                    if let Ok(status) = rest.memory_status().await {
                                        let _ = events.send(BackendEvent::Memory(status));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::MemoryFailed(format!(
                                        "reset {target}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::CreateKanbanBoard { slug } => {
                            match rest.create_kanban_board(&slug).await {
                                Ok(result) => {
                                    let _ = events.send(BackendEvent::KanbanBoardSaved { slug: result.board.slug, created: true });
                                    match rest.kanban_boards().await {
                                        Ok(boards) => { let _ = events.send(BackendEvent::KanbanBoards(boards)); }
                                        Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                    }
                                }
                                Err(err) => { let _ = events.send(BackendEvent::KanbanBoardSaveFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::RenameKanbanBoard { slug, name } => {
                            match rest.rename_kanban_board(&slug, &name).await {
                                Ok(result) => {
                                    let _ = events.send(BackendEvent::KanbanBoardSaved { slug: result.board.slug, created: false });
                                    match rest.kanban_boards().await {
                                        Ok(boards) => { let _ = events.send(BackendEvent::KanbanBoards(boards)); }
                                        Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                    }
                                }
                                Err(err) => { let _ = events.send(BackendEvent::KanbanBoardSaveFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::LoadKanbanBoards => match rest.kanban_boards().await {
                            Ok(boards) => { let _ = events.send(BackendEvent::KanbanBoards(boards)); }
                            Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                        },
                        Command::LoadKanban { board } => match rest.kanban_board(board.as_deref()).await {
                            Ok(data) => { let _ = events.send(BackendEvent::Kanban { board, data }); }
                            Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                        },
                        Command::CreateKanbanTask { title, board } => {
                            match rest.create_kanban_task(&title, board.as_deref()).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::KanbanCreated);
                                    match rest.kanban_board(board.as_deref()).await {
                                        Ok(data) => { let _ = events.send(BackendEvent::Kanban { board, data }); }
                                        Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                    }
                                },
                                Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::UpdateKanbanTask { id, patch, board, editing } => {
                            match rest.update_kanban_task(&id, patch, board.as_deref()).await {
                                Ok(_) => {
                                    if editing { let _ = events.send(BackendEvent::KanbanCreated); }
                                    match rest.kanban_board(board.as_deref()).await {
                                        Ok(data) => { let _ = events.send(BackendEvent::Kanban { board, data }); }
                                        Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                    }
                                },
                                Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::DeleteKanbanTask { id, board } => {
                            match rest.delete_kanban_task(&id, board.as_deref()).await {
                                Ok(_) => match rest.kanban_board(board.as_deref()).await {
                                    Ok(data) => { let _ = events.send(BackendEvent::Kanban { board, data }); }
                                    Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                },
                                Err(err) => { let _ = events.send(BackendEvent::KanbanFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::StartMcpOAuth { name, profile } => {
                            let rest = rest.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                match rest.start_mcp_oauth(&name, profile.as_deref()).await {
                                    Ok(flow) => { let _ = events.send(BackendEvent::McpOAuth(flow)); }
                                    Err(err) => { let _ = events.send(BackendEvent::McpOAuthFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                                }
                            });
                        }
                        Command::PollMcpOAuth { id } => {
                            match rest.mcp_oauth_status(&id).await {
                                Ok(flow) => { let _ = events.send(BackendEvent::McpOAuth(flow)); }
                                Err(err) => { let _ = events.send(BackendEvent::McpOAuthFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::CancelMcpOAuth { id } => {
                            match rest.cancel_mcp_oauth(&id).await {
                                Ok(_) => { let _ = events.send(BackendEvent::McpOAuthCancelled(id)); }
                                Err(err) => { let _ = events.send(BackendEvent::McpOAuthFailed(err.detail().unwrap_or_else(|| err.to_string()))); }
                            }
                        }
                        Command::LoadMcp => match rest.mcp_servers().await {
                            Ok(servers) => {
                                let _ = events.send(BackendEvent::Mcp(servers));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::McpFailed(message));
                            }
                        },
                        Command::CreateMcp { body } => {
                            let name = body
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            match rest.add_mcp_server(body).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::McpCreated(name));
                                    if let Ok(servers) = rest.mcp_servers().await {
                                        let _ = events.send(BackendEvent::Mcp(servers));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::McpFailed(format!(
                                        "create {name}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::LoadMcpConfig { name } => {
                            match rest.raw_config().await {
                                Ok(config) => match RestClient::mcp_config_entry(&config, &name) {
                                    Some(entry) => {
                                        let _ = events
                                            .send(BackendEvent::McpConfig { name, config: entry });
                                    }
                                    None => {
                                        let _ = events.send(BackendEvent::McpFailed(format!(
                                            "{name}: server not found in config"
                                        )));
                                    }
                                },
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::McpFailed(format!(
                                        "read {name}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::SaveMcp {
                            old_name,
                            name,
                            config,
                        } => {
                            match rest.raw_config().await {
                                Ok(raw) => {
                                    let root = raw.get("config").unwrap_or(&raw);
                                    let mut servers = root
                                        .get("mcp_servers")
                                        .and_then(Value::as_object)
                                        .cloned()
                                        .unwrap_or_default();
                                    if old_name != name && servers.contains_key(&name) {
                                        let _ = events.send(BackendEvent::McpFailed(format!(
                                            "{name}: a server with that name already exists"
                                        )));
                                        continue;
                                    }
                                    servers.remove(&old_name);
                                    servers.insert(name.clone(), config);
                                    match rest.replace_mcp_servers(servers).await {
                                        Ok(_) => {
                                            let _ = events.send(BackendEvent::McpEdited(name));
                                            if let Ok(rows) = rest.mcp_servers().await {
                                                let _ = events.send(BackendEvent::Mcp(rows));
                                            }
                                        }
                                        Err(err) => {
                                            let message =
                                                err.detail().unwrap_or_else(|| err.to_string());
                                            let _ = events.send(BackendEvent::McpFailed(format!(
                                                "save {name}: {message}"
                                            )));
                                        }
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::McpFailed(format!(
                                        "read config: {message}"
                                    )));
                                }
                            }
                        }
                        Command::DeleteMcp { name } => match rest.delete_mcp_server(&name).await {
                            Ok(_) => {
                                let _ = events.send(BackendEvent::McpDeleted(name));
                                if let Ok(servers) = rest.mcp_servers().await {
                                    let _ = events.send(BackendEvent::Mcp(servers));
                                }
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::McpFailed(format!(
                                    "delete {name}: {message}"
                                )));
                            }
                        },
                        Command::LoadMcpCatalog => match rest.mcp_catalog().await {
                            Ok(catalog) => {
                                let _ = events.send(BackendEvent::McpCatalog(catalog));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::McpCatalogFailed(message));
                            }
                        },
                        Command::InstallMcp { name, env, enable } => {
                            match rest.install_mcp_catalog(&name, env, enable).await {
                                Ok(result) => {
                                    let background = result
                                        .get("background")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false);
                                    let _ = events
                                        .send(BackendEvent::McpInstalled { name, background });
                                    if let Ok(servers) = rest.mcp_servers().await {
                                        let _ = events.send(BackendEvent::Mcp(servers));
                                    }
                                    if let Ok(catalog) = rest.mcp_catalog().await {
                                        let _ = events.send(BackendEvent::McpCatalog(catalog));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::McpFailed(format!(
                                        "install {name}: {message}"
                                    )));
                                }
                            }
                        },
                        Command::SetMcp { name, enabled } => {
                            match rest.set_mcp_enabled(&name, enabled).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::McpToggled {
                                        name: name.clone(),
                                        enabled,
                                    });
                                    if let Ok(servers) = rest.mcp_servers().await {
                                        let _ = events.send(BackendEvent::Mcp(servers));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::McpFailed(format!(
                                        "{name}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::TestMcp { name } => match rest.test_mcp(&name).await {
                            Ok(result) => {
                                let _ = events.send(BackendEvent::McpTested { name, result });
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::McpFailed(format!(
                                    "{name}: {message}"
                                )));
                            }
                        },
                        Command::LoadSkills => match rest.skills().await {
                            Ok(skills) => {
                                let _ = events.send(BackendEvent::Skills(skills));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::SkillsFailed(message));
                            }
                        },
                        Command::SetSkill { name, enabled } => {
                            match rest.set_skill_enabled(&name, enabled).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::SkillToggled {
                                        name: name.clone(),
                                        enabled,
                                    });
                                    if let Ok(skills) = rest.skills().await {
                                        let _ = events.send(BackendEvent::Skills(skills));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::SkillsFailed(format!(
                                        "{name}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::SearchSkillHub { query } => {
                            match rest.skill_hub_search(&query, 20).await {
                                Ok(answer) => {
                                    let _ = events.send(BackendEvent::HubSearched(answer.results));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::HubFailed(format!(
                                        "hub search: {message}"
                                    )));
                                }
                            }
                        }
                        Command::PreviewSkillHub { identifier } => {
                            match rest.skill_hub_preview(&identifier).await {
                                Ok(preview) => {
                                    let scan = rest.skill_hub_scan(&identifier).await.ok();
                                    let _ = events
                                        .send(BackendEvent::HubPreviewed { preview, scan });
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::HubFailed(format!(
                                        "preview {identifier}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::InstallSkillHub { identifier } => {
                            match rest.install_skill_hub(&identifier).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::HubInstalled {
                                        identifier: identifier.clone(),
                                    });
                                    let rest = rest.clone();
                                    let events = events.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_secs(2)).await;
                                        if let Ok(skills) = rest.skills().await {
                                            let _ = events.send(BackendEvent::Skills(skills));
                                        }
                                    });
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::HubFailed(format!(
                                        "install {identifier}: {message}"
                                    )));
                                }
                            }
                        }
                        Command::LoadSessions => match rest.sessions(50).await {
                            Ok(page) => {
                                let _ = events
                                    .send(BackendEvent::Sessions(page.sessions, page.total));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::SessionsFailed(message));
                            }
                        },
                        Command::LoadMedia(path) => {
                            let result = match rest.media_data_url(&path).await {
                                Ok(url) => {
                                    match url.split_once(",") {
                                        Some((header, data)) if header.starts_with("data:image/") && header.ends_with(";base64") => {
                                            match base64_decode(data) {
                                                Some(bytes) => Ok((header[5..header.len() - 7].to_string(), bytes)),
                                                None => Err("invalid image data".to_string()),
                                            }
                                        }
                                        _ => Err("invalid image data URL".to_string()),
                                    }
                                }
                                Err(err) => Err(err.detail().unwrap_or_else(|| err.to_string())),
                            };
                            match result {
                                Ok((mime, bytes)) => { let _ = events.send(BackendEvent::Media { path, mime, bytes }); }
                                Err(message) => { let _ = events.send(BackendEvent::MediaFailed { path, message }); }
                            }
                        }
                        Command::ExportSession { id, path } => {
                            let result = match rest.export_session(&id, session_profile.as_deref()).await {
                                Ok(value) => match serde_json::to_vec_pretty(&value) {
                                    Ok(bytes) => tokio::fs::write(&path, bytes).await.map_err(|err| err.to_string()),
                                    Err(err) => Err(err.to_string()),
                                },
                                Err(err) => Err(err.detail().unwrap_or_else(|| err.to_string())),
                            };
                            let message = match result {
                                Ok(()) => format!("Exported session to {}", path.display()),
                                Err(err) => format!("Session export failed: {err}"),
                            };
                            let _ = events.send(BackendEvent::Notice(message));
                        }
                        Command::ResumeSession { id } => {
                            match gateway::resume_session(
                                &client,
                                &id,
                                session_profile.as_deref(),
                            )
                            .await
                            {
                                Ok(resumed) => {
                                    session = Some(resumed.session_id.clone());
                                    let _ = events.send(BackendEvent::SessionResumed {
                                        stored_id: resumed.resumed.clone(),
                                        session_id: resumed.session_id,
                                        messages: resumed.messages,
                                    });
                                }
                                Err(err) => {
                                    // Older hosts may expose the transcript without
                                    // `session.resume`; keep a read-only fallback so
                                    // history is still inspectable, but say why.
                                    match rest.session_messages(&id).await {
                                        Ok(page) => {
                                            let _ = events.send(BackendEvent::SessionMessages {
                                                id: page.session_id,
                                                messages: page.messages,
                                            });
                                            let _ = events.send(BackendEvent::Notice(format!(
                                                "resume {id}: {err} (read-only transcript)"
                                            )));
                                        }
                                        Err(_) => {
                                            let _ = events.send(BackendEvent::Notice(format!(
                                                "resume {id}: {err}"
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                        Command::UpdateSession { id, patch } => {
                            match rest.update_session(&id, patch).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::SessionUpdated(id.clone()));
                                    if let Ok(page) = rest.sessions(50).await {
                                        let _ = events
                                            .send(BackendEvent::Sessions(page.sessions, page.total));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::Notice(format!(
                                        "session update failed: {message}"
                                    )));
                                }
                            }
                        }
                        Command::DeleteSession { id } => {
                            match rest.delete_session(&id).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::SessionDeleted(id));
                                    if let Ok(page) = rest.sessions(50).await {
                                        let _ = events
                                            .send(BackendEvent::Sessions(page.sessions, page.total));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::Notice(format!(
                                        "delete failed: {message}"
                                    )));
                                }
                            }
                        }
                        Command::LoadAuxiliary => {
                            match rest.auxiliary_models().await {
                                Ok(aux) => {
                                    let _ = events.send(BackendEvent::Auxiliary(aux.tasks));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ =
                                        events.send(BackendEvent::AuxiliaryFailed(message));
                                }
                            }
                            match rest.reasoning_effort().await {
                                Ok(level) => {
                                    let _ = events.send(BackendEvent::ReasoningEffort(level));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ =
                                        events.send(BackendEvent::ReasoningFailed(message));
                                }
                            }
                        }
                        Command::SetAuxModel {
                            task,
                            provider,
                            model,
                        } => match rest.set_auxiliary_model(&task, &provider, &model).await {
                            Ok(_) => {
                                let _ = events.send(BackendEvent::AuxiliarySaved {
                                    task,
                                    provider,
                                    model,
                                });
                                if let Ok(aux) = rest.auxiliary_models().await {
                                    let _ = events.send(BackendEvent::Auxiliary(aux.tasks));
                                }
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::ModelStatus(format!(
                                    "assignment failed: {message}"
                                )));
                            }
                        },
                        Command::LoadMoa => match rest.moa_config().await {
                            Ok(config) => {
                                let _ = events.send(BackendEvent::Moa(config));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::MoaFailed(message));
                            }
                        },
                        Command::SaveMoa(config) => match rest.set_moa_config(&config).await {
                            Ok(saved) => {
                                let _ = events.send(BackendEvent::MoaSaved(saved));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::MoaFailed(message));
                            }
                        },
                        Command::LoadCron => {
                            match (
                                rest.cron_jobs().await,
                                rest.cron_delivery_targets().await,
                            ) {
                                (Ok(jobs), Ok(targets)) => {
                                    let _ = events.send(BackendEvent::Cron(jobs, targets));
                                }
                                (Err(err), _) | (_, Err(err)) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::CronFailed(message));
                                }
                            }
                        }
                        Command::CreateCron { body } => {
                            match rest.create_cron_job(body).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::CronSaved(
                                        "job created".to_string(),
                                    ));
                                    refresh_cron(&rest, &events).await;
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::CronFailed(message));
                                }
                            }
                        }
                        Command::CronAction { id, action } => {
                            match rest.cron_action(&id, &action).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::CronSaved(format!(
                                        "{id}: {action}"
                                    )));
                                    refresh_cron(&rest, &events).await;
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::CronFailed(message));
                                }
                            }
                        }
                        Command::DeleteCron { id } => match rest.delete_cron_job(&id).await {
                            Ok(_) => {
                                let _ = events
                                    .send(BackendEvent::CronSaved(format!("{id} deleted")));
                                refresh_cron(&rest, &events).await;
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::CronFailed(message));
                            }
                        },
                        Command::LoadCronRuns { id } => match rest.cron_runs(&id).await {
                            Ok(runs) => {
                                let _ = events.send(BackendEvent::CronRuns { id, runs });
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::CronFailed(message));
                            }
                        },
                        Command::LoadOAuth => match rest.oauth_providers().await {
                            Ok(providers) => {
                                let _ = events.send(BackendEvent::OAuth(providers));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::OAuthFailed(message));
                            }
                        },
                        Command::StartOAuth { provider } => {
                            match rest.start_oauth(&provider).await {
                                Ok(start) => {
                                    let _ = events
                                        .send(BackendEvent::OAuthStarted { provider, start });
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::OAuthFailed(message));
                                }
                            }
                        }
                        Command::SubmitOAuth {
                            provider,
                            session_id,
                            code,
                        } => match rest.submit_oauth_code(&provider, &session_id, &code).await {
                            Ok(_) => {
                                let _ = events
                                    .send(BackendEvent::OAuthSaved("credentials saved".into()));
                                refresh_oauth(&rest, &events).await;
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::OAuthFailed(message));
                            }
                        },
                        Command::PollOAuth {
                            provider,
                            session_id,
                        } => match rest.poll_oauth(&provider, &session_id).await {
                            Ok(poll) => {
                                let done = !matches!(poll.status.as_str(), "pending" | "");
                                let _ = events.send(BackendEvent::OAuthPolled { provider, poll });
                                if done {
                                    refresh_oauth(&rest, &events).await;
                                }
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                let _ = events.send(BackendEvent::OAuthFailed(message));
                            }
                        },
                        Command::DisconnectOAuth { provider } => {
                            match rest.disconnect_oauth(&provider).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::OAuthSaved(format!(
                                        "{provider} disconnected"
                                    )));
                                    refresh_oauth(&rest, &events).await;
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::OAuthFailed(message));
                                }
                            }
                        }
                        Command::ResetAuxModels => {
                            match rest.reset_auxiliary_models().await {
                                Ok(_) => {
                                    if let Ok(aux) = rest.auxiliary_models().await {
                                        let _ = events.send(BackendEvent::Auxiliary(aux.tasks));
                                    }
                                    let _ = events.send(BackendEvent::ModelStatus(
                                        "auxiliary tasks follow the main model".to_string(),
                                    ));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::ModelStatus(format!(
                                        "reset failed: {message}"
                                    )));
                                }
                            }
                        }
                        Command::SetReasoning { level } => {
                            match rest.set_reasoning_effort(&level).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::ReasoningEffort(level));
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ =
                                        events.send(BackendEvent::ReasoningFailed(message));
                                }
                            }
                        }
                        Command::LoadModels => match rest.model_options().await {
                            Ok(options) => {
                                tracing::debug!(
                                    providers = options.providers.len(),
                                    "models: options loaded"
                                );
                                let _ = events.send(BackendEvent::Models(options));
                            }
                            Err(err) => {
                                let message = err.detail().unwrap_or_else(|| err.to_string());
                                tracing::warn!(%message, "models: options failed");
                                let _ = events.send(BackendEvent::ModelsFailed(message));
                            }
                        },
                        Command::SetModel { provider, model } => {
                            match rest.set_model(&provider, &model).await {
                                Ok(_) => {
                                    let _ = events.send(BackendEvent::ModelStatus(format!(
                                        "switched to {model} — new sessions only"
                                    )));
                                    if let Ok(options) = rest.model_options().await {
                                        let _ = events.send(BackendEvent::Models(options));
                                    }
                                    if let Ok(info) = rest.model_info().await {
                                        let _ = events.send(BackendEvent::ModelInfo(info));
                                    }
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    let _ = events.send(BackendEvent::ModelStatus(format!(
                                        "switch failed: {message}"
                                    )));
                                }
                            }
                        }
                        Command::LoadUsage { days } => {
                            let model = rest.model_info().await.ok();
                            let portal = rest.portal().await.ok();
                            match rest.usage_analytics(days).await {
                                Ok(analytics) => {
                                    tracing::debug!(
                                        days,
                                        models = analytics.by_model.len(),
                                        "usage: analytics loaded"
                                    );
                                    let _ = events.send(BackendEvent::Usage {
                                        analytics,
                                        model,
                                        portal,
                                    });
                                }
                                Err(err) => {
                                    let message =
                                        err.detail().unwrap_or_else(|| err.to_string());
                                    tracing::warn!(%message, "usage: analytics failed");
                                    let _ = events.send(BackendEvent::UsageFailed(message));
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

async fn refresh_oauth(rest: &RestClient, events: &mpsc::UnboundedSender<BackendEvent>) {
    match rest.oauth_providers().await {
        Ok(providers) => {
            let _ = events.send(BackendEvent::OAuth(providers));
        }
        Err(err) => {
            let message = err.detail().unwrap_or_else(|| err.to_string());
            let _ = events.send(BackendEvent::OAuthFailed(message));
        }
    }
}

async fn refresh_cron(rest: &RestClient, events: &mpsc::UnboundedSender<BackendEvent>) {
    match (rest.cron_jobs().await, rest.cron_delivery_targets().await) {
        (Ok(jobs), Ok(targets)) => {
            let _ = events.send(BackendEvent::Cron(jobs, targets));
        }
        (Err(err), _) | (_, Err(err)) => {
            let message = err.detail().unwrap_or_else(|| err.to_string());
            let _ = events.send(BackendEvent::CronFailed(message));
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
        let names = jobs
            .into_iter()
            .map(|job| job.name)
            .filter(|name| !name.is_empty())
            .collect();
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
    fn offline_actions_fail_without_being_replayed() {
        assert!(matches!(
            offline_event(Command::CreateKanbanBoard {
                slug: "work".into()
            }),
            BackendEvent::KanbanBoardSaveFailed(_)
        ));
        assert!(matches!(
            offline_event(Command::RenameKanbanBoard {
                slug: "work".into(),
                name: "Work".into()
            }),
            BackendEvent::KanbanBoardSaveFailed(_)
        ));
        assert!(matches!(
            offline_event(Command::LoadKanban { board: None }),
            BackendEvent::KanbanFailed(_)
        ));
        assert!(matches!(
            offline_event(Command::LoadSessions),
            BackendEvent::SessionsFailed(_)
        ));
        assert!(matches!(
            offline_event(Command::UpdateKanbanTask {
                id: "task".into(),
                patch: serde_json::json!({"status": "done"}),
                board: None,
                editing: false,
            }),
            BackendEvent::KanbanFailed(_)
        ));
        assert!(matches!(
            offline_event(Command::StartMcpOAuth {
                name: "server".into(),
                profile: None
            }),
            BackendEvent::McpOAuthFailed(_)
        ));
    }

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
