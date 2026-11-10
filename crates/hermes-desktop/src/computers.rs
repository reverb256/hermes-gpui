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
    CommandRecord, CommandRequest, CommandStatus, NodeSummary, PtySpec, SecretPrompt, SecretSpec,
    StreamChannel,
};
use uuid::Uuid;
use zeroize::Zeroize;

/// Remote commands are bounded so a stuck process cannot hold the node forever.
pub const COMMAND_TIMEOUT_MS: u64 = 120_000;
/// The geometry a terminal opens with, before the panel has been measured. A
/// terminal has no wall-clock timeout.
pub const TERMINAL_COLS: u16 = 120;
pub const TERMINAL_ROWS: u16 = 32;
/// One cell of the `text_xs` monospace output area. The panel converts its
/// measured pixel bounds with these to report a real grid to the node; the
/// exact glyph advance is font-dependent, so the result is advisory (a
/// full-screen program reflows to it) and clamped to what the protocol allows.
pub const TERMINAL_CELL_WIDTH: f32 = 7.2;
pub const TERMINAL_CELL_HEIGHT: f32 = 15.0;
/// Clamp reported geometry to the range `PtySpec::validate` accepts, keeping a
/// panel that is collapsed or absurdly large from proposing a rejected grid.
pub const TERMINAL_MIN_COLS: u16 = 20;
pub const TERMINAL_MAX_COLS: u16 = 500;
pub const TERMINAL_MIN_ROWS: u16 = 5;
pub const TERMINAL_MAX_ROWS: u16 = 300;
/// Keep the sheet responsive even when a command prints for a long time.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// A terminal scrolls, so it keeps more than one command's worth of output.
const MAX_TERMINAL_BYTES: usize = 512 * 1024;
/// Newest commands first; older entries leave the sheet.
const MAX_COMMANDS: usize = 20;
/// How many finished terminal ids are remembered, so their records never
/// reappear as ordinary command cards.
const MAX_TERMINALS: usize = 8;

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

/// Escape-sequence state that survives a chunk boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Esc {
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// Convert a measured output area in pixels to terminal columns and rows.
///
/// A zero, negative or non-finite measurement (an element that has not been
/// laid out yet) keeps the default geometry instead of reporting a 1×1 grid.
pub fn geometry_for(width_px: f32, height_px: f32) -> (u16, u16) {
    if !width_px.is_finite() || !height_px.is_finite() || width_px < 1.0 || height_px < 1.0 {
        return (TERMINAL_COLS, TERMINAL_ROWS);
    }
    let cols = (width_px / TERMINAL_CELL_WIDTH).floor();
    let rows = (height_px / TERMINAL_CELL_HEIGHT).floor();
    (
        cols.clamp(TERMINAL_MIN_COLS as f32, TERMINAL_MAX_COLS as f32) as u16,
        rows.clamp(TERMINAL_MIN_ROWS as f32, TERMINAL_MAX_ROWS as f32) as u16,
    )
}

/// A live (or finished) terminal on a computer.
///
/// The transcript is a plain text screen: escape sequences are dropped, `\r`
/// rewrites the current line and `\t` becomes spaces. Cursor addressing is not
/// rendered, which keeps a chat panel readable; the node side still sees the
/// real terminal. Terminal output is never stored in a command record.
pub struct Terminal {
    pub command_id: String,
    pub node_name: String,
    pub title: String,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub message: Option<String>,
    pub output: String,
    pub truncated: bool,
    /// The grid reported to the node; updated as the panel is resized.
    pub cols: u16,
    pub rows: u16,
    /// Keyboard input goes to this terminal while true.
    pub focused: bool,
    /// Cursor column in the current line, so `\r` rewrites instead of wiping.
    col: usize,
    esc: Esc,
}

impl Terminal {
    pub fn is_running(&self) -> bool {
        !self.status.is_terminal()
    }

    /// Write one character at the cursor, overwriting what was there.
    fn put(&mut self, ch: char) {
        let start = self.output.rfind('\n').map(|index| index + 1).unwrap_or(0);
        let existing = self.output[start..]
            .char_indices()
            .nth(self.col)
            .map(|(offset, ch)| (start + offset, ch.len_utf8()));
        match existing {
            Some((offset, len)) => self
                .output
                .replace_range(offset..offset + len, &ch.to_string()),
            None => self.output.push(ch),
        }
        self.col += 1;
    }

    fn feed(&mut self, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        for ch in text.chars() {
            match self.esc {
                Esc::Ground => match ch {
                    '\u{1b}' => self.esc = Esc::Escape,
                    '\r' => self.col = 0,
                    '\n' => {
                        self.output.push('\n');
                        self.col = 0;
                    }
                    '\u{08}' => self.col = self.col.saturating_sub(1),
                    '\t' => {
                        let next = (self.col / 8 + 1) * 8;
                        while self.col < next {
                            self.put(' ');
                        }
                    }
                    ch if (ch as u32) < 0x20 || ch == '\u{7f}' => {}
                    ch => self.put(ch),
                },
                Esc::Escape => match ch {
                    '[' => self.esc = Esc::Csi,
                    ']' => self.esc = Esc::Osc,
                    _ => self.esc = Esc::Ground,
                },
                Esc::Csi => {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        self.esc = Esc::Ground;
                    }
                }
                Esc::Osc => match ch {
                    '\u{07}' => self.esc = Esc::Ground,
                    '\u{1b}' => self.esc = Esc::OscEscape,
                    _ => {}
                },
                Esc::OscEscape => {
                    self.esc = if ch == '\\' { Esc::Ground } else { Esc::Osc };
                }
            }
        }
        if self.output.len() > MAX_TERMINAL_BYTES {
            let mut cut = self.output.len() - MAX_TERMINAL_BYTES;
            while cut < self.output.len() && !self.output.is_char_boundary(cut) {
                cut += 1;
            }
            self.output.drain(..cut);
            self.truncated = true;
            let line = self.output.rfind('\n').map(|index| index + 1).unwrap_or(0);
            self.col = self.col.min(self.output[line..].chars().count());
        }
    }
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
    /// The terminal panel, if a terminal was opened in this session.
    pub terminal: Option<Terminal>,
    /// The most recent grid measured from the panel's pixel size.
    terminal_geometry: (u16, u16),
    /// Every terminal command id seen, so their records stay out of the
    /// ordinary command list and their output stays out of storage.
    terminal_ids: Vec<String>,
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
            terminal_geometry: (TERMINAL_COLS, TERMINAL_ROWS),
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
            pty: None,
        };
        request.validate().map_err(|err| err.to_string())?;
        Ok(request)
    }

    pub fn submitted(&mut self, node: &str) {
        self.status = Some(format!("sent to {node}…"));
    }

    /// Whether the selected computer advertises a terminal.
    pub fn terminal_supported(&self) -> bool {
        self.selected_node()
            .is_some_and(|node| node.online && node.capabilities.pty)
    }

    /// Build the request that opens a terminal. A terminal is interactive, so
    /// it carries no secret spec and no timeout. Its geometry is the last one
    /// the panel measured, falling back to `TERMINAL_COLS`×`TERMINAL_ROWS`.
    pub fn build_terminal_request(&mut self) -> Result<CommandRequest, String> {
        let node = self
            .selected_node()
            .ok_or_else(|| "Select a computer first.".to_string())?;
        if !node.online {
            return Err(format!("{} is offline.", node.name));
        }
        if !node.capabilities.pty {
            return Err(format!("{} does not offer a terminal.", node.name));
        }
        if !self.secret_env.trim().is_empty() {
            return Err(
                "A terminal is already interactive; clear the secret variable first.".into(),
            );
        }
        let argv = parse_argv(&self.argv)?;
        if argv.is_empty() {
            return Err("Enter a program and its arguments.".into());
        }
        let cwd = self.cwd.trim();
        let (cols, rows) = self.terminal_geometry;
        let request = CommandRequest {
            id: format!("cmd-{}", Uuid::new_v4()),
            idempotency_key: format!("idem-{}", Uuid::new_v4()),
            argv,
            cwd: (!cwd.is_empty()).then(|| cwd.to_string()),
            env: Default::default(),
            timeout_ms: 0,
            secret: None,
            pty: Some(PtySpec { cols, rows }),
        };
        request.validate().map_err(|err| err.to_string())?;
        Ok(request)
    }

    /// Record a submitted terminal so its stream is routed to the panel.
    pub fn terminal_started(&mut self, request: &CommandRequest) {
        let node_name = self
            .selected_node()
            .map(|node| node.name.clone())
            .unwrap_or_else(|| "computer".into());
        self.terminal_ids.insert(0, request.id.clone());
        self.terminal_ids.truncate(MAX_TERMINALS);
        self.status = Some(format!("terminal opened on {node_name}"));
        self.terminal = Some(Terminal {
            command_id: request.id.clone(),
            node_name,
            title: request.argv.join(" "),
            status: CommandStatus::Queued,
            exit_code: None,
            message: None,
            output: String::new(),
            truncated: false,
            cols: request.pty.map(|pty| pty.cols).unwrap_or(TERMINAL_COLS),
            rows: request.pty.map(|pty| pty.rows).unwrap_or(TERMINAL_ROWS),
            focused: true,
            col: 0,
            esc: Esc::Ground,
        });
    }

    /// Convert the panel's measured pixel size into a grid, clamped to the
    /// protocol's bounds, and remember it for the next terminal.
    pub fn measured(&mut self, width_px: f32, height_px: f32) {
        self.terminal_geometry = geometry_for(width_px, height_px);
    }

    /// The resize frame to send for the live terminal, if the measured grid
    /// differs from the one the node already has. Returns `None` for an
    /// unmeasured panel, a finished session or an unchanged size, which keeps
    /// a repainting panel from flooding the hub.
    pub fn resize_terminal(&mut self) -> Option<(String, u16, u16)> {
        let (cols, rows) = self.terminal_geometry;
        let terminal = self.terminal.as_mut()?;
        if !terminal.is_running() || (terminal.cols, terminal.rows) == (cols, rows) {
            return None;
        }
        terminal.cols = cols;
        terminal.rows = rows;
        Some((terminal.command_id.clone(), cols, rows))
    }

    /// The command id that should receive keystrokes, if any.
    pub fn terminal_input_id(&self) -> Option<&str> {
        let terminal = self.terminal.as_ref()?;
        (terminal.focused && terminal.is_running()).then_some(terminal.command_id.as_str())
    }

    pub fn focus_terminal(&mut self, focused: bool) {
        if let Some(terminal) = self.terminal.as_mut()
            && terminal.is_running()
        {
            terminal.focused = focused;
        }
    }

    pub fn close_terminal(&mut self) {
        self.terminal = None;
    }

    /// Consume a record that belongs to a terminal instead of adding a card.
    fn terminal_state(&mut self, record: &CommandRecord) -> bool {
        if !self.terminal_ids.iter().any(|id| id == &record.id) {
            return false;
        }
        if let Some(terminal) = self.terminal.as_mut()
            && terminal.command_id == record.id
        {
            terminal.status = record.status;
            terminal.exit_code = record.exit_code;
            terminal.message = record.message.clone();
            if record.status.is_terminal() {
                terminal.focused = false;
                self.status = Some(format!("terminal {}", record.status.as_str()));
            }
        }
        true
    }

    pub fn command_state(&mut self, record: CommandRecord) {
        if self.terminal_state(&record) {
            return;
        }
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
        if channel == StreamChannel::Pty {
            if let Some(terminal) = self.terminal.as_mut()
                && terminal.command_id == command_id
            {
                terminal.feed(data);
            }
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

    fn terminal_node(id: &str) -> NodeSummary {
        let mut node = node(id, true);
        node.capabilities.pty = true;
        node
    }

    fn terminal_page() -> Computers {
        let mut page = Computers::new(true, None);
        page.nodes_loaded(vec![terminal_node("zephyr")]);
        page.argv = "bash -l".into();
        page
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
    fn terminal_request_carries_geometry_and_no_timeout() {
        let mut page = terminal_page();
        let request = page.build_terminal_request().expect("terminal request");
        assert_eq!(
            request.pty,
            Some(PtySpec {
                cols: TERMINAL_COLS,
                rows: TERMINAL_ROWS
            })
        );
        assert_eq!(request.timeout_ms, 0);
        assert!(request.secret.is_none());
        assert_eq!(request.argv, vec!["bash", "-l"]);
    }

    #[test]
    fn measured_geometry_is_clamped_and_used_for_the_next_terminal() {
        // 900×480 px at the panel's cell size is 125 columns and 32 rows.
        assert_eq!(geometry_for(900.0, 480.0), (125, 32));
        // An unlaid-out element keeps the defaults rather than reporting 1×1.
        assert_eq!(geometry_for(0.0, 0.0), (TERMINAL_COLS, TERMINAL_ROWS));
        assert_eq!(geometry_for(f32::NAN, 10.0), (TERMINAL_COLS, TERMINAL_ROWS));
        // Tiny and enormous panels stay inside the protocol's bounds.
        assert_eq!(
            geometry_for(4.0, 4.0),
            (TERMINAL_MIN_COLS, TERMINAL_MIN_ROWS)
        );
        assert_eq!(
            geometry_for(90_000.0, 90_000.0),
            (TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS)
        );
        // The clamp stays inside what `PtySpec::validate` accepts.
        const { assert!(TERMINAL_MAX_COLS <= 500 && TERMINAL_MAX_ROWS <= 300) };

        let mut page = terminal_page();
        page.measured(900.0, 480.0);
        let request = page.build_terminal_request().expect("terminal request");
        assert_eq!(
            request.pty,
            Some(PtySpec {
                cols: 125,
                rows: 32
            })
        );
        page.terminal_started(&request);
        let terminal = page.terminal.as_ref().expect("terminal panel");
        assert_eq!((terminal.cols, terminal.rows), (125, 32));
    }

    #[test]
    fn resizing_reports_one_frame_per_change_and_never_after_exit() {
        let mut page = terminal_page();
        let request = page.build_terminal_request().expect("terminal request");
        let id = request.id.clone();
        page.terminal_started(&request);

        // The opening geometry is already on the node, so nothing is sent.
        page.measured(TERMINAL_COLS as f32 * TERMINAL_CELL_WIDTH, 480.0);
        assert_eq!(page.resize_terminal(), None);

        page.measured(900.0, 480.0);
        assert_eq!(page.resize_terminal(), Some((id.clone(), 125, 32)));
        // The same size again does not repeat the frame.
        page.measured(900.0, 480.0);
        assert_eq!(page.resize_terminal(), None);

        page.command_state(record(&id, CommandStatus::Cancelled));
        page.measured(1440.0, 720.0);
        assert_eq!(page.resize_terminal(), None);
        let terminal = page.terminal.as_ref().expect("terminal panel");
        assert_eq!((terminal.cols, terminal.rows), (125, 32));
    }

    #[test]
    fn terminal_is_refused_without_the_capability_or_with_a_secret() {
        let mut page = Computers::new(true, None);
        page.nodes_loaded(vec![node("nexus", true)]);
        page.argv = "bash".into();
        assert!(
            page.build_terminal_request()
                .unwrap_err()
                .contains("does not offer a terminal")
        );

        let mut page = terminal_page();
        page.secret_env = "PGPASSWORD".into();
        assert!(
            page.build_terminal_request()
                .unwrap_err()
                .contains("already interactive")
        );

        let mut page = terminal_page();
        page.nodes_loaded(vec![{
            let mut offline = terminal_node("zephyr");
            offline.online = false;
            offline
        }]);
        assert!(
            page.build_terminal_request()
                .unwrap_err()
                .contains("offline")
        );
    }

    #[test]
    fn terminal_screen_rewrites_lines_and_drops_escapes() {
        let mut terminal = Terminal {
            command_id: "cmd-1".into(),
            node_name: "zephyr".into(),
            title: "bash".into(),
            status: CommandStatus::Running,
            exit_code: None,
            message: None,
            output: String::new(),
            truncated: false,
            cols: TERMINAL_COLS,
            rows: TERMINAL_ROWS,
            focused: true,
            col: 0,
            esc: Esc::Ground,
        };
        // Colour codes are dropped, CRLF stays one newline, a bare CR rewrites
        // the current line and a tab expands to the next tab stop.
        terminal.feed(b"\x1b[32mzephyr\x1b[0m:~$ ");
        terminal.feed(b"echo hi\r\n");
        terminal.feed(b"10%\r20%\n");
        terminal.feed(b"a\tb\n");
        assert_eq!(terminal.output, "zephyr:~$ echo hi\n20%\na       b\n");
    }

    #[test]
    fn terminal_screen_handles_escapes_split_across_chunks() {
        let mut terminal = Terminal {
            command_id: "cmd-1".into(),
            node_name: "zephyr".into(),
            title: "bash".into(),
            status: CommandStatus::Running,
            exit_code: None,
            message: None,
            output: String::new(),
            truncated: false,
            cols: TERMINAL_COLS,
            rows: TERMINAL_ROWS,
            focused: true,
            col: 0,
            esc: Esc::Ground,
        };
        terminal.feed(b"one\x1b[");
        terminal.feed(b"31mtwo\x1b]0;title\x07three");
        terminal.feed(b"\x1b]8;;http://x\x1b\\four");
        assert_eq!(terminal.output, "onetwothreefour");
    }

    #[test]
    fn terminal_stream_never_becomes_a_command_card() {
        let mut page = terminal_page();
        let request = page.build_terminal_request().expect("terminal request");
        let id = request.id.clone();
        page.terminal_started(&request);
        assert_eq!(page.terminal_input_id(), Some(id.as_str()));

        let mut running = record(&id, CommandStatus::Running);
        running.argv = request.argv.clone();
        page.command_state(running);
        assert!(
            page.commands.is_empty(),
            "terminal records stay off the list"
        );
        assert_eq!(
            page.terminal.as_ref().unwrap().status,
            CommandStatus::Running
        );

        page.stream(&id, StreamChannel::Pty, b"$ uptime\r\nload 0.1\r\n");
        assert_eq!(
            page.terminal.as_ref().unwrap().output,
            "$ uptime\nload 0.1\n"
        );
        page.stream(&id, StreamChannel::Stdout, b"ignored");
        assert_eq!(
            page.terminal.as_ref().unwrap().output,
            "$ uptime\nload 0.1\n"
        );

        page.command_state(record(&id, CommandStatus::Succeeded));
        let terminal = page.terminal.as_ref().expect("terminal panel");
        assert!(!terminal.focused);
        assert!(!terminal.is_running());
        assert_eq!(terminal.exit_code, None);
        assert_eq!(page.terminal_input_id(), None);
        assert!(page.commands.is_empty());
        page.close_terminal();
        assert!(page.terminal.is_none());
    }

    #[test]
    fn detaching_a_terminal_keeps_the_session_alive() {
        let mut page = terminal_page();
        let request = page.build_terminal_request().expect("terminal request");
        page.terminal_started(&request);
        page.focus_terminal(false);
        assert_eq!(page.terminal_input_id(), None);
        assert!(page.terminal.as_ref().unwrap().is_running());
        page.focus_terminal(true);
        assert!(page.terminal_input_id().is_some());
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
