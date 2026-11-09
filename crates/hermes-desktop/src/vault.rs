//! The vault: the host's credentials, grouped the way a person looks for them.
//!
//! There is no separate vault endpoint — `GET /api/env` already is the host's
//! credential store, and the agent reads the same values at run time. What this
//! window adds over the inspector's compact list is the browser: one tab per
//! catalog category with set counts, search, and a per-item editor.
//!
//! Values arrive redacted server-side and this client never calls the reveal
//! endpoint, so the window can write a credential but cannot read one back.
//! That is the promise printed in its footer, and it is enforced by the wire
//! surface rather than by the UI.

use hermes_api::EnvEntry;

use crate::inspector::ACCENT_CHOICES;

/// Which vault text field has the keystrokes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Search,
    Key,
    Value,
}

/// One category strip entry. `category == None` is the "All" tab.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tab {
    pub category: Option<String>,
    pub label: String,
    /// How many of the entries in this category the host has a value for.
    pub count: usize,
}

pub struct Vault {
    pub open: bool,
    pub query: String,
    /// `None` shows every category.
    pub tab: Option<String>,
    /// The "New item" form is showing.
    pub adding: bool,
    pub key: String,
    pub value: String,
    pub focus: Option<Field>,
    pub status: Option<String>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            tab: None,
            adding: false,
            key: String::new(),
            value: String::new(),
            focus: None,
            status: None,
        }
    }
}

impl Vault {
    pub fn show(&mut self) {
        self.open = true;
        self.status = None;
    }

    pub fn hide(&mut self) {
        self.open = false;
        self.adding = false;
        self.focus = None;
        self.key.clear();
        self.value.clear();
    }

    /// Start a new entry, optionally pre-filled from an unset catalog row.
    pub fn start_adding(&mut self, key: Option<&str>) {
        self.adding = true;
        self.key = key.unwrap_or_default().to_string();
        self.value.clear();
        self.focus = Some(if key.is_some() {
            Field::Value
        } else {
            Field::Key
        });
        self.status = None;
    }

    pub fn cancel_adding(&mut self) {
        self.adding = false;
        self.focus = None;
        self.key.clear();
        self.value.clear();
        self.status = None;
    }
}

/// Human label for a catalog category (`provider` → `Providers`). Tab labels
/// are plural headings, matching how the reference reads ("Logins 8").
pub fn category_label(category: &str) -> String {
    match category {
        "" => "Other".to_string(),
        "provider" => "Providers".to_string(),
        "custom" => "Custom".to_string(),
        "setting" => "Settings".to_string(),
        "skill" => "Skills".to_string(),
        "tool" => "Tools".to_string(),
        "model" => "Models".to_string(),
        "messaging" => "Messaging".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => "Other".to_string(),
            }
        }
    }
}

/// One tab per category the host reports, biggest first, with *set* counts.
/// Channel-managed keys are omitted: the Channels surface owns those.
pub fn tabs(rows: &[(String, EnvEntry)]) -> Vec<Tab> {
    let mut out = vec![Tab {
        category: None,
        label: "All".to_string(),
        count: 0,
    }];
    for (_, entry) in rows.iter().filter(|(_, entry)| !entry.channel_managed) {
        out[0].count += usize::from(entry.is_set);
        let label = category_label(&entry.category);
        match out
            .iter_mut()
            .find(|tab| tab.category.is_some() && tab.label == label)
        {
            Some(tab) => tab.count += usize::from(entry.is_set),
            None => out.push(Tab {
                category: Some(entry.category.clone()),
                label,
                count: usize::from(entry.is_set),
            }),
        }
    }
    out[1..].sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    out
}

/// Rows for the current tab and search query: set entries first, then by key.
pub fn visible<'a>(
    rows: &'a [(String, EnvEntry)],
    tab: Option<&str>,
    query: &str,
) -> Vec<&'a (String, EnvEntry)> {
    let needle = query.trim().to_lowercase();
    let mut out: Vec<&(String, EnvEntry)> = rows
        .iter()
        .filter(|(_, entry)| !entry.channel_managed)
        .filter(|(_, entry)| match tab {
            Some(category) => entry.category == category,
            None => true,
        })
        .filter(|(name, entry)| {
            needle.is_empty()
                || name.to_lowercase().contains(&needle)
                || entry.description.to_lowercase().contains(&needle)
                || entry.provider_label.to_lowercase().contains(&needle)
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.is_set
            .cmp(&a.1.is_set)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    out
}

/// Two-letter tile for a key: `OPENAI_API_KEY` → `OA`, `GH_TOKEN` → `GT`.
pub fn monogram(key: &str) -> String {
    let mut initials = key
        .split('_')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.chars().next());
    match (initials.next(), initials.next()) {
        (Some(first), Some(second)) => format!("{first}{second}").to_uppercase(),
        (Some(first), None) => first.to_uppercase().to_string(),
        _ => "?".to_string(),
    }
}

/// Stable per-key tile tint, drawn from the theme's checked accents.
pub fn tint(key: &str) -> u32 {
    let hash = key.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    ACCENT_CHOICES[(hash as usize) % ACCENT_CHOICES.len()]
}

/// The server accepts names like `OPENAI_API_KEY`; refuse obvious typos before
/// a round trip so the form can explain itself.
pub fn valid_key(key: &str) -> bool {
    let Some(first) = key.chars().next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && key
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, category: &str, is_set: bool) -> (String, EnvEntry) {
        (
            name.to_string(),
            EnvEntry {
                is_set,
                category: category.to_string(),
                description: format!("{name} description"),
                ..EnvEntry::default()
            },
        )
    }

    #[test]
    fn tabs_count_set_entries_per_category_and_skip_channel_owned_keys() {
        let rows = vec![
            row("OPENAI_API_KEY", "provider", true),
            row("ANTHROPIC_API_KEY", "provider", true),
            row("GH_TOKEN", "custom", true),
            row("TELEGRAM_BOT_TOKEN", "messaging", true),
        ];
        let strip = tabs(&rows);
        assert_eq!(strip[0].label, "All");
        assert_eq!(strip[0].count, 4);
        assert_eq!(strip[1].label, "Providers");
        assert_eq!(strip[1].count, 2);
        assert_eq!(strip[2].label, "Custom");
        assert_eq!(strip[3].label, "Messaging");
        assert_eq!(
            strip[1..].iter().map(|tab| tab.count).sum::<usize>(),
            4,
            "category counts must add up to the All count"
        );

        let mut channel = vec![row("SLACK_BOT_TOKEN", "messaging", true)];
        channel[0].1.channel_managed = true;
        assert_eq!(tabs(&channel)[0].count, 0);
        assert!(visible(&channel, None, "").is_empty());
    }

    #[test]
    fn category_labels_read_like_headings() {
        assert_eq!(category_label("provider"), "Providers");
        assert_eq!(category_label("custom"), "Custom");
        assert_eq!(category_label(""), "Other");
        assert_eq!(category_label("tools"), "Tools");
        assert_eq!(category_label("tool"), "Tools");
        assert_eq!(category_label("setting"), "Settings");
        assert_eq!(category_label("messaging"), "Messaging");
    }

    #[test]
    fn visible_narrows_by_tab_and_query_with_set_rows_first() {
        let rows = vec![
            row("OPENAI_API_KEY", "provider", false),
            row("GH_TOKEN", "custom", true),
            row("GITHUB_PAT", "custom", false),
        ];
        let all = visible(&rows, None, "");
        assert_eq!(all[0].0, "GH_TOKEN");
        assert_eq!(visible(&rows, Some("custom"), "").len(), 2);
        assert_eq!(visible(&rows, None, "github").len(), 1);
        assert_eq!(visible(&rows, None, "description").len(), 3);
    }

    #[test]
    fn monograms_and_tints_are_stable() {
        assert_eq!(monogram("OPENAI_API_KEY"), "OA");
        assert_eq!(monogram("GH_TOKEN"), "GT");
        assert_eq!(monogram("TOKEN"), "T");
        assert_eq!(monogram("_"), "?");
        assert_eq!(tint("OPENAI_API_KEY"), tint("OPENAI_API_KEY"));
        assert!(ACCENT_CHOICES.contains(&tint("ZAI_API_KEY")));
    }

    #[test]
    fn key_names_are_checked_before_a_round_trip() {
        assert!(valid_key("OPENAI_API_KEY"));
        assert!(valid_key("_PRIVATE"));
        assert!(valid_key("KEY2"));
        assert!(!valid_key("openai_api_key"));
        assert!(!valid_key("2FA_KEY"));
        assert!(!valid_key("GH-TOKEN"));
        assert!(!valid_key(""));
        assert!(!valid_key("GH TOKEN"));
    }
}
