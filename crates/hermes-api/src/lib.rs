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
        if let Ok(token) = std::env::var("HERMES_TOKEN")
            && !token.is_empty()
        {
            cfg.token = Some(token);
        }
        if let Ok(basic) = std::env::var("HERMES_BASIC")
            && let Some((user, pass)) = basic.split_once(':')
        {
            cfg.basic = Some((user.to_string(), pass.to_string()));
        }
        if let Ok(cookie) = std::env::var("HERMES_COOKIE")
            && !cookie.is_empty()
        {
            cfg.cookie = Some(cookie);
        }
        if let Ok(profile) = std::env::var("HERMES_PROFILE")
            && !profile.is_empty()
        {
            cfg.profile = Some(profile);
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
        } else if let Some(token) = &self.token {
            // Loopback Hermes validates WebSocket credentials in the query,
            // while REST uses the Authorization header.
            cfg.url = format!("{}?token={}", cfg.url, percent_encode(token));
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

/// Newest conversation summary a roster row carries (`profiles.list` →
/// `last_session`); the REST listing does not include it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastSession {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub last_active: i64,
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
    #[serde(default)]
    pub has_avatar: bool,
    /// Most recent conversation, when the roster came from `profiles.list`.
    #[serde(default)]
    pub last_session: Option<LastSession>,
    /// Client-agnostic UI metadata the host keeps in `profile.yaml` — the
    /// roster's `icon`, `accent` and anything else a client wants to share.
    #[serde(default)]
    pub ui_meta: Option<Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Profile {
    /// One rail line: what the bot last said, else what it is for.
    pub fn preview(&self) -> &str {
        if let Some(preview) = self
            .last_session
            .as_ref()
            .map(|session| session.preview.trim())
            .filter(|preview| !preview.is_empty())
        {
            return preview;
        }
        self.description.trim()
    }

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

/// `GET /api/mcp/catalog` — the curated optional-MCP manifests plus loader
/// diagnostics. Entries are already annotated with whether this profile has
/// installed and enabled them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCatalog {
    #[serde(default)]
    pub entries: Vec<McpCatalogEntry>,
    #[serde(default)]
    pub diagnostics: Vec<McpCatalogDiagnostic>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCatalogEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub auth_type: String,
    #[serde(default)]
    pub required_env: Vec<McpCatalogEnv>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub install_url: Option<String>,
    #[serde(default)]
    pub install_ref: Option<String>,
    #[serde(default)]
    pub bootstrap: Vec<String>,
    #[serde(default)]
    pub default_enabled: Option<Vec<String>>,
    #[serde(default)]
    pub post_install: String,
    #[serde(default)]
    pub needs_install: bool,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCatalogEnv {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCatalogDiagnostic {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

/// `GET /api/memory` — the active provider, discovered providers and the
/// built-in file sizes. Provider detail is kept raw beyond the stable fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStatus {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub providers: Vec<MemoryProvider>,
    #[serde(default)]
    pub builtin_files: MemoryFiles,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryProvider {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub configured: bool,
    /// `ready`, `needs_config`, `unavailable` or `missing`.
    #[serde(default)]
    pub status: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryFiles {
    #[serde(default)]
    pub memory: u64,
    #[serde(default)]
    pub user: u64,
}

/// `GET /api/memory/providers/{name}/config` — the provider's declared config
/// surface. Secrets are write-only: `value` is empty and `is_set` says whether
/// one already exists. `kind` may be `text`, `secret`, `boolean`, `integer`,
/// `number`, `select` or `json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryProviderConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub docs_url: String,
    #[serde(default)]
    pub fields: Vec<MemoryProviderField>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `GET /api/sessions/{id}/mutation-snapshot` — the revision fence a session
/// write has to carry. The owner store commits nothing unfenced: a client that
/// skips this gets `409 client_upgrade_required` on every edit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMutationSnapshot {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub runtime_revision: u64,
    #[serde(default)]
    pub runtime_generation: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryProviderField {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub is_set: bool,
    #[serde(default)]
    pub options: Vec<MemoryFieldOption>,
    #[serde(default)]
    pub url: String,
    /// Conditional visibility: every key/value pair must match the form.
    #[serde(default)]
    pub when: Option<Value>,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryFieldOption {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// One row of `GET /api/env`. Values arrive redacted; this client never asks
/// for the reveal endpoint, so plaintext secrets never enter the process.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EnvEntry {
    #[serde(default)]
    pub is_set: bool,
    #[serde(default)]
    pub redacted_value: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub provider_label: String,
    #[serde(default)]
    pub is_password: bool,
    /// A key the user added by hand rather than one from the catalog.
    #[serde(default)]
    pub custom: bool,
    /// Owned by a messaging-channel card; the dashboard hides those here too.
    #[serde(default)]
    pub channel_managed: bool,
}

/// One day of `/api/analytics/usage`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageDay {
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub api_calls: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub estimated_cost: f64,
}

/// One model's slice of the usage window.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageModel {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_calls: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub sessions: u64,
    #[serde(default)]
    pub estimated_cost: f64,
}

/// Window totals; the token fields are null on hosts that never recorded them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageTotals {
    #[serde(default)]
    pub total_sessions: u64,
    #[serde(default)]
    pub total_api_calls: Option<u64>,
    #[serde(default)]
    pub total_input: Option<u64>,
    #[serde(default)]
    pub total_output: Option<u64>,
    #[serde(default)]
    pub total_cache_read: Option<u64>,
    #[serde(default)]
    pub total_reasoning: Option<u64>,
    #[serde(default)]
    pub total_estimated_cost: f64,
    #[serde(default)]
    pub total_actual_cost: f64,
}

/// `GET /api/analytics/usage`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageAnalytics {
    #[serde(default)]
    pub period_days: u32,
    #[serde(default)]
    pub daily: Vec<UsageDay>,
    #[serde(default)]
    pub by_model: Vec<UsageModel>,
    #[serde(default)]
    pub totals: UsageTotals,
}

/// `GET /api/model/info` — the model this profile is running.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub effective_context_length: u64,
    #[serde(default)]
    pub auto_context_length: u64,
}

/// `GET /api/portal` — account tier and where billing is managed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PortalStatus {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub free_tier: bool,
    #[serde(default)]
    pub account_tier: Option<String>,
    #[serde(default)]
    pub portal_url: String,
    #[serde(default)]
    pub subscription_url: String,
}

/// One provider in `GET /api/model/options`, with its curated model ids.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelOptionProvider {
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_current: bool,
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub free_tier: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub featured_models: Vec<String>,
    #[serde(default)]
    pub total_models: u64,
}

/// One auxiliary task's pinned provider/model in `GET /api/model/auxiliary`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuxiliaryTask {
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuxiliaryMain {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

/// `GET /api/model/auxiliary` — per-task pins plus the main assignment.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuxiliaryModels {
    #[serde(default)]
    pub tasks: Vec<AuxiliaryTask>,
    #[serde(default)]
    pub main: Option<AuxiliaryMain>,
}

/// One provider/model slot in a MoA preset. Optional per-slot knobs are kept
/// so a round trip through the desktop never erases hand-set values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoaSlot {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn default_true() -> bool {
    true
}

/// One MoA preset exactly as `GET /api/model/moa` returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoaPreset {
    #[serde(default)]
    pub reference_models: Vec<MoaSlot>,
    #[serde(default)]
    pub aggregator: MoaSlot,
    #[serde(default)]
    pub reference_temperature: Option<f64>,
    #[serde(default)]
    pub aggregator_temperature: Option<f64>,
    #[serde(default)]
    pub reference_timeout: Option<f64>,
    #[serde(default = "default_loud")]
    pub degraded_reference_policy: String,
    #[serde(default = "default_moa_max_tokens")]
    pub max_tokens: u64,
    #[serde(default)]
    pub reference_max_tokens: Option<u64>,
    #[serde(default = "default_fanout")]
    pub fanout: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

fn default_loud() -> String {
    "loud".into()
}

fn default_moa_max_tokens() -> u64 {
    4096
}

fn default_fanout() -> String {
    "user_turn".into()
}

impl Default for MoaPreset {
    fn default() -> Self {
        Self {
            reference_models: Vec::new(),
            aggregator: MoaSlot::default(),
            reference_temperature: None,
            aggregator_temperature: None,
            reference_timeout: None,
            degraded_reference_policy: default_loud(),
            max_tokens: default_moa_max_tokens(),
            reference_max_tokens: None,
            fanout: default_fanout(),
            enabled: true,
            extra: serde_json::Map::new(),
        }
    }
}

/// `GET/PUT /api/model/moa`. The host also returns a flattened view of the
/// active preset; `extra` keeps those keys plus future host additions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoaConfig {
    #[serde(default)]
    pub default_preset: String,
    #[serde(default)]
    pub active_preset: String,
    #[serde(default)]
    pub presets: std::collections::BTreeMap<String, MoaPreset>,
    #[serde(default)]
    pub privacy_filter: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl MoaConfig {
    /// The preset the editor starts on: active, then default, then first.
    pub fn selected_preset(&self) -> Option<String> {
        if !self.active_preset.is_empty() && self.presets.contains_key(&self.active_preset) {
            return Some(self.active_preset.clone());
        }
        if !self.default_preset.is_empty() && self.presets.contains_key(&self.default_preset) {
            return Some(self.default_preset.clone());
        }
        self.presets.keys().next().cloned()
    }
}

/// One stored cron job from `GET /api/cron/jobs`. The host owns schedule
/// parsing and derives `state` from `enabled`; the UI only displays and acts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub schedule: String,
    #[serde(default)]
    pub schedule_display: String,
    #[serde(default)]
    pub deliver: String,
    #[serde(default)]
    pub state: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Default for CronJob {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            prompt: String::new(),
            schedule: String::new(),
            schedule_display: String::new(),
            deliver: String::new(),
            state: String::new(),
            enabled: true,
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronDeliveryTarget {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub home_target_set: bool,
    #[serde(default)]
    pub home_env_var: Option<String>,
}

/// One provider row from `GET /api/providers/oauth`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthProvider {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub cli_command: String,
    #[serde(default)]
    pub docs_url: String,
    #[serde(default)]
    pub disconnect_hint: Option<String>,
    #[serde(default)]
    pub disconnect_command: Option<String>,
    #[serde(default)]
    pub disconnectable: bool,
    #[serde(default)]
    pub status: OAuthStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthStatus {
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_label: String,
    #[serde(default)]
    pub token_preview: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub has_refresh_token: bool,
}

/// `POST /api/providers/oauth/{id}/start`. PKCE fills `auth_url`; device
/// flows fill `user_code`/`verification_url` and a poll interval.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OAuthStart {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub auth_url: String,
    #[serde(default)]
    pub user_code: String,
    #[serde(default)]
    pub verification_url: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub poll_interval: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OAuthPoll {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub expires_at: Option<f64>,
}

/// One installed skill from `GET /api/skills`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Skill {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub usage: u64,
    #[serde(default)]
    pub provenance: String,
}

/// One hit from `GET /api/skills/hub/search`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillHubResult {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub trust_level: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `GET /api/skills/hub/search`; the desktop only needs the result rows.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillHubSearch {
    #[serde(default)]
    pub results: Vec<SkillHubResult>,
}

/// `GET /api/skills/hub/preview` — the actual SKILL.md, before installing.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillHubPreview {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub trust_level: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub skill_md: String,
    #[serde(default)]
    pub files: Vec<String>,
}

/// One finding from the install-time security scan.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillHubFinding {
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub description: String,
}

/// `GET /api/skills/hub/scan` — the verdict the installer itself uses.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillHubScan {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub trust_level: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub policy: String,
    #[serde(default)]
    pub policy_reason: Option<String>,
    #[serde(default)]
    pub findings: Vec<SkillHubFinding>,
    #[serde(default)]
    pub severity_counts: std::collections::HashMap<String, u64>,
}

/// One row of `GET /api/sessions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub tool_call_count: u64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub profile_name: String,
    /// Working directory the session ran in; the desktop groups/filters on it.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub last_active: f64,
    #[serde(default)]
    pub started_at: f64,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanBoards {
    pub boards: Vec<KanbanBoardInfo>,
    pub current: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanBoardInfo {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanBoardResponse {
    pub board: KanbanBoardInfo,
}

fn kanban_scoped_path(path: &str, board: Option<&str>) -> String {
    match board.filter(|value| !value.is_empty()) {
        Some(board) => format!("{path}?board={}", percent_encode(board)),
        None => path.to_string(),
    }
}

/// Build the `multipart/form-data` body the plugin's upload route expects.
///
/// The boundary grows until it cannot occur in the payload, so a file that
/// contains the marker cannot end the body early. Returns the header value.
fn multipart_attachment(
    filename: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let mut boundary = String::from("----hermesgpui");
    while bytes
        .windows(boundary.len())
        .any(|window| window == boundary.as_bytes())
    {
        boundary.push('x');
    }
    let name = filename.replace(['"', '\r', '\n'], "_");
    let mut body = Vec::with_capacity(bytes.len() + 256);
    // The host defaults the author to "dashboard"; name the client so the
    // drawer shows where a file came from.
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"uploaded_by\"\r\n\r\nhermes-gpui\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
        )
        .as_bytes(),
    );
    if let Some(content_type) = content_type {
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Pull a filename out of a `Content-Disposition` header, preferring the
/// RFC 5987 `filename*` form the host uses for non-ASCII names.
fn disposition_filename(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').skip(1) {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename*=") {
            let encoded = rest.rsplit("''").next().unwrap_or(rest);
            return Some(percent_decode(encoded));
        }
        if let Some(rest) = part.strip_prefix("filename=") {
            plain = Some(rest.trim_matches('"').to_string());
        }
    }
    plain
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Ok(text) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(text, 16)
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A board returned by the optional Hermes Kanban plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct KanbanBoard {
    pub columns: Vec<KanbanColumn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanColumn {
    pub name: String,
    pub tasks: Vec<KanbanTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanTask {
    pub id: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    #[serde(default)]
    pub priority: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanTaskDetail {
    pub task: KanbanDetailedTask,
    #[serde(default)]
    pub comments: Vec<KanbanComment>,
    #[serde(default)]
    pub attachments: Vec<KanbanAttachment>,
}

/// One file stored by the plugin under the board's attachments root.
#[derive(Debug, Clone, Deserialize)]
pub struct KanbanAttachment {
    pub id: i64,
    pub filename: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub uploaded_by: Option<String>,
}

/// Bytes streamed back by `GET /attachments/{id}`.
#[derive(Debug, Clone)]
pub struct KanbanAttachmentFile {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanDetailedTask {
    pub id: String,
    pub title: String,
    pub status: String,
    pub body: Option<String>,
    pub assignee: Option<String>,
    pub latest_summary: Option<String>,
    pub result: Option<String>,
    #[serde(default)]
    pub priority: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KanbanComment {
    pub author: Option<String>,
    pub body: String,
}

/// State of a host-managed MCP OAuth authorization.
#[derive(Debug, Clone, Deserialize)]
pub struct McpOAuthFlow {
    pub flow_id: String,
    pub server_name: String,
    pub status: String,
    pub authorization_url: Option<String>,
    pub error: Option<String>,
}

/// `GET /api/sessions` — a page of history plus the unfiltered total.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionPage {
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub total: u64,
}

/// `GET /api/sessions/{id}/messages` — stored turns as opaque JSON, so a single
/// unfamiliar block cannot fail the page.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionMessages {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// `GET /api/model/options` — the active model plus every configured provider.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelOptions {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub providers: Vec<ModelOptionProvider>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NamedToggle {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Toolset {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tool_count: u32,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpToggle {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DetailModel {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub default: String,
}

/// `profiles.describe` — one profile's editable configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileDetail {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub model: DetailModel,
    #[serde(default)]
    pub skills: Vec<NamedToggle>,
    #[serde(default)]
    pub toolsets: Vec<Toolset>,
    #[serde(default)]
    pub mcp_servers: Vec<McpToggle>,
}

/// `profiles.get_asset` answer for a stored avatar.
#[derive(Debug, Clone, Default)]
pub struct AvatarAsset {
    pub mime: String,
    /// Base64 payload, no `data:` prefix.
    pub data: String,
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

impl ApiError {
    /// The server's own explanation for a failure, when the body carried one.
    /// Usage analytics reports `detail: {error, message}`; other routes use a
    /// plain string.
    pub fn detail(&self) -> Option<String> {
        let ApiError::Status { body, .. } = self else {
            return None;
        };
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        match value.get("detail")? {
            serde_json::Value::String(text) => Some(text.clone()),
            other => other
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }
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

    /// Request builder with this host's credentials applied.
    fn authed(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        } else if let Some(cookie) = &self.cookie {
            // A dashboard session wins over raw basic creds: the gated routers
            // only read the cookie, and basic-auth passwords are not forwarded.
            req = req.header("cookie", cookie.clone());
        } else if let Some((user, pass)) = &self.basic {
            req = req.basic_auth(user, Some(pass));
        }
        req
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let response = self
            .authed(reqwest::Method::GET, path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
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

    /// `GET /api/memory` — active provider, discovered providers and file sizes.
    pub async fn memory_status(&self) -> Result<MemoryStatus, ApiError> {
        self.get("/api/memory").await
    }

    /// `GET /api/memory/providers/{name}/config` — declared fields and values.
    pub async fn memory_config(&self, name: &str) -> Result<MemoryProviderConfig, ApiError> {
        self.get(&format!(
            "/api/memory/providers/{}/config",
            percent_encode(name)
        ))
        .await
    }

    /// `PUT /api/memory/providers/{name}/config` — saves fields and activates
    /// the provider once the host considers it ready.
    pub async fn set_memory_config(&self, name: &str, values: Value) -> Result<Value, ApiError> {
        let path = format!("/api/memory/providers/{}/config", percent_encode(name));
        let response = self
            .authed(reqwest::Method::PUT, &path)
            .json(&serde_json::json!({ "values": values }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `POST /api/memory/reset` — deletes the built-in memory files. `target`
    /// is `all`, `memory` or `user`.
    pub async fn reset_memory(&self, target: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/memory/reset";
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&serde_json::json!({ "target": target }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `PUT /api/memory/provider` — the host rejects providers that are not ready.
    pub async fn set_memory_provider(&self, provider: &str) -> Result<Value, ApiError> {
        let path = "/api/memory/provider";
        let response = self
            .authed(reqwest::Method::PUT, path)
            .json(&serde_json::json!({ "provider": provider }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.to_string(),
                source,
            })?;
        self.decode_response(path, response).await
    }

    pub async fn cron_jobs(&self) -> Result<Vec<CronJob>, ApiError> {
        self.get("/api/cron/jobs").await
    }

    pub async fn cron_delivery_targets(&self) -> Result<Vec<CronDeliveryTarget>, ApiError> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            targets: Vec<CronDeliveryTarget>,
        }
        Ok(self
            .get::<Response>("/api/cron/delivery-targets")
            .await?
            .targets)
    }

    /// `POST /api/cron/jobs` — raw body so future create fields pass through.
    pub async fn create_cron_job(&self, body: Value) -> Result<Value, ApiError> {
        const PATH: &str = "/api/cron/jobs";
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `pause`, `resume` or `trigger` one job.
    pub async fn cron_action(&self, id: &str, action: &str) -> Result<Value, ApiError> {
        let path = format!("/api/cron/jobs/{}/{action}", percent_encode(id));
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    pub async fn delete_cron_job(&self, id: &str) -> Result<Value, ApiError> {
        let path = format!("/api/cron/jobs/{}", percent_encode(id));
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `GET /api/cron/jobs/{id}/runs` — cron runs are ordinary sessions.
    pub async fn cron_runs(&self, id: &str) -> Result<Vec<SessionSummary>, ApiError> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            runs: Vec<SessionSummary>,
        }
        Ok(self
            .get::<Response>(&format!(
                "/api/cron/jobs/{}/runs?limit=20",
                percent_encode(id)
            ))
            .await?
            .runs)
    }

    pub async fn health(&self) -> Result<Value, ApiError> {
        self.get("/api/health").await
    }

    pub async fn status(&self) -> Result<Value, ApiError> {
        self.get("/api/status").await
    }

    /// `GET /api/env` — every catalogued key plus any arbitrary ones, with
    /// values redacted server-side. Sorted for a stable list.
    pub async fn env_vars(
        &self,
        profile: Option<&str>,
    ) -> Result<Vec<(String, EnvEntry)>, ApiError> {
        let path = match profile {
            Some(profile) if !profile.is_empty() => {
                format!("/api/env?profile={}", percent_encode(profile))
            }
            _ => "/api/env".to_string(),
        };
        let payload: std::collections::HashMap<String, EnvEntry> = self.get(&path).await?;
        let mut rows: Vec<(String, EnvEntry)> = payload.into_iter().collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }

    /// `GET /api/analytics/usage` — token and cost rollups for the window.
    pub async fn usage_analytics(&self, days: u32) -> Result<UsageAnalytics, ApiError> {
        let days = days.clamp(1, 365);
        self.get(&format!("/api/analytics/usage?days={days}")).await
    }

    /// `GET /api/model/info` — the model this profile is actually running.
    pub async fn model_info(&self) -> Result<ModelInfo, ApiError> {
        self.get("/api/model/info").await
    }

    /// `GET /api/portal` — account tier and where billing is managed.
    pub async fn portal(&self) -> Result<PortalStatus, ApiError> {
        self.get("/api/portal").await
    }

    /// `GET /api/model/options` — authenticated providers and their models.
    pub async fn model_options(&self) -> Result<ModelOptions, ApiError> {
        self.get("/api/model/options?explicit_only=1").await
    }

    /// `POST /api/mcp/servers` — add one server through the host's validated
    /// create path. The body is kept raw because `MCPServerCreate` grows
    /// transport-specific fields over time.
    pub async fn add_mcp_server(&self, body: Value) -> Result<Value, ApiError> {
        const PATH: &str = "/api/mcp/servers";
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `PUT /api/mcp/servers` — replace the whole map. This is the only host
    /// route that can remove nested keys, so edits go through it.
    pub async fn replace_mcp_servers(
        &self,
        servers: serde_json::Map<String, Value>,
    ) -> Result<Value, ApiError> {
        const PATH: &str = "/api/mcp/servers";
        let response = self
            .authed(reqwest::Method::PUT, PATH)
            .json(&serde_json::json!({ "servers": servers }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `DELETE /api/mcp/servers/{name}`.
    pub async fn delete_mcp_server(&self, name: &str) -> Result<Value, ApiError> {
        let path = format!("/api/mcp/servers/{}", percent_encode(name));
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// List boards without changing the host's active board.
    pub async fn kanban_boards(&self) -> Result<KanbanBoards, ApiError> {
        self.get("/api/plugins/kanban/boards").await
    }

    /// Create a board without switching the host's active board.
    pub async fn create_kanban_board(&self, slug: &str) -> Result<KanbanBoardResponse, ApiError> {
        let path = "/api/plugins/kanban/boards";
        let response = self
            .authed(reqwest::Method::POST, path)
            .json(&serde_json::json!({"slug": slug, "switch": false}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.into(),
                source,
            })?;
        let value = self.decode_response(path, response).await?;
        serde_json::from_value(value).map_err(|source| ApiError::Decode {
            path: path.into(),
            source,
        })
    }

    /// Rename a board without changing its immutable slug or other metadata.
    pub async fn rename_kanban_board(
        &self,
        slug: &str,
        name: &str,
    ) -> Result<KanbanBoardResponse, ApiError> {
        let path = format!("/api/plugins/kanban/boards/{}", percent_encode(slug));
        let response = self
            .authed(reqwest::Method::PATCH, &path)
            .json(&serde_json::json!({"name": name}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        let value = self.decode_response(&path, response).await?;
        serde_json::from_value(value).map_err(|source| ApiError::Decode { path, source })
    }

    /// Archive (default) or hard-delete a board. The host also prunes its own
    /// current-board pointer, so the caller only has to fix client selection.
    pub async fn delete_kanban_board(&self, slug: &str, hard: bool) -> Result<Value, ApiError> {
        let path = format!(
            "/api/plugins/kanban/boards/{}?delete={hard}",
            percent_encode(slug)
        );
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Read a board from the optional Kanban plugin.
    pub async fn kanban_board(&self, board: Option<&str>) -> Result<KanbanBoard, ApiError> {
        self.get(&kanban_scoped_path("/api/plugins/kanban/board", board))
            .await
    }

    /// Read task details and comments from the selected board.
    pub async fn kanban_task(
        &self,
        id: &str,
        board: Option<&str>,
    ) -> Result<KanbanTaskDetail, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/tasks/{}", percent_encode(id)),
            board,
        );
        self.get(&path).await
    }

    /// Add a comment to a task on the selected board.
    pub async fn comment_kanban_task(
        &self,
        id: &str,
        body: &str,
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/tasks/{}/comments", percent_encode(id)),
            board,
        );
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({"body": body}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Upload one file to a task. The plugin caps the size and sanitises the
    /// name; both failures come back as `ApiError::Status` with the reason.
    pub async fn upload_kanban_attachment(
        &self,
        id: &str,
        filename: &str,
        content_type: Option<&str>,
        bytes: &[u8],
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path(
            &format!(
                "/api/plugins/kanban/tasks/{}/attachments",
                percent_encode(id)
            ),
            board,
        );
        let (content_type_header, body) = multipart_attachment(filename, content_type, bytes);
        let response = self
            .authed(reqwest::Method::POST, &path)
            .header(reqwest::header::CONTENT_TYPE, content_type_header)
            .body(body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Download an attachment's bytes and the host's suggested filename.
    pub async fn download_kanban_attachment(
        &self,
        attachment_id: i64,
        board: Option<&str>,
    ) -> Result<KanbanAttachmentFile, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/attachments/{attachment_id}"),
            board,
        );
        let response = self
            .authed(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());
        let filename = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(disposition_filename);
        let bytes = response
            .bytes()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                path,
                body: String::from_utf8_lossy(&bytes).chars().take(400).collect(),
            });
        }
        Ok(KanbanAttachmentFile {
            filename,
            content_type,
            bytes: bytes.to_vec(),
        })
    }

    /// Remove an attachment record and its stored blob.
    pub async fn delete_kanban_attachment(
        &self,
        attachment_id: i64,
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/attachments/{attachment_id}"),
            board,
        );
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Create a task in triage, without assigning or dispatching a worker.
    pub async fn create_kanban_task(
        &self,
        title: &str,
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path("/api/plugins/kanban/tasks", board);
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({ "title": title, "triage": true }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Update task fields using the plugin's validation.
    pub async fn update_kanban_task(
        &self,
        id: &str,
        patch: Value,
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/tasks/{}", percent_encode(id)),
            board,
        );
        let response = self
            .authed(reqwest::Method::PATCH, &path)
            .json(&patch)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Delete a task from the selected board.
    pub async fn delete_kanban_task(
        &self,
        id: &str,
        board: Option<&str>,
    ) -> Result<Value, ApiError> {
        let path = kanban_scoped_path(
            &format!("/api/plugins/kanban/tasks/{}", percent_encode(id)),
            board,
        );
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// Start host-managed MCP OAuth in the selected profile.
    pub async fn start_mcp_oauth(
        &self,
        name: &str,
        profile: Option<&str>,
    ) -> Result<McpOAuthFlow, ApiError> {
        let mut path = format!("/api/mcp/servers/{}/auth", percent_encode(name));
        if let Some(profile) = profile.filter(|profile| !profile.is_empty()) {
            path.push_str(&format!("?profile={}", percent_encode(profile)));
        }
        let response = self
            .authed(reqwest::Method::POST, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        let value = self.decode_response(&path, response).await?;
        serde_json::from_value(value).map_err(|source| ApiError::Decode { path, source })
    }

    pub async fn mcp_oauth_status(&self, id: &str) -> Result<McpOAuthFlow, ApiError> {
        self.get(&format!("/api/mcp/oauth/flows/{}", percent_encode(id)))
            .await
    }

    pub async fn cancel_mcp_oauth(&self, id: &str) -> Result<Value, ApiError> {
        let path = format!("/api/mcp/oauth/flows/{}", percent_encode(id));
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `GET /api/mcp/catalog` — curated entries with install/enabled state.
    pub async fn mcp_catalog(&self) -> Result<McpCatalog, ApiError> {
        self.get("/api/mcp/catalog").await
    }

    /// `POST /api/mcp/catalog/install` — write config and required env vars.
    /// Git-bootstrap entries return `background: true` and finish host-side.
    pub async fn install_mcp_catalog(
        &self,
        name: &str,
        env: Value,
        enable: bool,
    ) -> Result<Value, ApiError> {
        const PATH: &str = "/api/mcp/catalog/install";
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&serde_json::json!({ "name": name, "env": env, "enable": enable }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `GET /api/config` — needed to inspect one `mcp_servers` entry before an
    /// edit. The dashboard may wrap the map under `config`, so callers should
    /// use [`Self::mcp_config_entry`].
    pub async fn raw_config(&self) -> Result<Value, ApiError> {
        self.get("/api/config").await
    }

    /// Read the full `mcp_servers` map from a raw config response, accepting
    /// both the unwrapped dashboard shape and the older wrapped one.
    pub fn mcp_config_entry(config: &Value, name: &str) -> Option<Value> {
        let root = config.get("config").unwrap_or(config);
        root.get("mcp_servers")?.get(name).cloned()
    }

    /// `PUT /api/mcp/servers/{name}/enabled`.
    pub async fn set_mcp_enabled(&self, name: &str, enabled: bool) -> Result<Value, ApiError> {
        let path = format!("/api/mcp/servers/{name}/enabled");
        let response = self
            .authed(reqwest::Method::PUT, &path)
            .json(&serde_json::json!({ "enabled": enabled }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `POST /api/mcp/servers/{name}/test` — asks the host to reach the server.
    pub async fn test_mcp(&self, name: &str) -> Result<Value, ApiError> {
        let path = format!("/api/mcp/servers/{name}/test");
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `GET /api/skills` — every installed skill with its enabled state.
    pub async fn skills(&self) -> Result<Vec<Skill>, ApiError> {
        self.get("/api/skills").await
    }

    /// `PUT /api/skills/toggle` — enable or disable one skill.
    pub async fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<Value, ApiError> {
        const PATH: &str = "/api/skills/toggle";
        let response = self
            .authed(reqwest::Method::PUT, PATH)
            .json(&serde_json::json!({ "name": name, "enabled": enabled }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `GET /api/skills/hub/search` — network-bound search across hub sources.
    pub async fn skill_hub_search(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<SkillHubSearch, ApiError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SkillHubSearch::default());
        }
        let limit = limit.clamp(1, 50);
        self.get(&format!(
            "/api/skills/hub/search?q={}&source=all&limit={limit}",
            percent_encode(query)
        ))
        .await
    }

    /// `GET /api/skills/hub/preview` — SKILL.md and manifest, no install.
    pub async fn skill_hub_preview(&self, identifier: &str) -> Result<SkillHubPreview, ApiError> {
        self.get(&format!(
            "/api/skills/hub/preview?identifier={}",
            percent_encode(identifier.trim())
        ))
        .await
    }

    /// `GET /api/skills/hub/scan` — the installer's security verdict.
    pub async fn skill_hub_scan(&self, identifier: &str) -> Result<SkillHubScan, ApiError> {
        self.get(&format!(
            "/api/skills/hub/scan?identifier={}",
            percent_encode(identifier.trim())
        ))
        .await
    }

    /// `POST /api/skills/hub/install` — spawns the host-side install action.
    pub async fn install_skill_hub(&self, identifier: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/skills/hub/install";
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&serde_json::json!({ "identifier": identifier }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `GET /api/sessions` — newest conversations, excluding archived rows.
    /// `offset` pages past the first window instead of stopping at the cap.
    pub async fn sessions(&self, limit: u32, offset: u32) -> Result<SessionPage, ApiError> {
        let limit = limit.clamp(1, 200);
        self.get(&format!(
            "/api/sessions?limit={limit}&offset={offset}&min_messages=1&archived=exclude&order=recent"
        ))
        .await
    }

    /// `GET /api/sessions/{id}/messages` — the stored transcript.
    pub async fn session_messages(&self, id: &str) -> Result<SessionMessages, ApiError> {
        self.get(&format!(
            "/api/sessions/{id}/messages?limit=200&order=latest&include_compacted=true"
        ))
        .await
    }

    /// Request an image from the host's restricted media endpoint.
    pub async fn media_data_url(&self, path: &str) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Media {
            data_url: String,
        }
        Ok(self
            .get::<Media>(&format!("/api/media?path={}", percent_encode(path)))
            .await?
            .data_url)
    }

    /// Export a complete session as JSON using the host's profile-aware route.
    pub async fn export_session(&self, id: &str, profile: Option<&str>) -> Result<Value, ApiError> {
        let mut path = format!("/api/sessions/{}/export", percent_encode(id));
        if let Some(profile) = profile.filter(|profile| !profile.is_empty()) {
            path.push_str(&format!("?profile={}", percent_encode(profile)));
        }
        self.get(&path).await
    }

    /// `PATCH /api/sessions/{id}` — rename, pin, archive or mark read.
    /// The fence is merged into `patch`, which keeps its own fields.
    pub async fn update_session(&self, id: &str, patch: Value) -> Result<Value, ApiError> {
        let path = format!("/api/sessions/{id}");
        let challenge = self.session_mutation_snapshot(id).await?;
        let mut body = patch;
        if let Some(fields) = body.as_object_mut() {
            fields.insert("request_id".into(), Value::String(new_request_id()));
            fields.insert(
                "expected_revision".into(),
                challenge.runtime_revision.into(),
            );
            if let Some(generation) = challenge.runtime_generation {
                fields.insert("expected_generation".into(), generation.into());
            }
        }
        let response = self
            .authed(reqwest::Method::PATCH, &path)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `GET /api/sessions/{id}/mutation-snapshot` — the fence for one session.
    pub async fn session_mutation_snapshot(
        &self,
        id: &str,
    ) -> Result<SessionMutationSnapshot, ApiError> {
        self.get(&format!(
            "/api/sessions/{}/mutation-snapshot",
            percent_encode(id)
        ))
        .await
    }

    /// `GET /api/providers/oauth` — every OAuth-capable provider and status.
    pub async fn oauth_providers(&self) -> Result<Vec<OAuthProvider>, ApiError> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            providers: Vec<OAuthProvider>,
        }
        Ok(self
            .get::<Response>("/api/providers/oauth")
            .await?
            .providers)
    }

    /// Start a PKCE or device-code login. The host keeps the flow state.
    pub async fn start_oauth(&self, provider: &str) -> Result<OAuthStart, ApiError> {
        let path = format!("/api/providers/oauth/{}/start", percent_encode(provider));
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        let value = self.decode_response(&path, response).await?;
        serde_json::from_value(value).map_err(|source| ApiError::Decode { path, source })
    }

    /// Submit the pasted authorization code for a PKCE flow.
    pub async fn submit_oauth_code(
        &self,
        provider: &str,
        session_id: &str,
        code: &str,
    ) -> Result<Value, ApiError> {
        let path = format!("/api/providers/oauth/{}/submit", percent_encode(provider));
        let response = self
            .authed(reqwest::Method::POST, &path)
            .json(&serde_json::json!({ "session_id": session_id, "code": code }))
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    pub async fn poll_oauth(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<OAuthPoll, ApiError> {
        let path = format!(
            "/api/providers/oauth/{}/poll/{}",
            percent_encode(provider),
            percent_encode(session_id)
        );
        self.get(&path).await
    }

    /// `DELETE /api/providers/oauth/{id}` — removes Hermes-managed creds only.
    pub async fn disconnect_oauth(&self, provider: &str) -> Result<Value, ApiError> {
        let path = format!("/api/providers/oauth/{}", percent_encode(provider));
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `DELETE /api/sessions/{id}` — removes one stored conversation.
    /// Fenced with the snapshot revision, like every other session write.
    pub async fn delete_session(&self, id: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/sessions";
        let challenge = self.session_mutation_snapshot(id).await?;
        let mut path = format!(
            "{PATH}/{}?request_id={}&expected_revision={}",
            percent_encode(id),
            percent_encode(&new_request_id()),
            challenge.runtime_revision
        );
        if let Some(generation) = challenge.runtime_generation {
            path.push_str(&format!("&expected_generation={generation}"));
        }
        let response = self
            .authed(reqwest::Method::DELETE, &path)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: path.clone(),
                source,
            })?;
        self.decode_response(&path, response).await
    }

    /// `GET /api/model/auxiliary` — the helper-task assignments.
    pub async fn auxiliary_models(&self) -> Result<AuxiliaryModels, ApiError> {
        self.get("/api/model/auxiliary").await
    }

    /// `POST /api/model/set` with `scope: auxiliary` — pins one task.
    pub async fn set_auxiliary_model(
        &self,
        task: &str,
        provider: &str,
        model: &str,
    ) -> Result<Value, ApiError> {
        const PATH: &str = "/api/model/set";
        let body = serde_json::json!({
            "scope": "auxiliary",
            "task": task,
            "provider": provider,
            "model": model,
        });
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `POST /api/model/set` with the reset sentinel: every task follows main.
    pub async fn reset_auxiliary_models(&self) -> Result<Value, ApiError> {
        const PATH: &str = "/api/model/set";
        let body = serde_json::json!({ "scope": "auxiliary", "task": "__reset__" });
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `GET /api/config` — the raw config, to read `agent.reasoning_effort`.
    pub async fn reasoning_effort(&self) -> Result<String, ApiError> {
        let config: Value = self.get("/api/config").await?;
        let root = config.get("config").unwrap_or(&config);
        Ok(root
            .pointer("/agent/reasoning_effort")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// `PUT /api/config` — deep-merges, so a partial `agent` block is enough.
    pub async fn set_reasoning_effort(&self, level: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/config";
        let body = serde_json::json!({
            "config": { "agent": { "reasoning_effort": level } }
        });
        let response = self
            .authed(reqwest::Method::PUT, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `GET /api/model/moa` — named Mixture-of-Agents presets.
    pub async fn moa_config(&self) -> Result<MoaConfig, ApiError> {
        self.get("/api/model/moa").await
    }

    /// `PUT /api/model/moa` — saves the whole preset map. The host rejects
    /// half-filled slots with 422 rather than silently repairing them.
    pub async fn set_moa_config(&self, config: &MoaConfig) -> Result<MoaConfig, ApiError> {
        const PATH: &str = "/api/model/moa";
        let response = self
            .authed(reqwest::Method::PUT, PATH)
            .json(config)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        let value = self.decode_response(PATH, response).await?;
        serde_json::from_value(value).map_err(|source| ApiError::Decode {
            path: PATH.to_string(),
            source,
        })
    }

    /// `POST /api/model/set` — writes the main model for *new* sessions.
    pub async fn set_model(&self, provider: &str, model: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/model/set";
        let body = serde_json::json!({ "scope": "main", "provider": provider, "model": model });
        let response = self
            .authed(reqwest::Method::POST, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `PUT /api/env` — writes `.env` and reconciles config.yaml mirrors.
    pub async fn set_env(
        &self,
        profile: Option<&str>,
        key: &str,
        value: &str,
    ) -> Result<Value, ApiError> {
        const PATH: &str = "/api/env";
        let body = serde_json::json!({ "key": key, "value": value, "profile": profile });
        let response = self
            .authed(reqwest::Method::PUT, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    /// `DELETE /api/env` — clears the key and every mirror of that credential.
    pub async fn delete_env(&self, profile: Option<&str>, key: &str) -> Result<Value, ApiError> {
        const PATH: &str = "/api/env";
        let body = serde_json::json!({ "key": key, "profile": profile });
        let response = self
            .authed(reqwest::Method::DELETE, PATH)
            .json(&body)
            .send()
            .await
            .map_err(|source| ApiError::Transport {
                path: PATH.to_string(),
                source,
            })?;
        self.decode_response(PATH, response).await
    }

    async fn decode_response(
        &self,
        path: &str,
        response: reqwest::Response,
    ) -> Result<Value, ApiError> {
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
        Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    /// Name of the password provider advertised by `GET /api/auth/providers`,
    /// e.g. `basic` for `HERMES_DASHBOARD_BASIC_AUTH_*` deployments.
    async fn password_provider(&self) -> Option<String> {
        let payload: Value = self.get("/api/auth/providers").await.ok()?;
        for provider in payload.get("providers")?.as_array()? {
            if provider.get("supports_password").and_then(Value::as_bool) == Some(true)
                && let Some(name) = provider.get("name").and_then(Value::as_str)
            {
                return Some(name.to_string());
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
/// Idempotency id for one session mutation. The owner replays the receipt of a
/// repeated `request_id`, so it must be fresh per attempt.
fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

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
///    /// Failures come back as human-readable notes rather than errors so a caller
/// can still attempt the connection (loopback hosts may require a token).
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

    /// `session.resume` answer. The runtime `session_id` is the id prompts must
    /// target; `resumed` is the durable transcript id that was reopened.
    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct ResumedSession {
        #[serde(default)]
        pub session_id: String,
        #[serde(default)]
        pub resumed: String,
        #[serde(default)]
        pub messages: Vec<Value>,
        #[serde(default)]
        pub message_count: u64,
        #[serde(default)]
        pub running: bool,
        #[serde(default)]
        pub info: Value,
    }

    /// Reopen a stored conversation and bind it to a live gateway runtime.
    /// The host hydrates the transcript and lazily builds the agent, so this is
    /// safe to call from the UI thread's async worker.
    pub async fn resume_session(
        client: &GatewayClient,
        id: &str,
        profile: Option<&str>,
    ) -> Result<ResumedSession, hermes_rpc::RpcError> {
        let mut params = serde_json::Map::new();
        params.insert("session_id".into(), Value::String(id.to_string()));
        params.insert("cols".into(), Value::from(120));
        params.insert("omit_messages".into(), Value::Bool(false));
        if let Some(profile) = profile {
            params.insert("profile".into(), Value::String(profile.to_string()));
        }
        client
            .request("session.resume", Value::Object(params))
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

    /// `profiles.list` — the roster with per-bot previews, `ui_meta` and
    /// `has_avatar`, none of which the REST listing carries.
    pub async fn list_profiles(
        client: &GatewayClient,
    ) -> Result<Vec<Profile>, hermes_rpc::RpcError> {
        #[derive(Deserialize)]
        struct Answer {
            #[serde(default)]
            profiles: Vec<Profile>,
        }
        let answer: Answer = client
            .request(
                "profiles.list",
                serde_json::json!({ "include_sessions": true }),
            )
            .await?;
        Ok(answer.profiles)
    }

    /// `profiles.describe` — soul, model, skills, toolsets and MCP servers.
    pub async fn describe_profile(
        client: &GatewayClient,
        name: &str,
    ) -> Result<ProfileDetail, hermes_rpc::RpcError> {
        client
            .request("profiles.describe", serde_json::json!({ "name": name }))
            .await
    }

    /// `profiles.configure` — every section is best-effort server-side; the
    /// result reports what was applied.
    pub async fn configure_profile(
        client: &GatewayClient,
        name: &str,
        mut patch: Value,
    ) -> Result<Value, hermes_rpc::RpcError> {
        if let Value::Object(map) = &mut patch {
            map.insert("name".into(), Value::String(name.to_string()));
        }
        client.request("profiles.configure", patch).await
    }

    /// `profiles.get_asset` — the stored avatar, when the profile has one.
    pub async fn profile_asset(
        client: &GatewayClient,
        name: &str,
    ) -> Result<Option<AvatarAsset>, hermes_rpc::RpcError> {
        let answer: Value = client
            .request(
                "profiles.get_asset",
                serde_json::json!({ "name": name, "asset": "avatar" }),
            )
            .await?;
        if answer.get("found").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let Some(data) = answer.get("data").and_then(Value::as_str) else {
            return Ok(None);
        };
        let mime = answer
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string();
        // The gateway hands back a data URL; the caller wants the payload.
        let payload = data
            .split_once(";base64,")
            .map(|(_, payload)| payload)
            .unwrap_or(data)
            .to_string();
        Ok(Some(AvatarAsset {
            mime,
            data: payload,
        }))
    }

    /// `profiles.set_asset` — store `data_url` as this profile's avatar.
    pub async fn set_profile_avatar(
        client: &GatewayClient,
        name: &str,
        data_url: &str,
    ) -> Result<Value, hermes_rpc::RpcError> {
        client
            .request(
                "profiles.set_asset",
                serde_json::json!({ "name": name, "asset": "avatar", "data": data_url }),
            )
            .await
    }

    /// `profiles.set_asset` with `clear` — removes `assets/avatar.*`.
    pub async fn clear_profile_avatar(
        client: &GatewayClient,
        name: &str,
    ) -> Result<Value, hermes_rpc::RpcError> {
        client
            .request(
                "profiles.set_asset",
                serde_json::json!({ "name": name, "asset": "avatar", "clear": true }),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kanban_and_mcp_oauth_use_authenticated_plugin_routes() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let replies = [
                r#"{"columns":[{"name":"todo","tasks":[{"id":"t/1","title":"Test","status":"todo","assignee":null}]}]}"#,
                r#"{"ok":true}"#,
                r#"{"task":{"id":"new"}}"#,
                r#"{"board":{"slug":"work","name":"Work"},"current":"default"}"#,
                r#"{"board":{"slug":"work","name":"Renamed"}}"#,
                r#"{"task":{"id":"t/1","title":"Test","status":"todo","body":"Detail","assignee":null,"latest_summary":null,"result":null},"comments":[{"author":"dashboard","body":"Hello"}]}"#,
                r#"{"ok":true}"#,
                r#"{"flow_id":"f/1","server_name":"demo","status":"authorization_required","authorization_url":"https://example.com/auth","error":null}"#,
                r#"{"flow_id":"f/1","server_name":"demo","status":"approved","authorization_url":null,"error":null}"#,
                r#"{"ok":true}"#,
            ];
            let mut requests = Vec::new();
            for reply in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    data.extend_from_slice(&buffer[..count]);
                    if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(data).unwrap());
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            }
            requests
        });
        let host = HostConfig {
            base_url: format!("http://{address}"),
            token: Some("test-token".into()),
            ..HostConfig::local()
        };
        let client = RestClient::new(&host);
        let board = client.kanban_board(Some("work & ops")).await.unwrap();
        assert_eq!(board.columns[0].tasks[0].id, "t/1");
        client
            .update_kanban_task(
                "t/1",
                serde_json::json!({"status":"ready"}),
                Some("work & ops"),
            )
            .await
            .unwrap();
        client
            .create_kanban_task("New task", Some("work & ops"))
            .await
            .unwrap();
        let created = client.create_kanban_board("work").await.unwrap();
        assert_eq!(created.board.slug, "work");
        let renamed = client
            .rename_kanban_board("work/name", "Renamed")
            .await
            .unwrap();
        assert_eq!(renamed.board.name, "Renamed");
        let detail = client.kanban_task("t/1", Some("work & ops")).await.unwrap();
        assert_eq!(detail.task.body.as_deref(), Some("Detail"));
        assert_eq!(detail.comments[0].body, "Hello");
        client
            .comment_kanban_task("t/1", "A comment", Some("work & ops"))
            .await
            .unwrap();
        let flow = client
            .start_mcp_oauth("demo/name", Some("work profile"))
            .await
            .unwrap();
        assert_eq!(flow.flow_id, "f/1");
        assert_eq!(
            client.mcp_oauth_status(&flow.flow_id).await.unwrap().status,
            "approved"
        );
        client.cancel_mcp_oauth(&flow.flow_id).await.unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /api/plugins/kanban/board?board=work%20%26%20ops "));
        assert!(
            requests[1]
                .starts_with("PATCH /api/plugins/kanban/tasks/t%2F1?board=work%20%26%20ops ")
        );
        assert!(requests[1].ends_with(r#"{"status":"ready"}"#));
        assert!(
            requests[7]
                .starts_with("POST /api/mcp/servers/demo%2Fname/auth?profile=work%20profile ")
        );
        assert!(requests[2].starts_with("POST /api/plugins/kanban/tasks?board=work%20%26%20ops "));
        assert!(requests[2].ends_with(r#"{"title":"New task","triage":true}"#));
        assert!(requests[3].starts_with("POST /api/plugins/kanban/boards "));
        let create_body = requests[3].split("\r\n\r\n").nth(1).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(create_body).unwrap(),
            serde_json::json!({"slug":"work","switch":false})
        );
        assert!(requests[4].starts_with("PATCH /api/plugins/kanban/boards/work%2Fname "));
        assert!(requests[4].ends_with(r#"{"name":"Renamed"}"#));
        assert!(
            requests[5].starts_with("GET /api/plugins/kanban/tasks/t%2F1?board=work%20%26%20ops ")
        );
        assert!(
            requests[6].starts_with(
                "POST /api/plugins/kanban/tasks/t%2F1/comments?board=work%20%26%20ops "
            )
        );
        assert!(requests[6].ends_with(r#"{"body":"A comment"}"#));
        assert!(requests[8].starts_with("GET /api/mcp/oauth/flows/f%2F1 "));
        assert!(requests[9].starts_with("DELETE /api/mcp/oauth/flows/f%2F1 "));
        assert!(requests.iter().all(|request| {
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        }));
    }

    #[tokio::test]
    async fn kanban_attachments_upload_download_and_delete() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let replies: [(&str, &str, &str); 3] = [
                (
                    "application/json",
                    "",
                    r#"{"attachment":{"id":7,"filename":"note.txt","content_type":"text/plain","size":5,"uploaded_by":"hermes-gpui"}}"#,
                ),
                (
                    "text/plain; charset=utf-8",
                    "Content-Disposition: attachment; filename=\"note.txt\"\r\n",
                    "hello",
                ),
                ("application/json", "", r#"{"ok":true,"id":7}"#),
            ];
            let mut requests = Vec::new();
            for (content_type, extra_headers, reply) in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    data.extend_from_slice(&buffer[..count]);
                    if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&data).into_owned());
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
            requests
        });
        let host = HostConfig {
            base_url: format!("http://{address}"),
            token: Some("test-token".into()),
            ..HostConfig::local()
        };
        let client = RestClient::new(&host);
        let uploaded = client
            .upload_kanban_attachment(
                "t/1",
                "note.txt",
                Some("text/plain"),
                b"hello",
                Some("work & ops"),
            )
            .await
            .unwrap();
        assert_eq!(uploaded["attachment"]["id"], 7);
        let file = client
            .download_kanban_attachment(7, Some("work & ops"))
            .await
            .unwrap();
        assert_eq!(file.bytes, b"hello");
        assert_eq!(file.filename.as_deref(), Some("note.txt"));
        assert_eq!(file.content_type.as_deref(), Some("text/plain"));
        client
            .delete_kanban_attachment(7, Some("work & ops"))
            .await
            .unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with(
            "POST /api/plugins/kanban/tasks/t%2F1/attachments?board=work%20%26%20ops "
        ));
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("content-type: multipart/form-data; boundary=----hermesgpui\r\n")
        );
        assert!(requests[0].contains("name=\"uploaded_by\"\r\n\r\nhermes-gpui\r\n"));
        assert!(requests[0].contains("name=\"file\"; filename=\"note.txt\"\r\n"));
        assert!(requests[0].contains("Content-Type: text/plain\r\n"));
        assert!(requests[0].ends_with("\r\nhello\r\n------hermesgpui--\r\n"));
        assert!(
            requests[1]
                .starts_with("GET /api/plugins/kanban/attachments/7?board=work%20%26%20ops ")
        );
        assert!(
            requests[2]
                .starts_with("DELETE /api/plugins/kanban/attachments/7?board=work%20%26%20ops ")
        );
    }

    /// Every session write is fenced: the client reads the mutation snapshot
    /// first and forwards its revision, because the owner store rejects
    /// unfenced mutations with `409 client_upgrade_required`.
    #[tokio::test]
    async fn session_mutations_carry_the_revision_fence() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let replies = [
                r#"{"session_id":"abc","exists":true,"runtime_revision":7,"runtime_generation":3}"#,
                r#"{"ok":true}"#,
                r#"{"session_id":"abc","exists":true,"runtime_revision":9,"runtime_generation":4}"#,
                r#"{"ok":true,"revision":10}"#,
            ];
            let mut requests = Vec::new();
            for reply in replies {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    data.extend_from_slice(&buffer[..count]);
                    if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&data).into_owned());
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
            requests
        });
        let host = HostConfig {
            base_url: format!("http://{address}"),
            token: Some("test-token".into()),
            ..HostConfig::local()
        };
        let client = RestClient::new(&host);

        client.delete_session("abc").await.unwrap();
        client
            .update_session("abc", serde_json::json!({ "title": "renamed" }))
            .await
            .unwrap();

        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /api/sessions/abc/mutation-snapshot "));
        assert!(
            requests[1].starts_with("DELETE /api/sessions/abc?request_id="),
            "unfenced delete: {}",
            requests[1]
        );
        assert!(requests[1].contains("&expected_revision=7&expected_generation=3 "));
        assert!(requests[2].starts_with("GET /api/sessions/abc/mutation-snapshot "));
        assert!(requests[3].starts_with("PATCH /api/sessions/abc "));
        for key in [
            r#""title":"renamed""#,
            r#""request_id":""#,
            r#""expected_revision":9"#,
            r#""expected_generation":4"#,
        ] {
            assert!(requests[3].contains(key), "missing {key}: {}", requests[3]);
        }
    }

    /// One real turn through the client stack: authenticate, create a session,
    /// submit a prompt and wait for the streamed reply, then delete that
    /// session. It never touches an existing conversation.
    ///
    /// `HERMES_GPUI_LIVE_HOST=http://host:9119 HERMES_GPUI_LIVE_BASIC=user:pass \
    ///  cargo test -p hermes-api -- --ignored --nocapture live_prompt_turn`
    #[tokio::test]
    #[ignore = "requires a reachable Hermes host via HERMES_GPUI_LIVE_HOST"]
    async fn live_prompt_turn_streams_a_reply() {
        let Ok(base) = std::env::var("HERMES_GPUI_LIVE_HOST") else {
            return;
        };
        let mut host = HostConfig {
            base_url: base,
            token: std::env::var("HERMES_GPUI_LIVE_TOKEN").ok(),
            ..HostConfig::local()
        };
        if let Ok(basic) = std::env::var("HERMES_GPUI_LIVE_BASIC")
            && let Some((user, pass)) = basic.split_once(':')
        {
            host.basic = Some((user.to_string(), pass.to_string()));
        }
        host.profile = std::env::var("HERMES_GPUI_LIVE_PROFILE")
            .ok()
            .filter(|profile| !profile.is_empty());

        let (host, notes) = authenticate(&host).await;
        assert!(notes.is_empty(), "auth notes: {notes:?}");
        assert!(
            host.cookie.is_some() || host.token.is_some(),
            "no credential was acquired"
        );

        let client = GatewayClient::connect(host.connection_config())
            .await
            .expect("gateway ws");
        let mut events = client.subscribe();

        let created = gateway::create_session(&client, host.profile.as_deref())
            .await
            .expect("session.create");
        assert!(!created.session_id.is_empty(), "empty session id");

        gateway::submit_prompt(&client, &created.session_id, "Reply with exactly: pong")
            .await
            .expect("prompt.submit");

        let mut reply = String::new();
        let mut tools = 0_u32;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "turn never completed; reply so far: {reply}"
            );
            let Ok(event) = tokio::time::timeout(remaining, events.recv()).await else {
                panic!("turn never completed; reply so far: {reply}");
            };
            let Ok(event) = event else { continue };
            if event
                .session_id
                .as_deref()
                .is_some_and(|id| id != created.session_id)
            {
                continue; // another session's traffic on the shared socket
            }
            if let Some(text) = event.text_delta() {
                reply.push_str(text);
            }
            match event.kind.as_str() {
                "tool.start" => tools += 1,
                "message.complete" => break,
                "error" => panic!("gateway error: {}", event.payload),
                _ => {}
            }
        }
        assert!(
            !reply.trim().is_empty(),
            "the turn completed without any assistant text"
        );
        println!("reply: {reply:?} ({tools} tool start(s))");

        // Leave no trace: this session existed only for the check.
        let rest = RestClient::new(&host);
        let id = created
            .stored_session_id
            .clone()
            .unwrap_or_else(|| created.session_id.clone());
        rest.delete_session(&id).await.expect("session cleanup");
        client.close();
    }

    /// Exercises the real plugin routes end to end. Point it at a *disposable*
    /// host: it creates a board, a task, a comment and an attachment.
    /// `HERMES_GPUI_LIVE_HOST=http://127.0.0.1:PORT HERMES_GPUI_LIVE_TOKEN=... \
    ///  cargo test -p hermes-api -- --ignored live_kanban`
    #[tokio::test]
    #[ignore = "requires a disposable Hermes host via HERMES_GPUI_LIVE_HOST/TOKEN"]
    async fn live_kanban_task_detail_comments_and_attachments() {
        let Ok(base) = std::env::var("HERMES_GPUI_LIVE_HOST") else {
            return;
        };
        let host = HostConfig {
            base_url: base,
            token: std::env::var("HERMES_GPUI_LIVE_TOKEN").ok(),
            ..HostConfig::local()
        };
        let client = RestClient::new(&host);
        let slug = format!("gpui-live-{}", std::process::id());
        client.create_kanban_board(&slug).await.unwrap();
        let created = client
            .create_kanban_task("Live verification", Some(&slug))
            .await
            .unwrap();
        let id = created["task"]["id"].as_str().unwrap().to_string();

        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert_eq!(detail.task.status, "triage");
        assert!(detail.comments.is_empty());
        assert!(detail.attachments.is_empty());

        client
            .update_kanban_task(
                &id,
                serde_json::json!({"body": "Live description", "assignee": "ops"}),
                Some(&slug),
            )
            .await
            .unwrap();
        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert_eq!(detail.task.body.as_deref(), Some("Live description"));
        assert_eq!(detail.task.assignee.as_deref(), Some("ops"));

        client
            .update_kanban_task(&id, serde_json::json!({"priority": 3}), Some(&slug))
            .await
            .unwrap();
        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert_eq!(detail.task.priority, 3);

        client
            .comment_kanban_task(&id, "Live comment", Some(&slug))
            .await
            .unwrap();
        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert_eq!(detail.comments.last().unwrap().body, "Live comment");

        let uploaded = client
            .upload_kanban_attachment(
                &id,
                "live-note.txt",
                Some("text/plain"),
                b"live bytes",
                Some(&slug),
            )
            .await
            .unwrap();
        let attachment_id = uploaded["attachment"]["id"].as_i64().unwrap();
        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert_eq!(detail.attachments.len(), 1);
        assert_eq!(detail.attachments[0].size, 10);

        let file = client
            .download_kanban_attachment(attachment_id, Some(&slug))
            .await
            .unwrap();
        assert_eq!(file.bytes, b"live bytes");
        assert_eq!(file.filename.as_deref(), Some("live-note.txt"));

        client
            .delete_kanban_attachment(attachment_id, Some(&slug))
            .await
            .unwrap();
        let detail = client.kanban_task(&id, Some(&slug)).await.unwrap();
        assert!(detail.attachments.is_empty());
        // Archiving is the plugin's default removal path and must not touch
        // the host's active board.
        client.delete_kanban_board(&slug, false).await.unwrap();
        let boards = client.kanban_boards().await.unwrap();
        assert!(!boards.boards.iter().any(|board| board.slug == slug));

        // The selected board is client-local: nothing leaked into the host's
        // active board.
        let active = client.kanban_board(None).await.unwrap();
        assert!(
            !active
                .columns
                .iter()
                .flat_map(|column| &column.tasks)
                .any(|task| task.id == id)
        );
    }

    #[test]
    fn bearer_token_is_sent_as_the_loopback_websocket_query() {
        let host = HostConfig {
            token: Some("a/b &c".into()),
            ..HostConfig::local()
        };
        assert_eq!(
            host.connection_config().url,
            "ws://127.0.0.1:9119/api/ws?token=a%2Fb%20%26c"
        );
    }

    #[test]
    fn ws_url_keeps_origin_and_swaps_scheme() {
        assert_eq!(HostConfig::local().ws_url(), "ws://127.0.0.1:9119/api/ws");

        let remote = HostConfig {
            base_url: "https://hermes.example.com".into(),
            ..HostConfig::local()
        };
        assert_eq!(remote.ws_url(), "wss://hermes.example.com/api/ws");
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
    fn roster_preview_prefers_the_last_message() {
        let mut profile: Profile = serde_json::from_value(serde_json::json!({
            "name": "infra",
            "description": "platform ops",
            "last_session": { "preview": "Updated every host", "last_active": 12 }
        }))
        .expect("roster row parses");
        assert_eq!(profile.preview(), "Updated every host");

        profile.last_session = None;
        assert_eq!(profile.preview(), "platform ops");

        let blank = Profile {
            name: "x".into(),
            description: "fallback".into(),
            last_session: Some(LastSession {
                preview: "   ".into(),
                ..LastSession::default()
            }),
            ..Profile::default()
        };
        assert_eq!(blank.preview(), "fallback");
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
        assert_eq!(cfg.url, "wss://hermes.example.com/api/ws?ticket=t-1_2");
        assert!(matches!(cfg.auth, Some(Auth::Cookie(_))));
    }

    #[test]
    fn percent_encode_escapes_query_unsafe_bytes() {
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn env_rows_parse_the_dashboard_shape() {
        let payload = serde_json::json!({
            "OPENAI_API_KEY": {
                "is_set": true,
                "redacted_value": "sk-…abcd",
                "description": "OpenAI",
                "category": "model",
                "provider_label": "OpenAI",
                "is_password": true,
                "custom": false
            },
            "MY_TOKEN": { "is_set": true, "custom": true, "is_password": true }
        });
        let rows: std::collections::HashMap<String, EnvEntry> =
            serde_json::from_value(payload).expect("env rows parse");
        assert!(rows["OPENAI_API_KEY"].is_set);
        assert_eq!(rows["OPENAI_API_KEY"].provider_label, "OpenAI");
        assert!(rows["MY_TOKEN"].custom);
        assert_eq!(rows["MY_TOKEN"].redacted_value, None);
    }

    #[test]
    fn profile_detail_parses_connectors_and_toolsets() {
        let payload = serde_json::json!({
            "name": "fleet-dev",
            "description": "builder",
            "model": { "provider": "deepseek", "default": "deepseek-v4.1-flash" },
            "skills": [{ "name": "rust", "enabled": true }],
            "toolsets": [{ "name": "web", "label": "Web", "tool_count": 4, "enabled": false }],
            "mcp_servers": [{ "name": "github", "enabled": true, "transport": "http" }]
        });
        let detail: ProfileDetail = serde_json::from_value(payload).expect("detail parses");
        assert_eq!(detail.model.default, "deepseek-v4.1-flash");
        assert_eq!(detail.toolsets[0].tool_count, 4);
        assert!(!detail.toolsets[0].enabled);
        assert_eq!(detail.mcp_servers[0].transport.as_deref(), Some("http"));
    }

    #[test]
    fn profile_keeps_ui_meta_out_of_extra() {
        let profile: Profile = serde_json::from_value(serde_json::json!({
            "name": "coo",
            "has_avatar": true,
            "ui_meta": { "icon": "✦", "accent": "#9ed072" }
        }))
        .expect("profile parses");
        assert!(profile.has_avatar);
        assert_eq!(
            profile
                .ui_meta
                .as_ref()
                .and_then(|m| m.get("icon"))
                .and_then(Value::as_str),
            Some("✦")
        );
        assert!(!profile.extra.contains_key("ui_meta"));
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

    #[test]
    fn hub_payloads_parse() {
        let search: SkillHubSearch = serde_json::from_value(serde_json::json!({
            "results": [{
                "name": "gif-search",
                "description": "Search GIFs",
                "source": "github",
                "identifier": "github/owner/repo/gif-search",
                "trust_level": "community",
                "repo": "owner/repo",
                "tags": ["gif"]
            }],
            "source_counts": { "github": 1 },
            "timed_out": [],
            "installed": {}
        }))
        .expect("hub search parses");
        assert_eq!(search.results[0].identifier, "github/owner/repo/gif-search");
        assert_eq!(search.results[0].tags, ["gif"]);

        let preview: SkillHubPreview = serde_json::from_value(serde_json::json!({
            "name": "gif-search",
            "description": "Search GIFs",
            "source": "github",
            "identifier": "github/owner/repo/gif-search",
            "trust_level": "community",
            "repo": "owner/repo",
            "tags": ["gif"],
            "skill_md": "# GIF search\n",
            "files": ["SKILL.md"]
        }))
        .expect("hub preview parses");
        assert_eq!(preview.skill_md, "# GIF search\n");

        let scan: SkillHubScan = serde_json::from_value(serde_json::json!({
            "name": "gif-search",
            "identifier": "github/owner/repo/gif-search",
            "source": "github",
            "trust_level": "community",
            "verdict": "review",
            "summary": "one finding",
            "policy": "ask",
            "policy_reason": "external source",
            "findings": [{
                "severity": "medium",
                "category": "network",
                "file": "SKILL.md",
                "line": 12,
                "description": "fetches a URL"
            }],
            "severity_counts": { "medium": 1 }
        }))
        .expect("hub scan parses");
        assert_eq!(scan.policy, "ask");
        assert_eq!(scan.findings[0].line, Some(12));
        assert_eq!(scan.severity_counts["medium"], 1);
    }
}
