//! Device WebSocket transport for the GPUI client.
//!
//! The hub sends node presence, command states/streams and one-time secret
//! prompts over `/v1/device`. This worker keeps that connection independent of
//! the normal Hermes gateway worker, reconnects with backoff, and never blocks
//! the UI thread.
//!
//! Secret answers are carried by `HubCommand::Answer`; its `Debug` output is
//! redacted and the value is zeroized if the command is discarded before it can
//! be sent.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{SinkExt, StreamExt};
use hermes_protocol::{
    CommandRequest, DeviceToHub, Frame, HubToDevice, MAX_FRAME_BYTES, PromptAnswer, PromptDecision,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{debug, warn};
use zeroize::Zeroize;

use crate::backend::BackendEvent;

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_secs(20);

/// Hub connection settings, read once at startup.
#[derive(Clone)]
pub struct HubConfig {
    pub url: String,
    pub token: String,
    pub device_id: String,
    pub name: String,
}

impl fmt::Debug for HubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubConfig")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("device_id", &self.device_id)
            .field("name", &self.name)
            .finish()
    }
}

impl HubConfig {
    pub fn new(
        url: impl Into<String>,
        token: impl Into<String>,
        device_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            token: token.into(),
            device_id: device_id.into(),
            name: name.into(),
        }
    }

    /// Read the client's hub settings. All three required variables must be
    /// present together; a partial set is a configuration error, not a silent
    /// fall back to the gateway-only client.
    pub fn from_env() -> Result<Option<Self>, String> {
        let get = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let url = get("HERMES_HUB_URL");
        let token = get("HERMES_HUB_DEVICE_TOKEN");
        let device_id = get("HERMES_HUB_DEVICE_ID");
        match (url, token, device_id) {
            (None, None, None) => Ok(None),
            (Some(url), Some(token), Some(device_id)) => Ok(Some(Self::new(
                url,
                token,
                device_id,
                get("HERMES_HUB_DEVICE_NAME").unwrap_or_else(|| "Hermes GPUI".into()),
            ))),
            _ => Err(
                "HERMES_HUB_URL, HERMES_HUB_DEVICE_ID and HERMES_HUB_DEVICE_TOKEN must be set together"
                    .into(),
            ),
        }
    }

    /// Normalise an HTTP(S) or WS(S) base URL to the device endpoint. A URL
    /// that already ends in `/v1/device` is left alone.
    pub fn websocket_url(&self) -> Result<String, String> {
        let raw = self.url.trim();
        let (scheme, rest) = if let Some(rest) = raw.strip_prefix("https://") {
            ("wss", rest)
        } else if let Some(rest) = raw.strip_prefix("http://") {
            ("ws", rest)
        } else if let Some(rest) = raw.strip_prefix("wss://") {
            ("wss", rest)
        } else if let Some(rest) = raw.strip_prefix("ws://") {
            ("ws", rest)
        } else {
            return Err("HERMES_HUB_URL must start with http://, https://, ws:// or wss://".into());
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Err("HERMES_HUB_URL has no host".into());
        }
        let suffix = if rest.ends_with("/v1/device") {
            ""
        } else {
            "/v1/device"
        };
        Ok(format!("{scheme}://{rest}{suffix}"))
    }

    fn request(
        &self,
    ) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, String> {
        let url = self.websocket_url()?;
        let mut request = url
            .into_client_request()
            .map_err(|err| format!("invalid hub URL: {err}"))?;
        let bearer = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|_| "device token contains invalid header characters".to_string())?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            bearer,
        );
        Ok(request)
    }
}

#[derive(Clone)]
pub struct HubLink {
    commands: mpsc::UnboundedSender<HubCommand>,
}

impl HubLink {
    pub fn spawn(
        config: HubConfig,
        events: mpsc::UnboundedSender<BackendEvent>,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        runtime.spawn(run(config, rx, events));
        Self { commands }
    }

    pub fn send(&self, command: HubCommand) -> bool {
        self.commands.send(command).is_ok()
    }
}

/// A request from the UI to the hub connection.
pub enum HubCommand {
    Submit {
        node_id: String,
        command: CommandRequest,
    },
    Cancel {
        command_id: String,
    },
    Answer {
        answer: PromptAnswer,
    },
}

impl fmt::Debug for HubCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Submit { node_id, command } => f
                .debug_struct("Submit")
                .field("node_id", node_id)
                .field("argv", &command.argv)
                .finish(),
            Self::Cancel { command_id } => f
                .debug_struct("Cancel")
                .field("command_id", command_id)
                .finish(),
            Self::Answer { answer } => f.debug_struct("Answer").field("answer", answer).finish(),
        }
    }
}

impl HubCommand {
    /// Drop a request that could not be delivered, zeroizing a secret answer.
    fn discard(self) {
        if let Self::Answer { mut answer } = self
            && let PromptDecision::Provide { value, .. } = &mut answer.decision
        {
            value.zeroize();
        }
    }
}

async fn run(
    config: HubConfig,
    mut commands: mpsc::UnboundedReceiver<HubCommand>,
    events: mpsc::UnboundedSender<BackendEvent>,
) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let _ = events.send(BackendEvent::HubConnection(false));
        match session(&config, &mut commands, &events).await {
            Ok(()) => debug!("hub device session closed"),
            Err(err) => {
                warn!(error = %err, "hub device session failed");
                let _ = events.send(BackendEvent::HubProblem(format!("hub connection: {err}")));
            }
        }
        let wait = tokio::time::sleep(backoff);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                command = commands.recv() => {
                    let Some(command) = command else { return };
                    let _ = events.send(BackendEvent::HubProblem(
                        "Hub offline; the request was not sent.".into(),
                    ));
                    command.discard();
                }
            }
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn session(
    config: &HubConfig,
    commands: &mut mpsc::UnboundedReceiver<HubCommand>,
    events: &mpsc::UnboundedSender<BackendEvent>,
) -> Result<(), String> {
    let request = config.request()?;
    let socket_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (socket, _) =
        tokio_tungstenite::connect_async_with_config(request, Some(socket_config), false)
            .await
            .map_err(|err| err.to_string())?;
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<DeviceToHub>();

    out_tx
        .send(DeviceToHub::Hello {
            device_id: config.device_id.clone(),
            name: config.name.clone(),
        })
        .map_err(|_| "hub writer closed during hello".to_string())?;

    let writer: JoinHandle<()> = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            let text = match Frame::new(message).encode() {
                Ok(text) => text,
                Err(err) => {
                    warn!(error = %err, "failed to encode a hub frame");
                    continue;
                }
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut connected = false;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                let Some(message) = command.into_message() else { continue };
                if out_tx.send(message).is_err() {
                    break;
                }
            }
            incoming = stream.next() => {
                let Some(incoming) = incoming else { break };
                let text = match incoming.map_err(|err| err.to_string())? {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => String::from_utf8(bytes.to_vec())
                        .map_err(|_| "hub sent a binary frame that is not UTF-8".to_string())?,
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                };
                let frame = Frame::<HubToDevice>::decode(&text)
                    .map_err(|err| format!("bad hub frame: {err}"))?;
                match frame.message {
                    HubToDevice::HelloAck { device_id, .. } => {
                        if device_id != config.device_id {
                            return Err("hub acknowledged a different device id".into());
                        }
                        connected = true;
                        let _ = events.send(BackendEvent::HubConnection(true));
                    }
                    HubToDevice::NodeList { nodes } => {
                        let _ = events.send(BackendEvent::HubNodes(nodes));
                    }
                    HubToDevice::NodePresence { node } => {
                        let _ = events.send(BackendEvent::HubNode(node));
                    }
                    HubToDevice::CommandState { command } => {
                        let _ = events.send(BackendEvent::HubCommand(command));
                    }
                    HubToDevice::CommandStream {
                        command_id,
                        channel,
                        data_b64,
                        ..
                    } => {
                        match BASE64.decode(data_b64) {
                            Ok(data) => {
                                let _ = events.send(BackendEvent::HubStream {
                                    command_id,
                                    channel,
                                    data,
                                });
                            }
                            Err(err) => {
                                let _ = events.send(BackendEvent::HubProblem(format!(
                                    "hub stream could not be decoded: {err}"
                                )));
                            }
                        }
                    }
                    HubToDevice::PromptRequest { prompt } => {
                        let _ = events.send(BackendEvent::HubPrompt(prompt));
                    }
                    HubToDevice::PromptClosed { prompt_id } => {
                        let _ = events.send(BackendEvent::HubPromptClosed(prompt_id));
                    }
                    HubToDevice::Pong { .. } => {}
                    HubToDevice::Error { code, message } => {
                        let _ = events.send(BackendEvent::HubProblem(format!(
                            "hub {code}: {message}"
                        )));
                    }
                }
            }
            _ = heartbeat.tick() => {
                if out_tx.send(DeviceToHub::Ping { at_ms: now_ms() }).is_err() {
                    break;
                }
            }
        }
    }

    drop(out_tx);
    writer.abort();
    let _ = writer.await;
    if connected {
        let _ = events.send(BackendEvent::HubConnection(false));
    }
    Ok(())
}

impl HubCommand {
    /// Turn a UI request into its wire message. Kept here so `app.rs` never
    /// constructs protocol frames.
    fn into_message(self) -> Option<DeviceToHub> {
        match self {
            Self::Submit { node_id, command } => {
                Some(DeviceToHub::SubmitCommand { node_id, command })
            }
            Self::Cancel { command_id } => Some(DeviceToHub::CancelCommand { command_id }),
            Self::Answer { answer } => Some(DeviceToHub::PromptAnswer { answer }),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_protocol::{NodeSummary, SecretPrompt, StreamChannel};
    use std::collections::BTreeMap;

    fn node() -> NodeSummary {
        NodeSummary {
            id: "node-1".into(),
            name: "forge".into(),
            hostname: "forge.lan".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            agent_version: "0.1.0".into(),
            capabilities: hermes_protocol::Capabilities::baseline(),
            labels: BTreeMap::new(),
            online: true,
            last_seen_ms: 1,
        }
    }

    #[test]
    fn config_normalises_urls_and_redacts_the_token() {
        let config = HubConfig::new("https://hermes", "secret-token", "device-1", "laptop");
        assert_eq!(config.websocket_url().unwrap(), "wss://hermes/v1/device");
        assert_eq!(
            HubConfig::new("http://127.0.0.1:8080/", "t", "d", "n")
                .websocket_url()
                .unwrap(),
            "ws://127.0.0.1:8080/v1/device"
        );
        assert_eq!(
            HubConfig::new("wss://hermes/v1/device", "t", "d", "n")
                .websocket_url()
                .unwrap(),
            "wss://hermes/v1/device"
        );
        assert!(
            HubConfig::new("hermes", "t", "d", "n")
                .websocket_url()
                .is_err()
        );
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn answer_debug_never_contains_the_secret() {
        let command = HubCommand::Answer {
            answer: PromptAnswer {
                prompt_id: "p-1".into(),
                challenge: "x".repeat(43),
                decision: PromptDecision::Provide {
                    value: "hunter2".into(),
                    remember: false,
                },
            },
        };
        let debug = format!("{command:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    async fn send_frame(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        message: HubToDevice,
    ) {
        let text = Frame::new(message).encode().expect("encode");
        socket
            .send(Message::Text(text.into()))
            .await
            .expect("send frame");
    }

    #[tokio::test]
    async fn session_connects_authenticates_and_routes_frames() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept ws");
            let hello = socket.next().await.expect("hello").expect("hello frame");
            let Message::Text(text) = hello else {
                panic!("expected text hello");
            };
            let hello: Frame<DeviceToHub> = Frame::decode(text.as_str()).expect("hello decode");
            assert!(matches!(
                hello.message,
                DeviceToHub::Hello { ref device_id, .. } if device_id == "device-1"
            ));

            send_frame(
                &mut socket,
                HubToDevice::HelloAck {
                    device_id: "device-1".into(),
                    heartbeat_ms: 15_000,
                },
            )
            .await;
            send_frame(
                &mut socket,
                HubToDevice::NodeList {
                    nodes: vec![node()],
                },
            )
            .await;

            // The client had a command queued before the session started.
            let submitted = socket
                .next()
                .await
                .expect("submitted")
                .expect("submit frame");
            let Message::Text(text) = submitted else {
                panic!("expected text submit");
            };
            let frame: Frame<DeviceToHub> = Frame::decode(text.as_str()).expect("submit decode");
            assert!(matches!(
                frame.message,
                DeviceToHub::SubmitCommand { ref node_id, .. } if node_id == "node-1"
            ));

            send_frame(
                &mut socket,
                HubToDevice::CommandStream {
                    node_id: "node-1".into(),
                    command_id: "cmd-1".into(),
                    seq: 1,
                    channel: StreamChannel::Stdout,
                    data_b64: BASE64.encode(b"hello\n"),
                },
            )
            .await;
            send_frame(
                &mut socket,
                HubToDevice::PromptRequest {
                    prompt: SecretPrompt {
                        id: "p-1".into(),
                        node_id: "node-1".into(),
                        command_id: "cmd-1".into(),
                        challenge: "x".repeat(43),
                        requester: "hermes-node@forge".into(),
                        purpose: "sudo".into(),
                        scope: "one command".into(),
                        env_var: "SUDO_PASSWORD".into(),
                        created_at_ms: 1,
                        expires_at_ms: 60_000,
                    },
                },
            )
            .await;
            let _ = socket.close(None).await;
        });

        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let config = HubConfig::new(
            format!("ws://{address}"),
            "device-token",
            "device-1",
            "test",
        );
        command_tx
            .send(HubCommand::Submit {
                node_id: "node-1".into(),
                command: CommandRequest {
                    id: "cmd-1".into(),
                    idempotency_key: "idem-1".into(),
                    argv: vec!["uptime".into()],
                    cwd: None,
                    env: BTreeMap::new(),
                    timeout_ms: 30_000,
                    secret: None,
                },
            })
            .expect("queue command");

        let client =
            tokio::spawn(async move { session(&config, &mut command_rx, &event_tx).await });

        let mut connected = false;
        let mut nodes = false;
        let mut streamed = false;
        let mut prompted = false;
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        while !(connected && nodes && streamed && prompted) {
            tokio::select! {
                _ = &mut deadline => panic!("timed out waiting for hub events"),
                event = event_rx.recv() => {
                    match event.expect("event channel") {
                        BackendEvent::HubConnection(state) => connected = state,
                        BackendEvent::HubNodes(rows) => nodes = rows.len() == 1,
                        BackendEvent::HubStream { command_id, data, .. } => {
                            assert_eq!(command_id, "cmd-1");
                            assert_eq!(data, b"hello\n");
                            streamed = true;
                        }
                        BackendEvent::HubPrompt(prompt) => {
                            assert_eq!(prompt.env_var, "SUDO_PASSWORD");
                            prompted = true;
                        }
                        BackendEvent::HubProblem(message) => panic!("hub problem: {message}"),
                        _ => {}
                    }
                }
            }
        }
        let _ = client.await;
        let _ = server.await;
    }
}
