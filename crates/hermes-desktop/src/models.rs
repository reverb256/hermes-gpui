//! The model picker: which provider and model new sessions start with.
//!
//! `/api/model/options` lists the authenticated providers and their curated
//! model ids; `POST /api/model/set` writes `config.yaml` for *new* sessions
//! only. The page says that rather than implying a live chat hot-swaps.

use hermes_api::{AuxiliaryTask, ModelOptions};

/// Hermes' reasoning scale; `""` means inherit the server default.
pub const REASONING_LEVELS: [&str; 8] = [
    "", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

#[derive(Debug, Default)]
pub struct Models {
    pub loaded: bool,
    pub error: Option<String>,
    pub options: Option<ModelOptions>,
    pub query: String,
    /// The search field owns the keystrokes while true.
    pub focus: bool,
    pub status: Option<String>,
    pub auxiliary: Vec<AuxiliaryTask>,
    pub aux_loaded: bool,
    pub aux_error: Option<String>,
    /// Set while the catalog is picking a model for this auxiliary task.
    pub aux_target: Option<String>,
    /// Raw `agent.reasoning_effort`; empty inherits.
    pub reasoning: String,
}

impl Models {
    pub fn loaded(&mut self, options: ModelOptions) {
        self.options = Some(options);
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }

    pub fn auxiliary_loaded(&mut self, tasks: Vec<AuxiliaryTask>) {
        self.auxiliary = tasks;
        self.aux_error = None;
        self.aux_loaded = true;
    }

    pub fn auxiliary_failed(&mut self, message: String) {
        self.aux_error = Some(message);
        self.aux_loaded = true;
    }

    pub fn reasoning_loaded(&mut self, level: String) {
        self.reasoning = level;
    }
}

/// Friendly names for the backend's task slots.
pub fn task_label(task: &str) -> String {
    match task {
        "vision" => "Vision".to_string(),
        "web_extract" => "Web extract".to_string(),
        "compression" => "Compression".to_string(),
        "skills_hub" => "Skills hub".to_string(),
        "approval" => "Approval".to_string(),
        "mcp" => "MCP".to_string(),
        "title_generation" => "Titles".to_string(),
        "review" => "Review".to_string(),
        "voice_chat" => "Voice chat".to_string(),
        "triage_specifier" => "Triage specifier".to_string(),
        "kanban_decomposer" => "Kanban decomposer".to_string(),
        "profile_describer" => "Profile describer".to_string(),
        "curator" => "Curator".to_string(),
        other => other.to_string(),
    }
}

/// `provider · model`, or the `auto` pin that means "use the main model".
pub fn aux_line(task: &AuxiliaryTask) -> String {
    let provider = task.provider.trim();
    if provider.is_empty() || provider == "auto" || task.model.trim().is_empty() {
        "Follows main".to_string()
    } else {
        format!("{provider} · {}", task.model)
    }
}

pub fn reasoning_label(level: &str) -> String {
    match level {
        "" => "Default".to_string(),
        "minimal" => "Minimal".to_string(),
        "low" => "Low".to_string(),
        "medium" => "Medium".to_string(),
        "high" => "High".to_string(),
        "xhigh" => "XHigh".to_string(),
        "max" => "Max".to_string(),
        "ultra" => "Ultra".to_string(),
        other => other.to_string(),
    }
}

/// One selectable model, flattened out of its provider.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub current: bool,
    /// The provider owning the active model, pinned ahead of the rest.
    pub provider_current: bool,
}

/// Provider-grouped rows, filtered by provider name/slug or model id. The
/// current provider leads, and the current model leads inside it, so the page
/// opens on what the host is actually running.
pub fn rows(options: Option<&ModelOptions>, query: &str) -> Vec<Row> {
    let Some(options) = options else {
        return Vec::new();
    };
    let needle = query.trim().to_lowercase();
    let mut out: Vec<Row> = Vec::new();
    for provider in &options.providers {
        let provider_hit = needle.is_empty()
            || provider.name.to_lowercase().contains(&needle)
            || provider.slug.to_lowercase().contains(&needle);
        let current_provider = provider.is_current || provider.slug == options.provider;
        for model in &provider.models {
            if !provider_hit && !model.to_lowercase().contains(&needle) {
                continue;
            }
            out.push(Row {
                provider: provider.slug.clone(),
                provider_name: provider.name.clone(),
                model: model.clone(),
                current: current_provider && *model == options.model,
                provider_current: current_provider,
            });
        }
    }
    out.sort_by(|a, b| {
        b.provider_current
            .cmp(&a.provider_current)
            .then_with(|| {
                a.provider_name
                    .to_lowercase()
                    .cmp(&b.provider_name.to_lowercase())
            })
            .then_with(|| b.current.cmp(&a.current))
            .then_with(|| a.model.to_lowercase().cmp(&b.model.to_lowercase()))
    });
    out
}

/// `provider · model`, or an em dash when the catalog has not answered yet.
pub fn current_line(options: Option<&ModelOptions>) -> String {
    match options {
        Some(options) if !options.model.is_empty() && !options.provider.is_empty() => {
            format!("{} · {}", options.provider, options.model)
        }
        Some(options) if !options.model.is_empty() => options.model.clone(),
        _ => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_api::ModelOptionProvider;

    fn provider(slug: &str, name: &str, models: &[&str], current: bool) -> ModelOptionProvider {
        ModelOptionProvider {
            slug: slug.to_string(),
            name: name.to_string(),
            models: models.iter().map(|model| model.to_string()).collect(),
            is_current: current,
            authenticated: true,
            ..ModelOptionProvider::default()
        }
    }

    fn catalog() -> ModelOptions {
        ModelOptions {
            provider: "nous".to_string(),
            model: "beam".to_string(),
            providers: vec![
                provider("nous", "Nous Portal", &["zeta", "beam", "alpha"], true),
                provider("kilo", "Kilo", &["kilo-coder"], false),
            ],
        }
    }

    #[test]
    fn current_model_sorts_first_within_its_provider() {
        let rows = rows(Some(&catalog()), "");
        assert_eq!(rows[0].provider, "nous");
        assert_eq!(rows[0].model, "beam");
        assert!(rows[0].current);
        assert!(rows[1..].iter().all(|row| !row.current));
        // Providers stay contiguous and sorted so the headers group cleanly.
        assert_eq!(rows.iter().filter(|row| row.provider == "kilo").count(), 1);
        assert!(
            rows.windows(2)
                .filter(|pair| pair[0].provider_current == pair[1].provider_current)
                .all(|pair| pair[0].provider_name <= pair[1].provider_name)
        );
    }

    #[test]
    fn search_matches_model_or_provider_name() {
        let catalog = catalog();
        assert_eq!(rows(Some(&catalog), "kilo").len(), 1);
        assert_eq!(rows(Some(&catalog), "portal").len(), 3);
        assert_eq!(rows(Some(&catalog), "alpha").len(), 1);
        assert!(rows(Some(&catalog), "nope").is_empty());
        assert!(rows(None, "").is_empty());
    }

    #[test]
    fn current_line_reads_provider_then_model() {
        assert_eq!(current_line(Some(&catalog())), "nous · beam");
        let model_only = ModelOptions {
            model: "solo".to_string(),
            ..ModelOptions::default()
        };
        assert_eq!(current_line(Some(&model_only)), "solo");
        assert_eq!(current_line(None), "—");
    }

    #[test]
    fn auxiliary_labels_and_lines_are_human() {
        let auto = AuxiliaryTask {
            task: "vision".to_string(),
            provider: "auto".to_string(),
            ..AuxiliaryTask::default()
        };
        assert_eq!(task_label("vision"), "Vision");
        assert_eq!(task_label("profile_describer"), "Profile describer");
        assert_eq!(task_label("custom_slot"), "custom_slot");
        assert_eq!(aux_line(&auto), "Follows main");
        let pinned = AuxiliaryTask {
            task: "vision".to_string(),
            provider: "nous".to_string(),
            model: "beam".to_string(),
            ..AuxiliaryTask::default()
        };
        assert_eq!(aux_line(&pinned), "nous · beam");
        assert_eq!(reasoning_label(""), "Default");
        assert_eq!(reasoning_label("xhigh"), "XHigh");
        assert!(REASONING_LEVELS.contains(&"ultra"));
    }

    #[test]
    fn failures_are_a_loaded_state() {
        let mut models = Models::default();
        models.failed("provider catalog unreachable".into());
        assert!(models.loaded);
        assert!(models.error.is_some());
        models.loaded(catalog());
        assert!(models.error.is_none());
    }
}
