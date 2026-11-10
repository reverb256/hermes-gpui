//! WebSocket client for the Hermes gateway JSON-RPC channel.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tracing::{debug, warn};

use crate::frame::{GatewayEvent, JsonRpcError, METHOD_NOT_FOUND, RpcError, ServerRequest};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_HEARTBEAT_DEADLINE: Duration = Duration::from_secs(45);
/// Approval answers ride the backend's own window (`approvals.timeout`, 300s).
pub const APPROVAL_RESPOND_TIMEOUT: Duration = Duration::from_secs(300);
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Credential presented on the WebSocket upgrade.
#[derive(Clone)]
pub enum Auth {
    Bearer(String),
    /// Pre-encoded `Authorization` header value, e.g. `Basic <base64>`.
    Header(String),
    /// `Cookie` header value, e.g. `__Host-hermes_session_at=…` (dashboard session).
    Cookie(String),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Auth(<redacted>)")
    }
}

/// How to reach a gateway.
#[derive(Clone)]
pub struct ConnectionConfig {
    /// `ws://host:9119/api/ws` or `wss://…/api/ws`.
    pub url: String,
    pub auth: Option<Auth>,
    pub request_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_deadline: Duration,
}

impl std::fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("url", &self.url.split('?').next().unwrap_or_default())
            .field("auth", &self.auth)
            .field("request_timeout", &self.request_timeout)
            .field("heartbeat_interval", &self.heartbeat_interval)
            .field("heartbeat_deadline", &self.heartbeat_deadline)
            .finish()
    }
}

impl ConnectionConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            heartbeat_deadline: DEFAULT_HEARTBEAT_DEADLINE,
        }
    }

    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(Auth::Bearer(token.into()));
        self
    }

    pub fn with_cookie(mut self, cookie: impl Into<String>) -> Self {
        self.auth = Some(Auth::Cookie(cookie.into()));
        self
    }

    pub fn with_basic(mut self, user: &str, password: &str) -> Self {
        self.auth = Some(Auth::Header(format!(
            "Basic {}",
            base64_encode(format!("{user}:{password}").as_bytes())
        )));
        self
    }
}

/// Observable connection lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Open,
    Closed,
}

type Pending = Arc<
    std::sync::Mutex<std::collections::HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>,
>;

/// Live JSON-RPC channel. Cloneable: every clone shares the socket, pending
/// calls and event bus, so views can hold one without ceremony.
#[derive(Clone)]
pub struct GatewayClient {
    outbound: mpsc::UnboundedSender<Message>,
    pending: Pending,
    events: broadcast::Sender<GatewayEvent>,
    state: watch::Receiver<ConnectionState>,
    next_id: Arc<AtomicU64>,
    server_requests: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ServerRequest>>>>,
    last_liveness: Arc<std::sync::Mutex<std::time::Instant>>,
    request_timeout: Duration,
    shutdown: broadcast::Sender<()>,
}

impl GatewayClient {
    /// Dial the gateway and start the reader + heartbeat tasks.
    pub async fn connect(cfg: ConnectionConfig) -> Result<Self, RpcError> {
        let mut request = cfg
            .url
            .as_str()
            .into_client_request()
            .map_err(|err| RpcError::Decode(err.to_string()))?;

        if let Some(auth) = &cfg.auth {
            let (name, value) = match auth {
                Auth::Bearer(token) => ("authorization", format!("Bearer {token}")),
                Auth::Header(value) => ("authorization", value.clone()),
                Auth::Cookie(value) => ("cookie", value.clone()),
            };
            let value =
                HeaderValue::from_str(&value).map_err(|err| RpcError::Decode(err.to_string()))?;
            request.headers_mut().insert(name, value);
        }

        let (socket, _response) = tokio_tungstenite::connect_async(request).await?;
        let (mut sink, mut stream) = socket.split();

        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<Message>();
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (state_tx, state_rx) = watch::channel(ConnectionState::Open);
        let (shutdown, _) = broadcast::channel::<()>(1);

        let pending: Pending = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let server_requests: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ServerRequest>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let last_liveness = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));

        // Writer: drains the outbound queue onto the socket.
        let writer_state = state_tx.clone();
        tokio::spawn(async move {
            while let Some(frame) = outbound_rx.recv().await {
                if let Err(err) = sink.send(frame).await {
                    warn!(?err, "gateway write failed");
                    break;
                }
            }
            let _ = writer_state.send(ConnectionState::Closed);
            let _ = sink.close().await;
        });

        // Reader: routes responses, events and server requests.
        let reader_pending = pending.clone();
        let reader_events = events.clone();
        let reader_requests = server_requests.clone();
        let reader_liveness = last_liveness.clone();
        let reader_outbound = outbound.clone();
        let reader_state = state_tx.clone();
        let reader_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut ping_ids: HashSet<String> = HashSet::new();
            let mut heartbeat_armed = false;

            while let Some(message) = stream.next().await {
                let text = match message {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Ok(Message::Close(frame)) => {
                        debug!(?frame, "gateway closed");
                        break;
                    }
                    Ok(_) => continue,
                    Err(err) => {
                        warn!(?err, "gateway read failed");
                        break;
                    }
                };

                let frame: Value = match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(err) => {
                        warn!(?err, "skipping non-JSON gateway frame");
                        continue;
                    }
                };

                *reader_liveness.lock().expect("liveness lock") = std::time::Instant::now();

                // Response to one of our calls, or a server->client request.
                if let Some(id) = frame.get("id").filter(|id| !id.is_null()) {
                    let id = match id {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    };
                    if let Some(id) = id {
                        if let Some(method) = frame.get("method").and_then(Value::as_str) {
                            if ping_ids.remove(&id) {
                                continue;
                            }
                            let request = ServerRequest {
                                id: id.clone(),
                                method: method.to_string(),
                                params: frame.get("params").cloned().unwrap_or(Value::Null),
                            };
                            let delivered = reader_requests
                                .lock()
                                .expect("server request lock")
                                .as_ref()
                                .map(|tx| tx.send(request).is_ok())
                                .unwrap_or(false);
                            if !delivered {
                                // Match the TS client: never leave the backend
                                // waiting out its deadline for an answer.
                                let _ = reply_error(&reader_outbound, &id, METHOD_NOT_FOUND);
                            }
                            continue;
                        }

                        let call = reader_pending.lock().expect("pending lock").remove(&id);
                        if let Some(call) = call {
                            let outcome = match frame.get("error").filter(|err| !err.is_null()) {
                                Some(err) => {
                                    match serde_json::from_value::<JsonRpcError>(err.clone()) {
                                        Ok(parsed) => Err(RpcError::from(parsed)),
                                        Err(decode) => Err(RpcError::Decode(decode.to_string())),
                                    }
                                }
                                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = call.send(outcome);
                        }
                    }
                    continue;
                }

                // `event` notification.
                if frame.get("method").and_then(Value::as_str) == Some("event") {
                    let params = frame.get("params").cloned().unwrap_or(Value::Null);
                    let Ok(mut event) = serde_json::from_value::<GatewayEvent>(params.clone())
                    else {
                        continue;
                    };
                    event.raw = params;

                    if event.kind == "gateway.ready" {
                        let heartbeat = event
                            .payload
                            .get("heartbeat")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        heartbeat_armed = heartbeat;
                        // Announce that this client answers server requests.
                        let capabilities = json!({
                            "jsonrpc": "2.0",
                            "id": "capabilities",
                            "method": "client.capabilities",
                            "params": { "server_requests": true },
                        });
                        let _ =
                            reader_outbound.send(Message::Text(capabilities.to_string().into()));
                    }

                    if heartbeat_armed && event.kind == "gateway.pong" {
                        continue;
                    }
                    let _ = reader_events.send(event);
                }
            }

            let _ = reader_state.send(ConnectionState::Closed);
            let _ = reader_shutdown.send(());
        });

        let client = Self {
            outbound: outbound.clone(),
            pending,
            events,
            state: state_rx,
            next_id: Arc::new(AtomicU64::new(0)),
            server_requests,
            last_liveness,
            request_timeout: cfg.request_timeout,
            shutdown,
        };

        // Keepalive: a silent socket (sleep, VPN drop, proxy idle timeout)
        // never fires `close`, so ping and judge the deadline ourselves.
        let heartbeat = client.clone();
        let pinger = client.clone();
        let interval = cfg.heartbeat_interval;
        let deadline = cfg.heartbeat_deadline;
        tokio::spawn(async move {
            if interval.is_zero() || deadline.is_zero() {
                return;
            }
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut shutdown_rx = heartbeat.shutdown.subscribe();
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = shutdown_rx.recv() => return,
                }
                let silent_for = heartbeat
                    .last_liveness
                    .lock()
                    .expect("liveness lock")
                    .elapsed();
                if silent_for >= deadline {
                    warn!(?silent_for, "gateway heartbeat deadline exceeded");
                    let _ = heartbeat.outbound.send(Message::Close(None));
                    return;
                }
                let _ = pinger.request::<Value>("gateway.ping", json!({})).await;
            }
        });

        Ok(client)
    }

    /// Issue a JSON-RPC request and await its result.
    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, RpcError> {
        let value = self
            .request_value_with_timeout(method, params, self.request_timeout)
            .await?;
        serde_json::from_value(value).map_err(|err| RpcError::Decode(err.to_string()))
    }

    /// Issue a request as raw JSON (no response type coercion).
    pub async fn request_value(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.request_value_with_timeout(method, params, self.request_timeout)
            .await
    }

    pub async fn request_value_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if *self.state.borrow() != ConnectionState::Open {
            return Err(RpcError::NotConnected);
        }

        let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "method": method,
            "params": params,
        });
        if self
            .outbound
            .send(Message::Text(frame.to_string().into()))
            .is_err()
        {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err(RpcError::NotConnected);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed("channel closed".into())),
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(RpcError::Timeout {
                    seconds: timeout.as_secs(),
                    method: method.to_string(),
                })
            }
        }
    }

    /// Subscribe to gateway event notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<GatewayEvent> {
        self.events.subscribe()
    }

    /// Take the server->client request stream. Once taken, requests are no
    /// longer auto-declined; the caller must answer each one.
    pub fn take_server_requests(&self) -> mpsc::UnboundedReceiver<ServerRequest> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.server_requests.lock().expect("server request lock") = Some(tx);
        rx
    }

    /// Answer a server->client request.
    pub fn respond(&self, id: &str, result: Value) {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = self.outbound.send(Message::Text(frame.to_string().into()));
    }

    /// Fail a server->client request (the backend treats it as unanswered).
    pub fn fail_request(&self, id: &str, code: i64, message: &str) {
        let _ = reply_error(&self.outbound, id, code);
        debug!(id, code, message, "declined server request");
    }

    pub fn state(&self) -> ConnectionState {
        self.state.borrow().clone()
    }

    /// Watch the connection lifecycle.
    pub fn state_changes(&self) -> watch::Receiver<ConnectionState> {
        self.state.clone()
    }

    pub fn close(&self) {
        let _ = self.outbound.send(Message::Close(None));
        let _ = self.shutdown.send(());
    }
}

fn reply_error(outbound: &mpsc::UnboundedSender<Message>, id: &str, code: i64) -> Result<(), ()> {
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": "no handler for server request" },
    });
    outbound
        .send(Message::Text(frame.to_string().into()))
        .map_err(|_| ())
}

pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(TABLE[(b[0] >> 2) as usize] as char);
        out.push(TABLE[(((b[0] & 0b11) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0b1111) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b[2] & 0b111111) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn basic_auth_encoding() {
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }
}
