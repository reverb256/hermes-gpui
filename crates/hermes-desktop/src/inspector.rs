//! The right-hand inspector: one bot's look, its connectors, and the host's
//! secrets.
//!
//! All three surfaces exist server-side and are shared between machines:
//!
//! * **Look** — a glyph and an accent colour, stored in the profile's
//!   `profile.yaml` under `ui_meta` (the gateway's own client-agnostic slot),
//!   plus an optional avatar bitmap stored as `assets/avatar.*`
//!   (`profiles.set_asset`). Nothing is client-local.
//! * **Connectors** — the profile's MCP servers and toolsets, read from
//!   `profiles.describe` and toggled through `profiles.configure`
//!   (`enabled_mcp_servers` / `enabled_toolsets`, replace semantics).
//! * **Secrets** — the host `.env` through `GET/PUT/DELETE /api/env`. Values
//!   arrive redacted; this client never asks for the reveal endpoint, so a
//!   secret is written and listed but never read back.
//!
//! The state and the pure helpers live here so they can be tested without a
//! window; rendering is in `app.rs`.

use std::sync::Arc;

use gpui::Image;
use hermes_api::{EnvEntry, ProfileDetail};
use serde_json::{Value, json};

use crate::theme::{ACCENT, BOT_ACCENT_CYAN, DANGER, REASONING, USER, WARN};

/// Icon choices for a bot. Deliberately monochrome glyphs: they inherit the
/// row's ink colour, so contrast is decided by the token layer, not the glyph.
pub const GLYPHS: [&str; 12] = ["◆", "✦", "✧", "❖", "▲", "●", "■", "◈", "∞", "☉", "☾", "⚑"];

/// Accent choices, all already contrast-checked against the window planes.
pub const ACCENT_CHOICES: [u32; 6] = [ACCENT, USER, REASONING, WARN, DANGER, BOT_ACCENT_CYAN];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Look,
    Connectors,
    Secrets,
}

impl Tab {
    pub fn all() -> [Tab; 3] {
        [Tab::Look, Tab::Connectors, Tab::Secrets]
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Look => "Look",
            Tab::Connectors => "Connectors",
            Tab::Secrets => "Secrets",
        }
    }
}

/// Which inspector text field has the keystrokes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Filter,
    SecretKey,
    SecretValue,
}

pub struct Inspector {
    pub open: bool,
    pub tab: Tab,
    /// Profile the loaded detail belongs to; `None` until the first reply.
    pub detail_for: Option<String>,
    pub detail: Option<ProfileDetail>,
    pub env: Vec<(String, EnvEntry)>,
    /// False until the first `GET /api/env` answer, which can take seconds
    /// while the host scans its catalog — say "loading" instead of "none".
    pub env_loaded: bool,
    /// Show every catalogued key, not just the ones the host has set.
    pub show_all: bool,
    pub filter: String,
    pub draft_key: String,
    pub draft_value: String,
    pub focus: Option<Field>,
    pub status: Option<String>,
    /// Pending look edits; saved with `profiles.configure`.
    pub glyph: String,
    pub accent: u32,
    pub avatar: Option<Arc<Image>>,
    pub avatar_for: Option<String>,
}

impl Default for Inspector {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Look,
            detail_for: None,
            detail: None,
            env: Vec::new(),
            env_loaded: false,
            show_all: false,
            filter: String::new(),
            draft_key: String::new(),
            draft_value: String::new(),
            focus: None,
            status: None,
            glyph: GLYPHS[0].to_string(),
            accent: ACCENT,
            avatar: None,
            avatar_for: None,
        }
    }
}

impl Inspector {
    /// Point the panel at a profile; its detail arrives asynchronously.
    pub fn target(&mut self, profile: &str) {
        if self.detail_for.as_deref() != Some(profile) {
            self.detail_for = Some(profile.to_string());
            self.detail = None;
        }
        if self.avatar_for.as_deref() != Some(profile) {
            self.avatar = None;
            self.avatar_for = Some(profile.to_string());
            self.env_loaded = false;
        }
    }

    pub fn loaded_env(&mut self, rows: Vec<(String, EnvEntry)>) {
        self.env = rows;
        self.env_loaded = true;
    }

    /// Seed the look editor from the roster row's `ui_meta`.
    pub fn seed_look(&mut self, meta: Option<&Value>) {
        let (glyph, accent) = look_from_meta(meta);
        self.glyph = glyph;
        self.accent = accent;
    }

    pub fn set_avatar(&mut self, profile: &str, image: Arc<Image>) {
        self.avatar_for = Some(profile.to_string());
        self.avatar = Some(image);
    }

    pub fn clear_avatar(&mut self, profile: &str) {
        if self.avatar_for.as_deref() == Some(profile) {
            self.avatar = None;
        }
    }
}

/// Look for a roster row: an explicit `ui_meta` wins; otherwise a stable
/// per-bot choice from the token palettes, so a fresh fleet is not monochrome.
pub fn look_for(name: &str, meta: Option<&Value>) -> (String, u32) {
    let hash = name.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    let glyph = meta
        .and_then(|meta| meta.get("icon"))
        .and_then(Value::as_str)
        .filter(|icon| !icon.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| GLYPHS[(hash as usize) % GLYPHS.len()].to_string());
    let accent = meta
        .and_then(|meta| meta.get("accent"))
        .and_then(Value::as_str)
        .and_then(parse_hex)
        .unwrap_or(ACCENT_CHOICES[(hash as usize / 7) % ACCENT_CHOICES.len()]);
    (glyph, accent)
}

/// Read the glyph and accent a bot stored in `ui_meta`, with fallbacks.
pub fn look_from_meta(meta: Option<&Value>) -> (String, u32) {
    let glyph = meta
        .and_then(|meta| meta.get("icon"))
        .and_then(Value::as_str)
        .filter(|icon| !icon.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| GLYPHS[0].to_string());
    let accent = meta
        .and_then(|meta| meta.get("accent"))
        .and_then(Value::as_str)
        .and_then(parse_hex)
        .unwrap_or(ACCENT);
    (glyph, accent)
}

/// `profiles.configure` params patch for a look change.
pub fn look_patch(glyph: &str, accent: u32) -> Value {
    json!({
        "ui_meta": {
            "icon": glyph,
            "accent": format!("#{accent:06x}"),
        }
    })
}

/// Parse `#rrggbb` / `rrggbb`, tolerating surrounding whitespace.
pub fn parse_hex(raw: &str) -> Option<u32> {
    let trimmed = raw.trim().trim_start_matches('#');
    if trimmed.len() != 6 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(trimmed, 16).ok()
}

/// Secrets to paint: set keys by default, everything when `show_all`, narrowed
/// by a case-insensitive match on key, description or provider.
pub fn visible_env<'a>(
    rows: &'a [(String, EnvEntry)],
    query: &str,
    show_all: bool,
) -> Vec<&'a (String, EnvEntry)> {
    let needle = query.trim().to_lowercase();
    let mut visible: Vec<&(String, EnvEntry)> = rows
        .iter()
        .filter(|(_, entry)| show_all || entry.is_set)
        .filter(|(name, entry)| {
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || entry.description.to_lowercase().contains(&needle)
                || entry.provider_label.to_lowercase().contains(&needle)
        })
        .collect();
    visible.sort_by(|a, b| {
        b.1.is_set
            .cmp(&a.1.is_set)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    visible
}

/// Number of keys the host actually has a value for.
pub fn set_secret_count(rows: &[(String, EnvEntry)]) -> usize {
    rows.iter().filter(|(_, entry)| entry.is_set).count()
}

/// Wrap raw base64 in a data URL the gateway's `profiles.set_asset` accepts.
pub fn avatar_data_url(mime: &str, base64: &str) -> String {
    format!("data:{mime};base64,{base64}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(is_set: bool) -> EnvEntry {
        EnvEntry {
            is_set,
            ..EnvEntry::default()
        }
    }

    #[test]
    fn look_round_trips_through_ui_meta() {
        // `look_patch` is configure params; the reader sees the `ui_meta` block.
        let patch = look_patch("✦", 0x9ed072);
        let (glyph, accent) = look_from_meta(patch.get("ui_meta"));
        assert_eq!(glyph, "✦");
        assert_eq!(accent, 0x9ed072);
    }

    #[test]
    fn derived_looks_are_stable_and_explicit_meta_wins() {
        assert_eq!(look_for("analyst", None), look_for("analyst", None));
        assert_ne!(look_for("analyst", None), look_for("clipbot", None));

        let meta = look_patch("✦", 0x9ed072);
        let (glyph, accent) = look_for("analyst", meta.get("ui_meta"));
        assert_eq!(glyph, "✦");
        assert_eq!(accent, 0x9ed072);
    }

    #[test]
    fn missing_or_malformed_meta_falls_back_to_tokens() {
        let (glyph, accent) = look_from_meta(None);
        assert_eq!(glyph, GLYPHS[0]);
        assert_eq!(accent, ACCENT);

        let (_, accent) = look_from_meta(Some(&json!({ "accent": "not-a-colour" })));
        assert_eq!(accent, ACCENT);
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("0x123456"), None);
        assert_eq!(parse_hex(" #7AA2F7 "), Some(0x7aa2f7));
    }

    #[test]
    fn accent_choices_stay_in_the_token_set() {
        for choice in ACCENT_CHOICES {
            assert!(
                [ACCENT, USER, REASONING, WARN, DANGER, BOT_ACCENT_CYAN].contains(&choice),
                "{choice:#08x} is not a theme token"
            );
        }
    }

    #[test]
    fn secrets_default_to_set_keys_and_search_narrows_them() {
        let rows = vec![
            (
                "OPENAI_API_KEY".to_string(),
                EnvEntry {
                    is_set: true,
                    description: "OpenAI key".into(),
                    ..entry(true)
                },
            ),
            ("ANTHROPIC_API_KEY".to_string(), entry(false)),
            (
                "GH_TOKEN".to_string(),
                EnvEntry {
                    is_set: true,
                    description: "GitHub".into(),
                    ..entry(true)
                },
            ),
        ];
        assert_eq!(visible_env(&rows, "", false).len(), 2);
        assert_eq!(visible_env(&rows, "", true).len(), 3);
        assert_eq!(visible_env(&rows, "github", true).len(), 1);
        assert_eq!(visible_env(&rows, "anthropic", false).len(), 0);
        assert_eq!(set_secret_count(&rows), 2);
    }

    #[test]
    fn avatar_data_url_matches_the_gateway_shape() {
        assert_eq!(
            avatar_data_url("image/png", "AAAA"),
            "data:image/png;base64,AAAA"
        );
    }
}
