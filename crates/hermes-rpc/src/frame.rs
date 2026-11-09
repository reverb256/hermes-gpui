//! Frame and event types for the Hermes gateway wire protocol.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A decoded `event` notification (`{"method": "event", "params": …}`).
///
/// `session_id` and `seq` are optional because not every backend revision
/// stamps every frame; `seq` drives reconnect replay ordering, so callers that
/// care must tolerate its absence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Event-specific body. Named `payload` in the wire contract; older
    /// frames put the fields at the top level, so `raw` keeps the whole frame.
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub raw: Value,
}

impl GatewayEvent {
    /// Text delta for a streaming assistant message, if this frame is one.
    pub fn text_delta(&self) -> Option<&str> {
        match self.kind.as_str() {
            "message.delta" => self
                .payload
                .get("text")
                .or_else(|| self.payload.get("delta"))
                .and_then(Value::as_str),
            _ => None,
        }
    }

    /// Reasoning/thinking delta, if this frame is one.
    pub fn reasoning_delta(&self) -> Option<&str> {
        match self.kind.as_str() {
            "reasoning.delta" | "thinking.delta" => self
                .payload
                .get("text")
                .or_else(|| self.payload.get("delta"))
                .and_then(Value::as_str),
            _ => None,
        }
    }
}

/// A server->client JSON-RPC request awaiting an answer from the UI.
#[derive(Debug, Clone)]
pub struct ServerRequest {
    pub id: String,
    pub method: String,
    pub params: Value,
}

/// Error body of a JSON-RPC response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

/// Any failure the channel can surface to a caller.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("gateway not connected")]
    NotConnected,
    #[error("request timed out after {seconds}s: {method}")]
    Timeout { seconds: u64, method: String },
    #[error("{message} (code {code})")]
    Remote {
        code: i64,
        message: String,
        data: Value,
    },
    #[error("transport closed: {0}")]
    Closed(String),
    #[error("transport error: {0}")]
    Transport(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("malformed frame: {0}")]
    Decode(String),
}

/// JSON-RPC `method not found`, used when no local handler accepts a request.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC `internal error`, used when a local request handler panics.
pub const INTERNAL_ERROR: i64 = -32603;
/// Hermes' "this client does not show that session" decline code.
pub const SESSION_NOT_SHOWN: i64 = 4404;

impl From<JsonRpcError> for RpcError {
    fn from(err: JsonRpcError) -> Self {
        RpcError::Remote {
            code: err.code,
            message: err.message,
            data: err.data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_delta_exposes_text() {
        let event: GatewayEvent = serde_json::from_value(serde_json::json!({
            "type": "message.delta",
            "session_id": "s-1",
            "seq": 7,
            "payload": { "text": "hello" }
        }))
        .expect("event parses");

        assert_eq!(event.kind, "message.delta");
        assert_eq!(event.session_id.as_deref(), Some("s-1"));
        assert_eq!(event.seq, Some(7));
        assert_eq!(event.text_delta(), Some("hello"));
    }

    #[test]
    fn reasoning_and_thinking_both_map_to_reasoning() {
        for kind in ["reasoning.delta", "thinking.delta"] {
            let event: GatewayEvent = serde_json::from_value(serde_json::json!({
                "type": kind,
                "payload": { "delta": "why" }
            }))
            .expect("event parses");
            assert_eq!(event.reasoning_delta(), Some("why"));
            assert_eq!(event.text_delta(), None);
        }
    }

    #[test]
    fn other_events_have_no_delta() {
        let event: GatewayEvent = serde_json::from_value(serde_json::json!({
            "type": "tool.start",
            "payload": { "tool": "bash" }
        }))
        .expect("event parses");
        assert_eq!(event.text_delta(), None);
        assert_eq!(event.reasoning_delta(), None);
    }

    #[test]
    fn seq_is_optional() {
        let event: GatewayEvent = serde_json::from_value(serde_json::json!({
            "type": "gateway.ready",
            "payload": { "heartbeat": true }
        }))
        .expect("event parses");
        assert_eq!(event.seq, None);
        assert_eq!(event.session_id, None);
    }
}
