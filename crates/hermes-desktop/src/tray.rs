//! StatusNotifierItem tray icon (works on Wayland and X11 without pulling in
//! GTK), with a connection-status label and a quit entry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{MenuItem, Tray};

/// What the tray knows about the app, updated from the GPUI thread.
#[derive(Clone, Default)]
pub struct TrayStatus {
    inner: Arc<Mutex<StatusInner>>,
    quit_requested: Arc<AtomicBool>,
}

#[derive(Default)]
struct StatusInner {
    host: String,
    profile: String,
    connected: bool,
}

impl TrayStatus {
    pub fn set(&self, host: &str, profile: &str, connected: bool) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.host = host.to_string();
            inner.profile = profile.to_string();
            inner.connected = connected;
        }
    }

    fn snapshot(&self) -> (String, String, bool) {
        match self.inner.lock() {
            Ok(inner) => (inner.host.clone(), inner.profile.clone(), inner.connected),
            Err(_) => (String::new(), String::new(), false),
        }
    }

    /// True once the user picked Quit, cleared on read.
    pub fn take_quit(&self) -> bool {
        self.quit_requested.swap(false, Ordering::SeqCst)
    }
}

struct HermesTray {
    status: TrayStatus,
}

impl Tray for HermesTray {
    fn id(&self) -> String {
        "hermes-gpui".into()
    }

    fn title(&self) -> String {
        "Hermes".into()
    }

    fn icon_name(&self) -> String {
        "utilities-terminal".into()
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let (host, profile, connected) = self.status.snapshot();
        ksni::ToolTip {
            title: "Hermes".into(),
            description: format!(
                "{host}\nprofile: {}\n{}",
                if profile.is_empty() {
                    "default"
                } else {
                    &profile
                },
                if connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let (host, profile, connected) = self.status.snapshot();
        vec![
            StandardItem {
                label: format!(
                    "{} · {}",
                    if connected {
                        "● connected"
                    } else {
                        "○ offline"
                    },
                    if host.is_empty() { "no host" } else { &host }
                ),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!(
                    "profile: {}",
                    if profile.is_empty() {
                        "default"
                    } else {
                        &profile
                    }
                ),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Hermes".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    tray.status.quit_requested.store(true, Ordering::SeqCst);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawn the tray on its own D-Bus thread. Returns the shared status handle.
pub fn spawn() -> TrayStatus {
    let status = TrayStatus::default();
    let tray = HermesTray {
        status: status.clone(),
    };

    std::thread::Builder::new()
        .name("hermes-tray".into())
        .spawn(move || match tray.assume_sni_available(true).spawn() {
            // The handle owns the service registration; keep it for the life of
            // the process rather than tearing the icon down on drop.
            Ok(handle) => std::mem::forget(handle),
            Err(err) => tracing::warn!(%err, "tray unavailable"),
        })
        .ok();

    status
}
