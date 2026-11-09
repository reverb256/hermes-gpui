//! Usage & Billing: what the host actually spent, and where billing lives.
//!
//! The dashboard serves `/api/analytics/usage` (token and cost rollups),
//! `/api/model/info` (the model this profile is running) and `/api/portal`
//! (account tier plus the subscription URL). Billing mutations on the reference
//! desktop run over the gateway (`billing.state`), not REST, so this page
//! reports usage and links out to the portal instead of pretending to manage
//! payments.
//!
//! A host can answer the analytics route with a 503 when its `state.db` is
//! unreadable. That is a first-class state here: the page shows the server's own
//! explanation rather than an empty chart.

use hermes_api::{ModelInfo, PortalStatus, UsageAnalytics, UsageModel};

#[derive(Debug, Default)]
pub struct Usage {
    pub days: u32,
    pub loaded: bool,
    pub error: Option<String>,
    pub analytics: Option<UsageAnalytics>,
    pub model: Option<ModelInfo>,
    pub portal: Option<PortalStatus>,
}

impl Usage {
    pub fn loaded(
        &mut self,
        analytics: UsageAnalytics,
        model: Option<ModelInfo>,
        portal: Option<PortalStatus>,
    ) {
        self.analytics = Some(analytics);
        if model.is_some() {
            self.model = model;
        }
        if portal.is_some() {
            self.portal = portal;
        }
        self.error = None;
        self.loaded = true;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
    }
}

/// `$0.00`, `$0.0042` for sub-cent amounts, `$12.34` otherwise.
pub fn money(usd: f64) -> String {
    if !usd.is_finite() {
        return "—".to_string();
    }
    if usd == 0.0 {
        return "$0.00".to_string();
    }
    if usd.abs() < 0.01 {
        return format!("${usd:.4}");
    }
    format!("${usd:.2}")
}

/// Compact token counts; `None` means the host did not report the figure.
pub fn tokens(count: Option<u64>) -> String {
    match count {
        None => "—".to_string(),
        Some(0) => "0".to_string(),
        Some(count) if count >= 1_000_000 => format!("{:.1}M", count as f64 / 1_000_000.),
        Some(count) if count >= 1_000 => format!("{:.1}K", count as f64 / 1_000.),
        Some(count) => count.to_string(),
    }
}

/// Context window as a short label (`262K context`).
pub fn context_label(length: u64) -> String {
    if length == 0 {
        return "—".to_string();
    }
    if length >= 1_000_000 {
        format!("{:.0}M context", length as f64 / 1_000_000.)
    } else {
        format!("{:.0}K context", length as f64 / 1_000.)
    }
}

/// What the plan line says. Portal state is the only real source: an anonymous
/// portal session is reported as such rather than guessed into a tier.
pub fn plan_line(portal: Option<&PortalStatus>) -> String {
    match portal {
        Some(portal) => match portal.account_tier.as_deref() {
            Some(tier) if !tier.is_empty() => tier.to_string(),
            _ if portal.logged_in => portal.provider.clone(),
            _ if portal.free_tier => "Free tier".to_string(),
            _ => "No portal session".to_string(),
        },
        None => "—".to_string(),
    }
}

/// Models worth listing: spend first, then call volume.
pub fn top_models(analytics: Option<&UsageAnalytics>, limit: usize) -> Vec<&UsageModel> {
    let Some(analytics) = analytics else {
        return Vec::new();
    };
    let mut models: Vec<&UsageModel> = analytics.by_model.iter().collect();
    models.sort_by(|a, b| {
        b.estimated_cost
            .partial_cmp(&a.estimated_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.api_calls.cmp(&a.api_calls))
            .then_with(|| a.model.cmp(&b.model))
    });
    models.truncate(limit);
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_api::UsageTotals;

    fn model(name: &str, calls: u64, cost: f64) -> UsageModel {
        UsageModel {
            model: name.to_string(),
            api_calls: calls,
            estimated_cost: cost,
            ..UsageModel::default()
        }
    }

    #[test]
    fn money_keeps_sub_cent_spend_visible() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(0.0042), "$0.0042");
        assert_eq!(money(1.5), "$1.50");
        assert_eq!(money(f64::NAN), "—");
    }

    #[test]
    fn tokens_and_context_stay_compact() {
        assert_eq!(tokens(None), "—");
        assert_eq!(tokens(Some(0)), "0");
        assert_eq!(tokens(Some(999)), "999");
        assert_eq!(tokens(Some(12_345)), "12.3K");
        assert_eq!(tokens(Some(2_500_000)), "2.5M");
        assert_eq!(context_label(262_144), "262K context");
        assert_eq!(context_label(0), "—");
    }

    #[test]
    fn plan_line_prefers_the_portal_tier_and_never_invents_one() {
        assert_eq!(plan_line(None), "—");
        let anonymous = PortalStatus::default();
        assert_eq!(plan_line(Some(&anonymous)), "No portal session");
        let free = PortalStatus {
            free_tier: true,
            ..PortalStatus::default()
        };
        assert_eq!(plan_line(Some(&free)), "Free tier");
        let paid = PortalStatus {
            logged_in: true,
            provider: "nous".to_string(),
            account_tier: Some("Ultra".to_string()),
            ..PortalStatus::default()
        };
        assert_eq!(plan_line(Some(&paid)), "Ultra");
    }

    #[test]
    fn top_models_rank_by_spend_then_calls() {
        let analytics = UsageAnalytics {
            by_model: vec![model("a", 5, 0.0), model("b", 1, 2.0), model("c", 9, 0.0)],
            totals: UsageTotals::default(),
            ..UsageAnalytics::default()
        };
        let ranked = top_models(Some(&analytics), 2);
        assert_eq!(ranked[0].model, "b");
        assert_eq!(ranked[1].model, "c");
        assert!(top_models(None, 5).is_empty());
    }

    #[test]
    fn failures_are_a_loaded_state_not_a_spinner_forever() {
        let mut usage = Usage {
            days: 30,
            ..Usage::default()
        };
        usage.failed("state.db corrupt — run `hermes doctor`".into());
        assert!(usage.loaded);
        assert!(usage.error.is_some());
        usage.loaded(UsageAnalytics::default(), Some(ModelInfo::default()), None);
        assert!(usage.loaded);
        assert!(usage.error.is_none());
    }
}
