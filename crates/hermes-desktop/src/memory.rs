//! The Memory page: the active provider, discovered backends and the size of
//! the built-in files. Provider configuration and reset are deliberately not
//! here yet; reset is destructive and needs a confirmation flow.

use hermes_api::{MemoryProvider, MemoryStatus};

#[derive(Debug, Default)]
pub struct Memory {
    pub loaded: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub active: String,
    pub providers: Vec<MemoryProvider>,
    pub memory_bytes: u64,
    pub user_bytes: u64,
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
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
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
}
