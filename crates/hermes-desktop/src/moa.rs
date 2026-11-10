//! Mixture-of-Agents editor state.
//!
//! The host stores named presets and rejects partial slots with 422. The page
//! edits one preset at a time but always saves the whole map, so switching
//! presets cannot drop the others.

use hermes_api::{MoaConfig, MoaPreset, MoaSlot};

pub const FANOUT_MODES: [&str; 3] = ["user_turn", "per_iteration", "every_n:3"];
pub const DEGRADED_POLICIES: [&str; 2] = ["loud", "silent"];

/// Which slot the shared model picker is filling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Aggregator,
    Reference(usize),
    NewReference,
}

#[derive(Debug, Default)]
pub struct Moa {
    pub loaded: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub config: Option<MoaConfig>,
    pub draft: Option<MoaConfig>,
    pub preset: String,
    pub picking: Option<Pick>,
    pub dirty: bool,
}

impl Moa {
    pub fn loaded(&mut self, config: MoaConfig) {
        self.preset = config
            .selected_preset()
            .unwrap_or_else(|| "default".to_string());
        self.config = Some(config.clone());
        self.draft = Some(config);
        self.error = None;
        self.status = None;
        self.loaded = true;
        self.dirty = false;
    }

    pub fn saved(&mut self, config: MoaConfig) {
        self.loaded(config);
        self.status = Some("MoA saved".into());
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }

    pub fn current_mut(&mut self) -> Option<&mut MoaPreset> {
        self.draft.as_mut()?.presets.get_mut(&self.preset)
    }

    pub fn cycle_preset(&mut self) {
        let Some(config) = self.draft.as_ref() else {
            return;
        };
        if config.presets.is_empty() {
            return;
        }
        let mut names: Vec<&String> = config.presets.keys().collect();
        let index = names
            .iter()
            .position(|name| **name == self.preset)
            .unwrap_or(usize::MAX);
        let next = names
            .get(index.wrapping_add(1))
            .or_else(|| names.first())
            .map(|name| (*name).clone());
        if let Some(next) = next {
            self.preset = next;
            self.status = None;
        }
        names.clear();
    }

    pub fn toggle_enabled(&mut self) {
        if let Some(preset) = self.current_mut() {
            preset.enabled = !preset.enabled;
            self.dirty = true;
        }
    }

    pub fn cycle_fanout(&mut self) {
        if let Some(preset) = self.current_mut() {
            let index = FANOUT_MODES
                .iter()
                .position(|mode| *mode == preset.fanout)
                .unwrap_or(0);
            preset.fanout = FANOUT_MODES[(index + 1) % FANOUT_MODES.len()].to_string();
            self.dirty = true;
        }
    }

    pub fn cycle_policy(&mut self) {
        if let Some(preset) = self.current_mut() {
            let index = DEGRADED_POLICIES
                .iter()
                .position(|policy| *policy == preset.degraded_reference_policy)
                .unwrap_or(0);
            preset.degraded_reference_policy =
                DEGRADED_POLICIES[(index + 1) % DEGRADED_POLICIES.len()].to_string();
            self.dirty = true;
        }
    }

    pub fn nudge_max_tokens(&mut self, delta: i64) {
        if let Some(preset) = self.current_mut() {
            preset.max_tokens = (preset.max_tokens as i64 + delta).clamp(128, 128_000) as u64;
            self.dirty = true;
        }
    }

    pub fn nudge_reference_max_tokens(&mut self, delta: i64) {
        if let Some(preset) = self.current_mut() {
            let current = preset.reference_max_tokens.unwrap_or(600) as i64;
            preset.reference_max_tokens = Some((current + delta).clamp(64, 64_000) as u64);
            self.dirty = true;
        }
    }

    pub fn clear_reference_max_tokens(&mut self) {
        if let Some(preset) = self.current_mut() {
            preset.reference_max_tokens = None;
            self.dirty = true;
        }
    }

    pub fn nudge_temperature(&mut self, aggregator: bool, delta: f64) {
        if let Some(preset) = self.current_mut() {
            let slot = if aggregator {
                &mut preset.aggregator_temperature
            } else {
                &mut preset.reference_temperature
            };
            let current = slot.unwrap_or(0.7);
            *slot = Some(((current + delta).clamp(0.0, 2.0) * 10.0).round() / 10.0);
            self.dirty = true;
        }
    }

    pub fn begin_pick(&mut self, pick: Pick) {
        self.picking = Some(pick);
        self.status = Some("choose a model".into());
    }

    pub fn apply_pick(&mut self, provider: &str, model: &str) {
        let Some(pick) = self.picking.take() else {
            return;
        };
        let Some(preset) = self.current_mut() else {
            return;
        };
        let slot = MoaSlot {
            provider: provider.to_string(),
            model: model.to_string(),
            enabled: true,
            ..MoaSlot::default()
        };
        match pick {
            Pick::Aggregator => preset.aggregator = slot,
            Pick::Reference(index) => {
                if let Some(existing) = preset.reference_models.get_mut(index) {
                    let reasoning_effort = existing.reasoning_effort.clone();
                    *existing = MoaSlot {
                        reasoning_effort,
                        ..slot
                    };
                }
            }
            Pick::NewReference => preset.reference_models.push(slot),
        }
        self.dirty = true;
        self.status = Some(format!("{provider} · {model} selected"));
    }

    pub fn remove_reference(&mut self, index: usize) {
        if let Some(preset) = self.current_mut()
            && index < preset.reference_models.len()
        {
            preset.reference_models.remove(index);
            self.dirty = true;
        }
    }

    pub fn make_default(&mut self) {
        if let Some(config) = self.draft.as_mut()
            && config.presets.contains_key(&self.preset)
        {
            config.default_preset = self.preset.clone();
            self.dirty = true;
        }
    }

    pub fn toggle_active(&mut self) {
        if let Some(config) = self.draft.as_mut() {
            if config.active_preset == self.preset {
                config.active_preset.clear();
            } else {
                config.active_preset = self.preset.clone();
            }
            self.dirty = true;
        }
    }

    /// Payload for `PUT /api/model/moa`. Rejects incomplete slots locally so
    /// the user sees which row is wrong instead of a generic 422.
    pub fn body(&self) -> Result<MoaConfig, String> {
        let Some(draft) = self.draft.clone() else {
            return Err("MoA config is not loaded".into());
        };
        if draft.presets.is_empty() {
            return Err("MoA has no presets to save".into());
        }
        if !draft.presets.contains_key(&draft.default_preset) {
            return Err("default preset is missing".into());
        }
        for (name, preset) in &draft.presets {
            if preset.reference_models.is_empty() {
                return Err(format!("{name}: at least one reference model is required"));
            }
            for (index, slot) in preset.reference_models.iter().enumerate() {
                check_slot(&format!("{name} reference {}", index + 1), slot)?;
            }
            check_slot(&format!("{name} aggregator"), &preset.aggregator)?;
        }
        Ok(draft)
    }

    pub fn mark_saving(&mut self) {
        self.status = Some("saving MoA…".into());
        self.error = None;
    }
}

fn check_slot(label: &str, slot: &MoaSlot) -> Result<(), String> {
    if slot.provider.trim().is_empty() {
        return Err(format!("{label}: provider is required"));
    }
    if slot.model.trim().is_empty() {
        return Err(format!("{label}: model is required"));
    }
    if slot.provider.eq_ignore_ascii_case("moa") {
        return Err(format!("{label}: MoA cannot be nested inside a preset"));
    }
    Ok(())
}

pub fn slot_label(slot: &MoaSlot) -> String {
    if slot.provider.is_empty() && slot.model.is_empty() {
        "not set".to_string()
    } else {
        format!("{} · {}", slot.provider, slot.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(provider: &str, model: &str) -> MoaSlot {
        MoaSlot {
            provider: provider.into(),
            model: model.into(),
            enabled: true,
            ..MoaSlot::default()
        }
    }

    fn config() -> MoaConfig {
        let preset = MoaPreset {
            reference_models: vec![slot("openai", "gpt")],
            aggregator: slot("openrouter", "opus"),
            ..MoaPreset::default()
        };
        MoaConfig {
            default_preset: "default".into(),
            active_preset: String::new(),
            presets: [("default".to_string(), preset)].into_iter().collect(),
            ..MoaConfig::default()
        }
    }

    #[test]
    fn saved_payload_keeps_every_preset() {
        let mut edit = Moa::default();
        edit.loaded(config());
        edit.begin_pick(Pick::NewReference);
        edit.apply_pick("openai", "gpt-2");
        let body = edit.body().expect("complete payload");
        assert_eq!(body.presets["default"].reference_models.len(), 2);
        assert_eq!(body.presets["default"].aggregator.model, "opus");
    }

    #[test]
    fn incomplete_reference_is_rejected_locally() {
        let mut edit = Moa::default();
        edit.loaded(config());
        edit.begin_pick(Pick::NewReference);
        edit.apply_pick("", "gpt-2");
        assert!(edit.body().unwrap_err().contains("provider"));
    }

    #[test]
    fn toggling_active_and_default_updates_the_draft() {
        let mut edit = Moa::default();
        edit.loaded(config());
        edit.toggle_active();
        assert_eq!(edit.draft.as_ref().unwrap().active_preset, "default");
        edit.toggle_active();
        assert_eq!(edit.draft.as_ref().unwrap().active_preset, "");
    }
}
