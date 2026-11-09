//! JSON-RPC 2.0 transport for the Hermes `tui_gateway`.
//!
//! Wire protocol is a port of the official TypeScript client in
//! `hermes-agent/apps/shared/src/json-rpc-channel.ts` and
//! `json-rpc-gateway.ts`:
//!
//! * newline-delimited JSON-RPC 2.0 frames in both directions over a WebSocket
//!   (`/api/ws` on the gateway/dashboard, default port 9119),
//! * `gateway.ready` arrives right after accept; if it advertises
//!   `payload.heartbeat == true` the client starts `gateway.ping` keepalive,
//! * notifications arrive as `{"method": "event", "params": GatewayEvent}`,
//! * server->client requests (`approval.request`, `clarify`, …) carry a string
//!   id and are answered by the UI.

mod client;
mod frame;

pub use client::{
    APPROVAL_RESPOND_TIMEOUT, Auth, ConnectionConfig, ConnectionState, GatewayClient,
};
pub use frame::{
    GatewayEvent, INTERNAL_ERROR, JsonRpcError, METHOD_NOT_FOUND, RpcError, SESSION_NOT_SHOWN,
    ServerRequest,
};
