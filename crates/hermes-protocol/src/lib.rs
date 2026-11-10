//! Versioned wire protocol between `hermes-hub`, `hermes-node` and devices.
//!
//! Every frame is JSON with a `protocol` version and a `type` tag:
//!
//! ```json
//! {"protocol":1,"type":"heartbeat","at_ms":1762750000000,"running":[]}
//! ```
//!
//! The hub accepts a node or device connection only at the version it speaks;
//! an unknown version is answered with an `error` frame and the socket closes,
//! so older agents fail closed instead of silently misbehaving.
//!
//! Two rules shape the types here:
//!
//! * a secret value never appears in `argv`, in the transcript, in a command
//!   record or in `Debug` output — it exists only inside a `PromptDecision`
//!   that is forwarded once and zeroized by its holder,
//! * a command is identified by an idempotency key, so a retried submission
//!   returns the first record instead of running twice.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire version implemented by this crate.
pub const PROTOCOL_VERSION: u16 = 1;
/// Largest accepted frame; anything bigger closes the connection.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest single stream chunk a node may send.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// Longest command timeout the hub will forward.
pub const MAX_COMMAND_TIMEOUT_MS: u64 = 15 * 60 * 1000;
/// Shortest and longest secret prompt lifetime.
pub const MIN_PROMPT_TIMEOUT_MS: u64 = 5_000;
pub const MAX_PROMPT_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Default heartbeat period the hub asks for.
pub const DEFAULT_HEARTBEAT_MS: u64 = 15_000;

/// A decoded wire frame: the version plus the message it carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame<T> {
    pub protocol: u16,
    #[serde(flatten)]
    pub message: T,
}

impl<T> Frame<T> {
    pub fn new(message: T) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            message,
        }
    }

    pub fn encode(&self) -> Result<String, ProtocolError>
    where
        T: Serialize,
    {
        Ok(serde_json::to_string(self)?)
    }
}

impl<T> Frame<T>
where
    T: for<'de> Deserialize<'de>,
{
    /// Decode one text frame, rejecting unknown protocol versions.
    pub fn decode(text: &str) -> Result<Self, ProtocolError> {
        if text.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge(text.len()));
        }
        let frame: Frame<T> = serde_json::from_str(text)?;
        if frame.protocol != PROTOCOL_VERSION {
            return Err(ProtocolError::Version {
                got: frame.protocol,
                want: PROTOCOL_VERSION,
            });
        }
        Ok(frame)
    }
}

/// Anything that can go wrong before a message reaches application code.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unsupported protocol version {got}, this build speaks {want}")]
    Version { got: u16, want: u16 },
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_BYTES} byte limit")]
    TooLarge(usize),
    #[error("invalid message: {0}")]
    Invalid(String),
    #[error("malformed JSON frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// Facts about the computer a node runs on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    /// Version of the `hermes-node` binary.
    pub agent_version: String,
}

/// What a node can do locally. The hub never assumes a capability that is not
/// advertised, and a node may advertise `false` and still accept commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub shell: bool,
    pub pty: bool,
    pub files: bool,
    pub screen: bool,
    pub browser: bool,
    pub gpu: bool,
    pub services: bool,
    /// The node can raise one-time secret/password prompt cards.
    pub secret_prompts: bool,
}

impl Capabilities {
    /// What a node with local execution but no optional integrations reports.
    pub fn baseline() -> Self {
        Self {
            shell: true,
            pty: false,
            files: true,
            screen: false,
            browser: false,
            gpu: false,
            services: false,
            secret_prompts: true,
        }
    }
}

/// A node as the hub and devices see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSummary {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub agent_version: String,
    pub capabilities: Capabilities,
    pub labels: BTreeMap<String, String>,
    pub online: bool,
    pub last_seen_ms: u64,
}

/// Stream channels a node can produce for one command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamChannel {
    Stdout,
    Stderr,
    /// Coarse progress note, e.g. `downloading 3/10`.
    Progress,
    /// File payload, for the file browser.
    File,
    /// Screen frame, once the screen executor exists.
    Screen,
    /// Raw PTY bytes.
    Pty,
}

/// Lifecycle of a command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Denied,
    TimedOut,
    Cancelled,
}

impl CommandStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::TimedOut | Self::Cancelled
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "denied" => Self::Denied,
            "timed_out" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

impl StreamChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Progress => "progress",
            Self::File => "file",
            Self::Screen => "screen",
            Self::Pty => "pty",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "stdout" => Self::Stdout,
            "stderr" => Self::Stderr,
            "progress" => Self::Progress,
            "file" => Self::File,
            "screen" => Self::Screen,
            "pty" => Self::Pty,
            _ => return None,
        })
    }
}

/// A request for one secret/password/credential value.
///
/// `env_var` names where the node will inject the value for the child process.
/// The value itself is never part of this request or of the command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretSpec {
    pub env_var: String,
    pub purpose: String,
    pub scope: String,
    pub timeout_ms: u64,
}

/// A command the hub asks a node to run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRequest {
    pub id: String,
    /// Retries with the same key return the first record instead of rerunning.
    pub idempotency_key: String,
    /// Program and arguments, executed without a shell.
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    /// Extra environment for the child. Never inherited wholesale.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    /// When set, the node raises a prompt card before spawning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretSpec>,
}

impl CommandRequest {
    /// Reject requests a node must never execute.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.id.trim().is_empty() {
            return Err(ProtocolError::Invalid("command id is empty".into()));
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(ProtocolError::Invalid("idempotency key is empty".into()));
        }
        if self.argv.is_empty() {
            return Err(ProtocolError::Invalid("argv is empty".into()));
        }
        if self.argv.iter().any(|arg| arg.contains('\0')) {
            return Err(ProtocolError::Invalid("argv contains NUL".into()));
        }
        if self.argv[0].trim().is_empty() {
            return Err(ProtocolError::Invalid("argv[0] is empty".into()));
        }
        if self.timeout_ms == 0 || self.timeout_ms > MAX_COMMAND_TIMEOUT_MS {
            return Err(ProtocolError::Invalid(format!(
                "timeout_ms must be 1..={MAX_COMMAND_TIMEOUT_MS}"
            )));
        }
        for (key, value) in &self.env {
            validate_env_name(key)?;
            if value.contains('\0') {
                return Err(ProtocolError::Invalid(format!(
                    "environment value for {key} contains NUL"
                )));
            }
        }
        if let Some(secret) = &self.secret {
            validate_env_name(&secret.env_var)?;
            if self.env.contains_key(&secret.env_var) {
                return Err(ProtocolError::Invalid(format!(
                    "secret env var {} must not also be set in env",
                    secret.env_var
                )));
            }
            if secret.purpose.trim().is_empty() || secret.scope.trim().is_empty() {
                return Err(ProtocolError::Invalid(
                    "secret purpose and scope are required".into(),
                ));
            }
            if secret.timeout_ms < MIN_PROMPT_TIMEOUT_MS
                || secret.timeout_ms > MAX_PROMPT_TIMEOUT_MS
            {
                return Err(ProtocolError::Invalid(format!(
                    "secret timeout_ms must be {MIN_PROMPT_TIMEOUT_MS}..={MAX_PROMPT_TIMEOUT_MS}"
                )));
            }
        }
        Ok(())
    }
}

fn validate_env_name(name: &str) -> Result<(), ProtocolError> {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => {
            return Err(ProtocolError::Invalid(format!(
                "environment name {name:?} must start with a letter or underscore"
            )));
        }
    }
    if !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return Err(ProtocolError::Invalid(format!(
            "environment name {name:?} has invalid characters"
        )));
    }
    Ok(())
}

/// A command as stored and shown. Deliberately has no env and no secret: the
/// hub persists only what a transcript and an audit may safely show.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRecord {
    pub id: String,
    pub node_id: String,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub status: CommandStatus,
    pub idempotency_key: String,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub message: Option<String>,
}

/// One stored stream chunk, replayable by sequence after a device reconnect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandEvent {
    pub seq: u64,
    pub channel: StreamChannel,
    pub data_b64: String,
    pub at_ms: u64,
}

/// A pending secret prompt, as shown on the card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretPrompt {
    pub id: String,
    pub node_id: String,
    pub command_id: String,
    /// Single-use nonce the answer must echo back.
    pub challenge: String,
    /// Who is asking, e.g. `hermes-node@forge`.
    pub requester: String,
    pub purpose: String,
    pub scope: String,
    pub env_var: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

impl SecretPrompt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.challenge.len() < 32 {
            return Err(ProtocolError::Invalid(
                "prompt challenge must be at least 32 characters".into(),
            ));
        }
        if self.expires_at_ms <= self.created_at_ms {
            return Err(ProtocolError::Invalid(
                "prompt expiry must be after creation".into(),
            ));
        }
        validate_env_name(&self.env_var)?;
        Ok(())
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

/// The answer to a prompt. `Provide` is the only place a secret value lives,
/// and its `Debug` output is redacted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PromptDecision {
    Provide {
        value: String,
        #[serde(default)]
        remember: bool,
    },
    Cancel,
}

impl fmt::Debug for PromptDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provide { remember, .. } => f
                .debug_struct("Provide")
                .field("value", &"<redacted>")
                .field("remember", remember)
                .finish(),
            Self::Cancel => f.write_str("Cancel"),
        }
    }
}

/// A device's answer to a prompt, echoing the challenge it was shown.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptAnswer {
    pub prompt_id: String,
    pub challenge: String,
    pub decision: PromptDecision,
}

impl fmt::Debug for PromptAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptAnswer")
            .field("prompt_id", &self.prompt_id)
            .field("challenge", &self.challenge)
            .field("decision", &self.decision)
            .finish()
    }
}

/// Messages a node sends to the hub.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeToHub {
    Hello {
        node_id: String,
        agent_version: String,
        host: HostInfo,
        capabilities: Capabilities,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    Heartbeat {
        at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        load: Option<f64>,
        #[serde(default)]
        running: Vec<String>,
    },
    CommandStarted {
        command_id: String,
        at_ms: u64,
        pid: Option<u32>,
    },
    Stream {
        command_id: String,
        seq: u64,
        channel: StreamChannel,
        data_b64: String,
    },
    CommandResult {
        command_id: String,
        status: CommandStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    PromptRequest {
        prompt: SecretPrompt,
    },
    /// The node no longer needs an answer, e.g. the command was cancelled.
    PromptClosed {
        prompt_id: String,
    },
}

/// Messages the hub sends to a node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToNode {
    HelloAck {
        node_id: String,
        heartbeat_ms: u64,
        max_command_timeout_ms: u64,
    },
    Command {
        command: CommandRequest,
    },
    Cancel {
        command_id: String,
    },
    PromptAnswer {
        answer: PromptAnswer,
    },
    Ping {
        at_ms: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Messages a device (the GPUI client or another UI) sends to the hub.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceToHub {
    Hello {
        device_id: String,
        name: String,
    },
    SubmitCommand {
        node_id: String,
        command: CommandRequest,
    },
    CancelCommand {
        command_id: String,
    },
    PromptAnswer {
        answer: PromptAnswer,
    },
    Ping {
        at_ms: u64,
    },
}

/// Messages the hub sends to a device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToDevice {
    HelloAck {
        device_id: String,
        heartbeat_ms: u64,
    },
    NodeList {
        nodes: Vec<NodeSummary>,
    },
    NodePresence {
        node: NodeSummary,
    },
    CommandState {
        command: CommandRecord,
    },
    CommandStream {
        node_id: String,
        command_id: String,
        seq: u64,
        channel: StreamChannel,
        data_b64: String,
    },
    PromptRequest {
        prompt: SecretPrompt,
    },
    /// The prompt is over, whether it was answered, cancelled or expired.
    PromptClosed {
        prompt_id: String,
    },
    Pong {
        at_ms: u64,
        server_at_ms: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Payload of an audit entry. Kept as a JSON value so a new event kind does
/// not need a protocol change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    pub id: String,
    pub at_ms: u64,
    pub actor: String,
    pub kind: String,
    pub detail: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> CommandRequest {
        CommandRequest {
            id: "c-1".into(),
            idempotency_key: "idem-1".into(),
            argv: vec!["uptime".into()],
            cwd: Some("/srv".into()),
            env: BTreeMap::new(),
            timeout_ms: 30_000,
            secret: None,
        }
    }

    #[test]
    fn frames_round_trip_with_a_version() {
        let frame = Frame::new(NodeToHub::Heartbeat {
            at_ms: 42,
            load: Some(0.5),
            running: vec!["c-1".into()],
        });
        let text = frame.encode().expect("encode");
        assert!(text.contains("\"protocol\":1"), "{text}");
        let decoded: Frame<NodeToHub> = Frame::decode(&text).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn other_protocol_versions_are_refused() {
        let text = r#"{"protocol":2,"type":"heartbeat","at_ms":1}"#;
        let err = Frame::<NodeToHub>::decode(text).expect_err("must refuse");
        assert!(matches!(err, ProtocolError::Version { got: 2, want: 1 }));
    }

    #[test]
    fn oversized_frames_are_refused_before_parsing() {
        let text = "x".repeat(MAX_FRAME_BYTES + 1);
        let err = Frame::<NodeToHub>::decode(&text).expect_err("must refuse");
        assert!(matches!(err, ProtocolError::TooLarge(_)));
    }

    #[test]
    fn command_validation_rejects_empty_argv_and_bad_timeouts() {
        let mut request = command();
        request.argv.clear();
        assert!(request.validate().is_err());

        let mut request = command();
        request.timeout_ms = 0;
        assert!(request.validate().is_err());

        let mut request = command();
        request.timeout_ms = MAX_COMMAND_TIMEOUT_MS + 1;
        assert!(request.validate().is_err());

        command().validate().expect("baseline is valid");
    }

    #[test]
    fn secret_must_not_also_travel_in_env() {
        let mut request = command();
        request.env.insert("PGPASSWORD".into(), "leak".into());
        request.secret = Some(SecretSpec {
            env_var: "PGPASSWORD".into(),
            purpose: "database backup".into(),
            scope: "backup job".into(),
            timeout_ms: 60_000,
        });
        let err = request.validate().expect_err("must refuse");
        assert!(err.to_string().contains("must not also be set"));
    }

    #[test]
    fn prompt_requires_a_long_challenge_and_a_future_expiry() {
        let mut prompt = SecretPrompt {
            id: "p-1".into(),
            node_id: "n-1".into(),
            command_id: "c-1".into(),
            challenge: "short".into(),
            requester: "hermes-node@forge".into(),
            purpose: "sudo".into(),
            scope: "one command".into(),
            env_var: "SUDO_PASSWORD".into(),
            created_at_ms: 100,
            expires_at_ms: 200,
        };
        assert!(prompt.validate().is_err());

        prompt.challenge = "x".repeat(43);
        prompt.expires_at_ms = prompt.created_at_ms;
        assert!(prompt.validate().is_err());

        prompt.expires_at_ms = prompt.created_at_ms + 1;
        prompt.validate().expect("valid prompt");
        assert!(!prompt.is_expired(100));
        assert!(prompt.is_expired(101));
    }

    #[test]
    fn secret_values_are_redacted_in_debug_output() {
        let decision = PromptDecision::Provide {
            value: "hunter2".into(),
            remember: true,
        };
        let debug = format!("{decision:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");

        let answer = PromptAnswer {
            prompt_id: "p-1".into(),
            challenge: "x".repeat(43),
            decision,
        };
        let debug = format!("{answer:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn command_records_cannot_carry_a_secret() {
        let record = CommandRecord {
            id: "c-1".into(),
            node_id: "n-1".into(),
            argv: vec!["backup".into()],
            cwd: None,
            status: CommandStatus::Queued,
            idempotency_key: "idem-1".into(),
            created_at_ms: 1,
            started_at_ms: None,
            finished_at_ms: None,
            exit_code: None,
            message: None,
        };
        let json = serde_json::to_string(&record).expect("serialize");
        assert!(!json.contains("env"), "{json}");
        assert!(!json.contains("secret"), "{json}");
        assert!(CommandStatus::Succeeded.is_terminal());
        assert!(!CommandStatus::Running.is_terminal());
    }

    #[test]
    fn device_messages_round_trip() {
        let frame = Frame::new(DeviceToHub::SubmitCommand {
            node_id: "n-1".into(),
            command: command(),
        });
        let text = frame.encode().expect("encode");
        let decoded: Frame<DeviceToHub> = Frame::decode(&text).expect("decode");
        assert_eq!(decoded, frame);
    }
}
