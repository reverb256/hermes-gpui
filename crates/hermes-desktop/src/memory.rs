//! The Memory page: the active provider, discovered backends, the size of the
//! built-in files, generic provider configuration and a confirmed reset.
//!
//! Provider fields come from the host's declared schema. The desktop renders
//! the same generic surface for every provider instead of hard-coding Honcho or
//! Hindsight forms; secrets stay write-only and blank means "leave existing".

use hermes_api::{
    MemoryFieldOption, MemoryProvider, MemoryProviderConfig, MemoryProviderField, MemoryStatus,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Secret,
    Boolean,
    Integer,
    Number,
    Select,
    Json,
}

impl Kind {
    fn parse(value: &str) -> Self {
        match value {
            "secret" => Self::Secret,
            "boolean" | "bool" => Self::Boolean,
            "integer" | "int" => Self::Integer,
            "number" | "float" => Self::Number,
            "select" => Self::Select,
            "json" => Self::Json,
            _ => Self::Text,
        }
    }

    pub fn is_text(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Secret | Self::Integer | Self::Number | Self::Json
        )
    }
}

#[derive(Debug, Clone)]
pub struct ValueField {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    pub description: String,
    pub placeholder: String,
    pub required: bool,
    pub is_set: bool,
    pub text: String,
    pub boolean: bool,
    pub selected: usize,
    pub options: Vec<MemoryFieldOption>,
    pub when: Option<Value>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub step: Option<f64>,
}

impl ValueField {
    fn from_field(field: &MemoryProviderField) -> Self {
        let kind = Kind::parse(&field.kind);
        let text = match &field.value {
            Value::String(value) => value.clone(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let boolean = field.value.as_bool().unwrap_or_else(|| {
            matches!(
                text.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
        let selected = field
            .options
            .iter()
            .position(|option| option.value == text)
            .unwrap_or(0);
        Self {
            key: field.key.clone(),
            label: if field.label.is_empty() {
                field.key.replace('_', " ")
            } else {
                field.label.clone()
            },
            kind,
            description: field.description.clone(),
            placeholder: field.placeholder.clone(),
            required: field.required,
            is_set: field.is_set,
            text,
            boolean,
            selected,
            options: field.options.clone(),
            when: field.when.clone(),
            minimum: field.minimum,
            maximum: field.maximum,
            step: field.step,
        }
    }

    /// The form value used for conditional visibility and submission, as
    /// opposed to the human-facing label returned by [`Self::display`].
    pub fn raw_value(&self) -> String {
        match self.kind {
            Kind::Boolean => self.boolean.to_string(),
            Kind::Select => self
                .options
                .get(self.selected)
                .map(|option| option.value.clone())
                .unwrap_or_else(|| self.text.clone()),
            _ => self.text.clone(),
        }
    }

    pub fn display(&self) -> String {
        match self.kind {
            Kind::Boolean => {
                if self.boolean {
                    "on".to_string()
                } else {
                    "off".to_string()
                }
            }
            Kind::Select => self
                .options
                .get(self.selected)
                .map(|option| {
                    if option.label.is_empty() {
                        option.value.clone()
                    } else {
                        option.label.clone()
                    }
                })
                .unwrap_or_default(),
            Kind::Secret if self.text.is_empty() && self.is_set => {
                "•••••••• (set; blank keeps it)".to_string()
            }
            _ => self.text.clone(),
        }
    }

    pub fn cycle(&mut self) {
        match self.kind {
            Kind::Boolean => self.boolean = !self.boolean,
            Kind::Select if !self.options.is_empty() => {
                self.selected = (self.selected + 1) % self.options.len();
            }
            _ => {}
        }
    }

    pub fn typed(&mut self, chars: &str) {
        if self.kind.is_text() {
            self.text.push_str(chars);
        }
    }

    pub fn backspace(&mut self) {
        if self.kind.is_text() {
            self.text.pop();
        }
    }
}

#[derive(Debug, Clone)]
pub struct Form {
    pub name: String,
    pub label: String,
    pub docs_url: String,
    pub fields: Vec<ValueField>,
    pub focus: Option<usize>,
    pub error: Option<String>,
}

impl Form {
    pub fn new(config: MemoryProviderConfig) -> Self {
        let fields = config
            .fields
            .iter()
            .map(ValueField::from_field)
            .collect::<Vec<_>>();
        let focus = (!fields.is_empty()).then_some(0);
        Self {
            name: config.name.clone(),
            label: if config.label.is_empty() {
                config.name
            } else {
                config.label
            },
            docs_url: config.docs_url,
            fields,
            focus,
            error: None,
        }
    }

    pub fn visible(&self, index: usize) -> bool {
        let Some(field) = self.fields.get(index) else {
            return false;
        };
        let Some(when) = field.when.as_ref().and_then(Value::as_object) else {
            return true;
        };
        when.iter().all(|(key, expected)| {
            let actual = self
                .fields
                .iter()
                .find(|candidate| &candidate.key == key)
                .map(ValueField::raw_value)
                .unwrap_or_default();
            let expected = match expected {
                Value::String(value) => value.clone(),
                Value::Bool(value) => value.to_string(),
                Value::Number(value) => value.to_string(),
                other => other.to_string(),
            };
            actual == expected
        })
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|index| self.visible(*index))
            .collect()
    }

    pub fn focus_next(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            self.focus = None;
            return;
        }
        let next = match self.focus {
            Some(current) => visible
                .iter()
                .position(|index| *index == current)
                .map(|position| visible[(position + 1) % visible.len()])
                .or(Some(visible[0])),
            None => Some(visible[0]),
        };
        self.focus = next;
    }

    pub fn focus_field(&mut self, index: usize) {
        if self.fields.get(index).is_some_and(|_| self.visible(index)) {
            self.focus = Some(index);
        }
    }

    pub fn typed(&mut self, chars: &str) {
        if let Some(field) = self.focus.and_then(|index| self.fields.get_mut(index)) {
            field.typed(chars);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(field) = self.focus.and_then(|index| self.fields.get_mut(index)) {
            field.backspace();
        }
    }

    pub fn cycle_focused(&mut self) {
        if let Some(field) = self.focus.and_then(|index| self.fields.get_mut(index)) {
            field.cycle();
        }
    }

    /// Values for `PUT /api/memory/providers/{name}/config`. Hidden fields are
    /// omitted so a mode switch cannot submit stale values for the other mode.
    pub fn values(&self) -> Result<Value, String> {
        let mut values = Map::new();
        for (index, field) in self.fields.iter().enumerate() {
            if !self.visible(index) {
                continue;
            }
            if field.kind == Kind::Secret {
                let text = field.text.trim();
                if !text.is_empty() {
                    values.insert(field.key.clone(), json!(text));
                } else if field.required && !field.is_set {
                    return Err(format!("{} is required", field.label));
                }
                continue;
            }
            if field.required && field.kind != Kind::Boolean && field.text.trim().is_empty() {
                return Err(format!("{} is required", field.label));
            }
            let value = match field.kind {
                Kind::Boolean => json!(field.boolean),
                Kind::Integer => {
                    let text = field.text.trim();
                    if text.is_empty() {
                        json!("")
                    } else {
                        let parsed = text
                            .parse::<i64>()
                            .map_err(|_| format!("{} must be an integer", field.label))?;
                        check_range(field, parsed as f64)?;
                        json!(parsed)
                    }
                }
                Kind::Number => {
                    let text = field.text.trim();
                    if text.is_empty() {
                        json!("")
                    } else {
                        let parsed = text
                            .parse::<f64>()
                            .map_err(|_| format!("{} must be a number", field.label))?;
                        check_range(field, parsed)?;
                        json!(parsed)
                    }
                }
                Kind::Select => {
                    let value = field
                        .options
                        .get(field.selected)
                        .map(|option| option.value.clone())
                        .unwrap_or_else(|| field.text.clone());
                    if field.required && value.is_empty() {
                        return Err(format!("{} is required", field.label));
                    }
                    json!(value)
                }
                Kind::Text | Kind::Secret | Kind::Json => json!(field.text),
            };
            values.insert(field.key.clone(), value);
        }
        Ok(Value::Object(values))
    }
}

fn check_range(field: &ValueField, value: f64) -> Result<(), String> {
    if let Some(minimum) = field.minimum
        && value < minimum
    {
        return Err(format!("{} must be at least {minimum}", field.label));
    }
    if let Some(maximum) = field.maximum
        && value > maximum
    {
        return Err(format!("{} must be at most {maximum}", field.label));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct Memory {
    pub loaded: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub active: String,
    pub providers: Vec<MemoryProvider>,
    pub memory_bytes: u64,
    pub user_bytes: u64,
    pub form: Option<Form>,
    pub config_loading: bool,
    pub confirm_reset: Option<String>,
}

impl Memory {
    pub fn loaded(&mut self, status: MemoryStatus) {
        self.active = status.active;
        self.providers = status.providers;
        self.memory_bytes = status.builtin_files.memory;
        self.user_bytes = status.builtin_files.user;
        self.status = Some(if self.active.is_empty() {
            "builtin memory".to_string()
        } else {
            format!("active: {}", self.active)
        });
        self.error = None;
        self.loaded = true;
        self.config_loading = false;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
        self.config_loading = false;
    }

    pub fn begin_config(&mut self, name: &str) {
        self.config_loading = true;
        self.status = Some(format!("loading {name}…"));
        self.confirm_reset = None;
    }

    pub fn config_loaded(&mut self, config: MemoryProviderConfig) {
        if config.fields.is_empty() {
            self.status = Some(format!("{} has no configurable fields", config.name));
            self.config_loading = false;
            self.form = None;
            return;
        }
        self.form = Some(Form::new(config));
        self.config_loading = false;
        self.status = None;
    }

    pub fn config_failed(&mut self, message: String) {
        if let Some(form) = self.form.as_mut() {
            form.error = Some(message);
        } else {
            self.status = Some(message);
        }
        self.config_loading = false;
    }

    pub fn saved(&mut self, name: &str) {
        self.form = None;
        self.status = Some(format!("{name} saved"));
    }
}

/// A provider is selectable only when the host says it is ready.
pub fn can_activate(provider: &MemoryProvider, active: &str) -> bool {
    provider.status == "ready"
        && provider.configured
        && provider.available
        && provider.name != active
}

pub fn provider_status(provider: &MemoryProvider) -> String {
    if provider.status.is_empty() {
        "unknown".to_string()
    } else {
        provider.status.replace('_', " ")
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub fn files_summary(memory: u64, user: u64) -> String {
    format!(
        "MEMORY.md {} · USER.md {}",
        human_bytes(memory),
        human_bytes(user)
    )
}

pub fn reset_label(target: &str) -> &'static str {
    match target {
        "memory" => "Reset MEMORY.md",
        "user" => "Reset USER.md",
        _ => "Reset both files",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &str, status: &str, available: bool, configured: bool) -> MemoryProvider {
        MemoryProvider {
            name: name.to_string(),
            status: status.to_string(),
            available,
            configured,
            ..MemoryProvider::default()
        }
    }

    #[test]
    fn only_ready_non_active_providers_are_selectable() {
        let ready = provider("honcho", "ready", true, true);
        assert!(can_activate(&ready, "builtin"));
        assert!(!can_activate(&ready, "honcho"));
        assert!(!can_activate(
            &provider("mem0", "needs_config", true, false),
            "builtin"
        ));
        assert!(!can_activate(
            &provider("supermemory", "unavailable", false, true),
            "builtin"
        ));
    }

    #[test]
    fn statuses_are_human_readable() {
        assert_eq!(
            provider_status(&provider("mem0", "needs_config", true, false)),
            "needs config"
        );
        assert_eq!(provider_status(&provider("x", "", true, true)), "unknown");
    }

    #[test]
    fn file_sizes_use_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
        assert_eq!(files_summary(0, 2048), "MEMORY.md 0 B · USER.md 2.0 KiB");
    }

    #[test]
    fn loading_and_failure_replace_state() {
        let mut memory = Memory::default();
        memory.loaded(MemoryStatus {
            active: "honcho".into(),
            providers: vec![provider("honcho", "ready", true, true)],
            builtin_files: hermes_api::MemoryFiles {
                memory: 10,
                user: 20,
            },
        });
        assert!(memory.loaded);
        assert_eq!(memory.active, "honcho");
        assert_eq!(memory.status.as_deref(), Some("active: honcho"));
        assert_eq!(memory.providers.len(), 1);
        assert_eq!(memory.memory_bytes, 10);
        assert_eq!(memory.user_bytes, 20);
        memory.failed("offline".into());
        assert!(memory.error.is_some());
    }

    #[test]
    fn forms_convert_typed_values_and_keep_secrets_blank() {
        let config = MemoryProviderConfig {
            name: "hindsight".into(),
            label: "Hindsight".into(),
            fields: vec![
                MemoryProviderField {
                    key: "mode".into(),
                    label: "mode".into(),
                    kind: "select".into(),
                    value: json!("cloud"),
                    options: vec![
                        MemoryFieldOption {
                            value: "cloud".into(),
                            label: "Cloud".into(),
                            ..MemoryFieldOption::default()
                        },
                        MemoryFieldOption {
                            value: "local".into(),
                            label: "Local".into(),
                            ..MemoryFieldOption::default()
                        },
                    ],
                    ..MemoryProviderField::default()
                },
                MemoryProviderField {
                    key: "api_key".into(),
                    kind: "secret".into(),
                    is_set: true,
                    required: true,
                    ..MemoryProviderField::default()
                },
                MemoryProviderField {
                    key: "recall_limit".into(),
                    kind: "integer".into(),
                    value: json!(12),
                    minimum: Some(1.0),
                    maximum: Some(100.0),
                    ..MemoryProviderField::default()
                },
                MemoryProviderField {
                    key: "resources".into(),
                    kind: "boolean".into(),
                    value: json!(false),
                    ..MemoryProviderField::default()
                },
            ],
            ..MemoryProviderConfig::default()
        };
        let mut form = Form::new(config);
        form.fields[1].text.clear();
        form.fields[2].text = "20".into();
        form.fields[3].boolean = true;
        let values = form.values().unwrap();
        assert_eq!(values["mode"], "cloud");
        assert!(values.get("api_key").is_none());
        assert_eq!(values["recall_limit"], 20);
        assert_eq!(values["resources"], true);

        form.fields[2].text = "101".into();
        assert!(form.values().unwrap_err().contains("at most"));
    }

    #[test]
    fn hidden_fields_are_not_submitted() {
        let config = MemoryProviderConfig {
            name: "hindsight".into(),
            fields: vec![
                MemoryProviderField {
                    key: "mode".into(),
                    kind: "select".into(),
                    value: json!("cloud"),
                    options: vec![
                        MemoryFieldOption {
                            value: "cloud".into(),
                            label: "Cloud".into(),
                            ..MemoryFieldOption::default()
                        },
                        MemoryFieldOption {
                            value: "local".into(),
                            label: "Local".into(),
                            ..MemoryFieldOption::default()
                        },
                    ],
                    ..MemoryProviderField::default()
                },
                MemoryProviderField {
                    key: "local_url".into(),
                    kind: "text".into(),
                    value: json!("http://localhost"),
                    when: Some(json!({"mode": "local"})),
                    ..MemoryProviderField::default()
                },
            ],
            ..MemoryProviderConfig::default()
        };
        let mut form = Form::new(config);
        assert!(!form.visible(1));
        assert!(form.values().unwrap().get("local_url").is_none());
        form.fields[0].selected = 1;
        assert!(form.visible(1));
        assert_eq!(form.values().unwrap()["local_url"], "http://localhost");
    }

    #[test]
    fn secret_required_only_when_not_already_set() {
        let config = MemoryProviderConfig {
            name: "mem0".into(),
            fields: vec![MemoryProviderField {
                key: "api_key".into(),
                kind: "secret".into(),
                required: true,
                ..MemoryProviderField::default()
            }],
            ..MemoryProviderConfig::default()
        };
        let form = Form::new(config);
        assert!(form.values().unwrap_err().contains("required"));
    }
}
