//! The skills page: the host's installed skills, their state and usage.
//!
//! `GET /api/skills` returns the whole catalog (hundreds of entries), and
//! `PUT /api/skills/toggle` flips one skill. Toggling changes what the agent can
//! load on its next turn, so the page states that instead of implying a live
//! chat picks it up mid-answer.

use hermes_api::Skill;

#[derive(Debug, Default)]
pub struct Skills {
    pub loaded: bool,
    pub error: Option<String>,
    pub rows: Vec<Skill>,
    pub query: String,
    /// The search field owns the keystrokes while true.
    pub focus: bool,
    pub status: Option<String>,
}

impl Skills {
    pub fn loaded(&mut self, rows: Vec<Skill>) {
        self.rows = rows;
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }
}

/// Search matches name, description, category and provenance.
pub fn filtered<'a>(rows: &'a [Skill], query: &str) -> Vec<&'a Skill> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|skill| {
            skill.name.to_lowercase().contains(&needle)
                || skill.description.to_lowercase().contains(&needle)
                || skill
                    .category
                    .as_deref()
                    .is_some_and(|category| category.to_lowercase().contains(&needle))
                || skill.provenance.to_lowercase().contains(&needle)
        })
        .collect()
}

pub fn enabled_count(rows: &[Skill]) -> usize {
    rows.iter().filter(|skill| skill.enabled).count()
}

/// `web · agent · 12 uses` — the provenance line under a skill name.
pub fn meta(skill: &Skill) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(category) = skill.category.as_deref().filter(|value| !value.is_empty()) {
        parts.push(category.to_string());
    }
    if !skill.provenance.is_empty() {
        parts.push(skill.provenance.clone());
    }
    if skill.usage > 0 {
        parts.push(format!("{} uses", skill.usage));
    }
    if parts.is_empty() {
        "installed".to_string()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str, category: Option<&str>, enabled: bool) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            category: category.map(str::to_string),
            enabled,
            usage: 3,
            provenance: "agent".to_string(),
        }
    }

    fn catalog() -> Vec<Skill> {
        vec![
            skill("accessibility", "WCAG audits", None, true),
            skill("web-design", "Layout systems", Some("web"), false),
        ]
    }

    #[test]
    fn filtering_matches_name_description_category_and_provenance() {
        let rows = catalog();
        assert_eq!(filtered(&rows, "").len(), 2);
        assert_eq!(filtered(&rows, "access").len(), 1);
        assert_eq!(filtered(&rows, "wcag").len(), 1);
        assert_eq!(filtered(&rows, "web").len(), 1);
        assert_eq!(filtered(&rows, "agent").len(), 2);
        assert!(filtered(&rows, "missing").is_empty());
    }

    #[test]
    fn counts_only_enabled_skills() {
        assert_eq!(enabled_count(&catalog()), 1);
        assert_eq!(enabled_count(&[]), 0);
    }

    #[test]
    fn meta_joins_category_provenance_and_usage() {
        assert_eq!(meta(&catalog()[0]), "agent · 3 uses");
        assert_eq!(meta(&catalog()[1]), "web · agent · 3 uses");
        let bare = Skill {
            provenance: String::new(),
            usage: 0,
            ..catalog()[0].clone()
        };
        assert_eq!(meta(&bare), "installed");
    }

    #[test]
    fn failures_are_a_loaded_state() {
        let mut skills = Skills::default();
        assert!(!skills.loaded);
        skills.failed("catalog unreachable".into());
        assert!(skills.loaded);
        assert!(skills.error.is_some());
        skills.loaded(catalog());
        assert!(skills.error.is_none());
        assert_eq!(skills.rows.len(), 2);
    }
}
