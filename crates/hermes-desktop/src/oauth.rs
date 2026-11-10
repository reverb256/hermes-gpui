//! Provider accounts: list status, start a PKCE or device-code flow, poll the
//! host session, submit a pasted code, and disconnect Hermes-managed creds.

use hermes_api::{OAuthPoll, OAuthProvider, OAuthStart};

#[derive(Debug, Clone)]
pub enum Flow {
    Pkce {
        provider: String,
        session_id: String,
        auth_url: String,
        code: String,
    },
    Device {
        provider: String,
        session_id: String,
        user_code: String,
        verification_url: String,
        status: String,
        terminal: bool,
    },
}

#[derive(Debug, Default)]
pub struct Accounts {
    pub loaded: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub providers: Vec<OAuthProvider>,
    pub flow: Option<Flow>,
    pub confirm_disconnect: Option<String>,
    pub busy: bool,
}

impl Accounts {
    pub fn loaded(&mut self, providers: Vec<OAuthProvider>) {
        self.providers = providers;
        self.loaded = true;
        self.error = None;
    }

    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
        self.loaded = true;
        self.busy = false;
    }

    pub fn begin(&mut self, provider: &str, start: OAuthStart) {
        self.busy = false;
        self.status = None;
        if start.flow == "device_code" {
            self.flow = Some(Flow::Device {
                provider: provider.to_string(),
                session_id: start.session_id,
                user_code: start.user_code,
                verification_url: start.verification_url,
                status: "pending".into(),
                terminal: false,
            });
        } else {
            self.flow = Some(Flow::Pkce {
                provider: provider.to_string(),
                session_id: start.session_id,
                auth_url: start.auth_url,
                code: String::new(),
            });
        }
    }

    pub fn apply_poll(&mut self, poll: OAuthPoll) {
        self.busy = false;
        let Some(Flow::Device {
            status, terminal, ..
        }) = self.flow.as_mut()
        else {
            return;
        };
        *status = if poll.status.is_empty() {
            "pending".into()
        } else {
            poll.status.clone()
        };
        *terminal = !matches!(status.as_str(), "pending" | "");
        if let Some(message) = poll.error_message {
            self.status = Some(message);
        }
    }

    pub fn close_flow(&mut self) {
        self.flow = None;
        self.busy = false;
    }

    pub fn flow_done(&self) -> bool {
        match &self.flow {
            Some(Flow::Device { terminal, .. }) => *terminal,
            _ => false,
        }
    }

    pub fn code_mut(&mut self) -> Option<&mut String> {
        match self.flow.as_mut() {
            Some(Flow::Pkce { code, .. }) => Some(code),
            _ => None,
        }
    }

    pub fn confirm_disconnect(&mut self, id: &str) -> bool {
        if self.confirm_disconnect.as_deref() == Some(id) {
            self.confirm_disconnect = None;
            true
        } else {
            self.confirm_disconnect = Some(id.to_string());
            self.status = Some(format!("click Disconnect again to remove {id} credentials"));
            false
        }
    }
}

pub fn status_line(provider: &OAuthProvider) -> String {
    if provider.status.logged_in {
        let mut line = "connected".to_string();
        if !provider.status.source_label.is_empty() {
            line.push_str(&format!(" · {}", provider.status.source_label));
        }
        if !provider.status.token_preview.is_empty() {
            line.push_str(&format!(" · {}", provider.status.token_preview));
        }
        line
    } else if provider.flow == "external" {
        "external credentials".to_string()
    } else {
        "not connected".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_status_becomes_terminal() {
        let mut accounts = Accounts::default();
        accounts.begin(
            "nous",
            OAuthStart {
                session_id: "s1".into(),
                flow: "device_code".into(),
                user_code: "ABC".into(),
                verification_url: "https://example.test".into(),
                ..OAuthStart::default()
            },
        );
        assert!(!accounts.flow_done());
        accounts.apply_poll(OAuthPoll {
            session_id: "s1".into(),
            status: "approved".into(),
            ..OAuthPoll::default()
        });
        assert!(accounts.flow_done());
    }

    #[test]
    fn pkce_code_round_trips() {
        let mut accounts = Accounts::default();
        accounts.begin(
            "anthropic",
            OAuthStart {
                session_id: "s2".into(),
                flow: "pkce".into(),
                auth_url: "https://example.test/auth".into(),
                ..OAuthStart::default()
            },
        );
        accounts.code_mut().unwrap().push_str("code-123");
        match accounts.flow.as_ref().unwrap() {
            Flow::Pkce { code, auth_url, .. } => {
                assert_eq!(code, "code-123");
                assert_eq!(auth_url, "https://example.test/auth");
            }
            _ => panic!("expected PKCE flow"),
        }
    }

    #[test]
    fn disconnect_needs_two_clicks() {
        let mut accounts = Accounts::default();
        assert!(!accounts.confirm_disconnect("anthropic"));
        assert!(accounts.confirm_disconnect("anthropic"));
    }
}
