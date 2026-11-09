//! The skills page: the host's installed skills plus the skills hub.
//!
//! `GET /api/skills` returns the whole installed catalog (hundreds of
//! entries), and `PUT /api/skills/toggle` flips one skill. Toggling changes
//! what the agent can load on its next turn, so the page states that instead
//! of implying a live chat picks it up mid-answer.
//!
//! The hub side is read-first: search (`/api/skills/hub/search`), preview the
//! actual SKILL.md (`/preview`), run the installer's security scan (`/scan`),
//! and only then install by identifier. Installs spawn a host-side action, so
//! the page reports that the installed list refreshes when it finishes.

use hermes_api::{Skill, SkillHubFinding, SkillHubPreview, SkillHubResult, SkillHubScan};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Installed,
    Hub,
}

#[derive(Debug, Default)]
pub struct Skills {
    pub loaded: bool,
    pub error: Option<String>,
    pub rows: Vec<Skill>,
    pub query: String,
    /// The search field owns the keystrokes while true.
    pub focus: bool,
    pub status: Option<String>,
    pub tab: Tab,
    pub hub_query: String,
    pub hub_focus: bool,
    pub hub_loaded: bool,
    pub hub_error: Option<String>,
    pub hub_results: Vec<SkillHubResult>,
    pub preview: Option<SkillHubPreview>,
    pub scan: Option<SkillHubScan>,
    pub installing: Option<String>,
    pub hub_status: Option<String>,
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

    pub fn hub_started(&mut self) {
        self.hub_loaded = false;
        self.hub_error = None;
        self.hub_status = None;
        self.preview = None;
        self.scan = None;
    }

    pub fn hub_searched(&mut self, rows: Vec<SkillHubResult>) {
        self.hub_results = rows;
        self.hub_loaded = true;
        self.hub_error = None;
        self.hub_status = None;
        self.preview = None;
        self.scan = None;
    }

    pub fn hub_failed(&mut self, message: String) {
        self.hub_loaded = true;
        self.hub_error = Some(message);
        self.installing = None;
    }

    pub fn hub_previewed(&mut self, preview: SkillHubPreview, scan: Option<SkillHubScan>) {
        self.preview = Some(preview);
        self.scan = scan;
        self.hub_error = None;
        self.hub_status = None;
    }

    pub fn hub_installed(&mut self, identifier: &str) {
        self.installing = None;
        self.hub_status = Some(format!(
            "{identifier}: install started · the installed list refreshes when it finishes"
        ));
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

/// `community · github` — where a hub result comes from and how much to trust it.
pub fn hub_meta(result: &SkillHubResult) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !result.trust_level.is_empty() {
        parts.push(result.trust_level.clone());
    }
    if !result.source.is_empty() {
        parts.push(result.source.clone());
    }
    if let Some(repo) = result.repo.as_deref().filter(|repo| !repo.is_empty()) {
        parts.push(repo.to_string());
    }
    if parts.is_empty() {
        "hub".to_string()
    } else {
        parts.join(" · ")
    }
}

/// A blocked scan is never installable; an "ask" result needs the explicit
/// "Install anyway" click, and a missing scan is allowed (the host enforces
/// its own install policy).
pub fn can_install(scan: Option<&SkillHubScan>) -> bool {
    scan.is_none_or(|scan| scan.policy != "block")
}

pub fn scan_line(scan: &SkillHubScan) -> String {
    let reason = scan
        .policy_reason
        .as_deref()
        .filter(|reason| !reason.is_empty())
        .map(|reason| format!(" · {reason}"))
        .unwrap_or_default();
    format!(
        "{} · policy {} · {} finding(s){reason}",
        scan.verdict,
        scan.policy,
        scan.findings.len()
    )
}

pub fn finding_line(finding: &SkillHubFinding) -> String {
    let location = match finding.line {
        Some(line) => format!("{}:{line}", finding.file),
        None => finding.file.clone(),
    };
    format!(
        "{} · {} · {location} — {}",
        finding.severity, finding.category, finding.description
    )
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

    fn hub_result(name: &str, identifier: &str) -> SkillHubResult {
        SkillHubResult {
            name: name.to_string(),
            description: "GIF search".to_string(),
            source: "github".to_string(),
            identifier: identifier.to_string(),
            trust_level: "community".to_string(),
            repo: Some("owner/repo".to_string()),
            tags: vec!["gif".to_string()],
        }
    }

    fn scan(policy: &str) -> SkillHubScan {
        SkillHubScan {
            name: "gif-search".to_string(),
            identifier: "github/owner/repo/gif-search".to_string(),
            source: "github".to_string(),
            trust_level: "community".to_string(),
            verdict: "review".to_string(),
            summary: "one medium finding".to_string(),
            policy: policy.to_string(),
            policy_reason: Some("external source".to_string()),
            findings: vec![SkillHubFinding {
                severity: "medium".to_string(),
                category: "network".to_string(),
                file: "SKILL.md".to_string(),
                line: Some(12),
                description: "fetches a remote URL".to_string(),
            }],
            severity_counts: Default::default(),
        }
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

    #[test]
    fn hub_results_and_failures_replace_the_list() {
        let mut skills = Skills::default();
        skills.hub_started();
        assert!(!skills.hub_loaded);
        skills.hub_searched(vec![hub_result(
            "gif-search",
            "github/owner/repo/gif-search",
        )]);
        assert!(skills.hub_loaded);
        assert_eq!(skills.hub_results.len(), 1);
        assert_eq!(
            hub_meta(&skills.hub_results[0]),
            "community · github · owner/repo"
        );

        skills.hub_failed("hub offline".into());
        assert_eq!(skills.hub_error.as_deref(), Some("hub offline"));
        assert!(skills.hub_loaded);
    }

    #[test]
    fn blocked_scans_cannot_be_installed() {
        assert!(can_install(None));
        assert!(can_install(Some(&scan("allow"))));
        assert!(can_install(Some(&scan("ask"))));
        assert!(!can_install(Some(&scan("block"))));
        assert!(scan_line(&scan("ask")).contains("policy ask"));
        assert_eq!(
            finding_line(&scan("ask").findings[0]),
            "medium · network · SKILL.md:12 — fetches a remote URL"
        );
    }

    #[test]
    fn preview_and_install_progress_are_tracked() {
        let mut skills = Skills::default();
        skills.hub_previewed(
            SkillHubPreview {
                name: "gif-search".to_string(),
                identifier: "github/owner/repo/gif-search".to_string(),
                ..SkillHubPreview::default()
            },
            Some(scan("allow")),
        );
        assert!(skills.preview.is_some());
        assert!(skills.scan.is_some());
        skills.installing = Some("github/owner/repo/gif-search".to_string());
        skills.hub_installed("github/owner/repo/gif-search");
        assert!(skills.installing.is_none());
        assert!(skills.hub_status.unwrap().contains("install started"));
    }
}
