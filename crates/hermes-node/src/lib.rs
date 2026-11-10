//! `hermes-node` — the installed agent on a computer.
//!
//! It dials the hub, registers capabilities, executes commands locally under its
//! own policy, streams output and asks for secrets through one-time prompt
//! cards. It never listens for inbound connections and never inherits its own
//! token into a child process.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use hermes_protocol::{
    Capabilities, CommandRequest, CommandStatus, Frame, HostInfo, HubToNode, MAX_PTY_INPUT_BYTES,
    NodeToHub, PromptAnswer, PromptDecision, PtySpec, SecretPrompt, SecretSpec, StreamChannel,
};
use rand::Rng;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tracing::{debug, info, warn};
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_RECONNECT_SECS: u64 = 30;
const STREAM_CHUNK: usize = 16 * 1024;

#[cfg(unix)]
mod pty;

/// What the hub forwards to one live terminal command.
pub enum TerminalControl {
    Input(Vec<u8>),
    Resize(PtySpec),
}

/// Node configuration. Secrets are deliberately not `Debug`.
#[derive(Clone)]
pub struct Config {
    pub hub_url: String,
    pub token: String,
    pub node_id: String,
    pub name: String,
    pub allowed_roots: Vec<PathBuf>,
    /// Empty means every program is allowed inside the roots.
    pub allowed_commands: Vec<String>,
    pub max_concurrency: usize,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("hub_url", &self.hub_url)
            .field("token", &"<redacted>")
            .field("node_id", &self.node_id)
            .field("name", &self.name)
            .field("allowed_roots", &self.allowed_roots)
            .field("allowed_commands", &self.allowed_commands)
            .field("max_concurrency", &self.max_concurrency)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let hub_url = std::env::var("HERMES_HUB_URL")
            .context("HERMES_HUB_URL is required (http(s)://hub or ws(s)://hub)")?;
        let hub_url = normalize_hub_url(&hub_url)?;
        let token = std::env::var("HERMES_NODE_TOKEN").context("HERMES_NODE_TOKEN is required")?;
        if token.trim().len() < 16 {
            bail!("HERMES_NODE_TOKEN is too short");
        }
        let node_id = std::env::var("HERMES_NODE_ID").context("HERMES_NODE_ID is required")?;
        if node_id.trim().is_empty() {
            bail!("HERMES_NODE_ID is empty");
        }
        let name = std::env::var("HERMES_NODE_NAME")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| {
                hostname::get()
                    .map(|h| h.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| node_id.clone())
            });
        let roots = std::env::var("HERMES_NODE_ALLOWED_ROOTS").ok();
        let allowed_roots = parse_roots(roots.as_deref())?;
        let allowed_commands = std::env::var("HERMES_NODE_ALLOW_COMMANDS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let max_concurrency = std::env::var("HERMES_NODE_MAX_CONCURRENCY")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("HERMES_NODE_MAX_CONCURRENCY must be an integer")?
            .unwrap_or(2)
            .clamp(1, 16);
        Ok(Self {
            hub_url,
            token,
            node_id,
            name,
            allowed_roots,
            allowed_commands,
            max_concurrency,
        })
    }

    #[cfg(test)]
    pub fn for_test(root: PathBuf) -> Self {
        Self {
            hub_url: "ws://127.0.0.1:1/v1/node".into(),
            token: "test-token-long-enough".into(),
            node_id: "node-test".into(),
            name: "test".into(),
            allowed_roots: vec![root],
            allowed_commands: vec!["sh".into(), "echo".into(), "sleep".into()],
            max_concurrency: 2,
        }
    }
}

/// Turn a user-facing hub URL into the node WebSocket endpoint.
pub fn normalize_hub_url(input: &str) -> Result<String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("hub URL is empty");
    }
    let mut url = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("wss://") || trimmed.starts_with("ws://") {
        trimmed.to_string()
    } else {
        bail!("hub URL must start with http://, https://, ws:// or wss://");
    };
    if !url.ends_with("/v1/node") {
        url.push_str("/v1/node");
    }
    Ok(url)
}

fn parse_roots(value: Option<&str>) -> Result<Vec<PathBuf>> {
    let raw: Vec<PathBuf> = match value {
        Some(value) if !value.trim().is_empty() => value
            .split(':')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set; set HERMES_NODE_ALLOWED_ROOTS")?;
            vec![home]
        }
    };
    let mut roots = Vec::new();
    for root in raw {
        let canonical = std::fs::canonicalize(&root)
            .with_context(|| format!("allowed root {} does not exist", root.display()))?;
        if !canonical.is_dir() {
            bail!("allowed root {} is not a directory", canonical.display());
        }
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    if roots.is_empty() {
        bail!("at least one allowed root is required");
    }
    Ok(roots)
}

/// The agent itself; cloneable so connection tasks can share state.
#[derive(Clone)]
pub struct Agent {
    config: Config,
    capabilities: Capabilities,
    running: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    /// Live terminals, keyed by command id, for input and resize frames.
    terminals: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<TerminalControl>>>>,
    pending_prompts: Arc<Mutex<HashMap<String, oneshot::Sender<PromptAnswer>>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        let slots = Arc::new(tokio::sync::Semaphore::new(config.max_concurrency));
        Self {
            config,
            capabilities: Capabilities {
                pty: cfg!(unix),
                ..Capabilities::baseline()
            },
            running: Arc::new(Mutex::new(HashMap::new())),
            terminals: Arc::new(Mutex::new(HashMap::new())),
            pending_prompts: Arc::new(Mutex::new(HashMap::new())),
            slots,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Reconnect forever with bounded exponential backoff.
    pub async fn run(&self) -> Result<()> {
        let mut backoff = 1u64;
        loop {
            match self.session().await {
                Ok(()) => {
                    info!("hub connection closed");
                    backoff = 1;
                }
                Err(err) => {
                    warn!(error = %err, "hub connection failed");
                    backoff = (backoff * 2).min(MAX_RECONNECT_SECS);
                }
            }
            self.cancel_running();
            tokio::time::sleep(Duration::from_secs(backoff)).await;
        }
    }

    /// Forward one control frame to a live terminal, if it is still running.
    fn terminal(&self, command_id: String, control: TerminalControl) {
        if let Some(sender) = self
            .terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&command_id)
        {
            let _ = sender.send(control);
        }
    }

    fn cancel_running(&self) {
        let senders: Vec<_> = self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect();
        for sender in senders {
            let _ = sender.send(true);
        }
    }

    async fn session(&self) -> Result<()> {
        let mut request = self
            .config
            .hub_url
            .as_str()
            .into_client_request()
            .context("invalid hub URL")?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.config.token))
                .context("node token contains invalid header characters")?,
        );
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .context("connect to hub")?;
        let (mut sink, mut stream) = socket.split();

        let hello = NodeToHub::Hello {
            node_id: self.config.node_id.clone(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
            host: HostInfo {
                hostname: self.config.name.clone(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                agent_version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: self.capabilities.clone(),
            labels: BTreeMap::new(),
        };
        sink.send(Message::Text(Frame::new(hello).encode()?.into()))
            .await?;

        let ack = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .context("hub did not answer hello within 10 seconds")?
            .context("hub closed before hello_ack")??;
        let ack_text = match ack {
            Message::Text(text) => text.to_string(),
            other => bail!("expected hello_ack, got {other:?}"),
        };
        let ack = Frame::<HubToNode>::decode(&ack_text)?;
        let heartbeat_ms = match ack.message {
            HubToNode::HelloAck { heartbeat_ms, .. } => heartbeat_ms,
            HubToNode::Error { code, message } => bail!("hub refused node: {code}: {message}"),
            other => bail!("expected hello_ack, got {other:?}"),
        };
        info!(hub = %self.config.hub_url, "node connected");

        let (tx, mut rx) = mpsc::unbounded_channel::<NodeToHub>();
        let writer = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                match Frame::new(message).encode() {
                    Ok(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => warn!(error = %err, "failed to encode a node frame"),
                }
            }
            let _ = sink.close().await;
        });

        let mut heartbeat = tokio::time::interval(Duration::from_millis(heartbeat_ms.max(1_000)));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let running = self
                        .running
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .keys()
                        .cloned()
                        .collect();
                    let _ = tx.send(NodeToHub::Heartbeat {
                        at_ms: now_ms(),
                        load: read_load(),
                        running,
                    });
                }
                incoming = stream.next() => {
                    let Some(incoming) = incoming else { break };
                    let text = match incoming {
                        Ok(Message::Text(text)) => text.to_string(),
                        Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                            Ok(text) => text,
                            Err(_) => {
                                warn!("hub sent a non-UTF-8 binary frame");
                                continue;
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Ok(_) => continue,
                        Err(err) => {
                            debug!(error = %err, "hub socket read failed");
                            break;
                        }
                    };
                    let frame = match Frame::<HubToNode>::decode(&text) {
                        Ok(frame) => frame,
                        Err(err) => {
                            warn!(error = %err, "hub frame rejected");
                            continue;
                        }
                    };
                    match frame.message {
                        HubToNode::Command { command } => {
                            let agent = self.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let _permit = match agent.slots.clone().acquire_owned().await {
                                    Ok(permit) => permit,
                                    Err(_) => return,
                                };
                                if let Err(err) = agent.execute(command, tx.clone()).await {
                                    warn!(error = %err, "command execution failed");
                                }
                            });
                        }
                        HubToNode::Cancel { command_id } => {
                            if let Some(sender) = self
                                .running
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .get(&command_id)
                            {
                                let _ = sender.send(true);
                            }
                        }
                        HubToNode::TerminalInput {
                            command_id,
                            data_b64,
                        } => {
                            // Input is never logged: a terminal may hold a
                            // secret the user typed on purpose.
                            match STANDARD.decode(&data_b64) {
                                Ok(data) if data.len() <= MAX_PTY_INPUT_BYTES => {
                                    self.terminal(command_id, TerminalControl::Input(data));
                                }
                                Ok(_) => {
                                    warn!("terminal input exceeded {MAX_PTY_INPUT_BYTES} bytes")
                                }
                                Err(_) => warn!("terminal input was not valid base64"),
                            }
                        }
                        HubToNode::TerminalResize {
                            command_id,
                            cols,
                            rows,
                        } => {
                            let spec = PtySpec { cols, rows };
                            if spec.validate().is_ok() {
                                self.terminal(command_id, TerminalControl::Resize(spec));
                            } else {
                                warn!(cols, rows, "terminal resize rejected");
                            }
                        }
                        HubToNode::PromptAnswer { answer } => {
                            if let Some(sender) = self
                                .pending_prompts
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .remove(&answer.prompt_id)
                            {
                                let _ = sender.send(answer);
                            }
                        }
                        HubToNode::Ping { .. } => {
                            let running = self
                                .running
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .keys()
                                .cloned()
                                .collect();
                            let _ = tx.send(NodeToHub::Heartbeat {
                                at_ms: now_ms(),
                                load: read_load(),
                                running,
                            });
                        }
                        HubToNode::Error { code, message } => {
                            warn!(%code, %message, "hub returned an error");
                            if code == "replaced" {
                                break;
                            }
                        }
                        HubToNode::HelloAck { .. } => {}
                    }
                }
            }
        }

        drop(tx);
        let _ = writer.await;
        self.cancel_running();
        Ok(())
    }

    /// Ask for one secret, returning a zeroizing value only when the device
    /// answered with the matching one-time challenge.
    async fn ask_secret(
        &self,
        command: &CommandRequest,
        spec: &SecretSpec,
        tx: &mpsc::UnboundedSender<NodeToHub>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Option<Zeroizing<String>> {
        let prompt_id = format!("prompt-{}", Uuid::new_v4());
        let challenge = random_challenge();
        let now = now_ms();
        let prompt = SecretPrompt {
            id: prompt_id.clone(),
            node_id: self.config.node_id.clone(),
            command_id: command.id.clone(),
            challenge: challenge.clone(),
            requester: format!("hermes-node@{}", self.config.name),
            purpose: spec.purpose.clone(),
            scope: spec.scope.clone(),
            env_var: spec.env_var.clone(),
            created_at_ms: now,
            expires_at_ms: now + spec.timeout_ms,
        };
        if prompt.validate().is_err() {
            return None;
        }
        let (answer_tx, answer_rx) = oneshot::channel();
        self.pending_prompts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(prompt_id.clone(), answer_tx);
        if tx.send(NodeToHub::PromptRequest { prompt }).is_err() {
            return None;
        }

        let timed = tokio::time::timeout(Duration::from_millis(spec.timeout_ms), async {
            tokio::select! {
                _ = cancel.changed() => None,
                answer = answer_rx => answer.ok(),
            }
        })
        .await;

        let value = match timed {
            Ok(Some(answer))
                if answer.prompt_id == prompt_id
                    && answer.challenge == challenge
                    && !cancel.borrow().to_owned() =>
            {
                match answer.decision {
                    PromptDecision::Provide { value, .. } => Some(Zeroizing::new(value)),
                    PromptDecision::Cancel => None,
                }
            }
            _ => None,
        };
        self.pending_prompts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&prompt_id);
        if value.is_none() {
            let _ = tx.send(NodeToHub::PromptClosed {
                prompt_id: prompt_id.clone(),
            });
        }
        value
    }

    async fn execute(
        &self,
        command: CommandRequest,
        tx: mpsc::UnboundedSender<NodeToHub>,
    ) -> Result<()> {
        let started = now_ms();
        if let Err(err) = command.validate() {
            let _ = tx.send(NodeToHub::CommandResult {
                command_id: command.id.clone(),
                status: CommandStatus::Denied,
                exit_code: None,
                duration_ms: 0,
                message: Some(err.to_string()),
            });
            return Ok(());
        }
        let cwd = match self.resolve_cwd(&command) {
            Ok(cwd) => cwd,
            Err(reason) => {
                let _ = tx.send(NodeToHub::CommandResult {
                    command_id: command.id.clone(),
                    status: CommandStatus::Denied,
                    exit_code: None,
                    duration_ms: 0,
                    message: Some(reason),
                });
                return Ok(());
            }
        };
        if let Err(reason) = self.check_program(&command) {
            let _ = tx.send(NodeToHub::CommandResult {
                command_id: command.id.clone(),
                status: CommandStatus::Denied,
                exit_code: None,
                duration_ms: 0,
                message: Some(reason),
            });
            return Ok(());
        }

        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(command.id.clone(), cancel_tx);

        let secret = match &command.secret {
            Some(spec) => match self.ask_secret(&command, spec, &tx, &mut cancel_rx).await {
                Some(value) => Some(value),
                None => {
                    self.running
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&command.id);
                    let cancelled = *cancel_rx.borrow();
                    let _ = tx.send(NodeToHub::CommandResult {
                        command_id: command.id.clone(),
                        status: if cancelled {
                            CommandStatus::Cancelled
                        } else {
                            CommandStatus::Denied
                        },
                        exit_code: None,
                        duration_ms: now_ms().saturating_sub(started),
                        message: Some(if cancelled {
                            "command cancelled while the secret prompt was open".into()
                        } else {
                            "secret prompt declined or expired".into()
                        }),
                    });
                    return Ok(());
                }
            },
            None => None,
        };

        let secret_binding = command
            .secret
            .as_ref()
            .zip(secret.as_ref())
            .map(|(spec, value)| (spec.env_var.as_str(), value.as_str()));
        if command.pty.is_some() {
            return self
                .run_terminal(&command, &cwd, secret_binding, cancel_rx, started, tx)
                .await;
        }
        let mut child = match self.build_child(&command, &cwd, secret_binding) {
            Ok(child) => child,
            Err(err) => {
                self.running
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&command.id);
                let _ = tx.send(NodeToHub::CommandResult {
                    command_id: command.id.clone(),
                    status: CommandStatus::Failed,
                    exit_code: None,
                    duration_ms: now_ms().saturating_sub(started),
                    message: Some(err.to_string()),
                });
                return Ok(());
            }
        };
        drop(secret);

        let pid = child.id();
        let _ = tx.send(NodeToHub::CommandStarted {
            command_id: command.id.clone(),
            at_ms: now_ms(),
            pid,
        });

        let seq = Arc::new(AtomicU64::new(1));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut stream_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        if let Some(stdout) = stdout {
            stream_tasks.push(tokio::spawn(stream_output(
                stdout,
                tx.clone(),
                command.id.clone(),
                StreamChannel::Stdout,
                seq.clone(),
            )));
        }
        if let Some(stderr) = stderr {
            stream_tasks.push(tokio::spawn(stream_output(
                stderr,
                tx.clone(),
                command.id.clone(),
                StreamChannel::Stderr,
                seq.clone(),
            )));
        }

        let timeout = Duration::from_millis(command.timeout_ms);
        let (status, exit_code) = tokio::select! {
            status = child.wait() => match status {
                Ok(status) => {
                    let code = status.code();
                    let command_status = if status.success() {
                        CommandStatus::Succeeded
                    } else {
                        CommandStatus::Failed
                    };
                    (command_status, code)
                }
                Err(err) => {
                    warn!(error = %err, "wait for child failed");
                    (CommandStatus::Failed, None)
                }
            },
            _ = tokio::time::sleep(timeout) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                (CommandStatus::TimedOut, None)
            },
            _ = cancel_rx.changed() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                (CommandStatus::Cancelled, None)
            },

        };
        for task in stream_tasks {
            let _ = task.await;
        }
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&command.id);
        let _ = tx.send(NodeToHub::CommandResult {
            command_id: command.id,
            status,
            exit_code,
            duration_ms: now_ms().saturating_sub(started),
            message: None,
        });
        Ok(())
    }

    /// Run a terminal command: no wall-clock timeout, input and resize frames
    /// until it exits or the hub cancels it.
    #[cfg(unix)]
    async fn run_terminal(
        &self,
        command: &CommandRequest,
        cwd: &Path,
        secret: Option<(&str, &str)>,
        cancel: watch::Receiver<bool>,
        started: u64,
        tx: mpsc::UnboundedSender<NodeToHub>,
    ) -> Result<()> {
        let spec = command.pty.unwrap_or(PtySpec { cols: 80, rows: 24 });
        let env = child_environment(cwd, &command.env, secret);
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(command.id.clone(), control_tx);

        let outcome = pty::run(
            pty::Job {
                command_id: command.id.clone(),
                argv: &command.argv,
                cwd,
                env: &env,
                spec,
            },
            tx.clone(),
            control_rx,
            cancel,
        )
        .await;

        self.terminals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&command.id);
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&command.id);
        let (status, exit_code, message) = match outcome {
            Ok((status, exit_code)) => (status, exit_code, None),
            Err(err) => {
                warn!(error = %err, "terminal session failed");
                (
                    CommandStatus::Failed,
                    None,
                    Some(format!("terminal: {err}")),
                )
            }
        };
        let _ = tx.send(NodeToHub::CommandResult {
            command_id: command.id.clone(),
            status,
            exit_code,
            duration_ms: now_ms().saturating_sub(started),
            message,
        });
        Ok(())
    }

    /// Terminals need a Unix pseudo-terminal; other platforms deny them.
    #[cfg(not(unix))]
    async fn run_terminal(
        &self,
        command: &CommandRequest,
        _cwd: &Path,
        _secret: Option<(&str, &str)>,
        _cancel: watch::Receiver<bool>,
        started: u64,
        tx: mpsc::UnboundedSender<NodeToHub>,
    ) -> Result<()> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&command.id);
        let _ = tx.send(NodeToHub::CommandResult {
            command_id: command.id.clone(),
            status: CommandStatus::Denied,
            exit_code: None,
            duration_ms: now_ms().saturating_sub(started),
            message: Some("terminals need a Unix node".into()),
        });
        Ok(())
    }

    fn resolve_cwd(&self, command: &CommandRequest) -> std::result::Result<PathBuf, String> {
        let requested = match &command.cwd {
            Some(cwd) if !cwd.trim().is_empty() => PathBuf::from(cwd),
            _ => self.config.allowed_roots[0].clone(),
        };
        let canonical = std::fs::canonicalize(&requested).map_err(|_| {
            format!(
                "working directory {} is not accessible",
                requested.display()
            )
        })?;
        if !self
            .config
            .allowed_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return Err(format!(
                "working directory {} is outside the allowed roots",
                canonical.display()
            ));
        }
        if !canonical.is_dir() {
            return Err(format!("{} is not a directory", canonical.display()));
        }
        Ok(canonical)
    }

    fn check_program(&self, command: &CommandRequest) -> std::result::Result<(), String> {
        if self.config.allowed_commands.is_empty() {
            return Ok(());
        }
        let program = Path::new(&command.argv[0]);
        let name = program
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(command.argv[0].as_str());
        if self
            .config
            .allowed_commands
            .iter()
            .any(|allowed| allowed == name || allowed == &command.argv[0])
        {
            Ok(())
        } else {
            Err(format!(
                "program {name} is not in HERMES_NODE_ALLOW_COMMANDS"
            ))
        }
    }

    fn build_child(
        &self,
        command: &CommandRequest,
        cwd: &Path,
        secret: Option<(&str, &str)>,
    ) -> Result<Child> {
        let mut process = tokio::process::Command::new(&command.argv[0]);
        process
            .args(&command.argv[1..])
            .current_dir(cwd)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in child_environment(cwd, &command.env, secret) {
            process.env(key, value);
        }
        process.spawn().context("spawn command")
    }
}

fn child_environment(
    cwd: &Path,
    extra: &BTreeMap<String, String>,
    secret: Option<(&str, &str)>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
    );
    env.insert(
        "HOME".into(),
        std::env::var("HOME").unwrap_or_else(|_| cwd.display().to_string()),
    );
    env.insert(
        "LANG".into(),
        std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
    );
    env.insert("TERM".into(), "dumb".into());
    for (key, value) in extra {
        env.insert(key.clone(), value.clone());
    }
    if let Some((name, value)) = secret {
        // The caller has already validated that this name is not also in
        // `extra`; it is the one place a secret enters the child.
        env.insert(name.to_string(), value.to_string());
    }
    env
}

async fn stream_output<R>(
    mut reader: R,
    tx: mpsc::UnboundedSender<NodeToHub>,
    command_id: String,
    channel: StreamChannel,
    seq: Arc<AtomicU64>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; STREAM_CHUNK];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let data_b64 = STANDARD.encode(&buffer[..read]);
                let seq = seq.fetch_add(1, Ordering::Relaxed);
                if tx
                    .send(NodeToHub::Stream {
                        command_id: command_id.clone(),
                        seq,
                        channel,
                        data_b64,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(err) => {
                debug!(error = %err, "output read failed");
                break;
            }
        }
    }
}

fn read_load() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().next()?.parse().ok()
}

fn random_challenge() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    STANDARD.encode(bytes)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn command(argv: Vec<&str>) -> CommandRequest {
        CommandRequest {
            id: "c-1".into(),
            idempotency_key: "idem-1".into(),
            argv: argv.into_iter().map(str::to_string).collect(),
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: 5_000,
            secret: None,
            pty: None,
        }
    }

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("hermes-node-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    #[test]
    fn hub_url_is_normalized_to_the_node_endpoint() {
        assert_eq!(
            normalize_hub_url("https://hub.example").expect("url"),
            "wss://hub.example/v1/node"
        );
        assert_eq!(
            normalize_hub_url("http://127.0.0.1:8080/").expect("url"),
            "ws://127.0.0.1:8080/v1/node"
        );
        assert_eq!(
            normalize_hub_url("wss://hub.example/v1/node").expect("url"),
            "wss://hub.example/v1/node"
        );
        assert!(normalize_hub_url("hub.example").is_err());
    }

    #[test]
    fn policy_rejects_a_cwd_outside_the_roots() {
        let root = temp_root();
        let agent = Agent::new(Config::for_test(root));
        let mut request = command(vec!["sh", "-c", "true"]);
        request.cwd = Some("/".into());
        let err = agent.resolve_cwd(&request).expect_err("must reject");
        assert!(err.contains("outside the allowed roots"), "{err}");
    }

    #[test]
    fn policy_rejects_a_program_outside_the_allowlist() {
        let root = temp_root();
        let agent = Agent::new(Config::for_test(root));
        let request = command(vec!["python3", "-V"]);
        let err = agent.check_program(&request).expect_err("must reject");
        assert!(err.contains("not in HERMES_NODE_ALLOW_COMMANDS"), "{err}");
        agent
            .check_program(&command(vec!["/bin/sh", "-c", "true"]))
            .expect("sh is allowed by basename");
    }

    #[test]
    fn child_environment_does_not_inherit_the_node_token() {
        let root = temp_root();
        unsafe { std::env::set_var("HERMES_NODE_TOKEN", "parent-only-secret") };
        let request = command(vec![
            "sh",
            "-c",
            "printf '%s|%s' \"$HERMES_NODE_TOKEN\" \"$INJECTED\"",
        ]);
        let env = child_environment(&root, &request.env, Some(("INJECTED", "child-secret")));
        assert!(!env.contains_key("HERMES_NODE_TOKEN"), "{env:?}");
        assert_eq!(
            env.get("INJECTED").map(String::as_str),
            Some("child-secret")
        );
    }

    #[tokio::test]
    async fn execution_streams_output_and_reports_success() {
        let root = temp_root();
        let agent = Agent::new(Config::for_test(root));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let request = command(vec!["sh", "-c", "printf hello; printf oops >&2"]);
        agent.execute(request, tx).await.expect("execute");
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut result = None;
        while let Some(message) = rx.recv().await {
            match message {
                NodeToHub::Stream {
                    channel, data_b64, ..
                } => {
                    let bytes = STANDARD.decode(data_b64).expect("base64");
                    match channel {
                        StreamChannel::Stdout => stdout.push_str(&String::from_utf8_lossy(&bytes)),
                        StreamChannel::Stderr => stderr.push_str(&String::from_utf8_lossy(&bytes)),
                        _ => {}
                    }
                }
                NodeToHub::CommandResult { status, .. } => {
                    result = Some(status);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(stdout, "hello");
        assert_eq!(stderr, "oops");
        assert_eq!(result, Some(CommandStatus::Succeeded));
    }

    #[tokio::test]
    async fn execution_times_out_and_kills_the_child() {
        let root = temp_root();
        let agent = Agent::new(Config::for_test(root));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut request = command(vec!["sh", "-c", "sleep 5"]);
        request.timeout_ms = 100;
        agent.execute(request, tx).await.expect("execute");
        let mut result = None;
        while let Some(message) = rx.recv().await {
            if let NodeToHub::CommandResult { status, .. } = message {
                result = Some(status);
                break;
            }
        }
        assert_eq!(result, Some(CommandStatus::TimedOut));
    }
}
