//! The MCP page: every configured server, its transport and reachability.
//!
//! `GET /api/mcp/servers` only guarantees `name`, `enabled` and `transport`;
//! per-transport detail (url/command/args/auth/source/plugin) lives in the raw
//! `extra` map. Enabling a server changes what the next agent turn can call, so
//! the page says that; testing pings from the host, not the desktop.

use hermes_api::McpServer;
use serde_json::Value;

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
}

impl Mcp {
    pub fn loaded(&mut self, rows: Vec<McpServer>) {
        self.rows = rows;
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
        self.testing = None;
    }
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
}
