//! State for the Computers sheet: the hub's node roster, remote commands and
//! one-time secret prompt cards.
//!
//! This module is deliberately free of GPUI types so the selection rules,
//! argv parsing, output capping and prompt lifecycle can be tested without a
//! window. The transport lives in `hub_client.rs`; `app.rs` only renders and
//! routes keystrokes.
//!
//! Secrets follow the protocol rule: a prompt answer exists only in the card's
//! draft until it is handed to the hub, and the draft is zeroized when the card
//! closes. Nothing here stores a value in the command record or status text.

use std::time::{SystemTime, UNIX_EPOCH};

use hermes_protocol::{
    CommandRecord, CommandRequest, NodeSummary, SecretPrompt, SecretSpec, StreamChannel,
};
use uuid::Uuid;
use zeroize::Zeroize;

/// Remote commands are bounded so a stuck process cannot hold the node forever.
pub const COMMAND_TIMEOUT_MS: u64 = 120_000;
/// Keep the sheet responsive even when a command prints for a long time.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// Newest commands first; older entries leave the sheet.
const MAX_COMMANDS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Argv,
    Cwd,
    SecretEnv,
    Purpose,
}

impl Field {
    pub fn next(self) -> Self {
        match self {
            Self::Argv => Self::Cwd,
            Self::Cwd => Self::SecretEnv,
            Self::SecretEnv => Self::Purpose,
            Self::Purpose => Self::Argv,
        }
    }
}

#[derive(Clone)]
pub struct RemoteCommand {
    pub record: CommandRecord,
    pub output: String,
    pub truncated: bool,
}

impl RemoteCommand {
    pub fn summary(&self) -> String {
        self.record.argv.join(" ")
    }

    pub fn is_running(&self) -> bool {
        !self.record.status.is_terminal()
    }
}

/// A pending credential request. `answer` is the only copy of the typed value;
/// it is cleared when the card is submitted, cancelled or closed.
pub struct PromptCard {
    pub prompt: SecretPrompt,
    pub answer: String,
    pub submitting: bool,
}

#[derive(Default)]
pub struct Computers {
    /// False when the hub environment variables are absent.
    pub configured: bool,
    /// Why the hub could not start, if configuration was incomplete.
    pub problem: Option<String>,
    pub connected: bool,
    pub nodes: Vec<NodeSummary>,
    pub selected: Option<String>,
    pub argv: String,
    pub cwd: String,
    pub secret_env: String,
    pub secret_purpose: String,
    pub focus: Option<Field>,
    pub commands: Vec<RemoteCommand>,
    pub status: Option<String>,
    pub prompts: Vec<PromptCard>,
}

impl Computers {
    pub fn new(configured: bool, problem: Option<String>) -> Self {
        let mut page = Self {
            configured,
            problem,
            status: if configured {
                Some("connecting to the hub…".into())
            } else {
                None
            },
            ..Self::default()
        };
        if page.problem.is_none() && !configured {
            page.problem = Some(
                "Set HERMES_HUB_URL, HERMES_HUB_DEVICE_ID and HERMES_HUB_DEVICE_TOKEN to use Computers."
                    .into(),
            );
        }
        page
    }

    pub fn nodes_loaded(&mut self, nodes: Vec<NodeSummary>) {
        self.nodes = nodes;
        self.ensure_selection();
    }

    pub fn node_presence(&mut self, node: NodeSummary) {
        match self.nodes.iter_mut().find(|known| known.id == node.id) {
            Some(known) => *known = node,
            None => self.nodes.push(node),
        }
        self.ensure_selection();
    }

    fn ensure_selection(&mut self) {
        let selected_still_exists = self
            .selected
            .as_deref()
            .is_some_and(|id| self.nodes.iter().any(|node| node.id == id));
        if selected_still_exists {
            return;
        }
        self.selected = self
            .nodes
            .iter()
            .find(|node| node.online)
            .or_else(|| self.nodes.first())
            .map(|node| node.id.clone());
    }

    pub fn selected_node(&self) -> Option<&NodeSummary> {
        let id = self.selected.as_deref()?;
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn select(&mut self, id: &str) {
        if self.nodes.iter().any(|node| node.id == id) {
            self.selected = Some(id.to_string());
            self.status = None;
        }
    }

    pub fn connection(&mut self, connected: bool) {
        self.connected = connected;
        if connected {
            self.problem = None;
            self.status = None;
        } else {
            self.status = Some("hub offline; reconnecting…".into());
            for prompt in &mut self.prompts {
                prompt.submitting = false;
            }
        }
    }

    pub fn problem(&mut self, message: String) {
        if self.configured {
            self.status = Some(message);
        } else {
            self.problem = Some(message);
        }
    }

    pub fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Argv => &mut self.argv,
            Field::Cwd => &mut self.cwd,
            Field::SecretEnv => &mut self.secret_env,
            Field::Purpose => &mut self.secret_purpose,
        }
    }

    /// Build the request the Run button submits. Validation is the protocol's,
    /// so the client and node agree on exactly what is acceptable.
    pub fn build_request(&mut self) -> Result<CommandRequest, String> {
        let node = self
            .selected_node()
            .ok_or_else(|| "Select a computer first.".to_string())?;
        if !node.online {
            return Err(format!("{} is offline.", node.name));
        }
        let argv = parse_argv(&self.argv)?;
        if argv.is_empty() {
            return Err("Enter a program and its arguments.".into());
        }
        let cwd = self.cwd.trim();
        let secret_env = self.secret_env.trim();
        let secret = if secret_env.is_empty() {
            None
        } else {
            if !valid_env_name(secret_env) {
                return Err("The secret variable must look like PGPASSWORD.".into());
            }
            let purpose = self.secret_purpose.trim();
            Some(SecretSpec {
                env_var: secret_env.to_string(),
                purpose: if purpose.is_empty() {
                    "remote command".into()
                } else {
                    purpose.to_string()
                },
                scope: "one command".into(),
                timeout_ms: COMMAND_TIMEOUT_MS,
            })
        };
        let request = CommandRequest {
            id: format!("cmd-{}", Uuid::new_v4()),
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            argv,
            cwd: (!cwd.is_empty()).then(|| cwd.to_string()),
            env: Default::default(),
            timeout_ms: COMMAND_TIMEOUT_MS,
            secret,
        };
        request.validate().map_err(|err| err.to_string())?;
        Ok(request)
    }

    pub fn submitted(&mut self, node: &str) {
        self.status = Some(format!("sent to {node}…"));
    }

    pub fn command_state(&mut self, record: CommandRecord) {
        if let Some(existing) = self
            .commands
            .iter_mut()
            .find(|command| command.record.id == record.id)
        {
            existing.record = record;
            return;
        }
        let summary = record.status.as_str().to_string();
        self.commands.insert(
            0,
            RemoteCommand {
                record,
                output: String::new(),
                truncated: false,
            },
        );
        self.commands.truncate(MAX_COMMANDS);
        self.status = Some(format!("command {summary}"));
    }

    pub fn stream(&mut self, command_id: &str, channel: StreamChannel, data: &[u8]) {
        if matches!(channel, StreamChannel::File | StreamChannel::Screen) {
            return;
        }
        let Some(command) = self
            .commands
            .iter_mut()
            .find(|command| command.record.id == command_id)
        else {
            return;
        };
        let text = String::from_utf8_lossy(data);
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(command.output.len());
        if text.len() > remaining {
            let mut cut = remaining;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            command.output.push_str(&text[..cut]);
            command.truncated = true;
        } else {
            command.output.push_str(&text);
        }
    }

    pub fn push_prompt(&mut self, prompt: SecretPrompt) {
        if self.prompts.iter().any(|card| card.prompt.id == prompt.id) {
            return;
        }
        self.prompts.push(PromptCard {
            prompt,
            answer: String::new(),
            submitting: false,
        });
    }

    pub fn prompt_front(&self) -> Option<&PromptCard> {
        self.prompts.first()
    }

    pub fn prompt_closed(&mut self, id: &str) {
        if let Some(index) = self.prompts.iter().position(|card| card.prompt.id == id) {
            let mut card = self.prompts.remove(index);
            card.answer.zeroize();
        }
    }

    /// Remove the first prompt and take its typed value, or keep it when the
    /// value is empty. The returned string is the caller's only copy.
    pub fn take_answer(&mut self) -> Option<(SecretPrompt, String)> {
        let card = self.prompts.first_mut()?;
        if card.submitting || card.answer.is_empty() {
            return None;
        }
        let prompt = card.prompt.clone();
        let value = std::mem::take(&mut card.answer);
        card.submitting = true;
        Some((prompt, value))
    }

    pub fn mark_submitting(&mut self, id: &str) {
        if let Some(card) = self.prompts.iter_mut().find(|card| card.prompt.id == id) {
            card.submitting = true;
        }
    }

    pub fn online_count(&self) -> usize {
        self.nodes.iter().filter(|node| node.online).count()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Seconds until a prompt expires, or zero once it has.
pub fn seconds_left(expires_at_ms: u64) -> u64 {
    expires_at_ms.saturating_sub(now_ms()) / 1000
}

/// Split a command line into argv without ever invoking a shell.
///
/// Whitespace separates arguments, single quotes are literal, double quotes
/// allow `\"` and `\\`, and a backslash outside quotes escapes the next byte.
/// An unterminated quote is an error rather than a best-effort guess.
pub fn parse_argv(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
                started = true;
            }
            Some(_) => match ch {
                '"' => quote = None,
                '\\' => match chars.next() {
                    Some(escaped @ ('"' | '\\')) => current.push(escaped),
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                    }
                    None => return Err("Command line ends with an escape.".into()),
                },
                _ => current.push(ch),
            },
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    started = true;
                }
                '\\' => match chars.next() {
                    Some(escaped) => {
                        current.push(escaped);
                        started = true;
                    }
                    None => return Err("Command line ends with an escape.".into()),
                },
                ch if ch.is_whitespace() => {
                    if started {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                _ => {
                    current.push(ch);
                    started = true;
                }
            },
        }
    }

    if let Some(single) = quote {
        return Err(format!("Unclosed {single} quote."));
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_protocol::CommandStatus;
    use std::collections::BTreeMap;

    fn node(id: &str, online: bool) -> NodeSummary {
        NodeSummary {
            id: id.into(),
            name: id.into(),
            hostname: format!("{id}.lan"),
            os: "linux".into(),
            arch: "x86_64".into(),
            agent_version: "0.1.0".into(),
            capabilities: hermes_protocol::Capabilities::baseline(),
            labels: BTreeMap::new(),
            online,
            last_seen_ms: 1,
        }
    }

    fn record(id: &str, status: CommandStatus) -> CommandRecord {
        CommandRecord {
            id: id.into(),
            node_id: "forge".into(),
            argv: vec!["uptime".into()],
            cwd: None,
            status,
            idempotency_key: "idem".into(),
            created_at_ms: 1,
            started_at_ms: None,
            finished_at_ms: None,
            exit_code: None,
            message: None,
        }
    }

    fn prompt() -> SecretPrompt {
        SecretPrompt {
            id: "prompt-1".into(),
            node_id: "forge".into(),
            command_id: "cmd-1".into(),
            challenge: "x".repeat(43),
            requester: "hermes-node@forge".into(),
            purpose: "database backup".into(),
            scope: "one command".into(),
            env_var: "PGPASSWORD".into(),
            created_at_ms: 1,
            expires_at_ms: now_ms() + 60_000,
        }
    }

    #[test]
    fn argv_parses_quotes_without_a_shell() {
        assert_eq!(parse_argv("uptime").unwrap(), vec!["uptime"]);
        assert_eq!(
            parse_argv("df -h /home").unwrap(),
            vec!["df", "-h", "/home"]
        );
        assert_eq!(
            parse_argv(r#"sh -c 'echo "hi there"'"#).unwrap(),
            vec!["sh", "-c", "echo \"hi there\""]
        );
        assert_eq!(
            parse_argv(r#"printf "a\"b\\c""#).unwrap(),
            vec!["printf", "a\"b\\c"]
        );
        assert_eq!(parse_argv(r"echo a\ b").unwrap(), vec!["echo", "a b"]);
        assert!(parse_argv("echo 'unterminated").is_err());
        assert!(parse_argv("echo \\").is_err());
    }

    #[test]
    fn request_requires_an_online_selected_node() {
        let mut page = Computers::new(true, None);
        assert!(page.build_request().is_err());
        page.nodes_loaded(vec![node("nexus", false)]);
        page.argv = "uptime".into();
        assert!(page.build_request().is_err());
        page.node_presence(node("nexus", true));
        let request = page.build_request().expect("online node");
        assert_eq!(request.argv, vec!["uptime"]);
        assert!(request.secret.is_none());
    }

    #[test]
    fn request_carries_only_a_secret_spec_not_a_value() {
        let mut page = Computers::new(true, None);
        page.nodes_loaded(vec![node("forge", true)]);
        page.argv = "pg_dump hermes".into();
        page.secret_env = "PGPASSWORD".into();
        let request = page.build_request().expect("valid");
        let secret = request.secret.as_ref().expect("secret spec");
        assert_eq!(secret.env_var, "PGPASSWORD");
        assert_eq!(secret.scope, "one command");
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(!json.contains("hunter2"));
    }

    #[test]
    fn output_is_capped_and_marks_truncation() {
        let mut page = Computers::new(true, None);
        page.nodes_loaded(vec![node("forge", true)]);
        page.command_state(record("cmd-1", CommandStatus::Running));
        page.stream(
            "cmd-1",
            StreamChannel::Stdout,
            &vec![b'x'; MAX_OUTPUT_BYTES + 10],
        );
        let command = &page.commands[0];
        assert_eq!(command.output.len(), MAX_OUTPUT_BYTES);
        assert!(command.truncated);
        page.stream("unknown", StreamChannel::Stdout, b"ignored");
        page.stream("cmd-1", StreamChannel::Screen, b"ignored");
        assert_eq!(page.commands[0].output.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn selection_prefers_an_online_node_and_follows_presence() {
        let mut page = Computers::new(true, None);
        page.nodes_loaded(vec![node("nexus", false), node("forge", true)]);
        assert_eq!(page.selected.as_deref(), Some("forge"));
        page.node_presence(node("nexus", true));
        assert_eq!(page.selected.as_deref(), Some("forge"));
        page.select("nexus");
        assert_eq!(page.selected.as_deref(), Some("nexus"));
        page.node_presence(node("nexus", false));
        assert_eq!(page.selected.as_deref(), Some("nexus"));
        assert_eq!(page.online_count(), 1);
    }

    #[test]
    fn prompt_lifecycle_never_copies_a_value_into_status() {
        let mut page = Computers::new(true, None);
        page.push_prompt(prompt());
        page.push_prompt(prompt());
        assert_eq!(page.prompts.len(), 1);
        page.prompts[0].answer = "hunter2".into();
        let (prompt, value) = page.take_answer().expect("answer");
        assert_eq!(value, "hunter2");
        assert!(page.prompts[0].submitting);
        page.mark_submitting(&prompt.id);
        assert!(page.take_answer().is_none());
        page.prompt_closed(&prompt.id);
        assert!(page.prompts.is_empty());
        assert!(
            !page
                .status
                .as_deref()
                .unwrap_or_default()
                .contains("hunter2")
        );
    }

    #[test]
    fn empty_prompt_answers_are_not_submitted() {
        let mut page = Computers::new(true, None);
        page.push_prompt(prompt());
        assert!(page.take_answer().is_none());
        assert_eq!(page.prompts.len(), 1);
        page.prompts[0].answer = "   ".into();
        assert!(page.take_answer().is_some());
    }

    #[test]
    fn failed_hub_connection_clears_prompt_busy_state() {
        let mut page = Computers::new(true, None);
        page.push_prompt(prompt());
        page.prompts[0].submitting = true;
        page.connection(false);
        assert!(!page.prompts[0].submitting);
        assert!(
            page.status
                .as_deref()
                .unwrap_or_default()
                .contains("offline")
        );
    }
}
