//! Typed client for a Hermes host: REST routers on the dashboard plus the
//! JSON-RPC session/prompt surface on the gateway.
//!
//! Both surfaces live on the same origin (default `:9119`): REST at
//! `/api/...`, chat at `/api/ws`. The types below mirror
//! `hermes_cli/web_routers/*.py`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use hermes_rpc::{Auth, ConnectionConfig, ConnectionState, GatewayClient, GatewayEvent};

/// Where a Hermes host lives and how to authenticate to it.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Origin only, e.g. `http://127.0.0.1:9119`.
    pub base_url: String,
    pub profile: Option<String>,
    pub token: Option<String>,
    /// `user:password` for hosts fronted by basic auth (e.g. the k3s pod's
    /// tailnet ingress).
    pub basic: Option<(String, String)>,
    /// Dashboard session cookie (`__Host-hermes_session_at=…`), set by
    /// [`authenticate`]. Sent on REST calls and used to mint WS tickets.
    pub cookie: Option<String>,
    /// Single-use `?ticket=` for the next WS upgrade, set by [`authenticate`].
    pub ticket: Option<String>,
}

impl HostConfig {
    pub fn local() -> Self {
        Self {
            base_url: "http://127.0.0.1:9119".into(),
            profile: None,
            token: None,
            basic: None,
            cookie: None,
            ticket: None,
        }
    }

    /// `HERMES_URL`, `HERMES_TOKEN`, `HERMES_BASIC` (`user:pass`), `HERMES_PROFILE`.
    pub fn from_env() -> Self {
        let mut cfg = Self::local();
        if let Ok(url) = std::env::var("HERMES_URL") {
            cfg.base_url = url.trim_end_matches('/').to_string();
        }
        if let Ok(token) = std::env::var("HERMES_TOKEN") {
            if !token.is_empty() {
                cfg.token = Some(token);
            }
        }
        if let Ok(basic) = std::env::var("HERMES_BASIC") {
            if let Some((user, pass)) = basic.split_once(':') {
                cfg.basic = Some((user.to_string(), pass.to_string()));
            }
        }
        if let Ok(cookie) = std::env::var("HERMES_COOKIE") {
            if !cookie.is_empty() {
                cfg.cookie = Some(cookie);
            }
        }
        if let Ok(profile) = std::env::var("HERMES_PROFILE") {
            if !profile.is_empty() {
                cfg.profile = Some(profile);
            }
        }
        cfg
    }

    /// Gateway WebSocket URL: same origin, `ws(s)` scheme, `/api/ws`.
    pub fn ws_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        format!("{ws}/api/ws")
    }

    pub fn connection_config(&self) -> ConnectionConfig {
        let mut cfg = ConnectionConfig::new(self.ws_url());
        if let Some(ticket) = &self.ticket {
            cfg.url = format!("{}?ticket={}", cfg.url, percent_encode(ticket));
        }
        if let Some(token) = &self.token {
            cfg = cfg.with_bearer(token.clone());
        } else if let Some(cookie) = &self.cookie {
            cfg = cfg.with_cookie(cookie.clone());
        } else if let Some((user, pass)) = &self.basic {
            cfg = cfg.with_basic(user, pass);
        }
        cfg
    }
}

/// One Hermes profile (`GET /api/profiles` → `profiles[]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub has_env: bool,
    #[serde(default)]
    pub skill_count: u32,
    #[serde(default)]
    pub gateway_running: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub bot_title: String,
    #[serde(default)]
    pub has_alias: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Profile {
    /// What a chat header should show: bot title, display name, or name.
    pub fn label(&self) -> &str {
        if !self.bot_title.is_empty() {
            &self.bot_title
        } else if !self.display_name.is_empty() {
            &self.display_name
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActiveProfile {
    pub active: String,
    pub current: String,
}

/// `GET /api/mcp/servers` keeps per-transport detail; only `name`, `enabled`
/// and `transport` are stable across revisions, so the rest is kept raw.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// REST side of a Hermes host.
#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
    basic: Option<(String, String)>,
    cookie: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("http {status} for {path}: {body}")]
    Status {
        status: u16,
        path: String,
        body: String,
    },
    #[error("request to {path} failed: {source}")]
    Transport {
        path: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("invalid payload from {path}: {source}")]
    Decode {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing `{field}` in response from {path}")]
    Missing { field: String, path: String },
    #[error(transparent)]
    Rpc(#[from] hermes_rpc::RpcError),
}

impl RestClient {
    pub fn new(host: &HostConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("hermes-gpui/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            base: host.base_url.trim_end_matches('/').to_string(),
            token: host.token.clone(),
            basic: host.basic.clone(),
            cookie: host.cookie.clone(),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let mut req = self.http.get(format!("{}{}", self.base, path));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        } else if let Some(cookie) = &self.cookie {
            // A dashboard session wins over raw basic creds: the gated routers
            // only read the cookie, and basic-auth passwords are not forwarded.
            req = req.header("cookie", cookie.clone());
        } else if let Some((user, pass)) = &self.basic {
            req = req.basic_auth(user, Some(pass));
        }

        let response = req.send().await.map_err(|source| ApiError::Transport {
            path: path.to_string(),
            source,
        })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.to_string(),
                source,
            })?;

        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                path: path.to_string(),
                body: body.chars().take(400).collect(),
            });
        }

        serde_json::from_str(&body).map_err(|source| ApiError::Decode {
            path: path.to_string(),
            source,
        })
    }

    pub async fn profiles(&self) -> Result<Vec<Profile>, ApiError> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            profiles: Vec<Profile>,
        }
        Ok(self.get::<Response>("/api/profiles").await?.profiles)
    }

    pub async fn active_profile(&self) -> Result<ActiveProfile, ApiError> {
        self.get("/api/profiles/active").await
    }

    pub async fn mcp_servers(&self) -> Result<Vec<McpServer>, ApiError> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            servers: Vec<McpServer>,
        }
        Ok(self.get::<Response>("/api/mcp/servers").await?.servers)
    }

    pub async fn cron_jobs(&self) -> Result<Value, ApiError> {
        self.get("/api/cron/jobs").await
    }

    pub async fn health(&self) -> Result<Value, ApiError> {
        self.get("/api/health").await
    }

    pub async fn status(&self) -> Result<Value, ApiError> {
        self.get("/api/status").await
    }

    /// Name of the password provider advertised by `GET /api/auth/providers`,
    /// e.g. `basic` for `HERMES_DASHBOARD_BASIC_AUTH_*` deployments.
    async fn password_provider(&self) -> Option<String> {
        let payload: Value = self.get("/api/auth/providers").await.ok()?;
        for provider in payload.get("providers")?.as_array()? {
            if provider.get("supports_password").and_then(Value::as_bool) == Some(true) {
                if let Some(name) = provider.get("name").and_then(Value::as_str) {
                    return Some(name.to_string());
                }
            }
        }
        None
    }

    /// `POST /auth/password-login`; returns the session cookie for reuse.
    pub async fn login(&self) -> Result<Option<String>, ApiError> {
        const PATH: &str = "/auth/password-login";
        let Some((user, pass)) = self.basic.clone() else {
            return Ok(None);
        };
        let provider = self
            .password_provider()
            .await
            .unwrap_or_else(|| "basic".into());
        let body = serde_json::json!({
            "provider": provider,
            "username": user,
            "password": pass,
            "next": "",
        });

        let mut req = self.http.post(format!("{}{PATH}", self.base)).json(&body);
        if let Some(cookie) = &self.cookie {
            req = req.header("cookie", cookie.clone());
        }

        let response = req.send().await.map_err(|source| ApiError::Transport {
            path: PATH.to_string(),
            source,
        })?;
        let status = response.status();

        // `hermes_session_at` holds the access token; keep the whole pair,
        // including the `__Host-`/`__Secure-` prefix the server chose.
        let mut session = None;
        for raw in response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
        {
            let Ok(raw) = raw.to_str() else { continue };
            let Some(pair) = raw.split(';').next() else {
                continue;
            };
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.contains("hermes_session_at") && !value.is_empty() {
                session = Some(format!("{name}={value}"));
            }
        }

        let text = response
            .text()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                path: PATH.to_string(),
                body: text.chars().take(400).collect(),
            });
        }
        Ok(session)
    }

    /// `POST /api/auth/ws-ticket` — single-use, 30s TTL upgrade credential.
    pub async fn ws_ticket(&self) -> Result<String, ApiError> {
        const PATH: &str = "/api/auth/ws-ticket";
        let mut req = self.http.post(format!("{}{PATH}", self.base));
        if let Some(cookie) = &self.cookie {
            req = req.header("cookie", cookie.clone());
        }
        let response = req.send().await.map_err(|source| ApiError::Transport {
            path: PATH.to_string(),
            source,
        })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                path: PATH.to_string(),
                body: text.chars().take(400).collect(),
            });
        }
        let payload: Value = serde_json::from_str(&text).map_err(|source| ApiError::Decode {
            path: PATH.to_string(),
            source,
        })?;
        payload
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|ticket| !ticket.is_empty())
            .map(str::to_string)
            .ok_or(ApiError::Missing {
                field: "ticket".into(),
                path: PATH.to_string(),
            })
    }
}

/// Percent-encode for a query value (tickets are base64url, but stay safe).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Resolve credentials for a host before dialing: password-login when only
/// basic credentials are configured, then mint a single-use WS ticket.
///
/// Failures come back as human-readable notes rather than errors so a caller
/// can still attempt the connection (loopback hosts need no auth at all).
pub async fn authenticate(host: &HostConfig) -> (HostConfig, Vec<String>) {
    let mut host = host.clone();
    let mut notes = Vec::new();

    if host.token.is_some() || host.basic.is_none() {
        return (host, notes);
    }

    if host.cookie.is_none() {
        match RestClient::new(&host).login().await {
            Ok(Some(cookie)) => host.cookie = Some(cookie),
            Ok(None) => {}
            Err(err) => notes.push(format!("login failed: {err}")),
        }
    }

    if host.cookie.is_some() {
        match RestClient::new(&host).ws_ticket().await {
            Ok(ticket) => host.ticket = Some(ticket),
            Err(err) => notes.push(format!("ws ticket failed: {err}")),
        }
    }

    (host, notes)
}

/// Session/prompt helpers over the gateway's JSON-RPC surface.
pub mod gateway {
    use super::*;

    /// `session.create` answer. `stored_session_id` is the durable transcript
    /// id the CLI can resume from.
    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct CreatedSession {
        #[serde(default)]
        pub session_id: String,
        #[serde(default)]
        pub stored_session_id: Option<String>,
        #[serde(default)]
        pub info: Value,
    }

    pub async fn create_session(
        client: &GatewayClient,
        profile: Option<&str>,
    ) -> Result<CreatedSession, hermes_rpc::RpcError> {
        let mut params = serde_json::Map::new();
        if let Some(profile) = profile {
            params.insert("profile".into(), Value::String(profile.to_string()));
        }
        client
            .request("session.create", Value::Object(params))
            .await
    }

    /// Send a user turn. The answer streams back as `message.delta` events on
    /// the same socket.
    pub async fn submit_prompt(
        client: &GatewayClient,
        session_id: &str,
        text: &str,
    ) -> Result<Value, hermes_rpc::RpcError> {
        client
            .request(
                "prompt.submit",
                serde_json::json!({ "session_id": session_id, "text": text }),
            )
            .await
    }

    pub async fn interrupt(
        client: &GatewayClient,
        session_id: &str,
    ) -> Result<Value, hermes_rpc::RpcError> {
        client
            .request(
                "session.interrupt",
                serde_json::json!({ "session_id": session_id }),
            )
            .await
    }

    pub async fn list_sessions(client: &GatewayClient) -> Result<Value, hermes_rpc::RpcError> {
        client.request("session.list", serde_json::json!({})).await
    }

    pub async fn history(
        client: &GatewayClient,
        session_id: &str,
    ) -> Result<Value, hermes_rpc::RpcError> {
        client
            .request(
                "session.history",
                serde_json::json!({ "session_id": session_id }),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_keeps_origin_and_swaps_scheme() {
        assert_eq!(HostConfig::local().ws_url(), "ws://127.0.0.1:9119/api/ws");

        let remote = HostConfig {
            base_url: "https://hermes.example.com".into(),
            ..HostConfig::local()
        };
        assert_eq!(
            remote.ws_url(),
            "wss://hermes.example.com/api/ws"
        );
    }

    #[test]
    fn trailing_slash_is_normalised() {
        let host = HostConfig {
            base_url: "http://hermes.local:9119/".into(),
            ..HostConfig::local()
        };
        assert_eq!(host.ws_url(), "ws://hermes.local:9119/api/ws");
    }

    #[test]
    fn profile_parses_dashboard_payload() {
        let payload = serde_json::json!({
            "name": "ops",
            "path": "/home/user/.hermes/profiles/ops",
            "is_default": false,
            "model": "claude-sonnet-4",
            "provider": "anthropic",
            "has_env": true,
            "skill_count": 42,
            "gateway_running": true,
            "description": "ops bot",
            "display_name": "Ops",
            "bot_title": "Fleet Ops",
            "has_alias": true,
            "unknown_future_field": 1
        });

        let profile: Profile = serde_json::from_value(payload).expect("profile parses");
        assert_eq!(profile.name, "ops");
        assert_eq!(profile.skill_count, 42);
        assert!(profile.gateway_running);
        assert!(profile.extra.contains_key("unknown_future_field"));
        assert_eq!(profile.label(), "Fleet Ops");
    }

    #[test]
    fn label_falls_back_to_name() {
        let profile = Profile {
            name: "default".into(),
            ..Default::default()
        };
        assert_eq!(profile.label(), "default");
    }

    #[test]
    fn ticket_is_appended_to_the_ws_url() {
        let host = HostConfig {
            base_url: "https://hermes.example.com".into(),
            cookie: Some("__Host-hermes_session_at=abc".into()),
            ticket: Some("t-1_2".into()),
            ..HostConfig::local()
        };
        let cfg = host.connection_config();
        assert_eq!(
            cfg.url,
            "wss://hermes.example.com/api/ws?ticket=t-1_2"
        );
        assert!(matches!(cfg.auth, Some(Auth::Cookie(_))));
    }

    #[test]
    fn percent_encode_escapes_query_unsafe_bytes() {
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn active_profile_reads_both_fields() {
        let active: ActiveProfile = serde_json::from_value(serde_json::json!({
            "active": "ops",
            "current": "default"
        }))
        .expect("active profile parses");
        assert_eq!(active.active, "ops");
        assert_eq!(active.current, "default");
    }
}
