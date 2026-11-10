//! The MCP page: configured servers, their transport and reachability, plus
//! the curated catalog and an explicit add/edit form.
//!
//! `GET /api/mcp/servers` only guarantees `name`, `enabled` and `transport`;
//! per-transport detail (url/command/args/auth/source/plugin) lives in the raw
//! `extra` map. Enabling a server changes what the next agent turn can call, so
//! the page says that; testing pings from the host, not the desktop.
//!
//! Edits use the host's whole-map `PUT /api/mcp/servers` route. A per-key merge
//! cannot remove nested fields (for example switching HTTP to stdio), so the
//! edit form starts from the raw entry and sends the complete replacement map.

use hermes_api::{McpCatalog, McpCatalogEntry, McpServer};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Servers,
    Catalog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Url,
    Command,
    Args,
    Env,
    Auth,
    Token,
}

#[derive(Debug, Clone)]
pub struct Form {
    pub old_name: Option<String>,
    pub name: String,
    pub url: String,
    pub command: String,
    pub args: String,
    pub env: String,
    /// `none`, `header` or `oauth`.
    pub auth: String,
    pub token: String,
    pub focus: Field,
    pub error: Option<String>,
    /// Raw server entry when editing; `None` for a new server.
    pub base: Option<Value>,
}

impl Form {
    pub fn create() -> Self {
        Self {
            old_name: None,
            name: String::new(),
            url: String::new(),
            command: String::new(),
            args: String::new(),
            env: String::new(),
            auth: "none".into(),
            token: String::new(),
            focus: Field::Name,
            error: None,
            base: None,
        }
    }

    pub fn edit(name: &str, config: Value) -> Self {
        let url = config
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let command = config
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let args = config
            .get("args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let env = config
            .get("env")
            .and_then(Value::as_object)
            .map(|env| {
                env.iter()
                    .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let auth = config
            .get("auth")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                config
                    .get("headers")
                    .and_then(Value::as_object)
                    .filter(|headers| {
                        headers
                            .keys()
                            .any(|key| key.eq_ignore_ascii_case("authorization"))
                    })
                    .map(|_| "header".to_string())
            })
            .unwrap_or_else(|| "none".into());
        Self {
            old_name: Some(name.to_string()),
            name: name.to_string(),
            url,
            command,
            args,
            env,
            auth,
            token: String::new(),
            focus: Field::Name,
            error: None,
            base: Some(config),
        }
    }

    pub fn is_edit(&self) -> bool {
        self.old_name.is_some()
    }

    pub fn cycle_auth(&mut self) {
        self.auth = match self.auth.as_str() {
            "none" => "header".into(),
            "header" => "oauth".into(),
            _ => "none".into(),
        };
    }

    pub fn focus_next(&mut self) {
        self.focus = match self.focus {
            Field::Name => Field::Url,
            Field::Url => Field::Command,
            Field::Command => Field::Args,
            Field::Args => Field::Env,
            Field::Env => Field::Auth,
            Field::Auth => Field::Token,
            Field::Token => Field::Name,
        };
    }

    pub fn typed(&mut self, chars: &str) {
        match self.focus {
            Field::Name => self.name.push_str(chars),
            Field::Url => self.url.push_str(chars),
            Field::Command => {
                self.command.push_str(chars);
                if self.url.trim().is_empty() {
                    self.auth = "none".into();
                }
            }
            Field::Args => self.args.push_str(chars),
            Field::Env => self.env.push_str(chars),
            Field::Auth => {}
            Field::Token => self.token.push_str(chars),
        }
    }

    pub fn backspace(&mut self) {
        match self.focus {
            Field::Name => {
                self.name.pop();
            }
            Field::Url => {
                self.url.pop();
            }
            Field::Command => {
                self.command.pop();
            }
            Field::Args => {
                self.args.pop();
            }
            Field::Env => {
                self.env.pop();
            }
            Field::Auth => {}
            Field::Token => {
                self.token.pop();
            }
        }
    }

    /// `POST /api/mcp/servers` body. The host validates the same contract; the
    /// local checks only keep the error beside the field instead of in a 400.
    pub fn create_body(&self) -> Result<Value, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let url = self.url.trim();
        let command = self.command.trim();
        if url.is_empty() == command.is_empty() {
            return Err("provide exactly one of URL or command".into());
        }
        let auth = self.auth.as_str();
        if !matches!(auth, "none" | "header" | "oauth") {
            return Err("auth must be none, header or oauth".into());
        }
        let mut body = Map::new();
        body.insert("name".into(), json!(name));
        if !url.is_empty() {
            if !self.args.trim().is_empty() || !self.env.trim().is_empty() {
                return Err("arguments and env are only valid for stdio servers".into());
            }
            body.insert("url".into(), json!(url));
            if auth != "none" {
                body.insert("auth".into(), json!(auth));
            }
            if auth == "header" {
                let token = self.token.trim();
                if token.is_empty() {
                    return Err("a bearer token is required for header auth".into());
                }
                body.insert("bearer_token".into(), json!(token));
            } else if !self.token.trim().is_empty() {
                return Err("a bearer token requires header auth".into());
            }
        } else {
            if auth != "none" || !self.token.trim().is_empty() {
                return Err("HTTP auth is not valid for stdio servers".into());
            }
            body.insert("command".into(), json!(command));
            let args = split_args(&self.args);
            if !args.is_empty() {
                body.insert("args".into(), json!(args));
            }
            let env = parse_env(&self.env)?;
            if !env.is_empty() {
                body.insert("env".into(), Value::Object(env));
            }
        }
        Ok(Value::Object(body))
    }

    /// Complete replacement entry for the host's whole-map PUT. Unknown keys
    /// (tools, enabled, headers) are preserved from the raw entry.
    pub fn edit_config(&self) -> Result<Value, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let url = self.url.trim();
        let command = self.command.trim();
        if url.is_empty() == command.is_empty() {
            return Err("provide exactly one of URL or command".into());
        }
        let mut object = self
            .base
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in ["url", "command", "args", "env"] {
            object.remove(key);
        }
        if !url.is_empty() {
            object.insert("url".into(), json!(url));
            match self.auth.as_str() {
                "oauth" => {
                    object.insert("auth".into(), json!("oauth"));
                }
                "header" => {
                    object.insert("auth".into(), json!("header"));
                    let has_headers = object
                        .get("headers")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| {
                            headers
                                .keys()
                                .any(|key| key.eq_ignore_ascii_case("authorization"))
                        });
                    if !has_headers && self.token.trim().is_empty() {
                        return Err("header auth needs an existing header or a token".into());
                    }
                }
                _ => {
                    object.remove("auth");
                    object.remove("headers");
                }
            }
        } else {
            if self.auth != "none" || !self.token.trim().is_empty() {
                return Err("HTTP auth is not valid for stdio servers".into());
            }
            object.remove("auth");
            object.remove("headers");
            object.insert("command".into(), json!(command));
            let args = split_args(&self.args);
            if !args.is_empty() {
                object.insert("args".into(), json!(args));
            }
            let env = parse_env(&self.env)?;
            if !env.is_empty() {
                object.insert("env".into(), Value::Object(env));
            }
        }
        Ok(Value::Object(object))
    }
}

#[derive(Debug, Clone)]
pub struct InstallEnv {
    pub key: String,
    pub prompt: String,
    pub required: bool,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Install {
    pub name: String,
    pub env: Vec<InstallEnv>,
    pub focus: usize,
    pub error: Option<String>,
}

impl Install {
    pub fn new(entry: &McpCatalogEntry) -> Self {
        Self {
            name: entry.name.clone(),
            env: entry
                .required_env
                .iter()
                .map(|field| InstallEnv {
                    key: field.name.clone(),
                    prompt: if field.prompt.is_empty() {
                        field.name.clone()
                    } else {
                        field.prompt.clone()
                    },
                    required: field.required,
                    value: String::new(),
                })
                .collect(),
            focus: 0,
            error: None,
        }
    }

    pub fn values(&self) -> Result<Value, String> {
        let mut env = Map::new();
        for field in &self.env {
            let value = field.value.trim();
            if field.required && value.is_empty() {
                return Err(format!("{} is required", field.key));
            }
            if !value.is_empty() {
                env.insert(field.key.clone(), json!(value));
            }
        }
        Ok(Value::Object(env))
    }

    pub fn focus_next(&mut self) {
        if !self.env.is_empty() {
            self.focus = (self.focus + 1) % self.env.len();
        }
    }
}

#[derive(Debug, Default)]
pub struct Mcp {
    pub loaded: bool,
    pub error: Option<String>,
    pub rows: Vec<McpServer>,
    pub query: String,
    /// The search field owns the keystrokes while true.
    pub focus: bool,
    pub status: Option<String>,
    /// Set while a server test is in flight.
    pub testing: Option<String>,
    pub oauth: Option<hermes_api::McpOAuthFlow>,
    pub oauth_opened: Option<String>,
    pub oauth_busy: bool,
    pub tab: Tab,
    pub form: Option<Form>,
    pub confirm_delete: Option<String>,
    pub catalog: Option<McpCatalog>,
    pub catalog_error: Option<String>,
    pub catalog_loading: bool,
    pub install: Option<Install>,
}

impl Mcp {
    pub fn loaded(&mut self, rows: Vec<McpServer>) {
        self.rows = rows;
        self.error = None;
        self.loaded = true;
        if self
            .confirm_delete
            .as_ref()
            .is_some_and(|name| !self.rows.iter().any(|row| &row.name == name))
        {
            self.confirm_delete = None;
        }
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
        self.testing = None;
    }

    pub fn begin_create(&mut self) {
        self.form = Some(Form::create());
        self.install = None;
        self.confirm_delete = None;
    }

    pub fn begin_edit(&mut self, name: &str, config: Value) {
        self.form = Some(Form::edit(name, config));
        self.install = None;
        self.confirm_delete = None;
    }

    pub fn catalog_loaded(&mut self, catalog: McpCatalog) {
        self.catalog = Some(catalog);
        self.catalog_error = None;
        self.catalog_loading = false;
    }

    pub fn catalog_failed(&mut self, message: String) {
        self.catalog_error = Some(message);
        self.catalog_loading = false;
    }

    pub fn begin_install(&mut self, entry: &McpCatalogEntry) {
        self.install = Some(Install::new(entry));
        self.form = None;
    }
}

fn split_args(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn parse_env(value: &str) -> Result<Map<String, Value>, String> {
    let mut env = Map::new();
    for (index, line) in value
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .enumerate()
    {
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("env entry {} needs KEY=VALUE", index + 1));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("env entry {} has an empty key", index + 1));
        }
        env.insert(key.to_string(), json!(value.trim()));
    }
    Ok(env)
}

fn extra_str(server: &McpServer, key: &str) -> Option<String> {
    server
        .extra
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub fn transport(server: &McpServer) -> String {
    server
        .transport
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "?".to_string())
}

pub fn source(server: &McpServer) -> String {
    extra_str(server, "source").unwrap_or_else(|| "config".to_string())
}

pub fn auth(server: &McpServer) -> Option<String> {
    extra_str(server, "auth").filter(|value| !value.is_empty())
}

pub fn plugin(server: &McpServer) -> Option<String> {
    extra_str(server, "plugin").filter(|value| !value.is_empty())
}

/// Search matches name, source, transport and endpoint/command.
pub fn filtered<'a>(rows: &'a [McpServer], query: &str) -> Vec<&'a McpServer> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|server| {
            server.name.to_lowercase().contains(&needle)
                || transport(server).to_lowercase().contains(&needle)
                || source(server).to_lowercase().contains(&needle)
                || endpoint(server).to_lowercase().contains(&needle)
        })
        .collect()
}

/// The line under a server name: URL for HTTP, command and args for stdio.
pub fn endpoint(server: &McpServer) -> String {
    if let Some(url) = extra_str(server, "url").filter(|value| !value.is_empty()) {
        return url;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(command) = extra_str(server, "command").filter(|value| !value.is_empty()) {
        parts.push(command);
    }
    if let Some(args) = server.extra.get("args").and_then(Value::as_array) {
        parts.extend(args.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if parts.is_empty() {
        "(no endpoint)".to_string()
    } else {
        parts.join(" ")
    }
}

pub fn enabled_count(rows: &[McpServer]) -> usize {
    rows.iter().filter(|server| server.enabled).count()
}

/// One-line result for the status banner after a test.
pub fn test_summary(name: &str, result: &Value) -> String {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.len())
        .or_else(|| {
            result
                .get("tool_count")
                .and_then(Value::as_u64)
                .map(|count| count as usize)
        });
    match tools {
        Some(count) => format!("{name}: reachable · {count} tools"),
        None => format!("{name}: reachable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, transport: &str, extra: Value) -> McpServer {
        let extra = match extra {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        McpServer {
            name: name.to_string(),
            transport: Some(transport.to_string()),
            enabled: true,
            extra,
            ..McpServer::default()
        }
    }

    fn catalog() -> Vec<McpServer> {
        vec![
            server(
                "jellyfin",
                "http",
                serde_json::json!({"url": "http://192.0.2.10:8080/mcp", "source": "config", "auth": "header"}),
            ),
            server(
                "k3s",
                "stdio",
                serde_json::json!({"command": "uv", "args": ["--directory", "/work/server"]}),
            ),
        ]
    }

    #[test]
    fn filtering_matches_name_transport_source_and_endpoint() {
        let rows = catalog();
        assert_eq!(filtered(&rows, "").len(), 2);
        assert_eq!(filtered(&rows, "jelly").len(), 1);
        assert_eq!(filtered(&rows, "stdio").len(), 1);
        assert_eq!(filtered(&rows, "config").len(), 2);
        assert_eq!(filtered(&rows, "8080").len(), 1);
        assert!(filtered(&rows, "missing").is_empty());
    }

    #[test]
    fn endpoints_follow_the_transport() {
        let rows = catalog();
        assert_eq!(endpoint(&rows[0]), "http://192.0.2.10:8080/mcp");
        assert_eq!(endpoint(&rows[1]), "uv --directory /work/server");
        assert_eq!(endpoint(&McpServer::default()), "(no endpoint)");
        assert_eq!(transport(&rows[0]), "http");
        assert_eq!(source(&rows[1]), "config");
        assert_eq!(auth(&rows[0]).as_deref(), Some("header"));
        assert!(plugin(&rows[0]).is_none());
    }

    #[test]
    fn test_summaries_report_tool_counts() {
        assert_eq!(
            test_summary("jellyfin", &serde_json::json!({"tools": [1, 2, 3]})),
            "jellyfin: reachable · 3 tools"
        );
        assert_eq!(
            test_summary("k3s", &serde_json::json!({"tool_count": 7})),
            "k3s: reachable · 7 tools"
        );
        assert_eq!(
            test_summary("k3s", &serde_json::json!({})),
            "k3s: reachable"
        );
    }

    #[test]
    fn counts_and_failures_are_tracked() {
        let mut mcp = Mcp::default();
        mcp.loaded(catalog());
        assert_eq!(enabled_count(&mcp.rows), 2);
        mcp.testing = Some("k3s".to_string());
        mcp.failed("unreachable".into());
        assert!(mcp.testing.is_none());
        assert!(mcp.error.is_some());
        assert!(mcp.loaded);
    }

    #[test]
    fn create_forms_split_http_and_stdio() {
        let mut http = Form::create();
        http.name = "github".into();
        http.url = "https://example.test/mcp".into();
        http.auth = "header".into();
        http.token = "secret".into();
        let body = http.create_body().unwrap();
        assert_eq!(body["name"], "github");
        assert_eq!(body["url"], "https://example.test/mcp");
        assert_eq!(body["auth"], "header");
        assert_eq!(body["bearer_token"], "secret");

        let mut stdio = Form::create();
        stdio.name = "local".into();
        stdio.command = "uvx".into();
        stdio.args = "pkg==1 --flag".into();
        stdio.env = "TOKEN=abc\n# comment\nEMPTY=".into();
        let body = stdio.create_body().unwrap();
        assert_eq!(body["command"], "uvx");
        assert_eq!(body["args"], serde_json::json!(["pkg==1", "--flag"]));
        assert_eq!(body["env"]["TOKEN"], "abc");
        assert_eq!(body["env"]["EMPTY"], "");
    }

    #[test]
    fn bad_forms_are_rejected() {
        let mut form = Form::create();
        assert!(form.create_body().is_err());
        form.name = "x".into();
        form.url = "https://example.test".into();
        form.command = "uvx".into();
        assert!(form.create_body().is_err());
        form.command.clear();
        form.env = "nope".into();
        assert!(form.create_body().is_err());
        form.env.clear();
        form.auth = "header".into();
        assert!(form.create_body().is_err());
    }

    #[test]
    fn edit_forms_preserve_unknown_keys_and_replace_transport() {
        let base = serde_json::json!({
            "url": "https://old.test/mcp",
            "auth": "oauth",
            "enabled": false,
            "tools": ["a"],
            "custom": 1
        });
        let mut form = Form::edit("old", base);
        form.name = "new".into();
        form.url.clear();
        form.command = "uvx".into();
        form.auth = "none".into();
        form.args = "pkg==2".into();
        let config = form.edit_config().unwrap();
        assert!(config.get("url").is_none());
        assert!(config.get("auth").is_none());
        assert_eq!(config["command"], "uvx");
        assert_eq!(config["args"], serde_json::json!(["pkg==2"]));
        assert_eq!(config["enabled"], false);
        assert_eq!(config["tools"], serde_json::json!(["a"]));
        assert_eq!(config["custom"], 1);
    }

    #[test]
    fn install_requires_declared_env_values() {
        let entry = McpCatalogEntry {
            name: "jellyfin".into(),
            required_env: vec![hermes_api::McpCatalogEnv {
                name: "JELLYFIN_TOKEN".into(),
                prompt: "API token".into(),
                required: true,
            }],
            ..McpCatalogEntry::default()
        };
        let mut install = Install::new(&entry);
        assert!(install.values().is_err());
        install.env[0].value = "abc".into();
        assert_eq!(install.values().unwrap()["JELLYFIN_TOKEN"], "abc");
    }
}
