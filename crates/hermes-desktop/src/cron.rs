//! Cron automation state: list, create form, actions and run history.
//!
//! The host owns schedule parsing and validation; the desktop only prevents
//! obviously empty submissions and shows the host's error text verbatim.

use hermes_api::{CronDeliveryTarget, CronJob, SessionSummary};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Schedule,
    Prompt,
    Deliver,
}

impl Field {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Schedule,
            Self::Schedule => Self::Prompt,
            Self::Prompt => Self::Deliver,
            Self::Deliver => Self::Name,
        }
    }
}

#[derive(Debug, Default)]
pub struct Form {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    pub deliver: String,
    pub focus: Option<Field>,
    pub error: Option<String>,
}

impl Form {
    pub fn body(&self) -> Result<Value, String> {
        if self.schedule.trim().is_empty() {
            return Err("A schedule is required (for example: every day at 9am).".into());
        }
        if self.prompt.trim().is_empty() {
            return Err("A prompt is required.".into());
        }
        Ok(json!({
            "name": self.name.trim(),
            "schedule": self.schedule.trim(),
            "prompt": self.prompt.trim(),
            "deliver": if self.deliver.trim().is_empty() { "local" } else { self.deliver.trim() },
        }))
    }

    pub fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Name => &mut self.name,
            Field::Schedule => &mut self.schedule,
            Field::Prompt => &mut self.prompt,
            Field::Deliver => &mut self.deliver,
        }
    }
}

#[derive(Debug, Default)]
pub struct Automations {
    pub loaded: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub jobs: Vec<CronJob>,
    pub targets: Vec<CronDeliveryTarget>,
    pub runs: Vec<SessionSummary>,
    pub runs_for: Option<String>,
    pub runs_loaded: bool,
    pub creating: bool,
    pub form: Form,
    pub confirm_delete: Option<String>,
}

impl Automations {
    pub fn loaded(&mut self, jobs: Vec<CronJob>, targets: Vec<CronDeliveryTarget>) {
        self.jobs = jobs;
        self.targets = targets;
        if self.form.deliver.is_empty() {
            self.form.deliver = "local".into();
        }
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }

    pub fn runs_loaded(&mut self, id: String, runs: Vec<SessionSummary>) {
        self.runs = runs;
        self.runs_for = Some(id);
        self.runs_loaded = true;
    }

    pub fn begin_create(&mut self) {
        self.creating = true;
        self.form = Form {
            deliver: "local".into(),
            focus: Some(Field::Schedule),
            ..Form::default()
        };
        self.confirm_delete = None;
        self.status = None;
    }

    pub fn cancel_create(&mut self) {
        self.creating = false;
        self.form = Form::default();
    }

    pub fn cycle_delivery(&mut self) {
        let mut options: Vec<String> = self
            .targets
            .iter()
            .map(|target| target.id.clone())
            .filter(|id| !id.is_empty())
            .collect();
        if options.is_empty() {
            options.push("local".into());
        }
        let index = options
            .iter()
            .position(|id| *id == self.form.deliver)
            .unwrap_or(0);
        self.form.deliver = options[(index + 1) % options.len()].clone();
    }

    pub fn delivery_label(&self, id: &str) -> String {
        self.targets
            .iter()
            .find(|target| target.id == id)
            .map(|target| target.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                if id.is_empty() {
                    "local".into()
                } else {
                    id.to_string()
                }
            })
    }

    pub fn confirm_delete(&mut self, id: &str) -> bool {
        if self.confirm_delete.as_deref() == Some(id) {
            self.confirm_delete = None;
            true
        } else {
            self.confirm_delete = Some(id.to_string());
            self.status = Some(format!("click Delete again to remove {id}"));
            false
        }
    }
}

/// Compact status line for one job.
pub fn job_meta(job: &CronJob) -> String {
    let mut parts = Vec::new();
    if !job.schedule_display.is_empty() {
        parts.push(job.schedule_display.clone());
    } else if !job.schedule.is_empty() {
        parts.push(job.schedule.clone());
    }
    if !job.state.is_empty() {
        parts.push(job.state.clone());
    }
    if let Some(status) = job
        .last_status
        .as_deref()
        .filter(|status| !status.is_empty())
    {
        parts.push(format!("last {status}"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_requires_schedule_and_prompt() {
        let mut form = Form::default();
        assert!(form.body().is_err());
        form.schedule = "every day at 9am".into();
        form.prompt = "summarise cluster health".into();
        let body = form.body().expect("valid body");
        assert_eq!(body["deliver"], "local");
        assert_eq!(body["prompt"], "summarise cluster health");
    }

    #[test]
    fn delivery_cycles_through_host_targets() {
        let mut page = Automations {
            targets: vec![
                CronDeliveryTarget {
                    id: "local".into(),
                    name: "Local".into(),
                    home_target_set: true,
                    home_env_var: None,
                },
                CronDeliveryTarget {
                    id: "telegram".into(),
                    name: "Telegram".into(),
                    home_target_set: true,
                    home_env_var: None,
                },
            ],
            ..Automations::default()
        };
        page.form.deliver = "local".into();
        page.cycle_delivery();
        assert_eq!(page.form.deliver, "telegram");
        page.cycle_delivery();
        assert_eq!(page.form.deliver, "local");
        assert_eq!(page.delivery_label("telegram"), "Telegram");
    }

    #[test]
    fn delete_needs_two_clicks() {
        let mut page = Automations::default();
        assert!(!page.confirm_delete("job-1"));
        assert_eq!(page.confirm_delete.as_deref(), Some("job-1"));
        assert!(page.confirm_delete("job-1"));
        assert!(page.confirm_delete.is_none());
    }
}
