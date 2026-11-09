//! Session history: the list behind the rail swap and the transcript hydration.
//!
//! `/api/sessions` pages newest-first; `/api/sessions/{id}/messages` returns the
//! stored turns. Messages arrive as opaque JSON so a tool card or an array
//! content block can never fail the whole page — the pure mapper below decides
//! what is renderable.

use serde_json::Value;

use crate::transcript::{Line, Role, clip};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameDraft {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Sessions {
    pub open: bool,
    pub loaded: bool,
    pub error: Option<String>,
    pub rows: Vec<hermes_api::SessionSummary>,
    pub total: u64,
    pub query: String,
    pub focus: bool,
    pub status: Option<String>,
    pub renaming: Option<RenameDraft>,
}

impl Sessions {
    pub fn loaded(&mut self, rows: Vec<hermes_api::SessionSummary>, total: u64) {
        self.rows = rows;
        self.total = total;
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }

    pub fn begin_rename(&mut self, id: &str, title: &str) {
        self.renaming = Some(RenameDraft {
            id: id.to_string(),
            title: title.to_string(),
        });
        self.focus = false;
    }

    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

/// Rows matching the search box, in the order the backend returned them.
pub fn filtered(
    rows: &[hermes_api::SessionSummary],
    query: &str,
) -> Vec<hermes_api::SessionSummary> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|row| {
            row.title.to_lowercase().contains(&needle)
                || row.preview.to_lowercase().contains(&needle)
                || row.id.to_lowercase().contains(&needle)
                || row.profile_name.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

pub fn title(row: &hermes_api::SessionSummary) -> String {
    if !row.title.trim().is_empty() {
        clip(&row.title, 34)
    } else if !row.preview.trim().is_empty() {
        clip(&row.preview, 34)
    } else if row.display_name.trim().is_empty() {
        row.id.clone()
    } else {
        clip(&row.display_name, 34)
    }
}

/// `55 msgs · 2h`, the compact second line of a session row.
pub fn meta(row: &hermes_api::SessionSummary, now: f64) -> String {
    let mut parts = vec![format!("{} msgs", row.message_count)];
    if row.tool_call_count > 0 {
        parts.push(format!("{} tools", row.tool_call_count));
    }
    parts.push(age(now, row.last_active.max(row.started_at)));
    parts.join(" · ")
}

pub fn age(now: f64, then: f64) -> String {
    let seconds = (now - then).max(0.0);
    if seconds < 60.0 {
        "now".to_string()
    } else if seconds < 3_600.0 {
        format!("{}m", (seconds / 60.0) as u64)
    } else if seconds < 86_400.0 {
        format!("{}h", (seconds / 3_600.0) as u64)
    } else {
        format!("{}d", (seconds / 86_400.0) as u64)
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .or_else(|| part.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// Stored turns to transcript lines. Reasoning is hoisted above its answer;
/// tool results become system lines so they read as activity, not prose.
pub fn lines_from_messages(messages: &[Value]) -> Vec<Line> {
    let mut lines = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reasoning = message
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let content = text_of(message.get("content").unwrap_or(&Value::Null));
        let content = content.trim();
        match role {
            "user" => {
                if !content.is_empty() {
                    lines.push(Line::new(Role::User, content));
                }
            }
            "assistant" => {
                if !reasoning.is_empty() {
                    lines.push(Line::new(Role::Reasoning, reasoning));
                }
                if !content.is_empty() {
                    lines.push(Line::new(Role::Assistant, content));
                }
            }
            "tool" => {
                let name = message
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let body = if content.is_empty() {
                    "(no output)"
                } else {
                    content
                };
                lines.push(Line::new(
                    Role::System,
                    format!("⚙ {name}: {}", clip(body, 400)),
                ));
            }
            "system" if !content.is_empty() => {
                lines.push(Line::new(Role::System, content));
            }
            _ => {}
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_api::SessionSummary;
    use serde_json::json;

    fn row(title: &str, preview: &str, messages: u64, last: f64) -> SessionSummary {
        SessionSummary {
            id: format!("id-{title}"),
            title: title.to_string(),
            preview: preview.to_string(),
            message_count: messages,
            last_active: last,
            started_at: last - 10.0,
            tool_call_count: 2,
            ..SessionSummary::default()
        }
    }

    #[test]
    fn filtering_matches_title_preview_and_id() {
        let rows = vec![row("Trading health", "restart gate", 5, 100.0)];
        assert_eq!(filtered(&rows, "trading").len(), 1);
        assert_eq!(filtered(&rows, "gate").len(), 1);
        assert_eq!(filtered(&rows, "id-").len(), 1);
        assert_eq!(filtered(&rows, "nope").len(), 0);
        assert_eq!(filtered(&rows, "  ").len(), 1);
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(1_000.0, 990.0), "now");
        assert_eq!(age(1_000.0, 880.0), "2m");
        assert_eq!(age(10_000.0, 1_000.0), "2h");
        assert_eq!(age(200_000.0, 1_000.0), "2d");
        assert_eq!(age(10.0, 2_000.0), "now");
    }

    #[test]
    fn meta_reports_messages_tools_and_age() {
        let summary = row("x", "", 55, 1_000.0);
        assert_eq!(meta(&summary, 1_060.0), "55 msgs · 2 tools · 1m");
    }

    #[test]
    fn messages_map_roles_reasoning_and_tools() {
        let messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "hello", "reasoning": "why"}),
            json!({"role": "tool", "tool_name": "shell", "content": "ok"}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "part"}]}),
        ];
        let lines = lines_from_messages(&messages);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].role, Role::User);
        assert_eq!(lines[1].role, Role::Reasoning);
        assert_eq!(lines[2].text, "hello");
        assert!(lines[3].text.starts_with("⚙ shell:"));
        assert_eq!(lines[4].text, "part");
    }

    #[test]
    fn rename_drafts_track_the_row_and_title() {
        let mut sessions = Sessions {
            focus: true,
            ..Sessions::default()
        };
        sessions.begin_rename("id-a", "Old title");
        let draft = sessions.renaming.as_ref().unwrap();
        assert_eq!(draft.id, "id-a");
        assert_eq!(draft.title, "Old title");
        assert!(
            !sessions.focus,
            "the search field gives up focus to the editor"
        );
        sessions.cancel_rename();
        assert!(sessions.renaming.is_none());
    }

    #[test]
    fn empty_and_unknown_messages_are_skipped() {
        let messages = vec![
            json!({"role": "assistant", "content": ""}),
            json!({"role": "mystery", "content": "x"}),
        ];
        assert!(lines_from_messages(&messages).is_empty());
    }
}
