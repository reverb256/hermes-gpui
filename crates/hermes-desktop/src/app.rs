//! The Hermes window: bot sidebar, streaming transcript, approval and clarify
//! cards, command palette, @-mentions and composer.
//!
//! Interaction design mirrors Grok Bot's: no modal dialogs, everything reachable
//! from the keyboard, and blocking questions show up as cards you answer.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Context, Entity, FocusHandle, Focusable, KeyDownEvent, Render, ScrollHandle, SharedString,
    Subscription, Window, div, px, rgb, rgba,
};
use gpui_ce_elements::editable_text::{EditableTextState, StringStorage, TextChanged, text_area};
use hermes_api::{ActiveProfile, HostConfig, Profile, SkillHubPreview};
use hermes_rpc::ConnectionState;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::backend::{
    ApprovalChoice, ApprovalRequest, Backend, BackendEvent, ClarifyRequest, Command,
};
use crate::cron::{self, Automations};
use crate::inspector::{self, Field, Inspector, Tab};
use crate::keys::{self, Action, Chord, Focus};
use crate::mcp::{self, Mcp};
use crate::memory::{self, Memory};
use crate::moa::{self, Moa};
use crate::models::{self, Models};
use crate::oauth::{self, Accounts};
use crate::sessions::{self, Sessions};
use crate::settings::{Section, Settings};
use crate::skills::{self, Skills};
use crate::theme::*;
use crate::transcript::{Line, Role, append_streaming, clip};
use crate::tray::{self, TrayStatus};
use crate::usage::{self, Usage};
use crate::vault::{self, Vault};

const SUGGESTIONS: [&str; 3] = [
    "What can you do?",
    "Show cluster health",
    "Summarise today's cron runs",
];

#[derive(Default)]
struct Palette {
    open: bool,
    query: String,
    index: usize,
}

#[derive(Default)]
struct Mention {
    open: bool,
    query: String,
    index: usize,
}

#[derive(Clone)]
enum PaletteAction {
    NewChat,
    Refresh,
    Interrupt,
    SwitchProfile(String),
    Quit,
}

#[derive(Clone)]
struct PaletteItem {
    label: String,
    hint: String,
    action: PaletteAction,
}

#[derive(Clone)]
enum CardKind {
    Approval(ApprovalRequest),
    Clarify(ClarifyRequest),
}

#[derive(Clone)]
struct Card {
    id: String,
    kind: CardKind,
}

pub struct HermesApp {
    host: HostConfig,
    backend: Backend,
    profiles: Vec<Profile>,
    active: Option<ActiveProfile>,
    routines: Vec<String>,
    session_id: Option<String>,
    lines: Vec<Line>,
    composer: String,
    composer_input: Entity<EditableTextState>,
    _composer_subscription: Subscription,
    palette: Palette,
    mention: Mention,
    cards: Vec<Card>,
    attachments: Vec<String>,
    connection: ConnectionState,
    busy: bool,
    error: Option<String>,
    voice_status: Option<String>,
    focus: FocusHandle,
    focused: bool,
    /// True while the window has focus; completion notifications only fire
    /// when it does not. Refreshed by `pump_presence` and on IPC show/hide.
    window_active: bool,
    scroll: ScrollHandle,
    tray: TrayStatus,
    tray_state: (bool, String),
    inspector: Inspector,
    settings: Settings,
    vault: Vault,
    usage: Usage,
    models: Models,
    sessions: Sessions,
    skills: Skills,
    mcp: Mcp,
    kanban: Option<hermes_api::KanbanBoard>,
    kanban_boards: Vec<hermes_api::KanbanBoardInfo>,
    kanban_selected: Option<String>,
    kanban_editing: Option<String>,
    kanban_delete: Option<String>,
    kanban_status: Option<String>,
    kanban_draft: Option<String>,
    kanban_creating: bool,
    kanban_board_draft: Option<String>,
    kanban_board_renaming: Option<String>,
    kanban_board_saving: bool,
    /// Board removal awaiting a second click: `(slug, hard_delete)`.
    kanban_board_remove: Option<(String, bool)>,
    kanban_detail_target: Option<(String, String)>,
    kanban_detail: Option<hermes_api::KanbanTaskDetail>,
    kanban_comment_draft: Option<String>,
    kanban_comment_saving: bool,
    kanban_attachment_delete: Option<i64>,
    kanban_attachment_busy: bool,
    kanban_body_draft: Option<String>,
    kanban_assignee_draft: Option<String>,
    kanban_priority_draft: Option<String>,
    kanban_detail_saving: bool,
    memory: Memory,
    moa: Moa,
    automations: Automations,
    accounts: Accounts,
    media: std::collections::HashMap<String, Result<Arc<gpui::Image>, String>>,
    ipc: Option<Arc<crate::ipc::Listener>>,
    /// Stored transcript on screen; sending is disabled until New chat.
    viewing_history: Option<String>,
    rail_search: bool,
    rail_filter: String,
}

impl Focusable for HermesApp {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HermesApp {
    pub fn new(
        host: HostConfig,
        listener: Option<crate::ipc::Listener>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (backend, events) = Backend::spawn(host.clone());
        let tray = tray::spawn();
        let composer_input = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let composer_subscription =
            cx.subscribe(&composer_input, |this, input, _: &TextChanged, cx| {
                this.composer = input.read(cx).as_str().to_string();
                this.refresh_mention();
                cx.notify();
            });

        let mut app = Self {
            host,
            backend,
            profiles: Vec::new(),
            active: None,
            routines: Vec::new(),
            session_id: None,
            lines: Vec::new(),
            composer: String::new(),
            composer_input,
            _composer_subscription: composer_subscription,
            palette: Palette::default(),
            mention: Mention::default(),
            cards: Vec::new(),
            attachments: Vec::new(),
            connection: ConnectionState::Connecting,
            busy: false,
            error: None,
            voice_status: None,
            focus: cx.focus_handle(),
            focused: false,
            window_active: true,
            scroll: ScrollHandle::new(),
            tray,
            tray_state: (false, String::new()),
            inspector: Inspector::default(),
            settings: Settings::default(),
            vault: Vault::default(),
            models: Models::default(),
            sessions: Sessions::default(),
            skills: Skills::default(),
            mcp: Mcp::default(),
            kanban: None,
            kanban_boards: Vec::new(),
            kanban_selected: None,
            kanban_editing: None,
            kanban_delete: None,
            kanban_status: None,
            kanban_draft: None,
            kanban_creating: false,
            kanban_board_draft: None,
            kanban_board_renaming: None,
            kanban_board_saving: false,
            kanban_board_remove: None,
            kanban_detail_target: None,
            kanban_detail: None,
            kanban_comment_draft: None,
            kanban_comment_saving: false,
            kanban_attachment_delete: None,
            kanban_attachment_busy: false,
            kanban_body_draft: None,
            kanban_assignee_draft: None,
            kanban_priority_draft: None,
            kanban_detail_saving: false,
            memory: Memory::default(),
            moa: Moa::default(),
            automations: Automations::default(),
            accounts: Accounts::default(),
            media: std::collections::HashMap::new(),
            ipc: listener.map(Arc::new),
            viewing_history: None,
            usage: Usage {
                days: 30,
                ..Usage::default()
            },
            rail_search: false,
            rail_filter: String::new(),
        };

        app.pump(events, cx);
        app.pump_tray(cx);
        app.pump_presence(cx);
        app.pump_theme(cx);
        app.pump_ipc(cx);
        app.pump_oauth(cx);
        // QA hook: seed a short transcript so a screenshot can review the
        // bubble styles without driving the composer (Wayland has no text
        // injection here). Never used against a real turn.
        if std::env::var("HERMES_DEMO").is_ok() {
            app.lines
                .push(Line::new(Role::User, "Show me the cluster health."));
            app.lines.push(Line::new(
                Role::Reasoning,
                "Check node readiness, then pods with rising restart counts.",
            ));
            app.lines.push(Line::new(
                Role::Assistant,
                "All four nodes are Ready. One pod is restarting; everything else is stable. GPU memory is at 78% and 41%.",
            ));
        }
        // QA hook: open the inspector (optionally on a given tab) at launch so
        // a screenshot run can review the panel without input automation.
        if matches!(std::env::var("HERMES_OPEN_INSPECTOR").as_deref(), Ok("1")) {
            app.inspector.tab = match std::env::var("HERMES_INSPECTOR_TAB").ok().as_deref() {
                Some("connectors") => Tab::Connectors,
                Some("secrets") => Tab::Secrets,
                _ => Tab::Look,
            };
            app.toggle_inspector(cx);
        }
        // QA hook: open the settings sheet at launch for screenshot review.
        if let Ok(target) = std::env::var("HERMES_OPEN_SETTINGS") {
            match target.as_str() {
                "model" => app.open_models(cx),
                "assignments" => app.load_assignments(cx),
                "sessions" => app.open_sessions(cx),
                "skills" => app.open_skills(cx),
                "mcp" => app.open_mcp(cx),
                "usage" => app.open_usage(cx),
                _ => app.open_vault(cx),
            }
        }
        app
    }

    fn pump(&mut self, mut events: UnboundedReceiver<BackendEvent>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |app, cx| app.on_backend_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn pump_tray(&mut self, cx: &mut Context<Self>) {
        let tray = self.tray.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                if tray.take_quit() {
                    cx.update(|cx| cx.quit());
                    return;
                }
                if tray.take_show()
                    && this
                        .update_in(cx, |app, window, cx| {
                            app.handle_ipc(crate::ipc::Request::Show, window, cx)
                        })
                        .is_err()
                {
                    return;
                }
                if this.update(cx, |app, cx| app.sync_tray(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Track window focus so a finished turn only interrupts when the user is
    /// looking somewhere else.
    fn pump_presence(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                if this
                    .update_in(cx, |app, window, _cx| {
                        app.window_active = window.is_window_active();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn pump_theme(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if !crate::theme::refresh() {
                    continue;
                }
                if this
                    .update(cx, |_app, cx| {
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn pump_ipc(&mut self, cx: &mut Context<Self>) {
        let Some(listener) = self.ipc.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                for request in listener.poll() {
                    if this
                        .update_in(cx, |app, window, cx| app.handle_ipc(request, window, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .detach();
    }

    fn handle_ipc(
        &mut self,
        request: crate::ipc::Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match request {
            crate::ipc::Request::Toggle => {
                if window.is_window_active() {
                    window.set_visible(false);
                } else {
                    window.set_visible(true);
                    window.activate();
                    self.focus.focus(window, cx);
                }
            }
            crate::ipc::Request::Show => {
                window.set_visible(true);
                window.activate();
                self.focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn sync_tray(&mut self, _cx: &mut Context<Self>) {
        let connected = self.connection == ConnectionState::Open;
        let profile = self.profile_name();
        if self.tray_state != (connected, profile.clone()) {
            self.tray.set(&self.host.base_url, &profile, connected);
            self.tray_state = (connected, profile);
        }
    }

    fn profile_name(&self) -> String {
        self.host
            .profile
            .clone()
            .or_else(|| self.active.as_ref().map(|active| active.current.clone()))
            .unwrap_or_else(|| "default".into())
    }

    fn current_profile(&self) -> Option<&Profile> {
        let name = self.profile_name();
        self.profiles.iter().find(|profile| profile.name == name)
    }

    fn has_conversation(&self) -> bool {
        self.lines
            .iter()
            .any(|line| matches!(line.role, Role::User | Role::Assistant))
    }

    fn on_backend_event(&mut self, event: BackendEvent, cx: &mut Context<Self>) {
        match event {
            BackendEvent::Connection(state) => {
                self.connection = state.clone();
                if state == ConnectionState::Closed {
                    self.busy = false;
                }
                self.publish_status();
            }
            BackendEvent::Profiles(profiles) => self.profiles = profiles,
            BackendEvent::ActiveProfile(active) => self.active = Some(active),
            BackendEvent::Routines(routines) => self.routines = routines,
            BackendEvent::SessionReady {
                session_id,
                profile,
            } => {
                self.session_id = Some(session_id);
                if let Some(profile) = profile {
                    self.host.profile = Some(profile);
                }
            }
            BackendEvent::Delta { text } => {
                append_streaming(&mut self.lines, Role::Assistant, &text);
                self.busy = true;
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::Reasoning { text } => {
                append_streaming(&mut self.lines, Role::Reasoning, &text);
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::MessageStart => {
                self.busy = true;
                self.publish_status();
            }
            BackendEvent::MessageComplete => {
                self.busy = false;
                if !self.window_active {
                    let summary = self
                        .lines
                        .iter()
                        .rev()
                        .find(|line| line.role == Role::Assistant && !line.text.trim().is_empty())
                        .map(|line| clip(&line.text, 120))
                        .unwrap_or_else(|| "reply ready".into());
                    crate::omarchy::notify_turn(&summary);
                }
                self.publish_status();
            }
            BackendEvent::ToolActivity(text) | BackendEvent::Notice(text) => {
                self.lines.push(Line::new(Role::System, text));
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::Error(error) => {
                self.error = Some(error.clone());
                self.lines
                    .push(Line::new(Role::System, format!("⚠ {error}")));
                self.busy = false;
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::Approval(request) => {
                if !self.window_active {
                    let summary = if request.description.trim().is_empty() {
                        request.command.clone()
                    } else {
                        request.description.clone()
                    };
                    crate::omarchy::notify_approval(&summary);
                }
                self.cards.push(Card {
                    id: request.id.clone(),
                    kind: CardKind::Approval(request),
                });
                self.publish_status();
            }
            BackendEvent::Clarify(request) => {
                self.cards.push(Card {
                    id: request.id.clone(),
                    kind: CardKind::Clarify(request),
                });
            }
            BackendEvent::ProfileDetail(detail) => {
                self.inspector.detail_for = Some(detail.name.clone());
                self.inspector.detail = Some(detail);
            }
            BackendEvent::Env(rows) => self.inspector.loaded_env(rows),
            BackendEvent::Usage {
                analytics,
                model,
                portal,
            } => self.usage.loaded(analytics, model, portal),
            BackendEvent::UsageFailed(message) => self.usage.failed(message),
            BackendEvent::Models(options) => self.models.loaded(options),
            BackendEvent::ModelsFailed(message) => self.models.failed(message),
            BackendEvent::ModelStatus(text) => self.models.status = Some(text),
            BackendEvent::ModelInfo(info) => self.usage.model = Some(info),
            BackendEvent::Mcp(rows) => self.mcp.loaded(rows),
            BackendEvent::McpFailed(message) => self.mcp.failed(message),
            BackendEvent::KanbanBoards(boards) => {
                self.kanban_boards = boards.boards;
                if self.kanban_selected.is_none() {
                    self.kanban_selected = Some(boards.current);
                    self.backend.send(Command::LoadKanban {
                        board: self.kanban_selected.clone(),
                    });
                }
            }
            BackendEvent::Kanban { board, data } => {
                if board == self.kanban_selected {
                    self.kanban = Some(data);
                    self.kanban_status = None;
                }
            }
            BackendEvent::KanbanCreated => {
                self.kanban_creating = false;
                self.kanban_draft = None;
                self.kanban_editing = None;
            }
            BackendEvent::KanbanBoardSaved { slug, created } => {
                self.kanban_board_saving = false;
                self.kanban_board_draft = None;
                self.kanban_board_renaming = None;
                if created {
                    self.kanban_selected = Some(slug.clone());
                    self.kanban = None;
                    self.kanban_status = Some("Loading board…".into());
                    self.backend.send(Command::LoadKanban { board: Some(slug) });
                } else {
                    self.kanban_status = Some("Board renamed.".into());
                }
            }
            BackendEvent::KanbanBoardSaveFailed(message) => {
                self.kanban_board_saving = false;
                self.kanban_status = Some(message);
            }
            BackendEvent::KanbanBoardRemoved { slug, hard } => {
                self.kanban_board_remove = None;
                self.kanban_detail_target = None;
                self.kanban_detail = None;
                self.kanban_body_draft = None;
                self.kanban_assignee_draft = None;
                self.kanban_priority_draft = None;
                if self.kanban_selected.as_deref() == Some(slug.as_str()) {
                    self.kanban_selected = None;
                    self.kanban = None;
                }
                self.kanban_status = Some(if hard {
                    format!("Board {slug} deleted.")
                } else {
                    format!("Board {slug} archived.")
                });
            }
            BackendEvent::KanbanBoardRemoveFailed(message) => {
                self.kanban_board_remove = None;
                self.kanban_status = Some(message);
            }
            BackendEvent::KanbanDetail { board, id, detail } => {
                if self.kanban_detail_target.as_ref() == Some(&(board, id)) {
                    self.kanban_detail = Some(detail);
                    self.kanban_status = None;
                }
            }
            BackendEvent::KanbanDetailFailed { board, id, message } => {
                self.kanban_detail_saving = false;
                if self.kanban_detail_target.as_ref() == Some(&(board, id)) {
                    self.kanban_status = Some(message);
                }
            }
            BackendEvent::KanbanDetailSaved { board, id, detail } => {
                self.kanban_detail_saving = false;
                self.kanban_body_draft = None;
                self.kanban_assignee_draft = None;
                self.kanban_priority_draft = None;
                if self.kanban_detail_target.as_ref() == Some(&(board, id)) {
                    self.kanban_detail = Some(detail);
                    self.kanban_status = Some("Task updated.".into());
                }
            }
            BackendEvent::KanbanCommentSaved { board, id } => {
                self.kanban_comment_saving = false;
                if self.kanban_detail_target.as_ref() == Some(&(board.clone(), id.clone())) {
                    self.kanban_comment_draft = None;
                    self.kanban_status = Some("Comment saved; refreshing…".into());
                    self.backend.send(Command::LoadKanbanTask { board, id });
                }
            }
            BackendEvent::KanbanCommentFailed { board, id, message } => {
                self.kanban_comment_saving = false;
                if self.kanban_detail_target.as_ref() == Some(&(board, id)) {
                    self.kanban_status = Some(message);
                }
            }
            BackendEvent::KanbanAttachmentUploaded { board, id } => {
                self.kanban_attachment_busy = false;
                if self.kanban_detail_target.as_ref() == Some(&(board.clone(), id.clone())) {
                    self.kanban_status = Some("Attachment uploaded; refreshing…".into());
                    self.backend.send(Command::LoadKanbanTask { board, id });
                }
            }
            BackendEvent::KanbanAttachmentDeleted { board, id } => {
                self.kanban_attachment_busy = false;
                self.kanban_attachment_delete = None;
                if self.kanban_detail_target.as_ref() == Some(&(board.clone(), id.clone())) {
                    self.kanban_status = Some("Attachment removed; refreshing…".into());
                    self.backend.send(Command::LoadKanbanTask { board, id });
                }
            }
            BackendEvent::KanbanAttachmentDownloaded { filename, bytes } => {
                self.kanban_attachment_busy = false;
                cx.spawn(async move |this, cx| {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_file_name(&filename)
                        .save_file()
                        .await;
                    let message = match picked {
                        Some(file) => {
                            let path = file.path().to_path_buf();
                            match tokio::fs::write(&path, bytes).await {
                                Ok(()) => format!("Saved {}", path.display()),
                                Err(err) => format!("Save failed: {err}"),
                            }
                        }
                        None => "Attachment save cancelled.".to_string(),
                    };
                    this.update(cx, |this, cx| {
                        this.kanban_status = Some(message);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            BackendEvent::KanbanAttachmentFailed { board, id, message } => {
                self.kanban_attachment_busy = false;
                self.kanban_attachment_delete = None;
                if self.kanban_detail_target.as_ref() == Some(&(board, id)) {
                    self.kanban_status = Some(message);
                }
            }
            BackendEvent::KanbanFailed(message) => {
                self.kanban_creating = false;
                self.kanban_status = Some(message);
            }
            BackendEvent::McpOAuth(flow) => {
                self.mcp.oauth_busy = false;
                self.mcp.status = Some(match flow.status.as_str() {
                    "approved" => format!("{} authorized", flow.server_name),
                    "error" => format!(
                        "{}: {}",
                        flow.server_name,
                        flow.error.as_deref().unwrap_or("authorization failed")
                    ),
                    _ => format!("{}: waiting for browser authorization", flow.server_name),
                });
                if self.mcp.oauth_opened.as_deref() != Some(flow.flow_id.as_str())
                    && flow.status == "authorization_required"
                    && let Some(url) = flow.authorization_url.as_deref()
                {
                    self.mcp.oauth_opened = Some(flow.flow_id.clone());
                    match reqwest::Url::parse(url) {
                        Ok(url) if matches!(url.scheme(), "http" | "https") => {
                            self.open_external(url.as_str())
                        }
                        _ => self.mcp.status = Some("Invalid OAuth authorization URL".into()),
                    }
                }
                if matches!(flow.status.as_str(), "approved" | "error") {
                    self.mcp.oauth = None;
                    self.mcp.oauth_opened = None;
                } else {
                    self.mcp.oauth = Some(flow);
                }
            }
            BackendEvent::McpOAuthFailed(message) => {
                self.mcp.oauth_busy = false;
                self.mcp.status = Some(format!("MCP OAuth: {message}"));
            }
            BackendEvent::McpOAuthCancelled(id) => {
                self.mcp.oauth_busy = false;
                if self
                    .mcp
                    .oauth
                    .as_ref()
                    .is_some_and(|flow| flow.flow_id == id)
                {
                    self.mcp.oauth = None;
                    self.mcp.oauth_opened = None;
                    self.mcp.status = Some("MCP authorization cancelled".into());
                }
            }
            BackendEvent::McpToggled { name, enabled } => {
                self.mcp.status = Some(format!(
                    "{name} {}",
                    if enabled { "enabled" } else { "disabled" }
                ));
            }
            BackendEvent::McpTested { name, result } => {
                self.mcp.testing = None;
                self.mcp.status = Some(mcp::test_summary(&name, &result));
            }
            BackendEvent::McpCatalog(catalog) => self.mcp.catalog_loaded(catalog),
            BackendEvent::McpCatalogFailed(message) => self.mcp.catalog_failed(message),
            BackendEvent::McpCreated(name) => {
                self.mcp.form = None;
                self.mcp.status = Some(format!("{name} added"));
            }
            BackendEvent::McpDeleted(name) => {
                self.mcp.confirm_delete = None;
                self.mcp.status = Some(format!("{name} deleted"));
            }
            BackendEvent::McpEdited(name) => {
                self.mcp.form = None;
                self.mcp.status = Some(format!("{name} saved"));
            }
            BackendEvent::McpConfig { name, config } => {
                self.mcp.begin_edit(&name, config);
                self.mcp.status = None;
            }
            BackendEvent::McpInstalled { name, background } => {
                self.mcp.install = None;
                self.mcp.status = Some(if background {
                    format!("{name}: cloning and building in the background")
                } else {
                    format!("{name} installed")
                });
            }
            BackendEvent::Memory(status) => self.memory.loaded(status),
            BackendEvent::MemoryFailed(message) => self.memory.failed(message),
            BackendEvent::MemoryProviderSet(provider) => {
                self.memory.status = Some(format!("switching to {provider}…"));
            }
            BackendEvent::MemoryConfig(config) => self.memory.config_loaded(config),
            BackendEvent::MemoryConfigFailed(message) => self.memory.config_failed(message),
            BackendEvent::MemoryConfigSaved(name) => self.memory.saved(&name),
            BackendEvent::MemoryReset(files) => {
                self.memory.confirm_reset = None;
                self.memory.status = Some(if files.is_empty() {
                    "nothing to reset".to_string()
                } else {
                    format!("reset {}", files.join(" and "))
                });
            }
            BackendEvent::Skills(rows) => self.skills.loaded(rows),
            BackendEvent::SkillsFailed(message) => self.skills.failed(message),
            BackendEvent::SkillToggled { name, enabled } => {
                self.skills.status = Some(format!(
                    "{name} {}",
                    if enabled { "enabled" } else { "disabled" }
                ));
            }
            BackendEvent::HubSearched(rows) => self.skills.hub_searched(rows),
            BackendEvent::HubPreviewed { preview, scan } => {
                self.skills.hub_previewed(preview, scan)
            }
            BackendEvent::HubInstalled { identifier } => self.skills.hub_installed(&identifier),
            BackendEvent::HubFailed(message) => self.skills.hub_failed(message),
            BackendEvent::Sessions {
                rows,
                total,
                offset,
            } => {
                if offset == 0 {
                    self.sessions.loaded(rows, total);
                } else {
                    self.sessions.append(rows, total);
                }
            }
            BackendEvent::SessionsFailed(message) => self.sessions.failed(message),
            BackendEvent::SessionUpdated(_id) => {
                self.sessions.status = Some("session updated".into());
            }
            BackendEvent::SessionMessages { id, messages } => {
                self.lines = sessions::lines_from_messages(&messages);
                self.viewing_history = Some(id);
                self.cards.clear();
                self.error = None;
                self.busy = false;
                self.sessions.open = false;
                self.sessions.focus = false;
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::SessionResumed {
                stored_id,
                session_id,
                messages,
            } => {
                self.session_id = Some(session_id);
                self.viewing_history = None;
                self.lines = sessions::lines_from_messages(&messages);
                self.cards.clear();
                self.error = None;
                self.busy = false;
                self.sessions.open = false;
                self.sessions.focus = false;
                self.sessions.status = Some(format!("resumed {stored_id}"));
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::SessionDeleted(id) => {
                self.sessions.rows.retain(|row| row.id != id);
                self.sessions.total = self.sessions.total.saturating_sub(1);
                if self.session_id.as_deref() == Some(id.as_str()) {
                    self.session_id = None;
                    self.lines.clear();
                }
            }
            BackendEvent::Auxiliary(tasks) => self.models.auxiliary_loaded(tasks),
            BackendEvent::AuxiliaryFailed(message) => self.models.auxiliary_failed(message),
            BackendEvent::AuxiliarySaved {
                task,
                provider,
                model,
            } => {
                self.models.aux_target = None;
                self.models.status = Some(format!(
                    "{} → {provider} · {model}",
                    models::task_label(&task)
                ));
                self.settings.show(Section::Assignments);
            }
            BackendEvent::ReasoningEffort(level) => self.models.reasoning_loaded(level),
            BackendEvent::ReasoningFailed(message) => {
                self.models.status = Some(format!("reasoning: {message}"))
            }
            BackendEvent::Moa(config) => self.moa.loaded(config),
            BackendEvent::MoaSaved(config) => self.moa.saved(config),
            BackendEvent::MoaFailed(message) => self.moa.failed(message),
            BackendEvent::Cron(jobs, targets) => self.automations.loaded(jobs, targets),
            BackendEvent::CronFailed(message) => self.automations.failed(message),
            BackendEvent::CronSaved(message) => self.automations.status = Some(message),
            BackendEvent::CronRuns { id, runs } => self.automations.runs_loaded(id, runs),
            BackendEvent::OAuth(providers) => self.accounts.loaded(providers),
            BackendEvent::OAuthStarted { provider, start } => {
                self.accounts.begin(&provider, start.clone());
                self.open_oauth_url(&start);
            }
            BackendEvent::OAuthPolled { provider, poll } => {
                let succeeded =
                    matches!(poll.status.as_str(), "approved" | "completed" | "success");
                self.accounts.apply_poll(poll);
                if succeeded && self.accounts.flow_done() {
                    self.accounts.status = Some(format!("{provider} login finished"));
                }
            }
            BackendEvent::OAuthSaved(message) => {
                self.accounts.close_flow();
                self.accounts.status = Some(message);
            }
            BackendEvent::OAuthFailed(message) => self.accounts.failed(message),
            BackendEvent::Avatar {
                profile,
                mime,
                bytes,
            } => {
                let format = match mime.as_str() {
                    "image/jpeg" => gpui::ImageFormat::Jpeg,
                    "image/webp" => gpui::ImageFormat::Webp,
                    _ => gpui::ImageFormat::Png,
                };
                let image = std::sync::Arc::new(gpui::Image::from_bytes(format, bytes));
                self.inspector.set_avatar(&profile, image);
            }
            BackendEvent::AvatarCleared { profile } => self.inspector.clear_avatar(&profile),
            BackendEvent::InspectorStatus(text) => {
                self.vault.status = Some(text.clone());
                self.inspector.status = Some(text);
            }
            BackendEvent::Captured { path } => {
                self.lines.push(Line::new(
                    Role::System,
                    format!("screenshot {}", path.display()),
                ));
                self.backend.send(Command::Attach(path));
                self.scroll.scroll_to_bottom();
                self.publish_status();
            }
            BackendEvent::CapturedText(text) => {
                let mut next = self.composer.clone();
                if !next.is_empty() && !next.ends_with(' ') {
                    next.push(' ');
                }
                next.push_str(&text);
                self.composer = next.clone();
                self.composer_input
                    .update(cx, |input, cx| input.emplace(&next, cx));
                self.refresh_mention();
                self.scroll.scroll_to_bottom();
                self.publish_status();
            }
            BackendEvent::CaptureFailed(message) => {
                self.error = Some(message.clone());
                self.lines
                    .push(Line::new(Role::System, format!("⚠ {message}")));
                self.scroll.scroll_to_bottom();
                self.publish_status();
            }
            BackendEvent::Attached { name } => {
                self.attachments.push(name.clone());
                self.lines
                    .push(Line::new(Role::System, format!("attached {name}")));
                self.scroll.scroll_to_bottom();
            }
            BackendEvent::Media { path, mime, bytes } => {
                let format = match mime.as_str() {
                    "image/jpeg" => Some(gpui::ImageFormat::Jpeg),
                    "image/webp" => Some(gpui::ImageFormat::Webp),
                    "image/png" => Some(gpui::ImageFormat::Png),
                    _ => None,
                };
                let image = format
                    .map(|format| Arc::new(gpui::Image::from_bytes(format, bytes)))
                    .ok_or_else(|| format!("Unsupported image type: {mime}"));
                self.media.insert(path, image);
            }
            BackendEvent::MediaFailed { path, message } => {
                self.media.insert(path, Err(message));
            }
            BackendEvent::Spoken => {
                self.voice_status = Some("spoke last reply".into());
            }
            BackendEvent::SpeakFailed(message) => {
                self.voice_status = Some(format!("voice: {message}"));
            }
        }
        self.sync_tray(cx);
        cx.notify();
    }

    // ---- keyboard -------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Settings is modal: it owns the keyboard while it is up, so chat
        // shortcuts cannot fire behind it.
        if self.settings.open {
            // Escape always leaves the sheet, even while a search field owns
            // the keystrokes; blurring first made the first press a no-op.
            if event.keystroke.key == "escape" {
                self.models.focus = false;
                self.vault.focus = None;
                self.close_settings(cx);
                return;
            }
            if (self.settings.section == Section::Vault && self.route_vault_typing(event, cx))
                || (self.settings.section == Section::Model && self.route_models_typing(event, cx))
                || (self.settings.section == Section::Skills && self.route_skills_typing(event, cx))
                || (self.settings.section == Section::Mcp && self.route_mcp_typing(event, cx))
                || (self.settings.section == Section::Kanban && self.route_kanban_typing(event, cx))
                || (self.settings.section == Section::Memory && self.route_memory_typing(event, cx))
                || (self.settings.section == Section::Automations
                    && self.route_cron_typing(event, cx))
                || (self.settings.section == Section::Accounts
                    && self.route_oauth_typing(event, cx))
            {
                return;
            }
            let keystroke = &event.keystroke;
            let primary = keystroke.modifiers.control || keystroke.modifiers.platform;
            if primary && keystroke.key == "v" {
                self.toggle_vault(cx);
            } else if primary && keystroke.key == "m" {
                self.toggle_models(cx);
            } else if primary && keystroke.key == "a" {
                self.toggle_assignments(cx);
            } else if primary && keystroke.key == "u" {
                self.toggle_usage(cx);
            }
            return;
        }
        if event.keystroke.key == "escape" && self.sessions.renaming.is_some() {
            self.sessions.cancel_rename();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && self.sessions.open {
            self.sessions.open = false;
            self.sessions.focus = false;
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape"
            && !self.palette.open
            && !self.mention.open
            && self.inspector.open
        {
            self.inspector.open = false;
            cx.notify();
            return;
        }
        if self.sessions.open && self.route_sessions_typing(event, cx) {
            return;
        }
        if self.rail_search && self.route_rail_typing(event, cx) {
            return;
        }
        if self.route_inspector_typing(event, cx) {
            return;
        }
        let keystroke = &event.keystroke;
        if self.composer_input.focus_handle(cx).is_focused(window) && !self.palette.open {
            let primary = keystroke.modifiers.control || keystroke.modifiers.platform;
            let app_shortcut = primary
                && matches!(
                    keystroke.key.as_str(),
                    "k" | "n"
                        | "r"
                        | "o"
                        | ","
                        | "comma"
                        | "1"
                        | "2"
                        | "3"
                        | "4"
                        | "5"
                        | "6"
                        | "7"
                        | "8"
                        | "9"
                )
                || primary
                    && keystroke.modifiers.shift
                    && matches!(keystroke.key.as_str(), "v" | "m" | "a" | "s" | "u" | "x");
            let mention_key = self.mention.open
                && matches!(
                    keystroke.key.as_str(),
                    "up" | "down" | "tab" | "enter" | "escape"
                );
            if !app_shortcut
                && !mention_key
                && keystroke.key != "escape"
                && !(keystroke.key == "enter" && !keystroke.modifiers.shift && !primary)
            {
                return;
            }
            if keystroke.key == "enter" && !keystroke.modifiers.shift && !primary {
                let composing = self.composer_input.update(cx, |input, cx| {
                    gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some()
                });
                if composing {
                    return;
                }
            }
            if (keystroke.key == "enter" && !keystroke.modifiers.shift && !primary)
                || mention_key
                || app_shortcut
            {
                cx.stop_propagation();
            }
        }
        let focus = if self.palette.open {
            Focus::Palette
        } else if self.mention.open {
            Focus::Mention
        } else {
            Focus::Composer
        };
        let action = keys::resolve(
            Chord {
                key: &keystroke.key,
                primary: keystroke.modifiers.control || keystroke.modifiers.platform,
                shift: keystroke.modifiers.shift,
                alt: keystroke.modifiers.alt,
                text: keystroke.key_char.as_deref(),
            },
            focus,
        );
        self.dispatch(action, cx);
    }

    fn dispatch(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Insert(text) => {
                if self.palette.open {
                    self.palette.query.push_str(&text);
                    self.palette.index = 0;
                } else if self.mention.open {
                    self.mention.query.push_str(&text);
                    self.mention.index = 0;
                } else {
                    self.composer.push_str(&text);
                    self.refresh_mention();
                }
            }
            Action::Backspace => {
                if self.palette.open {
                    self.palette.query.pop();
                    self.palette.index = 0;
                } else if self.mention.open {
                    self.mention.query.pop();
                    self.mention.index = 0;
                } else {
                    self.composer.pop();
                    self.refresh_mention();
                }
            }
            Action::Newline => self.composer.push('\n'),
            Action::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.composer.push_str(&text);
                    self.refresh_mention();
                }
            }
            Action::Send => {
                if self.palette.open {
                    self.accept_palette(cx);
                } else if self.mention.open {
                    self.accept_mention(cx);
                } else {
                    self.submit(cx);
                }
            }
            Action::PaletteAccept => self.accept_palette(cx),
            Action::ListAccept => self.accept_mention(cx),
            Action::PaletteUp | Action::ListUp => {
                if self.palette.open {
                    self.palette.index = self.palette.index.saturating_sub(1);
                } else {
                    self.mention.index = self.mention.index.saturating_sub(1);
                }
            }
            Action::PaletteDown | Action::ListDown => {
                if self.palette.open {
                    let count = self.palette_items().len();
                    self.palette.index = (self.palette.index + 1).min(count.saturating_sub(1));
                } else {
                    let count = self.mention_items().len();
                    self.mention.index = (self.mention.index + 1).min(count.saturating_sub(1));
                }
            }
            Action::OpenPalette => {
                self.palette = Palette {
                    open: true,
                    query: String::new(),
                    index: 0,
                };
                self.mention = Mention::default();
            }
            Action::Dismiss => {
                self.palette = Palette::default();
                self.mention = Mention::default();
            }
            Action::NewChat => self.new_chat(cx),
            Action::Refresh => self.refresh(cx),
            Action::Attach => self.pick_attachment(cx),
            Action::ToggleInspector => self.toggle_inspector(cx),
            Action::ToggleVault => self.toggle_vault(cx),
            Action::ToggleUsage => self.toggle_usage(cx),
            Action::ToggleModels => self.toggle_models(cx),
            Action::ToggleAssignments => self.toggle_assignments(cx),
            Action::ToggleSessions => self.toggle_sessions(cx),
            Action::ToggleSkills => self.toggle_skills(cx),
            Action::ToggleMcp => self.toggle_mcp_page(cx),
            Action::Interrupt => {
                if self.busy {
                    self.backend.send(Command::Interrupt);
                }
            }
            Action::SwitchProfile(index) => {
                if let Some(profile) = self.profiles.get(index - 1) {
                    let name = profile.name.clone();
                    self.select_profile(name, cx);
                }
            }
            Action::Nothing => return,
        }
        cx.notify();
    }

    // ---- mentions -------------------------------------------------------

    /// Open the mention list when the composer's last token starts with `@`.
    fn refresh_mention(&mut self) {
        let token = self
            .composer
            .rsplit(|c: char| c.is_whitespace() || c == '\n')
            .next()
            .unwrap_or_default();

        match token.strip_prefix('@') {
            Some(query) => {
                self.mention = Mention {
                    open: true,
                    query: query.to_string(),
                    index: 0,
                };
            }
            None => self.mention = Mention::default(),
        }
    }

    fn mention_items(&self) -> Vec<String> {
        let query = self.mention.query.to_lowercase();
        let mut items: Vec<String> = self
            .profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect();
        items.extend(self.routines.iter().cloned());
        if query.is_empty() {
            items.truncate(8);
            return items;
        }
        let mut matching: Vec<String> = items
            .into_iter()
            .filter(|item| item.to_lowercase().contains(&query))
            .collect();
        matching.truncate(8);
        matching
    }

    fn accept_mention(&mut self, cx: &mut Context<Self>) {
        let items = self.mention_items();
        let Some(item) = items.get(self.mention.index) else {
            self.mention = Mention::default();
            cx.notify();
            return;
        };

        // Replace the trailing `@query` token with the chosen name.
        match self.composer.rfind('@') {
            Some(at) => {
                self.composer.truncate(at);
                self.composer.push('@');
                self.composer.push_str(item);
                self.composer.push(' ');
            }
            None => {
                self.composer.push('@');
                self.composer.push_str(item);
                self.composer.push(' ');
            }
        }
        self.composer_input
            .update(cx, |input, cx| input.emplace(&self.composer, cx));
        self.mention = Mention::default();
        cx.notify();
    }

    // ---- actions --------------------------------------------------------

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.viewing_history.is_some() {
            self.lines.push(Line::new(
                Role::System,
                "Stored session is read-only — New chat to continue.",
            ));
            self.scroll.scroll_to_bottom();
            cx.notify();
            return;
        }
        let prompt = self.composer.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.composer.clear();
        self.composer_input
            .update(cx, |input, cx| input.emplace("", cx));
        self.mention = Mention::default();
        self.lines.push(Line::new(Role::User, prompt.clone()));
        self.lines.push(Line::new(Role::Assistant, String::new()));
        self.busy = true;
        self.scroll.scroll_to_bottom();
        self.backend.send(Command::Submit(prompt));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.backend.send(Command::Refresh);
        cx.notify();
    }

    fn new_chat(&mut self, cx: &mut Context<Self>) {
        self.backend.send(Command::NewSession);
        self.session_id = None;
        self.viewing_history = None;
        self.lines.clear();
        self.cards.clear();
        self.attachments.clear();
        self.busy = false;
        self.palette = Palette::default();
        self.mention = Mention::default();
        self.publish_status();
        cx.notify();
    }

    fn speak_last(&mut self, cx: &mut Context<Self>) {
        let text = self
            .lines
            .iter()
            .rev()
            .find(|line| line.role == Role::Assistant && !line.text.trim().is_empty())
            .map(|line| line.text.clone());
        match text {
            Some(text) => {
                self.voice_status = Some("synthesizing…".into());
                self.backend.send(Command::Speak { text });
            }
            None => self.voice_status = Some("no assistant reply to speak".into()),
        }
        cx.notify();
    }

    /// Publish the snapshot the Omarchy bar widget reads.
    fn publish_status(&self) {
        let last_reply = self
            .lines
            .iter()
            .rev()
            .find(|line| line.role == Role::Assistant && !line.text.trim().is_empty())
            .map(|line| clip(&line.text, 160));
        crate::status::publish(&crate::status::Snapshot {
            host: self.host.base_url.clone(),
            profile: self.profile_name(),
            connected: self.connection == ConnectionState::Open,
            busy: self.busy,
            approvals: self.cards.len(),
            session: self.session_id.clone(),
            last_reply,
        });
    }

    /// Attach a region screenshot through Omarchy's capture path.
    fn capture_screen(&mut self, cx: &mut Context<Self>) {
        if !crate::omarchy::has("omarchy") {
            self.error = Some("omarchy is not on PATH, so capture is unavailable".into());
            cx.notify();
            return;
        }
        self.lines
            .push(Line::new(Role::System, "select a region to attach…"));
        self.backend.send(Command::CaptureScreenshot);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// OCR a region and put the text in the composer for review.
    fn capture_text(&mut self, cx: &mut Context<Self>) {
        if !crate::omarchy::has("omarchy") {
            self.error = Some("omarchy is not on PATH, so OCR is unavailable".into());
            cx.notify();
            return;
        }
        self.lines
            .push(Line::new(Role::System, "select a region to read…"));
        self.backend.send(Command::CaptureText);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn pick_attachment(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |_this, _cx| {
            if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
                for file in files {
                    backend.send(Command::Attach(file.path().to_path_buf()));
                }
            }
        })
        .detach();
    }

    fn select_profile(&mut self, name: String, cx: &mut Context<Self>) {
        self.palette = Palette::default();
        if self.profile_name() == name {
            return;
        }
        self.host.profile = Some(name.clone());
        self.session_id = None;
        self.lines.clear();
        self.cards.clear();
        self.backend.send(Command::SwitchProfile(name.clone()));
        if self.inspector.open {
            let meta = self
                .profiles
                .iter()
                .find(|profile| profile.name == name)
                .and_then(|profile| profile.ui_meta.clone());
            self.inspector.target(&name);
            self.inspector.seed_look(meta.as_ref());
            self.backend.send(Command::Inspect {
                profile: name.clone(),
            });
        }
        self.lines
            .push(Line::new(Role::System, format!("Switched to {name}")));
        cx.notify();
    }

    /// Typing into the rail's search field.
    fn route_rail_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => {
                self.rail_search = false;
                self.rail_filter.clear();
            }
            "backspace" => {
                self.rail_filter.pop();
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.rail_filter.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    // ---- inspector ------------------------------------------------------

    /// Typing into the inspector's fields takes precedence over the chat
    /// shortcuts; returns true when the keystroke was consumed.
    fn route_inspector_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.inspector.focus else {
            return false;
        };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => self.inspector.focus = None,
            "enter" | "tab" => match field {
                Field::SecretKey => self.inspector.focus = Some(Field::SecretValue),
                Field::SecretValue => self.add_secret(cx),
                Field::Filter => self.inspector.focus = None,
            },
            "backspace" => {
                if let Some(text) = self.field_mut(field) {
                    text.pop();
                }
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref()
                    && let Some(text) = self.field_mut(field)
                {
                    text.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn field_mut(&mut self, field: Field) -> Option<&mut String> {
        match field {
            Field::Filter => Some(&mut self.inspector.filter),
            Field::SecretKey => Some(&mut self.inspector.draft_key),
            Field::SecretValue => Some(&mut self.inspector.draft_value),
        }
    }

    // ---- vault ----------------------------------------------------------

    fn open_vault(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Vault);
        // The sheet lists the host catalog, which rides along with the
        // inspector's describe request; ask even when the panel is closed.
        let profile = self.profile_name();
        self.backend.send(Command::Inspect { profile });
        cx.notify();
    }

    fn open_usage(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Usage);
        self.load_usage(self.usage.days, cx);
    }

    fn load_usage(&mut self, days: u32, cx: &mut Context<Self>) {
        self.settings.show(Section::Usage);
        self.usage.days = days.clamp(1, 365);
        self.usage.loaded = false;
        self.usage.error = None;
        self.backend.send(Command::LoadUsage {
            days: self.usage.days,
        });
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings.hide();
        self.vault.hide();
        self.skills.focus = false;
        self.skills.hub_focus = false;
        self.mcp.focus = false;
        self.models.focus = false;
        cx.notify();
    }

    fn toggle_vault(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Vault {
            self.close_settings(cx);
        } else {
            self.open_vault(cx);
        }
    }

    fn toggle_usage(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Usage {
            self.close_settings(cx);
        } else {
            self.open_usage(cx);
        }
    }

    fn vault_field_mut(&mut self, field: vault::Field) -> Option<&mut String> {
        match field {
            vault::Field::Search => Some(&mut self.vault.query),
            vault::Field::Key => Some(&mut self.vault.key),
            vault::Field::Value => Some(&mut self.vault.value),
        }
    }

    /// Typing into the vault takes precedence over the chat shortcuts.
    fn route_vault_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.vault.focus else {
            return false;
        };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => self.vault.focus = None,
            "enter" | "tab" => match field {
                vault::Field::Key => self.vault.focus = Some(vault::Field::Value),
                vault::Field::Value => self.save_vault_item(cx),
                vault::Field::Search => self.vault.focus = None,
            },
            "backspace" => {
                if let Some(text) = self.vault_field_mut(field) {
                    text.pop();
                }
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref()
                    && let Some(text) = self.vault_field_mut(field)
                {
                    text.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn save_vault_item(&mut self, cx: &mut Context<Self>) {
        let key = self.vault.key.trim().to_uppercase();
        if !vault::valid_key(&key) {
            self.vault.status = Some("Keys look like OPENAI_API_KEY.".into());
            cx.notify();
            return;
        }
        if self.vault.value.is_empty() {
            self.vault.status = Some("A value is required.".into());
            cx.notify();
            return;
        }
        let profile = self.profile_name();
        self.vault.status = Some(format!("saving {key}…"));
        self.backend.send(Command::SetSecret {
            profile,
            key,
            value: self.vault.value.clone(),
        });
        cx.notify();
    }

    fn delete_vault_item(&mut self, key: &str, cx: &mut Context<Self>) {
        let profile = self.profile_name();
        self.vault.status = Some(format!("removing {key}…"));
        self.backend.send(Command::DeleteSecret {
            profile,
            key: key.to_string(),
        });
        cx.notify();
    }

    fn vault_field(
        &self,
        label: &str,
        value: &str,
        placeholder: &str,
        field: vault::Field,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.section_label(label))
            .child(
                div()
                    .id(SharedString::from(format!("vault-field-{label}")))
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg()))
                    .border_1()
                    .when(focused, |this| this.border_color(rgb(accent())))
                    .when(!focused, |this| this.border_color(rgb(border())))
                    .text_xs()
                    .cursor_pointer()
                    .when(value.is_empty(), |this| {
                        this.text_color(rgb(text_dim()))
                            .child(SharedString::from(placeholder.to_string()))
                    })
                    .when(!value.is_empty(), |this| {
                        this.text_color(rgb(text()))
                            .child(SharedString::from(value.to_string()))
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.vault.focus = Some(field);
                        cx.notify();
                    })),
            )
    }

    fn open_memory(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Memory);
        self.load_memory(cx);
    }

    fn load_memory(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Memory);
        self.memory.loaded = false;
        self.memory.error = None;
        self.memory.status = None;
        self.backend.send(Command::LoadMemory);
        cx.notify();
    }

    fn activate_memory_provider(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.memory.status = Some(format!("activating {provider}…"));
        self.backend.send(Command::SetMemoryProvider {
            provider: provider.to_string(),
        });
        cx.notify();
    }

    fn configure_memory(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.memory.form = None;
        self.memory.begin_config(provider);
        self.memory.config_loading = true;
        self.backend.send(Command::LoadMemoryConfig {
            provider: provider.to_string(),
        });
        cx.notify();
    }

    fn save_memory_config(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.memory.form.as_ref() else {
            return;
        };
        match form.values() {
            Ok(values) => {
                let provider = form.name.clone();
                self.memory.status = Some(format!("saving {provider}…"));
                self.backend
                    .send(Command::UpdateMemoryConfig { provider, values });
                cx.notify();
            }
            Err(message) => {
                if let Some(form) = self.memory.form.as_mut() {
                    form.error = Some(message);
                }
                cx.notify();
            }
        }
    }

    fn reset_memory(&mut self, target: &str, cx: &mut Context<Self>) {
        if self.memory.confirm_reset.as_deref() != Some(target) {
            self.memory.confirm_reset = Some(target.to_string());
            self.memory.status = Some(format!(
                "click {} again to delete the file",
                memory::reset_label(target).to_lowercase()
            ));
        } else {
            self.memory.status = Some("resetting memory…".into());
            self.backend.send(Command::ResetMemory {
                target: target.to_string(),
            });
        }
        cx.notify();
    }

    fn route_memory_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.memory.form.is_none() {
            return false;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        let Some(form) = self.memory.form.as_mut() else {
            return false;
        };
        match keystroke.key.as_str() {
            "escape" => {
                self.memory.form = None;
                self.memory.status = None;
            }
            "enter" => self.save_memory_config(cx),
            "tab" => form.focus_next(),
            "backspace" => form.backspace(),
            "space" => {
                if form
                    .focus
                    .and_then(|index| form.fields.get(index))
                    .is_some_and(|field| !field.kind.is_text())
                {
                    form.cycle_focused();
                } else {
                    form.typed(" ");
                }
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    form.typed(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn open_kanban(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Kanban);
        self.kanban_status = Some("Loading board…".into());
        self.backend.send(Command::LoadKanbanBoards);
        if self.kanban_selected.is_some() {
            self.backend.send(Command::LoadKanban {
                board: self.kanban_selected.clone(),
            });
        }
        cx.notify();
    }

    fn route_kanban_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.kanban_priority_draft.is_some() {
            if self.kanban_detail_saving {
                return true;
            }
            let key = &event.keystroke;
            match key.key.as_str() {
                "escape" => {
                    self.kanban_priority_draft = None;
                    cx.notify();
                    return true;
                }
                "enter" => self.save_kanban_priority(cx),
                "backspace" => {
                    self.kanban_priority_draft.as_mut().unwrap().pop();
                }
                _ => {
                    if let Some(text) = key.key_char.as_deref()
                        && let Some(draft) = self.kanban_priority_draft.as_mut()
                        && text.chars().all(|c| c.is_ascii_digit())
                        && draft.len() < 4
                    {
                        draft.push_str(text);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if self.kanban_body_draft.is_some() {
            if self.kanban_detail_saving {
                return true;
            }
            let key = &event.keystroke;
            match key.key.as_str() {
                "escape" => {
                    self.kanban_body_draft = None;
                    cx.notify();
                    return true;
                }
                "enter" if key.modifiers.control || key.modifiers.platform => {
                    self.save_kanban_body(cx)
                }
                "enter" => self.kanban_body_draft.as_mut().unwrap().push('\n'),
                "backspace" => {
                    self.kanban_body_draft.as_mut().unwrap().pop();
                }
                "space" => self.kanban_body_draft.as_mut().unwrap().push(' '),
                _ => {
                    if let Some(text) = key.key_char.as_deref() {
                        self.kanban_body_draft.as_mut().unwrap().push_str(text);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if self.kanban_assignee_draft.is_some() {
            if self.kanban_detail_saving {
                return true;
            }
            let key = &event.keystroke;
            match key.key.as_str() {
                "escape" => {
                    self.kanban_assignee_draft = None;
                    cx.notify();
                    return true;
                }
                "enter" => self.save_kanban_assignee(cx),
                "backspace" => {
                    self.kanban_assignee_draft.as_mut().unwrap().pop();
                }
                "space" => self.kanban_assignee_draft.as_mut().unwrap().push(' '),
                _ => {
                    if let Some(text) = key.key_char.as_deref() {
                        self.kanban_assignee_draft.as_mut().unwrap().push_str(text);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if self.kanban_comment_draft.is_some() {
            if self.kanban_comment_saving {
                return true;
            }
            let key = &event.keystroke;
            if key.modifiers.control || key.modifiers.platform {
                if key.key == "v" {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.kanban_comment_draft.as_mut().unwrap().push_str(&text);
                    }
                    cx.notify();
                    return true;
                }
                return false;
            }
            match key.key.as_str() {
                "enter" if key.modifiers.shift => {
                    self.kanban_comment_draft.as_mut().unwrap().push('\n')
                }
                "enter" => self.submit_kanban_comment(cx),
                "backspace" => {
                    self.kanban_comment_draft.as_mut().unwrap().pop();
                }
                "space" => self.kanban_comment_draft.as_mut().unwrap().push(' '),
                _ => {
                    if let Some(text) = key.key_char.as_deref() {
                        self.kanban_comment_draft.as_mut().unwrap().push_str(text);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if self.kanban_board_draft.is_some() {
            if self.kanban_board_saving {
                return true;
            }
            let key = &event.keystroke;
            if key.modifiers.control || key.modifiers.platform {
                if key.key == "v" {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.kanban_board_draft
                            .as_mut()
                            .unwrap()
                            .push_str(&text.replace(['\n', '\r'], " "));
                    }
                    cx.notify();
                    return true;
                }
                return false;
            }
            match key.key.as_str() {
                "enter" => self.save_kanban_board(cx),
                "backspace" => {
                    self.kanban_board_draft.as_mut().unwrap().pop();
                }
                "space" => self.kanban_board_draft.as_mut().unwrap().push(' '),
                _ => {
                    if let Some(text) = key.key_char.as_deref() {
                        self.kanban_board_draft.as_mut().unwrap().push_str(text);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if self.kanban_creating && self.kanban_draft.is_some() {
            return true;
        }
        let Some(draft) = self.kanban_draft.as_mut() else {
            return false;
        };
        let key = &event.keystroke;
        if key.modifiers.control || key.modifiers.platform {
            if key.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    draft.push_str(&text.replace(['\n', '\r'], " "));
                }
                cx.notify();
                return true;
            }
            return false;
        }
        match key.key.as_str() {
            "enter" => self.create_kanban_task(cx),
            "backspace" => {
                draft.pop();
            }
            "space" => draft.push(' '),
            _ => {
                if let Some(text) = key.key_char.as_deref() {
                    draft.push_str(text);
                }
            }
        }
        cx.notify();
        true
    }

    fn save_kanban_body(&mut self, cx: &mut Context<Self>) {
        if self.kanban_detail_saving {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        let Some(body) = self.kanban_body_draft.as_deref() else {
            return;
        };
        let body = body.trim().to_string();
        self.kanban_detail_saving = true;
        self.kanban_status = Some("Saving description…".into());
        self.backend.send(Command::UpdateKanbanDetail {
            board,
            id,
            patch: serde_json::json!({ "body": body }),
        });
        cx.notify();
    }

    fn save_kanban_assignee(&mut self, cx: &mut Context<Self>) {
        if self.kanban_detail_saving {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        let Some(assignee) = self.kanban_assignee_draft.as_deref() else {
            return;
        };
        // The host treats an empty assignee as "unassigned".
        let assignee = assignee.trim().to_string();
        self.kanban_detail_saving = true;
        self.kanban_status = Some("Saving assignee…".into());
        self.backend.send(Command::UpdateKanbanDetail {
            board,
            id,
            patch: serde_json::json!({ "assignee": assignee }),
        });
        cx.notify();
    }

    fn save_kanban_priority(&mut self, cx: &mut Context<Self>) {
        if self.kanban_detail_saving {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        let Some(draft) = self.kanban_priority_draft.as_deref() else {
            return;
        };
        let Ok(priority) = draft.trim().parse::<i64>() else {
            self.kanban_status = Some("Priority must be a whole number.".into());
            cx.notify();
            return;
        };
        self.kanban_detail_saving = true;
        self.kanban_status = Some("Saving priority…".into());
        self.backend.send(Command::UpdateKanbanDetail {
            board,
            id,
            patch: serde_json::json!({ "priority": priority }),
        });
        cx.notify();
    }

    fn kanban_size_label(bytes: u64) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{bytes} B")
        }
    }

    fn attach_kanban_file(&mut self, cx: &mut Context<Self>) {
        if self.kanban_attachment_busy {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        let backend = self.backend.clone();
        self.kanban_attachment_busy = true;
        self.kanban_status = Some("Choose a file to attach…".into());
        cx.spawn(async move |this, cx| {
            let picked = rfd::AsyncFileDialog::new().pick_file().await;
            match picked {
                Some(file) => {
                    backend.send(Command::UploadKanbanAttachment {
                        board,
                        id,
                        path: file.path().to_path_buf(),
                    });
                    this.update(cx, |this, cx| {
                        this.kanban_status = Some("Uploading attachment…".into());
                        cx.notify();
                    })
                    .ok();
                }
                None => {
                    this.update(cx, |this, cx| {
                        this.kanban_attachment_busy = false;
                        this.kanban_status = None;
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn save_kanban_attachment(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
        if self.kanban_attachment_busy {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        self.kanban_attachment_busy = true;
        self.kanban_status = Some("Fetching attachment…".into());
        self.backend.send(Command::DownloadKanbanAttachment {
            board,
            id,
            attachment_id,
        });
        cx.notify();
    }

    fn delete_kanban_attachment(&mut self, attachment_id: i64, cx: &mut Context<Self>) {
        if self.kanban_attachment_busy {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        self.kanban_attachment_busy = true;
        self.kanban_attachment_delete = None;
        self.kanban_status = Some("Removing attachment…".into());
        self.backend.send(Command::DeleteKanbanAttachment {
            board,
            id,
            attachment_id,
        });
        cx.notify();
    }

    fn submit_kanban_comment(&mut self, cx: &mut Context<Self>) {
        if self.kanban_comment_saving {
            return;
        }
        let Some((board, id)) = self.kanban_detail_target.clone() else {
            return;
        };
        let Some(body) = self
            .kanban_comment_draft
            .as_deref()
            .map(str::trim)
            .filter(|body| !body.is_empty())
        else {
            self.kanban_status = Some("Enter a comment first.".into());
            cx.notify();
            return;
        };
        self.backend.send(Command::CommentKanbanTask {
            board,
            id,
            body: body.to_string(),
        });
        self.kanban_comment_saving = true;
        self.kanban_status = Some("Sending comment…".into());
        cx.notify();
    }

    fn save_kanban_board(&mut self, cx: &mut Context<Self>) {
        if self.kanban_board_saving {
            return;
        }
        let Some(value) = self
            .kanban_board_draft
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            self.kanban_status = Some("Enter a board name first.".into());
            cx.notify();
            return;
        };
        let command = if let Some(slug) = &self.kanban_board_renaming {
            Command::RenameKanbanBoard {
                slug: slug.clone(),
                name: value.to_string(),
            }
        } else {
            Command::CreateKanbanBoard {
                slug: value.to_string(),
            }
        };
        self.kanban_board_saving = true;
        self.kanban_status = Some("Saving board…".into());
        self.backend.send(command);
        cx.notify();
    }

    fn create_kanban_task(&mut self, cx: &mut Context<Self>) {
        if self.kanban_creating {
            return;
        }
        let Some(title) = self
            .kanban_draft
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        else {
            self.kanban_status = Some("Enter a task title first.".into());
            cx.notify();
            return;
        };
        self.kanban_creating = true;
        if let Some(id) = &self.kanban_editing {
            self.backend.send(Command::UpdateKanbanTask {
                id: id.clone(),
                patch: serde_json::json!({"title": title}),
                board: self.kanban_selected.clone(),
                editing: true,
            });
            self.kanban_status = Some("Updating task…".into());
        } else {
            self.backend.send(Command::CreateKanbanTask {
                title: title.to_string(),
                board: self.kanban_selected.clone(),
            });
            self.kanban_status = Some("Creating triage task…".into());
        }
        cx.notify();
    }

    fn kanban_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("kanban-board")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();
        if let Some(status) = &self.kanban_status {
            list = list.child(self.note(status));
        }
        if let Some(detail) = &self.kanban_detail {
            let task = &detail.task;
            let mut panel = div()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_3()
                .rounded_lg()
                .bg(rgb(surface()))
                .child(self.section_label(&format!("TASK · {}", task.status)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(SharedString::from(task.title.clone())),
                );
            for (label, value) in [
                ("Description", task.body.as_deref()),
                ("Assignee", task.assignee.as_deref()),
                ("Summary", task.latest_summary.as_deref()),
                ("Result", task.result.as_deref()),
            ] {
                if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                    panel = panel.child(self.section_label(label)).child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(value.to_string())),
                    );
                }
            }
            panel = panel.child(self.section_label("PRIORITY")).child(
                div()
                    .text_sm()
                    .text_color(rgb(text()))
                    .child(SharedString::from(task.priority.to_string())),
            );
            panel = panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .id("kanban-edit-body")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_detail_saving && !this.kanban_attachment_busy {
                                    let current = this
                                        .kanban_detail
                                        .as_ref()
                                        .and_then(|detail| detail.task.body.clone())
                                        .unwrap_or_default();
                                    this.kanban_assignee_draft = None;
                                    this.kanban_body_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit description"),
                    )
                    .child(
                        div()
                            .id("kanban-edit-assignee")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_detail_saving && !this.kanban_attachment_busy {
                                    let current = this
                                        .kanban_detail
                                        .as_ref()
                                        .and_then(|detail| detail.task.assignee.clone())
                                        .unwrap_or_default();
                                    this.kanban_body_draft = None;
                                    this.kanban_assignee_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit assignee"),
                    )
                    .child(
                        div()
                            .id("kanban-edit-priority")
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_detail_saving && !this.kanban_attachment_busy {
                                    let current = this
                                        .kanban_detail
                                        .as_ref()
                                        .map(|detail| detail.task.priority.to_string())
                                        .unwrap_or_default();
                                    this.kanban_body_draft = None;
                                    this.kanban_assignee_draft = None;
                                    this.kanban_priority_draft = Some(current);
                                    cx.notify();
                                }
                            }))
                            .child("Edit priority"),
                    ),
            );
            if let Some(draft) = &self.kanban_body_draft {
                panel = panel
                    .child(self.section_label("DESCRIPTION"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Ctrl+Enter saves · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-body-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_body(cx)),
                                    )
                                    .child(if self.kanban_detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save description"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-body-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban_detail_saving {
                                            this.kanban_body_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            if let Some(draft) = &self.kanban_assignee_draft {
                panel = panel
                    .child(self.section_label("ASSIGNEE"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Enter saves · empty clears the assignee · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-assignee-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_assignee(cx)),
                                    )
                                    .child(if self.kanban_detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save assignee"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-assignee-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban_detail_saving {
                                            this.kanban_assignee_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            if let Some(draft) = &self.kanban_priority_draft {
                panel = panel
                    .child(self.section_label("PRIORITY"))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Digits only · Enter saves · Escape cancels"))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("kanban-priority-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_sm()
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_kanban_priority(cx)),
                                    )
                                    .child(if self.kanban_detail_saving {
                                        "Saving…"
                                    } else {
                                        "Save priority"
                                    }),
                            )
                            .child(
                                div()
                                    .id("kanban-priority-cancel")
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.kanban_detail_saving {
                                            this.kanban_priority_draft = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child("Cancel"),
                            ),
                    );
            }
            panel = panel
                .child(self.section_label(&format!("ATTACHMENTS · {}", detail.attachments.len())));
            if detail.attachments.is_empty() {
                panel = panel.child(self.note("No attachments."));
            }
            for attachment in &detail.attachments {
                let attachment_id = attachment.id;
                let removing = self.kanban_attachment_delete == Some(attachment_id);
                let mut row = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(attachment.filename.clone())),
                            )
                            .child(div().text_xs().text_color(rgb(text_muted())).child(
                                SharedString::from(format!(
                                    "{} · {}",
                                    Self::kanban_size_label(attachment.size),
                                    attachment
                                        .uploaded_by
                                        .clone()
                                        .unwrap_or_else(|| "unknown".into())
                                )),
                            )),
                    );
                if removing {
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-remove-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_kanban_attachment(attachment_id, cx)
                                }))
                                .child("Confirm remove"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-cancel-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kanban_attachment_delete = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        );
                } else {
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-save-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_kanban_attachment(attachment_id, cx)
                                }))
                                .child("Save"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "kanban-attachment-delete-{attachment_id}"
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.kanban_attachment_busy {
                                        this.kanban_attachment_delete = Some(attachment_id);
                                        cx.notify();
                                    }
                                }))
                                .child("Remove"),
                        );
                }
                panel = panel.child(row);
            }
            panel = panel.child(
                div()
                    .id("kanban-attach-file")
                    .text_sm()
                    .text_color(rgb(accent()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.attach_kanban_file(cx)))
                    .child(if self.kanban_attachment_busy {
                        "Attaching…"
                    } else {
                        "Attach file"
                    }),
            );
            panel = panel.child(self.section_label("COMMENTS"));
            if detail.comments.is_empty() {
                panel = panel.child(self.note("No comments yet."));
            }
            for comment in &detail.comments {
                panel = panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .child(div().text_xs().text_color(rgb(text_muted())).child(
                            SharedString::from(
                                comment.author.clone().unwrap_or_else(|| "Unknown".into()),
                            ),
                        ))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(text()))
                                .child(SharedString::from(comment.body.clone())),
                        ),
                );
            }
            if let Some(draft) = &self.kanban_comment_draft {
                panel = panel
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(self.note("Enter sends · Shift+Enter adds a line"))
                    .child(
                        div()
                            .id("kanban-comment-submit")
                            .px_3()
                            .py_1()
                            .text_sm()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.submit_kanban_comment(cx)))
                            .child(if self.kanban_comment_saving {
                                "Sending…"
                            } else {
                                "Send comment"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-comment-cancel")
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_comment_saving {
                                    this.kanban_comment_draft = None;
                                    cx.notify();
                                }
                            }))
                            .child("Cancel comment"),
                    );
            } else {
                panel = panel.child(
                    div()
                        .id("kanban-comment-new")
                        .text_sm()
                        .text_color(rgb(accent()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.kanban_creating
                                && !this.kanban_board_saving
                                && this.kanban_draft.is_none()
                                && this.kanban_board_draft.is_none()
                            {
                                this.kanban_comment_draft = Some(String::new());
                                cx.notify();
                            }
                        }))
                        .child("Add comment"),
                );
            }
            panel = panel.child(
                div()
                    .id("kanban-detail-close")
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.kanban_comment_saving
                            && !this.kanban_attachment_busy
                            && !this.kanban_detail_saving
                        {
                            this.kanban_detail_target = None;
                            this.kanban_detail = None;
                            this.kanban_comment_draft = None;
                            this.kanban_attachment_delete = None;
                            this.kanban_body_draft = None;
                            this.kanban_assignee_draft = None;
                            this.kanban_priority_draft = None;
                            cx.notify();
                        }
                    }))
                    .child("Close details"),
            );
            list = list.child(panel);
        }
        if let Some(draft) = &self.kanban_board_draft {
            list = list.child(self.note(if self.kanban_board_renaming.is_some() {
                "Board display name (slug stays unchanged)"
            } else {
                "New board slug (host validates it; existing slugs open the existing board)"
            }));
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(
                        div()
                            .id("kanban-board-save")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.save_kanban_board(cx)))
                            .child(if self.kanban_board_saving {
                                "Saving…"
                            } else {
                                "Save"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-board-cancel")
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_board_saving {
                                    this.kanban_board_draft = None;
                                    this.kanban_board_renaming = None;
                                    cx.notify();
                                }
                            }))
                            .child("Cancel"),
                    ),
            );
        }
        if let Some(draft) = &self.kanban_draft {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .border_color(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!("{draft}▌"))),
                    )
                    .child(
                        div()
                            .id("kanban-create-submit")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_sm()
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.create_kanban_task(cx)))
                            .child(if self.kanban_editing.is_some() {
                                "Save"
                            } else {
                                "Create"
                            }),
                    )
                    .child(
                        div()
                            .id("kanban-create-cancel")
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_creating {
                                    this.kanban_draft = None;
                                    this.kanban_editing = None;
                                }
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            );
            if self.kanban_editing.is_none() {
                list = list.child(
                    self.note("New tasks start in triage; no worker is assigned or dispatched."),
                );
            }
        }
        if !self.kanban_boards.is_empty() {
            let mut choices = div().flex().flex_wrap().gap_2();
            for board in &self.kanban_boards {
                let slug = board.slug.clone();
                let selected = self.kanban_selected.as_ref() == Some(&slug);
                choices = choices.child(
                    div()
                        .id(SharedString::from(format!("kanban-board-{slug}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |v| {
                            v.bg(rgb(accent_soft())).text_color(rgb(accent()))
                        })
                        .when(!selected, |v| v.bg(rgb(surface())).text_color(rgb(text())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.kanban_creating
                                && !this.kanban_board_saving
                                && this.kanban_board_draft.is_none()
                                && !this.kanban_comment_saving
                                && this.kanban_comment_draft.is_none()
                                && !this.kanban_attachment_busy
                                && !this.kanban_detail_saving
                                && this.kanban_body_draft.is_none()
                                && this.kanban_assignee_draft.is_none()
                            {
                                this.kanban_selected = Some(slug.clone());
                                this.kanban_detail_target = None;
                                this.kanban_detail = None;
                                this.kanban_attachment_delete = None;
                                this.kanban_board_remove = None;
                                this.kanban_body_draft = None;
                                this.kanban_assignee_draft = None;
                                this.kanban_priority_draft = None;
                                this.kanban_comment_draft = None;
                                this.kanban = None;
                                this.kanban_draft = None;
                                this.kanban_editing = None;
                                this.kanban_delete = None;
                                this.kanban_status = Some("Loading board…".into());
                                this.backend.send(Command::LoadKanban {
                                    board: Some(slug.clone()),
                                });
                                cx.notify();
                            }
                        }))
                        .child(SharedString::from(board.name.clone())),
                );
            }
            list = list.child(choices);
        }
        if let Some(selected) = &self.kanban_selected {
            let archive_pending =
                self.kanban_board_remove.as_ref() == Some(&(selected.clone(), false));
            let delete_pending =
                self.kanban_board_remove.as_ref() == Some(&(selected.clone(), true));
            let archive_slug = selected.clone();
            let delete_slug = selected.clone();
            let archive_label = if archive_pending {
                "Confirm archive"
            } else {
                "Archive board"
            };
            let delete_label = if delete_pending {
                "Confirm delete"
            } else {
                "Delete board"
            };
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("kanban-board-archive")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.kanban_board_saving
                                    || this.kanban_detail_saving
                                    || this.kanban_attachment_busy
                                    || this.kanban_comment_saving
                                {
                                    return;
                                }
                                if this.kanban_board_remove.as_ref()
                                    == Some(&(archive_slug.clone(), false))
                                {
                                    this.kanban_board_remove = None;
                                    this.kanban_status = Some("Archiving board…".into());
                                    this.backend.send(Command::RemoveKanbanBoard {
                                        slug: archive_slug.clone(),
                                        hard: false,
                                    });
                                } else {
                                    this.kanban_board_remove =
                                        Some((archive_slug.clone(), false));
                                    this.kanban_status = Some(
                                        "Archive this board? Click again to confirm; its tasks stay on disk."
                                            .into(),
                                    );
                                }
                                cx.notify();
                            }))
                            .child(archive_label),
                    )
                    .child(
                        div()
                            .id("kanban-board-delete")
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(danger()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.kanban_board_saving
                                    || this.kanban_detail_saving
                                    || this.kanban_attachment_busy
                                    || this.kanban_comment_saving
                                {
                                    return;
                                }
                                if this.kanban_board_remove.as_ref()
                                    == Some(&(delete_slug.clone(), true))
                                {
                                    this.kanban_board_remove = None;
                                    this.kanban_status = Some("Deleting board…".into());
                                    this.backend.send(Command::RemoveKanbanBoard {
                                        slug: delete_slug.clone(),
                                        hard: true,
                                    });
                                } else {
                                    this.kanban_board_remove = Some((delete_slug.clone(), true));
                                    this.kanban_status = Some(
                                        "Delete this board and its tasks permanently? Click again to confirm."
                                            .into(),
                                    );
                                }
                                cx.notify();
                            }))
                            .child(delete_label),
                    )
                    .when(self.kanban_board_remove.is_some(), |this| {
                        this.child(
                            div()
                                .id("kanban-board-remove-cancel")
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.kanban_board_remove = None;
                                    this.kanban_status = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        )
                    }),
            );
        }
        if let Some(board) = &self.kanban {
            for column in &board.columns {
                let mut section =
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.section_label(&format!(
                            "{} · {}",
                            column.name.to_uppercase(),
                            column.tasks.len()
                        )));
                for task in &column.tasks {
                    let id = task.id.clone();
                    let next = match task.status.as_str() {
                        "triage" => Some("todo"),
                        "todo" => Some("ready"),
                        "ready" | "running" => Some("review"),
                        "review" => Some("done"),
                        "blocked" | "scheduled" => Some("ready"),
                        _ => None,
                    };
                    let mut row = div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(bg_elev()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(task.title.clone())),
                                )
                                .when_some(task.assignee.clone(), |this, assignee| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .child(SharedString::from(assignee)),
                                    )
                                }),
                        );
                    if let Some(next) = next {
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("kanban-move-{id}")))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(surface()))
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.kanban_status = Some(format!("Moving task to {next}…"));
                                    this.backend.send(Command::UpdateKanbanTask {
                                        id: id.clone(),
                                        patch: serde_json::json!({"status": next}),
                                        board: this.kanban_selected.clone(),
                                        editing: false,
                                    });
                                    cx.notify();
                                }))
                                .child(SharedString::from(format!("→ {next}"))),
                        );
                    }
                    let detail_id = task.id.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("kanban-detail-{}", task.id)))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.kanban_comment_saving
                                    && this.kanban_comment_draft.is_none()
                                    && this.kanban_board_draft.is_none()
                                    && this.kanban_draft.is_none()
                                    && let Some(board) = this.kanban_selected.clone()
                                {
                                    this.kanban_detail_target =
                                        Some((board.clone(), detail_id.clone()));
                                    this.kanban_detail = None;
                                    this.kanban_attachment_delete = None;
                                    this.kanban_body_draft = None;
                                    this.kanban_assignee_draft = None;
                                    this.kanban_status = Some("Loading task details…".into());
                                    this.backend.send(Command::LoadKanbanTask {
                                        board,
                                        id: detail_id.clone(),
                                    });
                                    cx.notify();
                                }
                            }))
                            .child("Details"),
                    );
                    let edit_id = task.id.clone();
                    let edit_title = task.title.clone();
                    let delete_id = task.id.clone();
                    let confirm_delete = self.kanban_delete.as_deref() == Some(task.id.as_str());
                    row = row
                        .child(
                            div()
                                .id(SharedString::from(format!("kanban-edit-{}", task.id)))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.kanban_creating
                                        && !this.kanban_board_saving
                                        && this.kanban_comment_draft.is_none()
                                        && !this.kanban_comment_saving
                                        && this.kanban_board_draft.is_none()
                                    {
                                        this.kanban_editing = Some(edit_id.clone());
                                        this.kanban_draft = Some(edit_title.clone());
                                        this.kanban_delete = None;
                                        cx.notify();
                                    }
                                }))
                                .child("Edit"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("kanban-delete-{}", task.id)))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.kanban_delete.as_deref() == Some(delete_id.as_str()) {
                                        this.kanban_delete = None;
                                        this.kanban_status = Some("Deleting task…".into());
                                        this.backend.send(Command::DeleteKanbanTask {
                                            id: delete_id.clone(),
                                            board: this.kanban_selected.clone(),
                                        });
                                    } else {
                                        this.kanban_delete = Some(delete_id.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(if confirm_delete {
                                    "Confirm delete"
                                } else {
                                    "Delete"
                                }),
                        );
                    section = section.child(row);
                }
                list = list.child(section);
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_xs().text_color(rgb(text_muted())).child(
                        "Board selection is local · status changes use the host's validation",
                    ))
                    .child(
                        div()
                            .id("kanban-create-board")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_board_saving
                                    && !this.kanban_creating
                                    && this.kanban_draft.is_none()
                                    && this.kanban_comment_draft.is_none()
                                    && !this.kanban_comment_saving
                                {
                                    this.kanban_board_renaming = None;
                                    this.kanban_board_draft = Some(String::new());
                                    cx.notify();
                                }
                            }))
                            .child("＋ Board"),
                    )
                    .child(
                        div()
                            .id("kanban-rename-board")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.kanban_board_saving
                                    && !this.kanban_creating
                                    && this.kanban_draft.is_none()
                                    && this.kanban_comment_draft.is_none()
                                    && !this.kanban_comment_saving
                                    && let Some(board) = this.kanban_boards.iter().find(|board| {
                                        Some(&board.slug) == this.kanban_selected.as_ref()
                                    })
                                {
                                    this.kanban_board_renaming = Some(board.slug.clone());
                                    this.kanban_board_draft = Some(board.name.clone());
                                    cx.notify();
                                }
                            }))
                            .child("Rename board"),
                    )
                    .child(
                        div()
                            .id("kanban-create")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(accent_soft()))
                            .text_xs()
                            .text_color(rgb(accent()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.kanban_draft.is_none()
                                    && this.kanban_board_draft.is_none()
                                    && !this.kanban_board_saving
                                    && this.kanban_comment_draft.is_none()
                                    && !this.kanban_comment_saving
                                {
                                    this.kanban_editing = None;
                                    this.kanban_draft = Some(String::new());
                                }
                                cx.notify();
                            }))
                            .child("＋ Task"),
                    )
                    .child(
                        div()
                            .id("kanban-refresh")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(bg_elev()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.open_kanban(cx)))
                            .child("Refresh"),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn open_mcp(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Mcp);
        self.load_mcp(cx);
    }

    fn load_mcp(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Mcp);
        self.mcp.loaded = false;
        self.mcp.error = None;
        self.mcp.status = None;
        self.mcp.testing = None;
        self.mcp.form = None;
        self.mcp.install = None;
        self.mcp.confirm_delete = None;
        self.backend.send(Command::LoadMcp);
        if self.mcp.tab == mcp::Tab::Catalog {
            self.load_mcp_catalog(cx);
        }
        cx.notify();
    }

    fn start_mcp_oauth(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.mcp.oauth_busy || self.mcp.oauth.is_some() {
            self.mcp.status = Some("An MCP authorization is already in progress".into());
        } else {
            self.mcp.oauth_busy = true;
            self.mcp.status = Some(format!("Starting authorization for {name}…"));
            self.backend.send(Command::StartMcpOAuth {
                name: name.into(),
                // The MCP list and management endpoints currently use the host's
                // default profile; authorize the same server that is displayed.
                profile: None,
            });
        }
        cx.notify();
    }

    fn cancel_mcp_oauth(&mut self, cx: &mut Context<Self>) {
        if !self.mcp.oauth_busy
            && let Some(flow) = &self.mcp.oauth
        {
            self.mcp.oauth_busy = true;
            self.backend.send(Command::CancelMcpOAuth {
                id: flow.flow_id.clone(),
            });
            self.mcp.status = Some("Cancelling authorization…".into());
            cx.notify();
        }
    }

    fn load_mcp_catalog(&mut self, cx: &mut Context<Self>) {
        self.mcp.catalog_loading = true;
        self.mcp.catalog_error = None;
        self.backend.send(Command::LoadMcpCatalog);
        cx.notify();
    }

    fn begin_mcp_create(&mut self, cx: &mut Context<Self>) {
        self.mcp.begin_create();
        cx.notify();
    }

    fn edit_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        self.mcp.status = Some(format!("loading {name}…"));
        self.backend.send(Command::LoadMcpConfig {
            name: name.to_string(),
        });
        cx.notify();
    }

    fn save_mcp_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.mcp.form.as_ref() else {
            return;
        };
        if form.is_edit() {
            match form.edit_config() {
                Ok(config) => {
                    let old_name = form
                        .old_name
                        .clone()
                        .unwrap_or_else(|| form.name.trim().to_string());
                    let name = form.name.trim().to_string();
                    self.mcp.status = Some(format!("saving {name}…"));
                    self.backend.send(Command::SaveMcp {
                        old_name,
                        name,
                        config,
                    });
                    cx.notify();
                }
                Err(message) => {
                    if let Some(form) = self.mcp.form.as_mut() {
                        form.error = Some(message);
                    }
                    cx.notify();
                }
            }
            return;
        }
        match form.create_body() {
            Ok(body) => {
                let name = form.name.trim().to_string();
                self.mcp.status = Some(format!("adding {name}…"));
                self.backend.send(Command::CreateMcp { body });
                cx.notify();
            }
            Err(message) => {
                if let Some(form) = self.mcp.form.as_mut() {
                    form.error = Some(message);
                }
                cx.notify();
            }
        }
    }

    fn delete_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.mcp.confirm_delete.as_deref() != Some(name) {
            self.mcp.confirm_delete = Some(name.to_string());
            self.mcp.status = Some(format!("click Delete again to remove {name}"));
        } else {
            self.mcp.status = Some(format!("deleting {name}…"));
            self.backend.send(Command::DeleteMcp {
                name: name.to_string(),
            });
        }
        cx.notify();
    }

    fn install_catalog(&mut self, entry: &hermes_api::McpCatalogEntry, cx: &mut Context<Self>) {
        if entry.required_env.iter().any(|field| field.required) {
            self.mcp.begin_install(entry);
            cx.notify();
            return;
        }
        self.mcp.status = Some(format!("installing {}…", entry.name));
        self.backend.send(Command::InstallMcp {
            name: entry.name.clone(),
            env: serde_json::json!({}),
            enable: true,
        });
        cx.notify();
    }

    fn submit_catalog_install(&mut self, cx: &mut Context<Self>) {
        let Some(install) = self.mcp.install.as_ref() else {
            return;
        };
        match install.values() {
            Ok(env) => {
                let name = install.name.clone();
                self.mcp.status = Some(format!("installing {name}…"));
                self.backend.send(Command::InstallMcp {
                    name,
                    env,
                    enable: true,
                });
                cx.notify();
            }
            Err(message) => {
                if let Some(install) = self.mcp.install.as_mut() {
                    install.error = Some(message);
                }
                cx.notify();
            }
        }
    }

    fn toggle_mcp_page(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Mcp {
            self.close_settings(cx);
        } else {
            self.open_mcp(cx);
        }
    }

    fn set_mcp_enabled(&mut self, name: &str, enabled: bool, cx: &mut Context<Self>) {
        self.mcp.status = Some(format!(
            "{} {}…",
            name,
            if enabled { "enabling" } else { "disabling" }
        ));
        self.backend.send(Command::SetMcp {
            name: name.to_string(),
            enabled,
        });
        cx.notify();
    }

    fn test_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        self.mcp.testing = Some(name.to_string());
        self.mcp.status = None;
        self.backend.send(Command::TestMcp {
            name: name.to_string(),
        });
        cx.notify();
    }

    fn route_mcp_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        if let Some(form) = self.mcp.form.as_mut() {
            match keystroke.key.as_str() {
                "escape" => {
                    self.mcp.form = None;
                    self.mcp.status = None;
                }
                "enter" => {
                    self.save_mcp_form(cx);
                }
                "tab" => form.focus_next(),
                "backspace" => form.backspace(),
                _ => {
                    if let Some(chars) = keystroke.key_char.as_deref() {
                        form.typed(chars);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if let Some(install) = self.mcp.install.as_mut() {
            match keystroke.key.as_str() {
                "escape" => {
                    self.mcp.install = None;
                    self.mcp.status = None;
                }
                "enter" => self.submit_catalog_install(cx),
                "tab" => install.focus_next(),
                "backspace" => {
                    if let Some(field) = install.env.get_mut(install.focus) {
                        field.value.pop();
                    }
                }
                _ => {
                    if let Some(chars) = keystroke.key_char.as_deref()
                        && let Some(field) = install.env.get_mut(install.focus)
                    {
                        field.value.push_str(chars);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if !self.mcp.focus {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" | "enter" => self.mcp.focus = false,
            "backspace" => {
                self.mcp.query.pop();
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.mcp.query.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn memory_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.memory.form.is_some() {
            return self.memory_form_body(cx);
        }
        let mut list = div()
            .id("memory-list")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();
        let mut reset_row = div().flex().items_center().gap_2();
        for (target, label) in [
            ("memory", "Reset MEMORY.md"),
            ("user", "Reset USER.md"),
            ("all", "Reset both"),
        ] {
            let confirming = self.memory.confirm_reset.as_deref() == Some(target);
            reset_row = reset_row.child(
                div()
                    .id(SharedString::from(format!("memory-reset-{target}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if confirming { accent_soft() } else { surface() }))
                    .text_xs()
                    .text_color(rgb(if confirming { accent() } else { text_muted() }))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
                    .on_click(
                        cx.listener(move |this, _event, _window, cx| this.reset_memory(target, cx)),
                    )
                    .child(if confirming { "Confirm reset" } else { label }),
            );
        }
        if !self.memory.loaded {
            list = list.child(self.note("Loading memory providers…"));
        } else if let Some(error) = self.memory.error.as_deref() {
            list = list.child(self.note(error));
        } else {
            list = list
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .child(self.section_label("ACTIVE PROVIDER"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(text()))
                                .child(SharedString::from(if self.memory.active.is_empty() {
                                    "builtin (none selected)".to_string()
                                } else {
                                    self.memory.active.clone()
                                })),
                        )
                        .child(div().text_xs().text_color(rgb(text_muted())).child(
                            SharedString::from(memory::files_summary(
                                self.memory.memory_bytes,
                                self.memory.user_bytes,
                            )),
                        ))
                        .child(reset_row),
                )
                .child(self.section_label("DISCOVERED"));
            for provider in &self.memory.providers {
                let name = provider.name.clone();
                let use_name = name.clone();
                let config_name = name.clone();
                let can_activate = memory::can_activate(provider, &self.memory.active);
                let is_active = provider.name == self.memory.active;
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(provider.name.clone()))
                                        .child(
                                            div()
                                                .px_1()
                                                .rounded_sm()
                                                .bg(rgb(surface()))
                                                .text_color(rgb(if is_active {
                                                    accent()
                                                } else {
                                                    text_dim()
                                                }))
                                                .child(SharedString::from(
                                                    memory::provider_status(provider),
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .child(SharedString::from(provider.description.clone())),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(can_activate, |this| {
                                    this.child(
                                        div()
                                            .id(SharedString::from(format!("memory-use-{name}")))
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .bg(rgb(accent_soft()))
                                            .text_xs()
                                            .text_color(rgb(accent()))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(rgb(surface_hi())))
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    this.activate_memory_provider(&use_name, cx)
                                                },
                                            ))
                                            .child("Use"),
                                    )
                                })
                                .child(
                                    div()
                                        .id(SharedString::from(format!("memory-config-{name}")))
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(surface()))
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .cursor_pointer()
                                        .hover(|this| {
                                            this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                        })
                                        .on_click(cx.listener(move |this, _event, _window, cx| {
                                            this.configure_memory(&config_name, cx)
                                        }))
                                        .child("Configure"),
                                ),
                        ),
                );
            }
            if self.memory.providers.is_empty() {
                list = list.child(self.note("No memory providers discovered."));
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when_some(self.memory.status.clone(), |this, status| {
                this.child(
                    div()
                        .mx_5()
                        .mt_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(surface_hi()))
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(SharedString::from(status)),
                )
            })
            .child(list)
            .into_any_element()
    }

    fn mcp_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.mcp.form.is_some() {
            return self.mcp_form_body(cx);
        }
        if self.mcp.install.is_some() {
            return self.mcp_install_body(cx);
        }
        if self.mcp.tab == mcp::Tab::Catalog {
            return self.mcp_catalog_body(cx);
        }
        let rows = mcp::filtered(&self.mcp.rows, &self.mcp.query);
        let shown = rows.len();
        let focus = self.mcp.focus;
        let enabled = mcp::enabled_count(&self.mcp.rows);
        let total = self.mcp.rows.len();
        let testing = self.mcp.testing.clone();
        let confirm_delete = self.mcp.confirm_delete.clone();

        let mut list = div()
            .id("mcp-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if !self.mcp.loaded {
            list = list.child(self.note("Loading MCP servers…"));
        } else if let Some(error) = self.mcp.error.as_deref() {
            list = list.child(self.note(error));
        } else if rows.is_empty() {
            list = list.child(self.note("No MCP servers match this search."));
        }
        for server in rows {
            let toggle_name = server.name.clone();
            let test_name = server.name.clone();
            let auth_name = server.name.clone();
            let can_auth =
                mcp::transport(server) == "http" && mcp::auth(server).as_deref() != Some("header");
            let edit_name = server.name.clone();
            let delete_name = server.name.clone();
            let is_enabled = server.enabled;
            let is_testing = testing.as_deref() == Some(server.name.as_str());
            let is_deleting = confirm_delete.as_deref() == Some(server.name.as_str());
            list = list.child(
                div()
                    .id(SharedString::from(format!("mcp-{}", server.name)))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .when(is_enabled, |this| {
                                                        this.text_color(rgb(text()))
                                                    })
                                                    .when(!is_enabled, |this| {
                                                        this.text_color(rgb(text_muted()))
                                                    })
                                                    .child(SharedString::from(server.name.clone())),
                                            )
                                            .child(
                                                div()
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded_full()
                                                    .bg(rgb(surface()))
                                                    .text_xs()
                                                    .text_color(rgb(text_dim()))
                                                    .child(SharedString::from(format!(
                                                        "{} · {}",
                                                        mcp::transport(server),
                                                        mcp::source(server)
                                                    ))),
                                            )
                                            .when_some(mcp::auth(server), |this, auth| {
                                                this.child(
                                                    div()
                                                        .px_2()
                                                        .py_0p5()
                                                        .rounded_full()
                                                        .bg(rgb(surface()))
                                                        .text_xs()
                                                        .text_color(rgb(text_dim()))
                                                        .child(SharedString::from(format!(
                                                            "auth: {auth}"
                                                        ))),
                                                )
                                            }),
                                    )
                                    .child(div().text_xs().text_color(rgb(text_dim())).child(
                                        SharedString::from(crate::transcript::clip(
                                            &mcp::endpoint(server),
                                            120,
                                        )),
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(can_auth, |this| {
                                        this.child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "mcp-auth-{}",
                                                    server.name
                                                )))
                                                .px_3()
                                                .py_1()
                                                .rounded_md()
                                                .bg(rgb(surface()))
                                                .text_xs()
                                                .text_color(rgb(accent()))
                                                .cursor_pointer()
                                                .hover(|this| this.bg(rgb(surface_hi())))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.start_mcp_oauth(&auth_name, cx)
                                                }))
                                                .child("Authorize"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "mcp-test-{}",
                                                server.name
                                            )))
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .bg(rgb(surface()))
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .cursor_pointer()
                                            .hover(|this| {
                                                this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                            })
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    this.test_mcp(&test_name, cx)
                                                },
                                            ))
                                            .child(if is_testing { "Testing…" } else { "Test" }),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "mcp-toggle-{}",
                                                server.name
                                            )))
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .cursor_pointer()
                                            .when(is_enabled, |this| {
                                                this.bg(rgb(accent_soft()))
                                                    .text_color(rgb(accent()))
                                            })
                                            .when(!is_enabled, |this| {
                                                this.bg(rgb(surface()))
                                                    .text_color(rgb(text_muted()))
                                            })
                                            .hover(|this| {
                                                this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                            })
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    this.set_mcp_enabled(
                                                        &toggle_name,
                                                        !is_enabled,
                                                        cx,
                                                    )
                                                },
                                            ))
                                            .child(if is_enabled { "On" } else { "Off" }),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "mcp-edit-{}",
                                                server.name
                                            )))
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .bg(rgb(surface()))
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .cursor_pointer()
                                            .hover(|this| {
                                                this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                            })
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    this.edit_mcp(&edit_name, cx)
                                                },
                                            ))
                                            .child("Edit"),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "mcp-delete-{}",
                                                server.name
                                            )))
                                            .px_3()
                                            .py_1()
                                            .rounded_md()
                                            .bg(rgb(surface()))
                                            .text_xs()
                                            .text_color(rgb(if is_deleting {
                                                accent()
                                            } else {
                                                text_muted()
                                            }))
                                            .cursor_pointer()
                                            .hover(|this| {
                                                this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                            })
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    this.delete_mcp(&delete_name, cx)
                                                },
                                            ))
                                            .child(if is_deleting { "Confirm" } else { "Delete" }),
                                    ),
                            ),
                    )
                    .when_some(mcp::plugin(server), |this, plugin| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_dim()))
                                .child(SharedString::from(format!("plugin: {plugin}"))),
                        )
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.mcp_tabs(cx))
            .when(self.mcp.oauth.is_some(), |this| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child("MCP authorization in progress"),
                        )
                        .child(
                            div()
                                .id("mcp-oauth-cancel")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(surface()))
                                .text_xs()
                                .text_color(rgb(text()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_mcp_oauth(cx)))
                                .child("Cancel"),
                        ),
                )
            })
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .id("mcp-search")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .when(focus, |this| this.border_color(rgb(accent())))
                            .when(!focus, |this| this.border_color(rgb(border())))
                            .text_xs()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.mcp.focus = true;
                                cx.notify();
                            }))
                            .child(div().text_color(rgb(text_dim())).child("⌕"))
                            .when(self.mcp.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text_dim()))
                                        .child("Search MCP servers"),
                                )
                            })
                            .when(!self.mcp.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(self.mcp.query.clone())),
                                )
                            }),
                    ),
            )
            .when_some(self.mcp.status.clone(), |this, status| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .bg(rgb(accent_soft()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(status)),
                )
            })
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(format!("{enabled} of {total} enabled")))
                    .child(SharedString::from(format!(
                        "{shown} shown · toggles apply to new turns"
                    ))),
            )
            .into_any_element()
    }

    fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Skills);
        self.load_skills(cx);
    }

    fn load_skills(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Skills);
        self.skills.loaded = false;
        self.skills.error = None;
        self.skills.status = None;
        self.backend.send(Command::LoadSkills);
        cx.notify();
    }

    fn toggle_skills(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Skills {
            self.close_settings(cx);
        } else {
            self.open_skills(cx);
        }
    }

    fn toggle_skill(&mut self, name: &str, enabled: bool, cx: &mut Context<Self>) {
        self.skills.status = Some(format!(
            "{} {}…",
            name,
            if enabled { "enabling" } else { "disabling" }
        ));
        self.backend.send(Command::SetSkill {
            name: name.to_string(),
            enabled,
        });
        cx.notify();
    }

    fn route_skills_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.skills.tab == skills::Tab::Hub {
            if !self.skills.hub_focus {
                return false;
            }
            let keystroke = &event.keystroke;
            if keystroke.modifiers.control || keystroke.modifiers.platform {
                return false;
            }
            match keystroke.key.as_str() {
                "escape" => self.skills.hub_focus = false,
                "enter" => {
                    self.skills.hub_focus = false;
                    self.search_skill_hub(cx);
                    return true;
                }
                "backspace" => {
                    self.skills.hub_query.pop();
                }
                _ => {
                    if let Some(chars) = keystroke.key_char.as_deref() {
                        self.skills.hub_query.push_str(chars);
                    }
                }
            }
            cx.notify();
            return true;
        }

        if !self.skills.focus {
            return false;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" | "enter" => self.skills.focus = false,
            "backspace" => {
                self.skills.query.pop();
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.skills.query.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn skills_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let body = match self.skills.tab {
            skills::Tab::Installed => self.installed_skills_body(cx),
            skills::Tab::Hub => self.hub_body(cx),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.skills_tab_bar(cx))
            .child(body)
            .into_any_element()
    }

    fn skills_tab_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.skills.tab;
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .px_5()
            .py_2()
            .border_b_1()
            .border_color(rgb(border()));
        for (label, tab) in [
            ("Installed", skills::Tab::Installed),
            ("Browse hub", skills::Tab::Hub),
        ] {
            let selected = active == tab;
            row = row.child(
                div()
                    .id(SharedString::from(format!("skills-tab-{label}")))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                    })
                    .when(!selected, |this| this.text_color(rgb(text_muted())))
                    .hover(|this| this.text_color(rgb(text())))
                    .on_click(
                        cx.listener(move |this, _event, _window, cx| this.set_skills_tab(tab, cx)),
                    )
                    .child(label),
            );
        }
        row.into_any_element()
    }

    fn installed_skills_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = skills::filtered(&self.skills.rows, &self.skills.query);
        let shown = rows.len();
        let focus = self.skills.focus;
        let enabled = skills::enabled_count(&self.skills.rows);
        let total = self.skills.rows.len();

        let mut list = div()
            .id("skills-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if !self.skills.loaded {
            list = list.child(self.note("Loading skills…"));
        } else if let Some(error) = self.skills.error.as_deref() {
            list = list.child(self.note(error));
        } else if rows.is_empty() {
            list = list.child(self.note("No skills match this search."));
        }
        for skill in rows {
            let name = skill.name.clone();
            let toggle_name = skill.name.clone();
            let is_enabled = skill.enabled;
            list = list.child(
                div()
                    .id(SharedString::from(format!("skill-{}", skill.name)))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .when(is_enabled, |this| this.text_color(rgb(text())))
                                            .when(!is_enabled, |this| {
                                                this.text_color(rgb(text_muted()))
                                            })
                                            .child(SharedString::from(name)),
                                    )
                                    .when_some(skill.category.clone(), |this, category| {
                                        this.child(
                                            div()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_full()
                                                .bg(rgb(surface()))
                                                .text_xs()
                                                .text_color(rgb(text_dim()))
                                                .child(SharedString::from(category)),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("skill-toggle-{}", skill.name)))
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(is_enabled, |this| {
                                        this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                                    })
                                    .when(!is_enabled, |this| {
                                        this.bg(rgb(surface())).text_color(rgb(text_muted()))
                                    })
                                    .hover(|this| {
                                        this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                    })
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.toggle_skill(&toggle_name, !is_enabled, cx)
                                    }))
                                    .child(if is_enabled { "On" } else { "Off" }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(SharedString::from(crate::transcript::clip(
                                &skill.description,
                                180,
                            ))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child(SharedString::from(skills::meta(skill))),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .id("skills-search")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .when(focus, |this| this.border_color(rgb(accent())))
                            .when(!focus, |this| this.border_color(rgb(border())))
                            .text_xs()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.skills.focus = true;
                                cx.notify();
                            }))
                            .child(div().text_color(rgb(text_dim())).child("⌕"))
                            .when(self.skills.query.is_empty(), |this| {
                                this.child(div().text_color(rgb(text_dim())).child("Search skills"))
                            })
                            .when(!self.skills.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(self.skills.query.clone())),
                                )
                            }),
                    ),
            )
            .when_some(self.skills.status.clone(), |this, status| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .bg(rgb(accent_soft()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(status)),
                )
            })
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(format!("{enabled} of {total} enabled")))
                    .child(SharedString::from(format!(
                        "{shown} shown · changes apply to new turns"
                    ))),
            )
            .into_any_element()
    }

    fn set_skills_tab(&mut self, tab: skills::Tab, cx: &mut Context<Self>) {
        if self.skills.tab == tab {
            return;
        }
        self.skills.tab = tab;
        self.skills.focus = false;
        self.skills.hub_focus = false;
        self.skills.preview = None;
        self.skills.scan = None;
        self.skills.hub_error = None;
        cx.notify();
    }

    fn search_skill_hub(&mut self, cx: &mut Context<Self>) {
        let query = self.skills.hub_query.trim().to_string();
        if query.is_empty() {
            self.skills.hub_loaded = true;
            self.skills.hub_error = Some("Type a skill name or keyword first.".into());
            cx.notify();
            return;
        }
        self.skills.hub_started();
        self.backend.send(Command::SearchSkillHub { query });
        cx.notify();
    }

    fn preview_skill_hub(&mut self, identifier: &str, cx: &mut Context<Self>) {
        self.skills.hub_status = Some(format!("Loading {identifier}…"));
        self.skills.hub_error = None;
        self.backend.send(Command::PreviewSkillHub {
            identifier: identifier.to_string(),
        });
        cx.notify();
    }

    fn install_skill_hub(&mut self, identifier: &str, cx: &mut Context<Self>) {
        if !skills::can_install(self.skills.scan.as_ref()) {
            return;
        }
        self.skills.installing = Some(identifier.to_string());
        self.skills.hub_status = Some(format!("{identifier}: installing…"));
        self.backend.send(Command::InstallSkillHub {
            identifier: identifier.to_string(),
        });
        cx.notify();
    }

    fn hub_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(preview) = self.skills.preview.as_ref() {
            return self.hub_preview_body(preview, cx);
        }
        let focus = self.skills.hub_focus;
        let mut list = div()
            .id("hub-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if !self.skills.hub_loaded {
            list = list.child(self.note("Search the hub to preview a skill before installing."));
        } else if let Some(error) = self.skills.hub_error.as_deref() {
            list = list.child(self.note(error));
        } else if self.skills.hub_results.is_empty() {
            list = list.child(self.note("No hub skills match this search."));
        }
        for result in &self.skills.hub_results {
            let identifier = result.identifier.clone();
            let name = result.name.clone();
            let description = result.description.clone();
            let meta = skills::hub_meta(result);
            list =
                list.child(
                    div()
                        .id(SharedString::from(format!("hub-{identifier}")))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(surface_hi())))
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.preview_skill_hub(&identifier, cx)
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(name)),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(accent())).child("Preview →"),
                                ),
                        )
                        .child(div().text_xs().text_color(rgb(text_muted())).child(
                            SharedString::from(crate::transcript::clip(&description, 180)),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_dim()))
                                .child(SharedString::from(meta)),
                        ),
                );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("hub-search")
                                    .flex()
                                    .flex_1()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(bg()))
                                    .border_1()
                                    .when(focus, |this| this.border_color(rgb(accent())))
                                    .when(!focus, |this| this.border_color(rgb(border())))
                                    .text_xs()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.skills.hub_focus = true;
                                        cx.notify();
                                    }))
                                    .child(div().text_color(rgb(text_dim())).child("⌕"))
                                    .when(self.skills.hub_query.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_color(rgb(text_dim()))
                                                .child("Search the skills hub"),
                                        )
                                    })
                                    .when(!self.skills.hub_query.is_empty(), |this| {
                                        this.child(div().text_color(rgb(text())).child(
                                            SharedString::from(self.skills.hub_query.clone()),
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .id("hub-search-run")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(accent_soft()))
                                    .text_xs()
                                    .text_color(rgb(accent()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(surface_hi())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.search_skill_hub(cx)
                                    }))
                                    .child("Search"),
                            ),
                    ),
            )
            .when_some(self.skills.hub_status.clone(), |this, status| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .bg(rgb(accent_soft()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(status)),
                )
            })
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(format!(
                        "{} result(s)",
                        self.skills.hub_results.len()
                    )))
                    .child("Preview first · installs are confirmed here"),
            )
            .into_any_element()
    }

    fn hub_preview_body(
        &self,
        preview: &SkillHubPreview,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let identifier = preview.identifier.clone();
        let scan = self.skills.scan.as_ref();
        let policy = scan.map(|scan| scan.policy.as_str()).unwrap_or("unscanned");
        let allowed = skills::can_install(scan);
        let installing = self
            .skills
            .installing
            .as_deref()
            .is_some_and(|active| active == identifier);
        let policy_color = match policy {
            "allow" => user(),
            "ask" => warn(),
            _ => danger(),
        };
        let action = if installing {
            "Installing…"
        } else if !allowed {
            "Blocked by scan"
        } else if policy == "ask" {
            "Install anyway"
        } else {
            "Install"
        };
        let meta = match preview.repo.as_deref().filter(|repo| !repo.is_empty()) {
            Some(repo) => format!("{} · {} · {repo}", preview.trust_level, preview.source),
            None => format!("{} · {}", preview.trust_level, preview.source),
        };
        let scan_text = scan.map(skills::scan_line);
        let scan_reason = scan.and_then(|scan| scan.policy_reason.clone());
        let mut findings = div().flex().flex_col().gap_1();
        if let Some(scan) = scan {
            for finding in scan.findings.iter().take(12) {
                findings = findings.child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(SharedString::from(skills::finding_line(finding))),
                );
            }
        }
        let install_id = identifier.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .id("hub-preview-back")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.skills.preview = None;
                                        this.skills.scan = None;
                                        cx.notify();
                                    }))
                                    .child("← Back"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(preview.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(SharedString::from(meta)),
                            ),
                    )
                    .child(
                        div()
                            .id("hub-install")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .when(allowed && !installing, |this| {
                                this.bg(rgb(accent()))
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(accent_hi())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.install_skill_hub(&install_id, cx)
                                    }))
                            })
                            .when(!allowed || installing, |this| {
                                this.bg(rgb(surface())).text_color(rgb(text_dim()))
                            })
                            .child(action),
                    ),
            )
            .child(
                div()
                    .id("hub-preview")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(SharedString::from(preview.description.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child(SharedString::from(format!(
                                "{} · {} file(s) · {}",
                                preview.identifier,
                                preview.files.len(),
                                preview.tags.join(", ")
                            ))),
                    )
                    .when_some(scan_text, |this, text| {
                        this.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(surface()))
                                .text_xs()
                                .text_color(rgb(policy_color))
                                .child(SharedString::from(text)),
                        )
                    })
                    .child(findings)
                    .when_some(scan_reason, |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(SharedString::from(reason)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text()))
                            .child(SharedString::from(crate::transcript::clip(
                                &preview.skill_md,
                                12000,
                            ))),
                    ),
            )
            .into_any_element()
    }

    fn open_models(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Model);
        self.load_models(cx);
    }

    fn load_models(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Model);
        self.models.loaded = false;
        self.models.error = None;
        self.models.status = None;
        self.backend.send(Command::LoadModels);
        cx.notify();
    }

    fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        if self.moa.picking.is_some() {
            self.moa.apply_pick(provider, model);
            self.settings.show(Section::Moa);
            cx.notify();
            return;
        }
        if let Some(task) = self.models.aux_target.clone() {
            self.models.status = Some(format!("assigning {}…", models::task_label(&task)));
            self.backend.send(Command::SetAuxModel {
                task,
                provider: provider.to_string(),
                model: model.to_string(),
            });
        } else {
            self.models.status = Some(format!("switching to {model}…"));
            self.backend.send(Command::SetModel {
                provider: provider.to_string(),
                model: model.to_string(),
            });
        }
        cx.notify();
    }

    fn toggle_models(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Model {
            self.close_settings(cx);
        } else {
            self.open_models(cx);
        }
    }

    /// Typing into the model picker's search field.
    fn route_models_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if !self.models.focus {
            return false;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "escape" | "enter" => self.models.focus = false,
            "backspace" => {
                self.models.query.pop();
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.models.query.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn open_moa(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Moa);
        if self.moa.draft.is_none() {
            self.moa.loaded = false;
            self.moa.error = None;
            self.backend.send(Command::LoadMoa);
        }
        cx.notify();
    }

    fn reload_moa(&mut self, cx: &mut Context<Self>) {
        self.moa.loaded = false;
        self.moa.error = None;
        self.moa.draft = None;
        self.backend.send(Command::LoadMoa);
        cx.notify();
    }

    fn save_moa(&mut self, cx: &mut Context<Self>) {
        match self.moa.body() {
            Ok(config) => {
                self.moa.mark_saving();
                self.backend.send(Command::SaveMoa(config));
            }
            Err(message) => self.moa.error = Some(message),
        }
        cx.notify();
    }

    fn pick_moa_model(&mut self, pick: moa::Pick, cx: &mut Context<Self>) {
        self.moa.begin_pick(pick);
        self.settings.show(Section::Model);
        if self.models.options.is_none() {
            self.models.loaded = false;
            self.backend.send(Command::LoadModels);
        }
        cx.notify();
    }

    fn open_cron(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Automations);
        self.load_cron(cx);
    }

    fn load_cron(&mut self, cx: &mut Context<Self>) {
        self.automations.loaded = false;
        self.automations.error = None;
        self.backend.send(Command::LoadCron);
        cx.notify();
    }

    fn begin_cron(&mut self, cx: &mut Context<Self>) {
        self.automations.begin_create();
        cx.notify();
    }

    fn cancel_cron(&mut self, cx: &mut Context<Self>) {
        self.automations.cancel_create();
        cx.notify();
    }

    fn save_cron(&mut self, cx: &mut Context<Self>) {
        match self.automations.form.body() {
            Ok(body) => {
                self.automations.status = Some("creating job…".into());
                self.automations.form.error = None;
                self.backend.send(Command::CreateCron { body });
                self.automations.creating = false;
                self.automations.form = cron::Form::default();
            }
            Err(message) => self.automations.form.error = Some(message),
        }
        cx.notify();
    }

    fn cron_action(&mut self, id: &str, action: &str, cx: &mut Context<Self>) {
        self.automations.status = Some(format!("{id}: {action}…"));
        self.backend.send(Command::CronAction {
            id: id.to_string(),
            action: action.to_string(),
        });
        cx.notify();
    }

    fn delete_cron(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.automations.confirm_delete(id) {
            self.automations.status = Some(format!("deleting {id}…"));
            self.backend
                .send(Command::DeleteCron { id: id.to_string() });
        }
        cx.notify();
    }

    fn load_cron_runs(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.automations.runs_for.as_deref() == Some(id) {
            self.automations.runs_for = None;
            cx.notify();
            return;
        }
        self.automations.runs_loaded = false;
        self.automations.runs.clear();
        self.backend
            .send(Command::LoadCronRuns { id: id.to_string() });
        cx.notify();
    }

    fn route_cron_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if !self.automations.creating {
            return false;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        let field = self.automations.form.focus.unwrap_or(cron::Field::Schedule);
        match keystroke.key.as_str() {
            "escape" => self.automations.cancel_create(),
            "enter" => self.save_cron(cx),
            "tab" => self.automations.form.focus = Some(field.next()),
            "backspace" => {
                self.automations.form.text_mut(field).pop();
            }
            "space" => self.automations.form.text_mut(field).push(' '),
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.automations.form.text_mut(field).push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn open_accounts(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Accounts);
        self.load_accounts(cx);
    }

    fn load_accounts(&mut self, cx: &mut Context<Self>) {
        self.accounts.loaded = false;
        self.accounts.error = None;
        self.accounts.status = None;
        self.backend.send(Command::LoadOAuth);
        cx.notify();
    }

    fn start_oauth(&mut self, provider: &str, cx: &mut Context<Self>) {
        self.accounts.busy = true;
        self.accounts.status = Some(format!("starting {provider} login…"));
        self.backend.send(Command::StartOAuth {
            provider: provider.to_string(),
        });
        cx.notify();
    }

    fn open_oauth_url(&self, start: &hermes_api::OAuthStart) {
        let url = if !start.auth_url.is_empty() {
            start.auth_url.as_str()
        } else {
            start.verification_url.as_str()
        };
        self.open_external(url);
    }

    fn open_external(&self, url: &str) {
        if !url.is_empty() {
            let _ = std::process::Command::new("xdg-open").arg(url).spawn();
        }
    }

    fn submit_oauth(&mut self, cx: &mut Context<Self>) {
        let Some(oauth::Flow::Pkce {
            provider,
            session_id,
            code,
            ..
        }) = self.accounts.flow.clone()
        else {
            return;
        };
        if code.trim().is_empty() {
            self.accounts.status = Some("paste the authorization code first".into());
            cx.notify();
            return;
        }
        self.accounts.busy = true;
        self.accounts.status = Some("submitting code…".into());
        self.backend.send(Command::SubmitOAuth {
            provider,
            session_id,
            code: code.trim().to_string(),
        });
        cx.notify();
    }

    fn disconnect_oauth(&mut self, provider: &str, cx: &mut Context<Self>) {
        if self.accounts.confirm_disconnect(provider) {
            self.accounts.busy = true;
            self.accounts.status = Some(format!("disconnecting {provider}…"));
            self.backend.send(Command::DisconnectOAuth {
                provider: provider.to_string(),
            });
        }
        cx.notify();
    }

    fn route_oauth_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(code) = self.accounts.code_mut() else {
            return false;
        };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            if keystroke.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    code.push_str(&text);
                }
                cx.notify();
                return true;
            }
            return false;
        }
        match keystroke.key.as_str() {
            "escape" => self.accounts.close_flow(),
            "enter" => self.submit_oauth(cx),
            "backspace" => {
                code.pop();
            }
            "space" => code.push(' '),
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    code.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn pump_oauth(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update(cx, |app, cx| {
                        if !app.mcp.oauth_busy
                            && let Some(flow) = &app.mcp.oauth
                        {
                            app.mcp.oauth_busy = true;
                            app.backend.send(Command::PollMcpOAuth {
                                id: flow.flow_id.clone(),
                            });
                        }
                        if app.accounts.busy {
                            return;
                        }
                        let Some(oauth::Flow::Device {
                            provider,
                            session_id,
                            terminal: false,
                            ..
                        }) = app.accounts.flow.clone()
                        else {
                            return;
                        };
                        app.accounts.busy = true;
                        app.backend.send(Command::PollOAuth {
                            provider,
                            session_id,
                        });
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    fn load_assignments(&mut self, cx: &mut Context<Self>) {
        self.settings.show(Section::Assignments);
        self.models.aux_loaded = false;
        self.models.aux_error = None;
        self.models.status = None;
        self.backend.send(Command::LoadAuxiliary);
        cx.notify();
    }

    fn toggle_assignments(&mut self, cx: &mut Context<Self>) {
        if self.settings.open && self.settings.section == Section::Assignments {
            self.close_settings(cx);
        } else {
            self.load_assignments(cx);
        }
    }

    fn assign_to(&mut self, task: &str, cx: &mut Context<Self>) {
        self.models.aux_target = Some(task.to_string());
        self.settings.show(Section::Model);
        cx.notify();
    }

    fn reset_aux(&mut self, cx: &mut Context<Self>) {
        self.models.status = Some("resetting auxiliary tasks…".to_string());
        self.backend.send(Command::ResetAuxModels);
        cx.notify();
    }

    fn set_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        self.models.status = Some(format!(
            "reasoning: {}…",
            models::reasoning_label(level).to_lowercase()
        ));
        self.backend.send(Command::SetReasoning {
            level: level.to_string(),
        });
        cx.notify();
    }

    fn moa_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut body = div()
            .id("moa-body")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();

        if !self.moa.loaded && self.moa.draft.is_none() {
            return body
                .child(self.note("Loading MoA configuration…"))
                .into_any_element();
        }
        if let Some(error) = self.moa.error.as_deref() {
            body = body.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .text_xs()
                    .text_color(rgb(danger()))
                    .child(SharedString::from(error.to_string())),
            );
        }
        let Some(draft) = self.moa.draft.clone() else {
            return body
                .child(self.note("MoA configuration is unavailable."))
                .into_any_element();
        };
        let preset_name = self.moa.preset.clone();
        let Some(preset) = draft.presets.get(&preset_name).cloned() else {
            return body
                .child(self.note("This preset no longer exists; reload the page."))
                .into_any_element();
        };
        let is_default = draft.default_preset == preset_name;
        let is_active = draft.active_preset == preset_name;

        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("moa-preset")
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(bg_elev()))
                                .text_sm()
                                .text_color(rgb(text()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(surface_hi())))
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.moa.cycle_preset();
                                    cx.notify();
                                }))
                                .child(SharedString::from(format!("Preset: {preset_name}"))),
                        )
                        .when(is_default, |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_full()
                                    .bg(rgb(accent_soft()))
                                    .text_xs()
                                    .text_color(rgb(accent()))
                                    .child("default"),
                            )
                        })
                        .when(is_active, |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_full()
                                    .bg(rgb(accent_soft()))
                                    .text_xs()
                                    .text_color(rgb(accent()))
                                    .child("active"),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("moa-reload")
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(bg_elev())).text_color(rgb(text())))
                                .on_click(
                                    cx.listener(|this, _event, _window, cx| this.reload_moa(cx)),
                                )
                                .child("Reload"),
                        )
                        .child(
                            div()
                                .id("moa-save")
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(accent()))
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(bg()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(accent_hi())))
                                .on_click(
                                    cx.listener(|this, _event, _window, cx| this.save_moa(cx)),
                                )
                                .child(if self.moa.dirty {
                                    "Save MoA *"
                                } else {
                                    "Save MoA"
                                }),
                        ),
                ),
        );

        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(self.moa_chip(
                    "moa-enabled",
                    if preset.enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                    preset.enabled,
                    cx,
                    |this| this.moa.toggle_enabled(),
                ))
                .child(self.moa_chip(
                    "moa-default",
                    if is_default {
                        "Default preset"
                    } else {
                        "Make default"
                    },
                    is_default,
                    cx,
                    |this| this.moa.make_default(),
                ))
                .child(self.moa_chip(
                    "moa-active",
                    if is_active { "Deactivate" } else { "Activate" },
                    is_active,
                    cx,
                    |this| this.moa.toggle_active(),
                ))
                .child(self.moa_chip(
                    "moa-fanout",
                    &format!("Fan-out: {}", preset.fanout),
                    false,
                    cx,
                    |this| this.moa.cycle_fanout(),
                ))
                .child(self.moa_chip(
                    "moa-policy",
                    &format!("Degraded: {}", preset.degraded_reference_policy),
                    false,
                    cx,
                    |this| this.moa.cycle_policy(),
                )),
        );

        let references = preset.reference_models.clone();
        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(self.section_label("REFERENCE MODELS"))
                .child(
                    div()
                        .id("moa-add-reference")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .text_xs()
                        .text_color(rgb(text()))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(surface_hi())))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.pick_moa_model(moa::Pick::NewReference, cx)
                        }))
                        .child("＋ Add reference"),
                ),
        );
        for (index, slot) in references.iter().enumerate() {
            body = body.child(
                div()
                    .id(SharedString::from(format!("moa-reference-{index}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(moa::slot_label(slot))),
                            )
                            .when(!slot.enabled, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_dim()))
                                        .child("disabled"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("moa-reference-pick-{index}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.pick_moa_model(moa::Pick::Reference(index), cx)
                                    }))
                                    .child("change"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("moa-reference-remove-{index}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(danger())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.moa.remove_reference(index);
                                        cx.notify();
                                    }))
                                    .child("remove"),
                            ),
                    ),
            );
        }

        body = body
            .child(self.section_label("AGGREGATOR"))
            .child(
                div()
                    .id("moa-aggregator")
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.pick_moa_model(moa::Pick::Aggregator, cx)
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(moa::slot_label(&preset.aggregator))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child("change"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!(
                                "Aggregator temperature: {}",
                                preset
                                    .aggregator_temperature
                                    .map(|value| format!("{value:.1}"))
                                    .unwrap_or_else(|| "provider default".into())
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.moa_mini("moa-temp-minus", "−", cx, |this| {
                                this.moa.nudge_temperature(true, -0.1)
                            }))
                            .child(self.moa_mini("moa-temp-plus", "＋", cx, |this| {
                                this.moa.nudge_temperature(true, 0.1)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!(
                                "Reference temperature: {}",
                                preset
                                    .reference_temperature
                                    .map(|value| format!("{value:.1}"))
                                    .unwrap_or_else(|| "provider default".into())
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.moa_mini("moa-ref-temp-minus", "−", cx, |this| {
                                this.moa.nudge_temperature(false, -0.1)
                            }))
                            .child(self.moa_mini("moa-ref-temp-plus", "＋", cx, |this| {
                                this.moa.nudge_temperature(false, 0.1)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(format!(
                                "Answer max tokens: {}",
                                preset.max_tokens
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.moa_mini("moa-tokens-minus", "−", cx, |this| {
                                this.moa.nudge_max_tokens(-512)
                            }))
                            .child(self.moa_mini("moa-tokens-plus", "＋", cx, |this| {
                                this.moa.nudge_max_tokens(512)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text()))
                            .child(SharedString::from(match preset.reference_max_tokens {
                                Some(tokens) => format!("Reference max tokens: {tokens}"),
                                None => "Reference max tokens: uncapped".to_string(),
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.moa_mini("moa-ref-tokens-minus", "−", cx, |this| {
                                this.moa.nudge_reference_max_tokens(-128)
                            }))
                            .child(self.moa_mini("moa-ref-tokens-plus", "＋", cx, |this| {
                                this.moa.nudge_reference_max_tokens(128)
                            }))
                            .child(self.moa_mini("moa-ref-tokens-clear", "clear", cx, |this| {
                                this.moa.clear_reference_max_tokens()
                            })),
                    ),
            );

        if let Some(status) = self.moa.status.as_deref() {
            body = body.child(self.note(status));
        }
        body.child(
            div()
                .text_xs()
                .text_color(rgb(text_dim()))
                .child("Presets are saved together. Model changes apply to new turns; disabled presets stay available in the model picker."),
        )
        .into_any_element()
    }

    fn moa_chip(
        &self,
        id: &'static str,
        label: &str,
        selected: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(selected, |this| {
                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
            })
            .when(!selected, |this| {
                this.bg(rgb(bg_elev()))
                    .text_color(rgb(text_muted()))
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
            })
            .on_click(cx.listener(move |this, _event, _window, cx| {
                action(this);
                cx.notify();
            }))
            .child(SharedString::from(label.to_string()))
    }

    fn moa_mini(
        &self,
        id: &'static str,
        label: &str,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(surface()))
            .text_xs()
            .text_color(rgb(text_muted()))
            .cursor_pointer()
            .hover(|this| this.text_color(rgb(text())))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                action(this);
                cx.notify();
            }))
            .child(SharedString::from(label.to_string()))
    }

    fn automation_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut body = div()
            .id("automation-body")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();

        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_xs().text_color(rgb(text_dim())).child(
                    if self.automations.loaded {
                        format!(
                            "{} jobs · runs are ordinary sessions",
                            self.automations.jobs.len()
                        )
                    } else {
                        "Loading jobs…".to_string()
                    },
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("cron-reload")
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(bg_elev())).text_color(rgb(text())))
                                .on_click(
                                    cx.listener(|this, _event, _window, cx| this.load_cron(cx)),
                                )
                                .child("Reload"),
                        )
                        .child(
                            div()
                                .id("cron-new")
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(accent()))
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(bg()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(accent_hi())))
                                .on_click(
                                    cx.listener(|this, _event, _window, cx| this.begin_cron(cx)),
                                )
                                .child("＋ New job"),
                        ),
                ),
        );

        if self.automations.creating {
            let form = &self.automations.form;
            body =
                body.child(self.section_label("NEW JOB"))
                    .child(self.cron_field(
                        "Name (optional)",
                        &form.name,
                        "e.g. Morning brief",
                        cron::Field::Name,
                        cx,
                    ))
                    .child(self.cron_field(
                        "Schedule",
                        &form.schedule,
                        "e.g. every day at 9am",
                        cron::Field::Schedule,
                        cx,
                    ))
                    .child(self.cron_field(
                        "Prompt",
                        &form.prompt,
                        "What should the agent do?",
                        cron::Field::Prompt,
                        cx,
                    ))
                    .child(
                        div()
                            .id("cron-field-deliver")
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg_elev()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.automations.form.focus = Some(cron::Field::Deliver);
                                this.automations.cycle_delivery();
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child("Deliver to"),
                            )
                            .child(div().text_sm().text_color(rgb(text())).child(
                                SharedString::from(self.automations.delivery_label(&form.deliver)),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child("Tab moves · Enter creates · Esc cancels"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("cron-cancel")
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .cursor_pointer()
                                            .hover(|this| this.text_color(rgb(text())))
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.cancel_cron(cx)
                                            }))
                                            .child("Cancel"),
                                    )
                                    .child(
                                        div()
                                            .id("cron-create")
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(rgb(accent()))
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(rgb(bg()))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(rgb(accent_hi())))
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.save_cron(cx)
                                            }))
                                            .child("Create job"),
                                    ),
                            ),
                    );
            if let Some(error) = form.error.as_deref() {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(rgb(danger()))
                        .child(SharedString::from(error.to_string())),
                );
            }
        }

        if let Some(error) = self.automations.error.as_deref() {
            body = body.child(self.note(error));
        }
        if self.automations.loaded && self.automations.jobs.is_empty() {
            body = body.child(self.note("No cron jobs yet. Create one above."));
        }

        for job in self.automations.jobs.clone() {
            let id = job.id.clone();
            let id_pause = id.clone();
            let id_trigger = id.clone();
            let id_runs = id.clone();
            let id_delete = id.clone();
            let paused = !job.enabled || job.state == "paused";
            let deleting = self.automations.confirm_delete.as_deref() == Some(id.as_str());
            let show_runs = self.automations.runs_for.as_deref() == Some(id.as_str());
            let name = if job.name.is_empty() {
                id.clone()
            } else {
                job.name.clone()
            };
            body = body.child(
                div()
                    .id(SharedString::from(format!("cron-job-{id}")))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(text()))
                                            .child(SharedString::from(name)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .child(SharedString::from(cron::job_meta(&job))),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(self.cron_action_chip(
                                        "cron-pause",
                                        if paused { "Resume" } else { "Pause" },
                                        cx,
                                        move |this, cx| {
                                            this.cron_action(
                                                &id_pause,
                                                if paused { "resume" } else { "pause" },
                                                cx,
                                            )
                                        },
                                    ))
                                    .child(self.cron_action_chip(
                                        "cron-trigger",
                                        "Run now",
                                        cx,
                                        move |this, cx| {
                                            this.cron_action(&id_trigger, "trigger", cx)
                                        },
                                    ))
                                    .child(self.cron_action_chip(
                                        "cron-runs",
                                        "Runs",
                                        cx,
                                        move |this, cx| this.load_cron_runs(&id_runs, cx),
                                    ))
                                    .child(self.cron_action_chip(
                                        "cron-delete",
                                        if deleting { "Delete?" } else { "Delete" },
                                        cx,
                                        move |this, cx| this.delete_cron(&id_delete, cx),
                                    )),
                            ),
                    )
                    .when(!job.prompt.is_empty(), |this| {
                        this.child(div().text_xs().text_color(rgb(text_dim())).child(
                            SharedString::from(crate::transcript::clip(&job.prompt, 240)),
                        ))
                    })
                    .when(!job.deliver.is_empty(), |this| {
                        this.child(div().text_xs().text_color(rgb(text_dim())).child(
                            SharedString::from(format!(
                                "delivers to {}",
                                self.automations.delivery_label(&job.deliver)
                            )),
                        ))
                    })
                    .when(show_runs, |this| {
                        let mut runs = div().flex().flex_col().gap_1().pt_1();
                        if !self.automations.runs_loaded {
                            runs = runs.child(self.note("Loading runs…"));
                        } else if self.automations.runs.is_empty() {
                            runs = runs.child(self.note("No runs recorded for this job."));
                        } else {
                            for run in &self.automations.runs {
                                let run_id = run.id.clone();
                                runs = runs.child(
                                    div()
                                        .id(SharedString::from(format!("cron-run-{run_id}")))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(bg()))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(surface_hi())))
                                        .on_click(cx.listener(move |this, _event, _window, cx| {
                                            this.close_settings(cx);
                                            this.open_session(&run_id, cx);
                                        }))
                                        .child(div().text_xs().text_color(rgb(text())).child(
                                            SharedString::from(if run.title.is_empty() {
                                                run.id.clone()
                                            } else {
                                                run.title.clone()
                                            }),
                                        ))
                                        .child(
                                            div().text_xs().text_color(rgb(text_muted())).child(
                                                SharedString::from(sessions::meta(
                                                    run,
                                                    sessions::now_secs(),
                                                )),
                                            ),
                                        ),
                                );
                            }
                        }
                        this.child(runs)
                    }),
            );
        }

        if let Some(status) = self.automations.status.as_deref() {
            body = body.child(self.note(status));
        }
        body.child(
            div()
                .text_xs()
                .text_color(rgb(text_dim()))
                .child("Schedules are parsed by the host. Trigger runs immediately; pause keeps the job and its history."),
        )
        .into_any_element()
    }

    fn cron_field(
        &self,
        label: &str,
        value: &str,
        placeholder: &str,
        field: cron::Field,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focused = self.automations.form.focus == Some(field);
        div()
            .id(SharedString::from(format!("cron-field-{label}")))
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(bg_elev()))
            .border_1()
            .when(focused, |this| this.border_color(rgb(accent())))
            .when(!focused, |this| this.border_color(rgb(border())))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.automations.form.focus = Some(field);
                cx.notify();
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if value.is_empty() {
                        rgb(text_dim())
                    } else {
                        rgb(text())
                    })
                    .child(SharedString::from(if value.is_empty() {
                        placeholder.to_string()
                    } else {
                        value.to_string()
                    })),
            )
    }

    fn cron_action_chip(
        &self,
        id: &'static str,
        label: &str,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .text_color(rgb(text_muted()))
            .cursor_pointer()
            .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
            .on_click(cx.listener(move |this, _event, _window, cx| action(this, cx)))
            .child(SharedString::from(label.to_string()))
    }

    fn accounts_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut body = div()
            .id("accounts-body")
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();

        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child("OAuth providers managed by Hermes. External CLI credentials are read-only here."),
                )
                .child(
                    div()
                        .id("oauth-reload")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(bg_elev())).text_color(rgb(text())))
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.load_accounts(cx)
                        }))
                        .child("Reload"),
                ),
        );

        if let Some(flow) = self.accounts.flow.clone() {
            match flow {
                oauth::Flow::Pkce {
                    provider,
                    auth_url,
                    code,
                    ..
                } => {
                    body = body.child(self.section_label("SIGN-IN")).child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(bg_elev()))
                            .child(div().text_sm().text_color(rgb(text())).child(
                                SharedString::from(format!("Finish {provider} in your browser")),
                            ))
                            .child(
                                div()
                                    .id("oauth-open")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(accent()))
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(bg()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(accent_hi())))
                                    .on_click(cx.listener(move |this, _event, _window, _cx| {
                                        this.open_external(&auth_url)
                                    }))
                                    .child("Open sign-in page"),
                            )
                            .child(
                                div()
                                    .id("oauth-code")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(bg()))
                                    .border_1()
                                    .border_color(rgb(accent()))
                                    .text_sm()
                                    .text_color(if code.is_empty() {
                                        rgb(text_dim())
                                    } else {
                                        rgb(text())
                                    })
                                    .child(SharedString::from(if code.is_empty() {
                                        "Paste the authorization code".to_string()
                                    } else {
                                        code.clone()
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_dim()))
                                            .child("Enter submits · Esc cancels"),
                                    )
                                    .child(
                                        div()
                                            .id("oauth-submit")
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(rgb(accent()))
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(rgb(bg()))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(rgb(accent_hi())))
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.submit_oauth(cx)
                                            }))
                                            .child("Submit code"),
                                    ),
                            ),
                    );
                }
                oauth::Flow::Device {
                    provider,
                    user_code,
                    verification_url,
                    status,
                    terminal,
                    ..
                } => {
                    body =
                        body.child(self.section_label("SIGN-IN")).child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .p_3()
                                .rounded_md()
                                .bg(rgb(bg_elev()))
                                .child(div().text_sm().text_color(rgb(text())).child(
                                    SharedString::from(format!("{provider}: enter this code")),
                                ))
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgb(accent()))
                                        .child(SharedString::from(user_code)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .child(SharedString::from(status.clone())),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("oauth-open")
                                                .px_3()
                                                .py_2()
                                                .rounded_md()
                                                .bg(rgb(accent()))
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .text_color(rgb(bg()))
                                                .cursor_pointer()
                                                .hover(|this| this.bg(rgb(accent_hi())))
                                                .on_click(cx.listener(
                                                    move |this, _event, _window, _cx| {
                                                        this.open_external(&verification_url)
                                                    },
                                                ))
                                                .child("Open verification page"),
                                        )
                                        .child(
                                            div()
                                                .id("oauth-close")
                                                .px_3()
                                                .py_2()
                                                .rounded_md()
                                                .text_xs()
                                                .text_color(rgb(text_muted()))
                                                .cursor_pointer()
                                                .hover(|this| this.text_color(rgb(text())))
                                                .on_click(cx.listener(
                                                    |this, _event, _window, cx| {
                                                        this.accounts.close_flow();
                                                        cx.notify();
                                                    },
                                                ))
                                                .child(if terminal { "Done" } else { "Cancel" }),
                                        ),
                                ),
                        );
                }
            }
        }

        if !self.accounts.loaded {
            body = body.child(self.note("Loading provider accounts…"));
        }
        if let Some(error) = self.accounts.error.as_deref() {
            body = body.child(self.note(error));
        }

        for provider in self.accounts.providers.clone() {
            let id = provider.id.clone();
            let id_connect = id.clone();
            let id_disconnect = id.clone();
            let connected = provider.status.logged_in;
            let can_disconnect = provider.disconnectable && connected;
            let confirming = self.accounts.confirm_disconnect.as_deref() == Some(id.as_str());
            body = body.child(
                div()
                    .id(SharedString::from(format!("oauth-provider-{id}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(text()))
                                            .child(SharedString::from(provider.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(rgb(surface()))
                                            .text_xs()
                                            .text_color(rgb(text_dim()))
                                            .child(SharedString::from(provider.flow.clone())),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if connected {
                                        rgb(accent())
                                    } else {
                                        rgb(text_muted())
                                    })
                                    .child(SharedString::from(oauth::status_line(&provider))),
                            )
                            .when(!provider.cli_command.is_empty(), |this| {
                                this.child(div().text_xs().text_color(rgb(text_dim())).child(
                                    SharedString::from(format!(
                                        "CLI fallback: {}",
                                        provider.cli_command
                                    )),
                                ))
                            })
                            .when_some(provider.disconnect_hint.clone(), |this, hint| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_dim()))
                                        .child(SharedString::from(hint)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(provider.flow != "external" && !connected, |this| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "oauth-connect-{id_connect}"
                                        )))
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(accent()))
                                        .text_xs()
                                        .text_color(rgb(bg()))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(accent_hi())))
                                        .on_click(cx.listener(move |this, _event, _window, cx| {
                                            this.start_oauth(&id_connect, cx)
                                        }))
                                        .child("Connect"),
                                )
                            })
                            .when(can_disconnect, |this| {
                                this.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "oauth-disconnect-{id_disconnect}"
                                        )))
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .text_xs()
                                        .text_color(if confirming {
                                            rgb(danger())
                                        } else {
                                            rgb(text_muted())
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.text_color(rgb(danger())))
                                        .on_click(cx.listener(move |this, _event, _window, cx| {
                                            this.disconnect_oauth(&id_disconnect, cx)
                                        }))
                                        .child(if confirming {
                                            "Disconnect?"
                                        } else {
                                            "Disconnect"
                                        }),
                                )
                            }),
                    ),
            );
        }

        if let Some(status) = self.accounts.status.as_deref() {
            body = body.child(self.note(status));
        }
        body.child(
            div()
                .text_xs()
                .text_color(rgb(text_dim()))
                .child("Codes are exchanged by the host and never stored by the desktop. External provider credentials stay owned by their CLI."),
        )
        .into_any_element()
    }

    fn assignments_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("assignments-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if !self.models.aux_loaded {
            list = list.child(self.note("Loading assignments…"));
        } else if let Some(error) = self.models.aux_error.as_deref() {
            list = list.child(self.note(error));
        } else if self.models.auxiliary.is_empty() {
            list = list.child(self.note("No auxiliary tasks reported."));
        } else {
            for task in &self.models.auxiliary {
                let key = task.task.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("aux-{}", task.task)))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(surface_hi())))
                        .on_click(
                            cx.listener(move |this, _event, _window, cx| this.assign_to(&key, cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(models::task_label(&task.task))),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_dim()))
                                        .child(SharedString::from(models::aux_line(task))),
                                ),
                        )
                        .child(div().text_xs().text_color(rgb(accent())).child("Change")),
                );
            }
        }

        let mut chips = div().flex().flex_wrap().gap_2();
        for level in models::REASONING_LEVELS {
            let level = level.to_string();
            let selected = self.models.reasoning.trim() == level;
            let label = models::reasoning_label(&level);
            let id = format!(
                "reasoning-{}",
                if level.is_empty() { "default" } else { &level }
            );
            chips =
                chips.child(
                    div()
                        .id(SharedString::from(id))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .when(selected, |this| {
                            this.bg(rgb(accent())).text_color(rgb(bg()))
                        })
                        .when(!selected, |this| {
                            this.bg(rgb(surface_hi())).text_color(rgb(text()))
                        })
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(border_hi())))
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.set_reasoning(&level, cx)
                        }))
                        .child(SharedString::from(label)),
                );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text_dim()))
                            .child("AUXILIARY TASKS"),
                    )
                    .child(
                        div()
                            .id("aux-reset")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface_hi()))
                            .text_xs()
                            .text_color(rgb(text()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(border_hi())))
                            .on_click(cx.listener(|this, _event, _window, cx| this.reset_aux(cx)))
                            .child("Reset all to main"),
                    ),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text_dim()))
                            .child("REASONING EFFORT"),
                    )
                    .child(chips)
                    .child(
                        div().text_xs().text_color(rgb(text_muted())).child(
                            "Default inherits the server's level (medium). New sessions only.",
                        ),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(SharedString::from(
                                self.models
                                    .status
                                    .clone()
                                    .or_else(|| self.models.aux_error.clone())
                                    .unwrap_or_else(|| {
                                        "Pinned tasks keep their own model until reset.".to_string()
                                    }),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn models_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = models::rows(self.models.options.as_ref(), &self.models.query);
        let focus = self.models.focus;
        let current = models::current_line(self.models.options.as_ref());

        let mut list = div()
            .id("models-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if rows.is_empty() {
            list = list.child(self.note(if !self.models.loaded {
                "Loading models…"
            } else if self.models.error.is_some() {
                "The provider catalog is unavailable."
            } else {
                "No models match this search."
            }));
        }
        let mut last_provider: Option<String> = None;
        for row in rows {
            if last_provider.as_deref() != Some(row.provider.as_str()) {
                last_provider = Some(row.provider.clone());
                list = list.child(
                    div()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(row.provider_name.clone())),
                );
            }
            let provider = row.provider.clone();
            let model = row.model.clone();
            let is_current = row.current;
            let label = row.model.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!(
                        "model-{}-{}",
                        row.provider, row.model
                    )))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())))
                    .when(is_current, |this| this.bg(rgb(accent_soft())))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.select_model(&provider, &model, cx)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .when(is_current, |this| this.text_color(rgb(accent())))
                            .when(!is_current, |this| this.text_color(rgb(text())))
                            .child(SharedString::from(label)),
                    )
                    .when(is_current, |this| {
                        this.child(div().text_xs().text_color(rgb(accent())).child("Current"))
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .id("models-search")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .when(focus, |this| this.border_color(rgb(accent())))
                            .when(!focus, |this| this.border_color(rgb(border())))
                            .text_xs()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.models.focus = true;
                                cx.notify();
                            }))
                            .child(div().text_color(rgb(text_dim())).child("⌕"))
                            .when(self.models.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text_dim()))
                                        .child("Search providers and models"),
                                )
                            })
                            .when(!self.models.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(self.models.query.clone())),
                                )
                            }),
                    ),
            )
            .when_some(self.models.aux_target.clone(), |this, task| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .px_5()
                        .py_2()
                        .bg(rgb(accent_soft()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .child(SharedString::from(format!(
                                    "Assigning {} — pick a model",
                                    models::task_label(&task)
                                ))),
                        )
                        .child(
                            div()
                                .id("aux-cancel")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(surface()))
                                .text_xs()
                                .text_color(rgb(text()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.models.aux_target = None;
                                    this.settings.show(Section::Assignments);
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_5()
                    .py_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child(SharedString::from(format!("Current: {current}"))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("New sessions only"),
                    ),
            )
            .child(list)
            .child(
                div()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(SharedString::from(
                            self.models
                                .status
                                .clone()
                                .or_else(|| self.models.error.clone())
                                .unwrap_or_else(|| {
                                    "Switching writes config.yaml; the open chat keeps its model."
                                        .to_string()
                                }),
                        )),
                    ),
            )
            .into_any_element()
    }

    fn vault_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tabs = vault::tabs(&self.inspector.env);
        let rows = vault::visible(
            &self.inspector.env,
            self.vault.tab.as_deref(),
            &self.vault.query,
        );
        let loaded = self.inspector.env_loaded;
        let focus = self.vault.focus;
        let selected_tab = self.vault.tab.clone();

        let mut list = div()
            .id("vault-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_2();
        if rows.is_empty() {
            list = list.child(self.note(if loaded {
                "No credentials match this view."
            } else {
                "Loading vault…"
            }));
        }
        for (name, entry) in rows {
            let subtitle = if entry.description.is_empty() {
                entry
                    .redacted_value
                    .clone()
                    .unwrap_or_else(|| "not set".into())
            } else {
                clip(&entry.description, 72)
            };
            let is_set = entry.is_set;
            let redacted = entry.redacted_value.clone().unwrap_or_default();
            let tint = vault::tint(name);
            let monogram = vault::monogram(name);
            let row_key = name.clone();
            let delete_key = name.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("vault-row-{name}")))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.vault.start_adding(Some(&row_key));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(30.))
                            .h(px(30.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(tint))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(bg()))
                                    .child(SharedString::from(monogram)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(SharedString::from(subtitle)),
                            ),
                    )
                    .when(is_set, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(SharedString::from(redacted)),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("vault-delete-{name}")))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(danger_soft())))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.delete_vault_item(&delete_key, cx)
                                }))
                                .child("Remove"),
                        )
                    })
                    .when(!is_set, |this| {
                        this.child(div().text_xs().text_color(rgb(accent())).child("Set"))
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .children(tabs.into_iter().map(|tab| {
                        let selected = selected_tab == tab.category;
                        let category = tab.category.clone();
                        div()
                            .id(SharedString::from(format!("vault-tab-{}", tab.label)))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!selected, |this| {
                                this.text_color(rgb(text_muted()))
                                    .hover(|this| this.bg(rgb(bg_elev())).text_color(rgb(text())))
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.vault.tab = category.clone();
                                cx.notify();
                            }))
                            .child(SharedString::from(format!("{} {}", tab.label, tab.count)))
                    })),
            )
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .id("vault-search")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .when(focus == Some(vault::Field::Search), |this| {
                                this.border_color(rgb(accent()))
                            })
                            .when(focus != Some(vault::Field::Search), |this| {
                                this.border_color(rgb(border()))
                            })
                            .text_xs()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.vault.focus = Some(vault::Field::Search);
                                cx.notify();
                            }))
                            .child(div().text_color(rgb(text_dim())).child("⌕"))
                            .when(self.vault.query.is_empty(), |this| {
                                this.child(div().text_color(rgb(text_dim())).child("Search vault"))
                            })
                            .when(!self.vault.query.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_color(rgb(text()))
                                        .child(SharedString::from(self.vault.query.clone())),
                                )
                            }),
                    ),
            )
            .child(list)
            .when(self.vault.adding, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .px_5()
                        .py_3()
                        .border_t_1()
                        .border_color(rgb(border()))
                        .child(self.vault_field(
                            "KEY",
                            &self.vault.key,
                            "OPENAI_API_KEY",
                            vault::Field::Key,
                            focus == Some(vault::Field::Key),
                            cx,
                        ))
                        .child(self.vault_field(
                            "VALUE",
                            &self.vault.value,
                            "sk-…",
                            vault::Field::Value,
                            focus == Some(vault::Field::Value),
                            cx,
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("vault-cancel")
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(bg_elev())))
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.vault.cancel_adding();
                                            cx.notify();
                                        }))
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .id("vault-save")
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(accent()))
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(rgb(bg()))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(rgb(accent_hi())))
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.save_vault_item(cx)
                                        }))
                                        .child("Save"),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .child(
                        div().text_xs().text_color(rgb(text_dim())).child(
                            "Hermes can use these values, but this client never reveals them.",
                        ),
                    )
                    .when_some(self.vault.status.clone(), |this, status| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(SharedString::from(status)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn usage_row(&self, label: &str, value: String) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text()))
                    .child(SharedString::from(value)),
            )
    }

    fn stat_card(&self, label: &str, value: String) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(rgb(bg_elev()))
            .min_w(px(120.))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(text()))
                    .child(SharedString::from(value)),
            )
    }

    fn usage_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.usage;
        let totals = state
            .analytics
            .as_ref()
            .map(|analytics| analytics.totals.clone())
            .unwrap_or_default();
        let plan = usage::plan_line(state.portal.as_ref());
        let provider = state
            .model
            .as_ref()
            .map(|info| info.provider.clone())
            .unwrap_or_else(|| "—".to_string());
        let model_name = state
            .model
            .as_ref()
            .map(|info| info.model.clone())
            .unwrap_or_else(|| "—".to_string());
        let context = state
            .model
            .as_ref()
            .map(|info| usage::context_label(info.effective_context_length))
            .unwrap_or_else(|| "—".to_string());
        let days = state.days;
        let subscription = state
            .portal
            .as_ref()
            .map(|portal| portal.subscription_url.replace("https://", ""))
            .filter(|url| !url.is_empty());

        let mut body = div()
            .id("usage-body")
            .flex()
            .flex_col()
            .gap_4()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4();

        body = body.child(self.section_label("MY PLAN")).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(rgb(bg_elev()))
                .child(self.usage_row("Plan", plan))
                .child(self.usage_row("Provider", provider))
                .child(self.usage_row("Model", model_name))
                .child(self.usage_row("Context", context))
                .when_some(subscription, |this, url| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child(SharedString::from(format!("Billing is managed at {url}"))),
                    )
                }),
        );

        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(self.section_label("USAGE LIMITS"))
                .child(div().flex().items_center().gap_1().children(
                    [7u32, 30, 90].into_iter().map(|candidate| {
                        let selected = candidate == days;
                        div()
                            .id(SharedString::from(format!("usage-{candidate}")))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!selected, |this| {
                                this.text_color(rgb(text_muted()))
                                    .hover(|this| this.bg(rgb(bg_elev())))
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.load_usage(candidate, cx)
                            }))
                            .child(SharedString::from(format!("{candidate}d")))
                    }),
                )),
        );

        if let Some(error) = &state.error {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(warn_soft()))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(warn()))
                            .child("Usage history unavailable"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(SharedString::from(error.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("Repair the host's state.db, then reopen this page."),
                    ),
            );
        }

        body = body.child(
            div()
                .flex()
                .gap_2()
                .flex_wrap()
                .child(self.stat_card("Estimated cost", usage::money(totals.total_estimated_cost)))
                .child(self.stat_card("Sessions", totals.total_sessions.to_string()))
                .child(
                    self.stat_card(
                        "API calls",
                        totals
                            .total_api_calls
                            .map(|calls| calls.to_string())
                            .unwrap_or_else(|| "—".to_string()),
                    ),
                )
                .child(self.stat_card("Input tokens", usage::tokens(totals.total_input)))
                .child(self.stat_card("Output tokens", usage::tokens(totals.total_output)))
                .child(self.stat_card("Cache read", usage::tokens(totals.total_cache_read))),
        );

        body = body.child(self.section_label("BY MODEL"));
        let models = usage::top_models(state.analytics.as_ref(), 8);
        if models.is_empty() {
            body = body.child(self.note(if !state.loaded {
                "Loading usage…"
            } else if state.error.is_some() {
                "Usage history unavailable."
            } else {
                "No model usage recorded in this window."
            }));
        }
        for model in models {
            let title = if model.model.is_empty() {
                "unknown model".to_string()
            } else {
                model.model.clone()
            };
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg_elev()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(title)),
                            )
                            .child(div().text_xs().text_color(rgb(text_dim())).child(
                                SharedString::from(format!(
                                    "{} calls · {} sessions",
                                    model.api_calls, model.sessions
                                )),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().text_xs().text_color(rgb(text_muted())).child(
                                SharedString::from(format!(
                                    "{} in · {} out",
                                    usage::tokens(Some(model.input_tokens)),
                                    usage::tokens(Some(model.output_tokens))
                                )),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .child(SharedString::from(usage::money(model.estimated_cost))),
                            ),
                    ),
            );
        }

        body.child(
            div()
                .text_xs()
                .text_color(rgb(text_dim()))
                .child("Costs are the host's estimates from recorded token usage."),
        )
        .into_any_element()
    }

    fn settings_sheet(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let section = self.settings.section;
        let body = match section {
            Section::Vault => self.vault_body(cx),
            Section::Model => self.models_body(cx),
            Section::Assignments => self.assignments_body(cx),
            Section::Moa => self.moa_body(cx),
            Section::Automations => self.automation_body(cx),
            Section::Kanban => self.kanban_body(cx),
            Section::Accounts => self.accounts_body(cx),
            Section::Skills => self.skills_body(cx),
            Section::Mcp => self.mcp_body(cx),
            Section::Memory => self.memory_body(cx),
            Section::Usage => self.usage_body(cx),
        };

        div()
            .w(px(SHEET_W))
            .h(px(SHEET_H))
            .flex()
            .rounded_xl()
            .bg(rgb(surface()))
            .border_1()
            .border_color(rgb(border_hi()))
            .overflow_hidden()
            .child(
                div()
                    .id("settings-navigation")
                    .w(px(190.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .overflow_y_scroll()
                    .bg(rgb(bg_elev()))
                    .border_r_1()
                    .border_color(rgb(border()))
                    .child(self.section_label("SETTINGS"))
                    .children(Section::all().into_iter().map(|candidate| {
                        let selected = candidate == section;
                        div()
                            .id(SharedString::from(format!(
                                "settings-{}",
                                candidate.label()
                            )))
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .text_sm()
                            .cursor_pointer()
                            .when(selected, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!selected, |this| {
                                this.text_color(rgb(text_muted()))
                                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                            })
                            .on_click(cx.listener(
                                move |this, _event, _window, cx| match candidate {
                                    Section::Usage => this.open_usage(cx),
                                    Section::Model => this.open_models(cx),
                                    Section::Assignments => this.load_assignments(cx),
                                    Section::Moa => this.open_moa(cx),
                                    Section::Automations => this.open_cron(cx),
                                    Section::Kanban => this.open_kanban(cx),
                                    Section::Accounts => this.open_accounts(cx),
                                    Section::Skills => this.open_skills(cx),
                                    Section::Mcp => this.open_mcp(cx),
                                    Section::Memory => this.open_memory(cx),
                                    Section::Vault => this.open_vault(cx),
                                },
                            ))
                            .child(div().child(SharedString::from(candidate.glyph())))
                            .child(div().child(SharedString::from(candidate.label())))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(HEADER_H))
                            .px_5()
                            .border_b_1()
                            .border_color(rgb(border()))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(SharedString::from(section.label())),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(section == Section::Vault, |this| {
                                        this.child(
                                            div()
                                                .id("vault-new")
                                                .px_3()
                                                .py_2()
                                                .rounded_md()
                                                .bg(rgb(accent()))
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .text_color(rgb(bg()))
                                                .cursor_pointer()
                                                .hover(|this| this.bg(rgb(accent_hi())))
                                                .on_click(cx.listener(
                                                    |this, _event, _window, cx| {
                                                        this.vault.start_adding(None);
                                                        cx.notify();
                                                    },
                                                ))
                                                .child("＋ New item"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("settings-close")
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_sm()
                                            .text_color(rgb(text_muted()))
                                            .cursor_pointer()
                                            .hover(|this| {
                                                this.bg(rgb(bg_elev())).text_color(rgb(text()))
                                            })
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.close_settings(cx)
                                            }))
                                            .child("×"),
                                    ),
                            ),
                    )
                    .child(body),
            )
            .into_any_element()
    }
    fn settings_overlay(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(scrim()))
            .child(self.settings_sheet(cx))
            .into_any_element()
    }

    fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.inspector.open = !self.inspector.open;
        if self.inspector.open {
            let name = self.profile_name();
            let meta = self
                .current_profile()
                .and_then(|profile| profile.ui_meta.clone());
            self.inspector.target(&name);
            self.inspector.seed_look(meta.as_ref());
            self.backend.send(Command::Inspect { profile: name });
        } else {
            self.inspector.focus = None;
        }
        cx.notify();
    }

    fn save_look(&mut self, cx: &mut Context<Self>) {
        let profile = self.profile_name();
        let patch = inspector::look_patch(&self.inspector.glyph, self.inspector.accent);
        self.inspector.status = Some("saving look…".into());
        self.backend
            .send(Command::ConfigureProfile { profile, patch });
        cx.notify();
    }

    fn pick_avatar(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let profile = self.profile_name();
        cx.spawn(async move |_this, _cx| {
            if let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("image", &["png", "jpg", "jpeg", "webp"])
                .pick_file()
                .await
            {
                backend.send(Command::AttachAvatar {
                    profile,
                    path: file.path().to_path_buf(),
                });
            }
        })
        .detach();
    }

    fn remove_avatar(&mut self, cx: &mut Context<Self>) {
        let profile = self.profile_name();
        self.inspector.clear_avatar(&profile);
        self.backend.send(Command::ClearAvatar { profile });
        cx.notify();
    }

    fn toggle_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(detail) = self.inspector.detail.clone() else {
            return;
        };
        let mut enabled: Vec<String> = detail
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.name.clone())
            .collect();
        match enabled.iter().position(|candidate| candidate == name) {
            Some(index) => {
                enabled.remove(index);
            }
            None => enabled.push(name.to_string()),
        }
        let profile = self.profile_name();
        self.backend.send(Command::ConfigureProfile {
            profile,
            patch: serde_json::json!({ "enabled_mcp_servers": enabled }),
        });
        if let Some(detail) = self.inspector.detail.as_mut() {
            for server in &mut detail.mcp_servers {
                if server.name == name {
                    server.enabled = !server.enabled;
                }
            }
        }
        cx.notify();
    }

    fn toggle_toolset(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(detail) = self.inspector.detail.clone() else {
            return;
        };
        let mut enabled: Vec<String> = detail
            .toolsets
            .iter()
            .filter(|toolset| toolset.enabled)
            .map(|toolset| toolset.name.clone())
            .collect();
        match enabled.iter().position(|candidate| candidate == name) {
            Some(index) => {
                // An empty pin means "everything enabled" server-side, so the
                // last toolset cannot be switched off from here.
                if enabled.len() == 1 {
                    self.inspector.status = Some("a bot always keeps one toolset".into());
                    cx.notify();
                    return;
                }
                enabled.remove(index);
            }
            None => enabled.push(name.to_string()),
        }
        let profile = self.profile_name();
        self.backend.send(Command::ConfigureProfile {
            profile,
            patch: serde_json::json!({ "enabled_toolsets": enabled }),
        });
        if let Some(detail) = self.inspector.detail.as_mut() {
            for toolset in &mut detail.toolsets {
                if toolset.name == name {
                    toolset.enabled = !toolset.enabled;
                }
            }
        }
        cx.notify();
    }

    fn add_secret(&mut self, cx: &mut Context<Self>) {
        let key = self.inspector.draft_key.trim().to_string();
        let value = self.inspector.draft_value.clone();
        if key.is_empty() || value.is_empty() {
            self.inspector.status = Some("a secret needs a key and a value".into());
            cx.notify();
            return;
        }
        let profile = self.profile_name();
        self.backend.send(Command::SetSecret {
            profile,
            key: key.clone(),
            value,
        });
        self.inspector.draft_key.clear();
        self.inspector.draft_value.clear();
        self.inspector.focus = None;
        self.inspector.status = Some(format!("saving {key}…"));
        cx.notify();
    }

    fn delete_secret(&mut self, key: &str, cx: &mut Context<Self>) {
        self.backend.send(Command::DeleteSecret {
            profile: self.profile_name(),
            key: key.to_string(),
        });
        self.inspector.status = Some(format!("removing {key}…"));
        cx.notify();
    }

    fn resolve_card(&mut self, id: &str, cx: &mut Context<Self>) {
        self.cards.retain(|card| card.id != id);
        cx.notify();
    }

    fn approve(&mut self, id: &str, choice: ApprovalChoice, cx: &mut Context<Self>) {
        if let Some(card) = self.cards.iter().find(|card| card.id == id)
            && let CardKind::Approval(request) = &card.kind
        {
            self.backend.send(Command::Approve {
                request: request.clone(),
                choice,
            });
        }
        self.resolve_card(id, cx);
    }

    fn answer(&mut self, id: &str, answer: String, cx: &mut Context<Self>) {
        if let Some(card) = self.cards.iter().find(|card| card.id == id)
            && let CardKind::Clarify(request) = &card.kind
        {
            self.backend.send(Command::Answer {
                request: request.clone(),
                answer,
            });
        }
        self.resolve_card(id, cx);
    }

    fn dismiss_card(&mut self, id: &str, cx: &mut Context<Self>) {
        let session_id = self
            .cards
            .iter()
            .find(|card| card.id == id)
            .and_then(|card| match &card.kind {
                CardKind::Approval(request) => request.session_id.clone(),
                CardKind::Clarify(request) => request.session_id.clone(),
            });
        self.backend.send(Command::Decline {
            id: id.to_string(),
            session_id,
        });
        self.resolve_card(id, cx);
    }

    fn palette_items(&self) -> Vec<PaletteItem> {
        let query = self.palette.query.to_lowercase();
        let mut items = vec![
            PaletteItem {
                label: "New chat".into(),
                hint: "Ctrl+N".into(),
                action: PaletteAction::NewChat,
            },
            PaletteItem {
                label: "Refresh host".into(),
                hint: "Ctrl+R".into(),
                action: PaletteAction::Refresh,
            },
        ];
        if self.busy {
            items.push(PaletteItem {
                label: "Interrupt turn".into(),
                hint: "Esc".into(),
                action: PaletteAction::Interrupt,
            });
        }
        for (index, profile) in self.profiles.iter().enumerate() {
            items.push(PaletteItem {
                label: format!("Switch to {}", profile.label()),
                hint: format!("Ctrl+{}", index + 1),
                action: PaletteAction::SwitchProfile(profile.name.clone()),
            });
        }
        items.push(PaletteItem {
            label: "Quit".into(),
            hint: String::new(),
            action: PaletteAction::Quit,
        });

        if query.is_empty() {
            return items;
        }
        items
            .into_iter()
            .filter(|item| {
                item.label.to_lowercase().contains(&query)
                    || item.hint.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn accept_palette(&mut self, cx: &mut Context<Self>) {
        let items = self.palette_items();
        let Some(item) = items.get(self.palette.index).cloned() else {
            self.palette = Palette::default();
            cx.notify();
            return;
        };
        self.palette = Palette::default();
        match item.action {
            PaletteAction::NewChat => self.new_chat(cx),
            PaletteAction::Refresh => self.refresh(cx),
            PaletteAction::Interrupt => self.backend.send(Command::Interrupt),
            PaletteAction::SwitchProfile(name) => self.select_profile(name, cx),
            PaletteAction::Quit => cx.quit(),
        }
        cx.notify();
    }

    fn use_suggestion(&mut self, text: &str, cx: &mut Context<Self>) {
        self.composer = text.to_string();
        self.composer_input
            .update(cx, |input, cx| input.emplace(text, cx));
        cx.notify();
    }

    // ---- rendering ------------------------------------------------------

    /// Hermes' footer strip: gateway health on the left, what you are running
    /// on the right. It sits below the pane row so every pane ends on one line.
    fn status_bar(&self) -> impl IntoElement {
        let (label, color) = match self.connection {
            ConnectionState::Open => ("Gateway ready", user()),
            ConnectionState::Connecting => ("Connecting…", warn()),
            ConnectionState::Closed => ("Gateway offline", danger()),
        };
        let model = self
            .usage
            .model
            .as_ref()
            .map(|info| info.model.clone())
            .filter(|model| !model.is_empty())
            .or_else(|| {
                self.models
                    .options
                    .as_ref()
                    .map(|options| options.model.clone())
                    .filter(|model| !model.is_empty())
            })
            .unwrap_or_else(|| self.profile_name());
        let version = env!("CARGO_PKG_VERSION");

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .h(px(STATUS_H))
            .flex_shrink_0()
            .px_4()
            .bg(rgb(bg_elev()))
            .border_t_1()
            .border_color(rgb(border()))
            .text_xs()
            .text_color(rgb(text_muted()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb(color)))
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(SharedString::from(model))
                    .child(SharedString::from(format!("v{version}"))),
            )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, color) = match self.connection {
            ConnectionState::Open => ("connected", user()),
            ConnectionState::Connecting => ("connecting", warn()),
            ConnectionState::Closed => ("offline", danger()),
        };
        let model = self
            .current_profile()
            .and_then(|profile| profile.model.clone())
            .unwrap_or_default();

        div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(HEADER_H))
            .px_4()
            .bg(rgb(bg_elev()))
            .border_b_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(rgb(accent())).child("◆"))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text()))
                            .child("Hermes"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(self.host.base_url.clone())),
            )
            .child(
                div()
                    .id("connection")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(surface()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.refresh(cx)))
                    .child(div().text_xs().text_color(rgb(color)).child("●"))
                    .child(div().text_xs().text_color(rgb(text_muted())).child(label)),
            )
            .when(!model.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(surface()))
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(SharedString::from(model)),
                )
            })
            .when(self.busy, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(accent_soft()))
                        .child(div().text_xs().text_color(rgb(accent())).child("◐"))
                        .child(div().text_xs().text_color(rgb(accent())).child("working")),
                )
            })
            .when(!self.cards.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(warn_soft()))
                        .text_xs()
                        .text_color(rgb(warn()))
                        .child(format!("{} awaiting you", self.cards.len())),
                )
            })
            .when_some(self.voice_status.clone(), |this, status| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(surface()))
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(crate::transcript::clip(&status, 56))),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("inspector-toggle")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(self.inspector.open, |this| {
                        this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                    })
                    .when(!self.inspector.open, |this| {
                        this.text_color(rgb(text_muted()))
                            .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| this.toggle_inspector(cx)))
                    .child("⚙ Bot"),
            )
            .child(
                div()
                    .id("attach")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.pick_attachment(cx)))
                    .child("＋ Attach"),
            )
            .child(
                div()
                    .id("capture-screen")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.capture_screen(cx)))
                    .child("▣ Shot"),
            )
            .child(
                div()
                    .id("capture-text")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.capture_text(cx)))
                    .child("T Text"),
            )
            .child(
                div()
                    .id("speak-last")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.speak_last(cx)))
                    .child("▷ Speak"),
            )
            .child(
                div()
                    .id("palette-hint")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.dispatch(Action::OpenPalette, cx)
                    }))
                    .child("⌘K"),
            )
            .child(
                div()
                    .id("sessions-toggle")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.toggle_sessions(cx)))
                    .child("Sessions"),
            )
            .child(
                div()
                    .id("new-chat")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(surface_hi()))
                    .text_xs()
                    .text_color(rgb(text()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(border_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.new_chat(cx)))
                    .child("New chat"),
            )
    }

    fn open_sessions(&mut self, cx: &mut Context<Self>) {
        if self.sessions.open {
            self.sessions.open = false;
            self.sessions.focus = false;
        } else {
            self.sessions.open = true;
            self.sessions.focus = true;
            if !self.sessions.loaded {
                self.backend.send(Command::LoadSessions { offset: 0 });
            }
        }
        cx.notify();
    }

    fn toggle_sessions(&mut self, cx: &mut Context<Self>) {
        self.open_sessions(cx);
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions.loaded = false;
        self.sessions.error = None;
        self.backend.send(Command::LoadSessions { offset: 0 });
        cx.notify();
    }

    fn copy_reply(&mut self, text: String, cx: &mut Context<Self>) {
        if text.trim().is_empty() {
            return;
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.inspector.status = Some("copied reply".into());
        cx.notify();
    }

    fn load_more_sessions(&mut self, cx: &mut Context<Self>) {
        if !self.sessions.has_more() {
            return;
        }
        let offset = self.sessions.rows.len() as u32;
        self.sessions.error = None;
        self.sessions.status = Some("loading more…".into());
        self.backend.send(Command::LoadSessions { offset });
        cx.notify();
    }

    fn open_session(&mut self, id: &str, cx: &mut Context<Self>) {
        self.sessions.status = Some("resuming session…".to_string());
        self.backend
            .send(Command::ResumeSession { id: id.to_string() });
        cx.notify();
    }

    fn export_session(&mut self, id: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |_this, _cx| {
            if let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name("hermes-session.json")
                .add_filter("JSON", &["json"])
                .save_file()
                .await
            {
                backend.send(Command::ExportSession {
                    id,
                    path: file.path().to_path_buf(),
                });
            }
        })
        .detach();
    }

    fn cycle_project(&mut self, cx: &mut Context<Self>) {
        let projects = sessions::projects(&self.sessions.rows);
        self.sessions.project = match self.sessions.project.as_deref() {
            None => projects.first().cloned(),
            Some(current) => projects
                .iter()
                .position(|project| project == current)
                .and_then(|index| projects.get(index + 1).cloned()),
        };
        cx.notify();
    }

    fn pin_session(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        self.sessions.status = Some(if pinned { "pinning…" } else { "unpinning…" }.into());
        self.backend.send(Command::UpdateSession {
            id: id.to_string(),
            patch: serde_json::json!({ "pinned": pinned }),
        });
        cx.notify();
    }

    fn start_rename_session(&mut self, id: &str, title: &str, cx: &mut Context<Self>) {
        self.sessions.begin_rename(id, title);
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.sessions.renaming.take() else {
            return;
        };
        self.sessions.status = Some("renaming…".into());
        self.backend.send(Command::UpdateSession {
            id: draft.id,
            patch: serde_json::json!({ "title": draft.title.trim() }),
        });
        cx.notify();
    }

    fn archive_session(&mut self, id: &str, cx: &mut Context<Self>) {
        self.sessions.status = Some("archiving…".into());
        self.backend.send(Command::UpdateSession {
            id: id.to_string(),
            patch: serde_json::json!({ "archived": true }),
        });
        cx.notify();
    }

    fn delete_session(&mut self, id: &str, cx: &mut Context<Self>) {
        self.backend
            .send(Command::DeleteSession { id: id.to_string() });
        cx.notify();
    }

    fn route_sessions_typing(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.sessions.renaming.is_some() {
            let keystroke = &event.keystroke;
            if keystroke.modifiers.control || keystroke.modifiers.platform {
                return false;
            }
            match keystroke.key.as_str() {
                "enter" => self.commit_rename(cx),
                "escape" => self.sessions.cancel_rename(),
                "backspace" => {
                    if let Some(draft) = self.sessions.renaming.as_mut() {
                        draft.title.pop();
                    }
                }
                _ => {
                    if let Some(chars) = keystroke.key_char.as_deref()
                        && let Some(draft) = self.sessions.renaming.as_mut()
                    {
                        draft.title.push_str(chars);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if !self.sessions.focus {
            return false;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "enter" => self.sessions.focus = false,
            "backspace" => {
                self.sessions.query.pop();
            }
            _ => {
                if let Some(chars) = keystroke.key_char.as_deref() {
                    self.sessions.query.push_str(chars);
                }
            }
        }
        cx.notify();
        true
    }

    fn sessions_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = sessions::filtered_project(
            &self.sessions.rows,
            &self.sessions.query,
            self.sessions.project.as_deref(),
        );
        let now = sessions::now_secs();
        let focus = self.sessions.focus;
        let project_label = self
            .sessions
            .project
            .as_deref()
            .map(sessions::project_label)
            .unwrap_or_else(|| "All projects".to_string());

        let mut list = div()
            .id("sessions-list")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_3()
            .py_2();
        if !self.sessions.loaded {
            list = list.child(self.note("Loading sessions…"));
        } else if let Some(error) = self.sessions.error.as_deref() {
            list = list.child(self.note(error));
        } else if rows.is_empty() {
            list = list.child(self.note(if self.sessions.query.is_empty() {
                "No sessions yet."
            } else {
                "No sessions match this search."
            }));
        }
        for row in rows {
            let open_id = row.id.clone();
            let delete_id = row.id.clone();
            let export_id = row.id.clone();
            let pin_id = row.id.clone();
            let archive_id = row.id.clone();
            let rename_id = row.id.clone();
            let rename_title = row.title.clone();
            let pinned = row.pinned;
            let rename_draft = self
                .sessions
                .renaming
                .as_ref()
                .filter(|draft| draft.id == row.id)
                .map(|draft| draft.title.clone());
            let renaming = rename_draft.is_some();
            let active = self.viewing_history.as_deref() == Some(row.id.as_str());
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(surface()))
                    .when(active, |this| this.bg(rgb(accent_soft())))
                    .child(
                        div()
                            .id(SharedString::from(format!("session-{}", row.id)))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .px_1()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.open_session(&open_id, cx)
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .when(active, |this| this.text_color(rgb(accent())))
                                    .when(!active, |this| this.text_color(rgb(text())))
                                    .child(SharedString::from(match &rename_draft {
                                        Some(draft) => format!("{draft}▌"),
                                        None => sessions::title(&row),
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .child(SharedString::from(sessions::meta(&row, now))),
                            ),
                    )
                    .child(if renaming {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-save-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(accent()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(accent_hi())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.commit_rename(cx)
                                    }))
                                    .child("↵"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-cancel-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.sessions.cancel_rename();
                                        cx.notify();
                                    }))
                                    .child("Esc"),
                            )
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-rename-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.start_rename_session(&rename_id, &rename_title, cx)
                                    }))
                                    .child("✎"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-pin-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(if pinned { accent() } else { text_muted() }))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.pin_session(&pin_id, !pinned, cx)
                                    }))
                                    .child(if pinned { "Unpin" } else { "Pin" }),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-arch-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.archive_session(&archive_id, cx)
                                    }))
                                    .child("Arch"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-export-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.export_session(export_id.clone(), cx)
                                    }))
                                    .child("↓"),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-del-{}", row.id)))
                                    .px_1()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(danger())))
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.delete_session(&delete_id, cx)
                                    }))
                                    .child("×"),
                            )
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .w(px(RAIL_W))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(bg_elev()))
            .border_r_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text_dim()))
                            .child("SESSIONS"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("sessions-refresh")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_sessions(cx)
                                    }))
                                    .child("⟳"),
                            )
                            .child(
                                div()
                                    .id("sessions-bots")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.text_color(rgb(text())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.sessions.open = false;
                                        this.sessions.focus = false;
                                        cx.notify();
                                    }))
                                    .child("Bots"),
                            ),
                    ),
            )
            .child(
                div().px_3().pb_2().child(
                    div()
                        .id("sessions-search")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(bg()))
                        .border_1()
                        .when(focus, |this| this.border_color(rgb(accent())))
                        .when(!focus, |this| this.border_color(rgb(border())))
                        .text_xs()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.sessions.focus = true;
                            cx.notify();
                        }))
                        .child(div().text_color(rgb(text_dim())).child("⌕"))
                        .when(self.sessions.query.is_empty(), |this| {
                            this.child(div().text_color(rgb(text_dim())).child("Search sessions"))
                        })
                        .when(!self.sessions.query.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(self.sessions.query.clone())),
                            )
                        }),
                ),
            )
            .child(
                div().px_3().pb_2().child(
                    div()
                        .id("sessions-project")
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(surface()))
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
                        .on_click(cx.listener(|this, _event, _window, cx| this.cycle_project(cx)))
                        .child(SharedString::from(format!("Project: {project_label}")))
                        .child("cycle"),
                ),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(SharedString::from(format!(
                        "{} of {} sessions",
                        self.sessions.rows.len(),
                        self.sessions.total
                    )))
                    .when(self.sessions.has_more(), |this| {
                        this.child(
                            div()
                                .id("sessions-load-more")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(accent()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(surface_hi())))
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.load_more_sessions(cx)
                                }))
                                .child("Load more"),
                        )
                    }),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.profile_name();
        let needle = self.rail_filter.trim().to_lowercase();
        let visible: Vec<&Profile> = self
            .profiles
            .iter()
            .filter(|profile| {
                needle.is_empty()
                    || profile.label().to_lowercase().contains(&needle)
                    || profile.name.to_lowercase().contains(&needle)
                    || profile.preview().to_lowercase().contains(&needle)
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .w(px(RAIL_W))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(bg_elev()))
            .border_r_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text_dim()))
                            .child("BOTS"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("rail-search")
                                    .w(px(24.))
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(rgb(surface()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(surface_hi())))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.rail_search = !this.rail_search;
                                        if !this.rail_search {
                                            this.rail_filter.clear();
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div().text_xs().text_color(rgb(text_muted())).child("⌕"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(format!("{}", visible.len())),
                            ),
                    ),
            )
            .when(self.rail_search, |this| {
                this.child(
                    div()
                        .mx_3()
                        .mb_2()
                        .px_3()
                        .py_2()
                        .rounded_full()
                        .bg(rgb(surface()))
                        .border_1()
                        .border_color(rgb(border()))
                        .text_xs()
                        .when(self.rail_filter.is_empty(), |this| {
                            this.text_color(rgb(text_dim())).child("Search bots…")
                        })
                        .when(!self.rail_filter.is_empty(), |this| {
                            this.text_color(rgb(text()))
                                .child(SharedString::from(self.rail_filter.clone()))
                        }),
                )
            })
            .child(
                div()
                    .id("profiles")
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_2()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(visible.iter().map(|profile| {
                        let selected = profile.name == current;
                        let name = profile.name.clone();
                        let (glyph, accent) =
                            inspector::look_for(&profile.name, profile.ui_meta.as_ref());
                        let preview = clip(profile.preview(), 58);
                        let has_history = profile.last_session.is_some();

                        div()
                            .id(SharedString::from(format!("profile-{}", profile.name)))
                            .flex()
                            .items_stretch()
                            .gap_2()
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(selected, |this| this.bg(rgb(surface())))
                            .when(!selected, |this| this.hover(|this| this.bg(rgb(surface()))))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.select_profile(name.clone(), cx);
                            }))
                            .child(
                                div()
                                    .w(px(2.))
                                    .rounded_full()
                                    .when(selected, move |this| this.bg(rgb(accent))),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .h(px(32.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(rgb(accent_soft()))
                                    .border_1()
                                    .border_color(rgb(accent))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(accent))
                                            .child(SharedString::from(glyph)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .when(selected, |this| {
                                                        this.text_color(rgb(crate::theme::accent()))
                                                    })
                                                    .when(!selected, |this| {
                                                        this.text_color(rgb(text()))
                                                    })
                                                    .child(SharedString::from(
                                                        profile.label().to_string(),
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .when(has_history, |this| {
                                                this.text_color(rgb(text_muted()))
                                            })
                                            .when(!has_history, |this| {
                                                this.text_color(rgb(text_dim()))
                                            })
                                            .child(SharedString::from(preview)),
                                    ),
                            )
                    })),
            )
            .when(!self.attachments.is_empty(), |this| {
                this.child(
                    div()
                        .border_t_1()
                        .border_color(rgb(border()))
                        .mt_2()
                        .px_4()
                        .pt_2()
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(self.attachments.join(" · "))),
                )
            })
            .when_some(self.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .mx_2()
                        .mb_2()
                        .px_2()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(danger_soft()))
                        .text_xs()
                        .text_color(rgb(danger()))
                        .child(SharedString::from(error.clone())),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .mx_2()
                    .mb_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .w(px(28.))
                                    .h(px(28.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(rgb(surface_hi()))
                                    .child(
                                        div().text_xs().text_color(rgb(text_muted())).child("◍"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .min_w_0()
                                    .child(div().text_xs().text_color(rgb(text())).child("You"))
                                    .child(
                                        div().text_xs().text_color(rgb(text_dim())).child(
                                            SharedString::from(clip(&self.host.base_url, 24)),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("sidebar-apps")
                            .w(px(28.))
                            .h(px(28.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(rgb(surface()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.toggle_inspector(cx)),
                            )
                            .child(div().text_xs().text_color(rgb(text_muted())).child("⊞")),
                    ),
            )
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .h_full()
            .w_full()
            .px_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_5()
                    .w_full()
                    .max_w(px(READ_W))
                    .child(
                        div()
                            .w(px(44.))
                            .h(px(44.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_xl()
                            .bg(rgb(accent_soft()))
                            .child(div().text_lg().text_color(rgb(accent())).child("◆")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(text()))
                                    .child("What should we get done?"),
                            )
                            .child(div().text_sm().text_color(rgb(text_muted())).child(
                                SharedString::from(format!("Talking to {}", self.profile_name())),
                            )),
                    )
                    .child(div().flex().flex_col().gap_2().w_full().children(
                        SUGGESTIONS.iter().map(|suggestion| {
                            let text = suggestion.to_string();
                            div()
                                .id(SharedString::from(format!("suggestion-{suggestion}")))
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(border()))
                                .bg(rgb(surface()))
                                .cursor_pointer()
                                .hover(|this| {
                                    this.bg(rgb(surface_hi())).border_color(rgb(border_hi()))
                                })
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.use_suggestion(&text.clone(), cx)
                                }))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(crate::theme::text()))
                                        .child(SharedString::from(*suggestion)),
                                )
                                .child(div().text_xs().text_color(rgb(text_dim())).child("↵"))
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("Enter to send")
                            .child("·")
                            .child("Shift+Enter newline")
                            .child("·")
                            .child("@ to mention")
                            .child("·")
                            .child("⌘K commands"),
                    ),
            )
    }

    fn chat(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.has_conversation() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(self.empty_state(cx))
                .into_any_element();
        }

        div()
            .id("transcript")
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .bg(rgb(bg()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .w_full()
                    .max_w(px(READ_W))
                    .mx_auto()
                    .px_6()
                    .py_5()
                    .children(self.lines.iter().map(|line| self.render_line(line, cx))),
            )
            .into_any_element()
    }

    fn render_line(&self, line: &Line, cx: &mut Context<Self>) -> gpui::AnyElement {
        let streaming = self.busy
            && line.role == Role::Assistant
            && std::ptr::eq(line, self.lines.last().unwrap_or(line));

        let mut text = if line.text.is_empty() {
            "…".to_string()
        } else {
            line.text.clone()
        };
        if streaming {
            text.push('▍');
        }
        let mut copy_hash: u64 = 1469598103934665603;
        for byte in text.as_bytes() {
            copy_hash ^= *byte as u64;
            copy_hash = copy_hash.wrapping_mul(1099511628211);
        }

        match line.role {
            Role::User => div()
                .flex()
                .flex_col()
                .items_end()
                .gap_2()
                .w_full()
                .child(
                    div()
                        .max_w(px(BUBBLE_W))
                        .px_4()
                        .py_3()
                        .rounded_xl()
                        .bg(rgb(user()))
                        .text_sm()
                        .text_color(rgb(bg()))
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
            Role::Assistant => {
                let (bot_glyph, _) = self
                    .current_profile()
                    .map(|profile| inspector::look_for(&profile.name, profile.ui_meta.as_ref()))
                    .unwrap_or_else(|| inspector::look_for(&self.profile_name(), None));
                let bot_name = self
                    .current_profile()
                    .map(|profile| profile.label().to_string())
                    .unwrap_or_else(|| self.profile_name());
                let copy_text = text.clone();
                let copy_id = SharedString::from(format!("copy-{copy_hash:x}"));
                let mut body = div().flex().flex_col().gap_1();
                for (index, block) in crate::transcript::markdown_blocks(&text)
                    .into_iter()
                    .enumerate()
                {
                    use crate::transcript::MarkdownBlock;
                    if let MarkdownBlock::Image(path) = block {
                        match self.media.get(&path) {
                            Some(Ok(image)) => {
                                body = body.child(
                                    gpui::img(image.clone())
                                        .max_w(px(BUBBLE_W))
                                        .max_h(px(BUBBLE_W)),
                                );
                            }
                            Some(Err(message)) => {
                                body = body.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .child(SharedString::from(message.clone())),
                                );
                            }
                            None => {
                                body = body.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "media-{}-{index}",
                                            line.text
                                        )))
                                        .text_xs()
                                        .text_color(rgb(accent()))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.backend.send(Command::LoadMedia(path.clone()));
                                            cx.notify();
                                        }))
                                        .child("Load image"),
                                );
                            }
                        }
                    } else {
                        let (value, heading, code, quote, item) = match block {
                            MarkdownBlock::Paragraph(value) => (value, false, false, false, false),
                            MarkdownBlock::Heading(value) => (value, true, false, false, false),
                            MarkdownBlock::Code(value) => (value, false, true, false, false),
                            MarkdownBlock::Quote(value) => (value, false, false, true, false),
                            MarkdownBlock::ListItem(value) => (value, false, false, false, true),
                            MarkdownBlock::Image(_) => unreachable!(),
                        };
                        let prefix = if item { "• " } else { "" };
                        let highlights = value
                            .spans
                            .into_iter()
                            .map(|span| {
                                let style = gpui::HighlightStyle {
                                    font_weight: span.bold.then_some(gpui::FontWeight::BOLD),
                                    font_style: span.italic.then_some(gpui::FontStyle::Italic),
                                    background_color: span
                                        .code
                                        .then(|| gpui::HighlightStyle::from(rgb(bg_elev())).color)
                                        .flatten(),
                                    color: span
                                        .link
                                        .then(|| gpui::HighlightStyle::from(rgb(accent())).color)
                                        .flatten(),
                                    ..Default::default()
                                };
                                (
                                    (span.range.start + prefix.len())
                                        ..(span.range.end + prefix.len()),
                                    style,
                                )
                            })
                            .collect::<Vec<_>>();
                        let styled = gpui::StyledText::new(format!("{prefix}{}", value.text))
                            .with_highlights(highlights);
                        body = body.child(
                            div()
                                .text_sm()
                                .text_color(rgb(crate::theme::text()))
                                .when(heading, |v| {
                                    v.font_weight(gpui::FontWeight::BOLD).text_base()
                                })
                                .when(code, |v| {
                                    v.font_family("monospace")
                                        .bg(rgb(bg_elev()))
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                })
                                .when(quote, |v| v.pl_3().border_l_2().border_color(rgb(accent())))
                                .child(styled),
                        );
                    }
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .w_full()
                            .py_1()
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(rgb(accent_soft()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(accent()))
                                            .child(SharedString::from(bot_glyph)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .child(SharedString::from(format!("Messaged {bot_name}"))),
                            )
                            .child(
                                div()
                                    .id(copy_id)
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(accent()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(surface_hi())))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.copy_reply(copy_text.clone(), cx)
                                    }))
                                    .child("Copy"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w_full()
                            .px_4()
                            .py_3()
                            .rounded_xl()
                            .bg(rgb(surface()))
                            .child(body),
                    )
                    .into_any_element()
            }
            Role::Reasoning => div()
                .flex()
                .flex_col()
                .gap_2()
                .w_full()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(reasoning()))
                        .child("THINKING"),
                )
                .child(
                    div()
                        .pl_3()
                        .border_l_2()
                        .border_color(rgb(reasoning_soft()))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(text.split('\n').map(|part| {
                            div()
                                .text_xs()
                                .text_color(rgb(reasoning()))
                                .child(SharedString::from(part.to_string()))
                        })),
                )
                .into_any_element(),
            Role::System => div()
                .flex()
                .items_start()
                .gap_2()
                .w_full()
                .child(div().text_xs().text_color(rgb(text_dim())).child("·"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
        }
    }

    fn cards_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pb_2()
            .children(self.cards.iter().map(|card| match &card.kind {
                CardKind::Approval(request) => self.approval_card(card, request, cx),
                CardKind::Clarify(request) => self.clarify_card(card, request, cx),
            }))
    }

    fn approval_card(
        &self,
        card: &Card,
        request: &ApprovalRequest,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = card.id.clone();
        let body = if !request.command.is_empty() {
            request.command.clone()
        } else if !request.description.is_empty() {
            request.description.clone()
        } else {
            "A command needs your approval".to_string()
        };
        let detail = if !request.description.is_empty() && !request.command.is_empty() {
            Some(request.description.clone())
        } else {
            None
        };
        let allow_permanent = request.allow_permanent;

        let id_once = id.clone();
        let id_always = id.clone();
        let id_deny = id.clone();

        div()
            .id(SharedString::from(format!("approval-{id}")))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .bg(rgb(surface()))
            .border_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(warn_soft()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(warn()))
                            .child("APPROVAL"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child(if allow_permanent {
                                "can be remembered"
                            } else {
                                "one-off"
                            }),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg()))
                    .text_sm()
                    .text_color(rgb(text()))
                    .child(SharedString::from(body)),
            )
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(SharedString::from(detail)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("allow-once-{id}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(accent_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.approve(&id_once, ApprovalChoice::Once, cx)
                            }))
                            .child("Allow once"),
                    )
                    .when(allow_permanent, |this| {
                        let id_always = id_always.clone();
                        this.child(
                            div()
                                .id(SharedString::from(format!("allow-always-{id}")))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(surface_hi()))
                                .text_xs()
                                .text_color(rgb(user()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(border_hi())))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.approve(&id_always, ApprovalChoice::Always, cx)
                                }))
                                .child("Always allow"),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("deny-{id}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface_hi()))
                            .text_xs()
                            .text_color(rgb(danger()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(danger_soft())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.approve(&id_deny, ApprovalChoice::Deny, cx)
                            }))
                            .child("Deny"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("Esc to dismiss"),
                    ),
            )
            .into_any_element()
    }

    fn clarify_card(
        &self,
        card: &Card,
        request: &ClarifyRequest,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = card.id.clone();
        let typed_answer = self.composer.trim().to_string();
        let id_skip = id.clone();

        let mut choices = div().flex().flex_col().gap_1();
        for (index, choice) in request.choices.iter().enumerate() {
            let answer = choice.clone();
            let card_id = id.clone();
            choices = choices.child(
                div()
                    .id(SharedString::from(format!("choice-{id}-{index}")))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(bg()))
                    .text_xs()
                    .text_color(rgb(text()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(accent())))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.answer(&card_id, answer.clone(), cx)
                    }))
                    .child(SharedString::from(choice.clone())),
            );
        }

        div()
            .id(SharedString::from(format!("clarify-{id}")))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .bg(rgb(surface()))
            .border_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(accent_soft()))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(accent()))
                    .child("QUESTION"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(text()))
                    .child(SharedString::from(request.question.clone())),
            )
            .child(choices)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(!typed_answer.is_empty(), |this| {
                        let card_id = id.clone();
                        let answer = typed_answer.clone();
                        this.child(
                            div()
                                .id(SharedString::from(format!("answer-{card_id}")))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(accent()))
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(bg()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(accent_hi())))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.answer(&card_id, answer.clone(), cx)
                                }))
                                .child("Answer with message"),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("skip-{id}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface_hi()))
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(border_hi())).text_color(rgb(text())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.dismiss_card(&id_skip, cx)
                            }))
                            .child("Skip"),
                    )
                    .child(div().flex_1())
                    .when(typed_answer.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgb(text_dim()))
                                .child("Type an answer, then Enter"),
                        )
                    }),
            )
            .into_any_element()
    }

    fn palette_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.palette_items();
        let selected = self.palette.index.min(items.len().saturating_sub(1));

        div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(READ_W))
            .mx_auto()
            .mb_2()
            .rounded_xl()
            .bg(rgb(surface()))
            .border_1()
            .border_color(rgb(border_hi()))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(div().text_sm().text_color(rgb(accent())).child("›"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .when(self.palette.query.is_empty(), |this| {
                                this.text_color(rgb(text_dim()))
                            })
                            .when(!self.palette.query.is_empty(), |this| {
                                this.text_color(rgb(text()))
                            })
                            .child(SharedString::from(if self.palette.query.is_empty() {
                                "Type a command…".to_string()
                            } else {
                                self.palette.query.clone()
                            })),
                    )
                    .child(div().text_xs().text_color(rgb(text_dim())).child("esc")),
            )
            .child(
                div()
                    .id("palette-list")
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_2()
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(items.into_iter().enumerate().map(|(index, item)| {
                        div()
                            .id(SharedString::from(format!("palette-{index}")))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(index == selected, |this| this.bg(rgb(accent_soft())))
                            .when(index != selected, |this| {
                                this.hover(|this| this.bg(rgb(surface_hi())))
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.palette.index = index;
                                this.accept_palette(cx);
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .when(index == selected, |this| this.text_color(rgb(accent())))
                                    .when(index != selected, |this| this.text_color(rgb(text())))
                                    .child(SharedString::from(item.label)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(SharedString::from(item.hint)),
                            )
                    })),
            )
    }

    fn mention_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.mention_items();
        let selected = self.mention.index.min(items.len().saturating_sub(1));

        div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(READ_W))
            .mx_auto()
            .mb_2()
            .rounded_xl()
            .bg(rgb(surface()))
            .border_1()
            .border_color(rgb(border_hi()))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(reasoning()))
                            .child(SharedString::from(format!("@{}", self.mention.query))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("↑↓ · Enter"),
                    ),
            )
            .child(
                div()
                    .id("mention-list")
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_2()
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .children(items.into_iter().enumerate().map(|(index, item)| {
                        div()
                            .id(SharedString::from(format!("mention-{index}")))
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(index == selected, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(index != selected, |this| {
                                this.text_color(rgb(text()))
                                    .hover(|this| this.bg(rgb(surface_hi())))
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.mention.index = index;
                                this.accept_mention(cx);
                            }))
                            .child(SharedString::from(format!("@{item}")))
                    })),
            )
    }

    fn section_label(&self, text: &str) -> gpui::Div {
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(text_dim()))
            .child(SharedString::from(text.to_string()))
    }

    fn note(&self, text: &str) -> gpui::Div {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(surface()))
            .text_xs()
            .text_color(rgb(text_dim()))
            .child(SharedString::from(text.to_string()))
    }

    fn stat(&self, label: &str, value: String) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .child(SharedString::from(value)),
            )
    }

    fn inspector_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tab = self.inspector.tab;
        let body = match tab {
            Tab::Look => self.look_tab(cx),
            Tab::Connectors => self.connectors_tab(cx),
            Tab::Secrets => self.secrets_tab(cx),
        };

        div()
            .w(px(PANEL_W))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(bg_elev()))
            .border_l_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(HEADER_H))
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(text_dim()))
                            .child("BOT INSPECTOR"),
                    )
                    .child(
                        div()
                            .id("close-inspector")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.toggle_inspector(cx)),
                            )
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .children(Tab::all().into_iter().map(|candidate| {
                        let selected = candidate == tab;
                        div()
                            .id(SharedString::from(format!(
                                "inspector-tab-{}",
                                candidate.label()
                            )))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!selected, |this| {
                                this.text_color(rgb(text_muted()))
                                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.inspector.tab = candidate;
                                cx.notify();
                            }))
                            .child(candidate.label())
                    })),
            )
            .when_some(self.inspector.status.clone(), |this, status| {
                this.child(
                    div()
                        .px_4()
                        .py_2()
                        .bg(rgb(surface()))
                        .text_xs()
                        .text_color(rgb(text_muted()))
                        .child(SharedString::from(status)),
                )
            })
            .child(
                div()
                    .id("inspector-body")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }

    fn look_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let profile = self.inspector.detail_for.clone().unwrap_or_default();
        let accent = self.inspector.accent;
        let glyph = self.inspector.glyph.clone();
        let avatar = self.inspector.avatar.clone();
        let has_avatar = avatar.is_some();

        let mut tile = div()
            .w(px(52.))
            .h(px(52.))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .rounded_xl()
            .overflow_hidden()
            .bg(rgb(surface_hi()))
            .border_1()
            .border_color(rgb(accent));
        if let Some(image) = avatar {
            tile = tile.child(gpui::img(image).w(px(52.)).h(px(52.)));
        } else {
            tile = tile.child(
                div()
                    .text_xl()
                    .text_color(rgb(accent))
                    .child(SharedString::from(glyph.clone())),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div().flex().items_center().gap_3().child(tile).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(text()))
                                .child(SharedString::from(profile.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id("avatar-pick")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(surface()))
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .cursor_pointer()
                                        .hover(|this| {
                                            this.bg(rgb(surface_hi())).text_color(rgb(text()))
                                        })
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.pick_avatar(cx)
                                        }))
                                        .child("Image…"),
                                )
                                .when(has_avatar, |this| {
                                    this.child(
                                        div()
                                            .id("avatar-clear")
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .text_color(rgb(danger()))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(rgb(danger_soft())))
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.remove_avatar(cx)
                                            }))
                                            .child("Remove"),
                                    )
                                }),
                        ),
                ),
            )
            .child(self.section_label("ICON"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(inspector::GLYPHS.iter().map(|candidate| {
                        let selected = *candidate == self.inspector.glyph;
                        let value = (*candidate).to_string();
                        div()
                            .id(SharedString::from(format!("glyph-{candidate}")))
                            .w(px(28.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_sm()
                            .when(selected, |this| {
                                this.border_1()
                                    .border_color(rgb(accent))
                                    .text_color(rgb(accent))
                            })
                            .when(!selected, |this| this.text_color(rgb(text_muted())))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.inspector.glyph = value.clone();
                                cx.notify();
                            }))
                            .child(*candidate)
                    })),
            )
            .child(self.section_label("ACCENT"))
            .child(div().flex().items_center().gap_2().children(
                inspector::accent_choices().into_iter().map(|choice| {
                    let selected = choice == accent;
                    let value = choice;
                    div()
                        .id(SharedString::from(format!("accent-{value:06x}")))
                        .w(px(22.))
                        .h(px(22.))
                        .rounded_full()
                        .bg(rgb(value))
                        .when(selected, |this| this.border_2().border_color(rgb(text())))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _event, _window, cx| {
                            this.inspector.accent = value;
                            cx.notify();
                        }))
                }),
            ))
            .child(
                div()
                    .id("save-look")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(crate::theme::accent()))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(bg()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(accent_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.save_look(cx)))
                    .child("Save look"),
            )
            .when_some(self.inspector.detail.clone(), |this, detail| {
                this.child(self.section_label("MODEL"))
                    .child(self.stat("provider", detail.model.provider))
                    .child(self.stat("model", detail.model.default))
                    .child(self.stat("skills", format!("{}", detail.skills.len())))
                    .child(self.stat(
                        "connectors",
                        format!(
                            "{} MCP · {} toolsets",
                            detail.mcp_servers.len(),
                            detail.toolsets.len()
                        ),
                    ))
            })
            .into_any_element()
    }

    fn connectors_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(detail) = self.inspector.detail.clone() else {
            return self.note("Loading connectors…").into_any_element();
        };

        let mut panel = div().flex().flex_col().gap_2();
        panel = panel.child(self.section_label("MCP SERVERS"));
        if detail.mcp_servers.is_empty() {
            panel = panel.child(self.note("This bot has no MCP servers configured."));
        }
        for server in &detail.mcp_servers {
            let name = server.name.clone();
            let enabled = server.enabled;
            let transport = server
                .transport
                .clone()
                .unwrap_or_else(|| "stdio".to_string());
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(SharedString::from(format!("MCP · {transport}"))),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("mcp-{name}")))
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .text_xs()
                            .cursor_pointer()
                            .when(enabled, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!enabled, |this| {
                                this.bg(rgb(surface_hi())).text_color(rgb(text_dim()))
                            })
                            .hover(|this| this.bg(rgb(border_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.toggle_mcp(&name, cx)
                            }))
                            .child(if enabled { "On" } else { "Off" }),
                    ),
            );
        }

        panel = panel.child(self.section_label("TOOLSETS"));
        if detail.toolsets.is_empty() {
            panel = panel.child(self.note("No toolsets reported."));
        }
        for toolset in &detail.toolsets {
            let name = toolset.name.clone();
            let enabled = toolset.enabled;
            let label = if toolset.label.is_empty() {
                toolset.name.clone()
            } else {
                toolset.label.clone()
            };
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(label)),
                            )
                            .child(div().text_xs().text_color(rgb(text_dim())).child(
                                SharedString::from(format!("{} tools", toolset.tool_count)),
                            )),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("toolset-{name}")))
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .text_xs()
                            .cursor_pointer()
                            .when(enabled, |this| {
                                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                            })
                            .when(!enabled, |this| {
                                this.bg(rgb(surface_hi())).text_color(rgb(text_dim()))
                            })
                            .hover(|this| this.bg(rgb(border_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.toggle_toolset(&name, cx)
                            }))
                            .child(if enabled { "On" } else { "Off" }),
                    ),
            );
        }

        panel.into_any_element()
    }

    fn secret_field(
        &self,
        label: &str,
        value: &str,
        placeholder: &str,
        focused: bool,
        target: Field,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.section_label(label))
            .child(
                div()
                    .id(SharedString::from(format!("secret-field-{label}")))
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .border_1()
                    .when(focused, |this| this.border_color(rgb(accent())))
                    .when(!focused, |this| this.border_color(rgb(border())))
                    .text_xs()
                    .cursor_pointer()
                    .when(value.is_empty(), |this| {
                        this.text_color(rgb(text_dim()))
                            .child(SharedString::from(placeholder.to_string()))
                    })
                    .when(!value.is_empty(), |this| {
                        this.text_color(rgb(text()))
                            .child(SharedString::from(value.to_string()))
                    })
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.inspector.focus = Some(target);
                        cx.notify();
                    })),
            )
    }

    fn secrets_tab(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = inspector::visible_env(
            &self.inspector.env,
            &self.inspector.filter,
            self.inspector.show_all,
        );
        let set_count = inspector::set_secret_count(&self.inspector.env);
        let filter = self.inspector.filter.clone();
        let key = self.inspector.draft_key.clone();
        let value = self.inspector.draft_value.clone();
        let focus = self.inspector.focus;

        let mut list = div().flex().flex_col().gap_1();
        if rows.is_empty() {
            list = list.child(self.note(if !self.inspector.env_loaded {
                "Loading secrets…"
            } else if self.inspector.show_all {
                "No keys match this filter."
            } else {
                "No secrets set on this host yet."
            }));
        }
        let header = if self.inspector.env_loaded {
            format!("SECRETS · {set_count} set")
        } else {
            "SECRETS".to_string()
        };
        for (name, entry) in rows {
            let name_for_delete = name.clone();
            let shown = entry.redacted_value.clone().unwrap_or_else(|| {
                if entry.is_set {
                    "set".to_string()
                } else {
                    "not set".to_string()
                }
            });
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(surface()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text()))
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(text_dim()))
                                    .child(SharedString::from(shown)),
                            ),
                    )
                    .when(entry.is_set, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("secret-{name}")))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(danger()))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(danger_soft())))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.delete_secret(&name_for_delete, cx)
                                }))
                                .child("×"),
                        )
                    }),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(self.section_label(&header))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id("open-vault")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(surface())).text_color(rgb(text())))
                                    .on_click(
                                        cx.listener(|this, _event, _window, cx| {
                                            this.open_vault(cx)
                                        }),
                                    )
                                    .child("Vault ⇗"),
                            )
                            .child(
                                div()
                                    .id("show-all-secrets")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(self.inspector.show_all, |this| {
                                        this.bg(rgb(accent_soft())).text_color(rgb(accent()))
                                    })
                                    .when(!self.inspector.show_all, |this| {
                                        this.text_color(rgb(text_muted()))
                                            .hover(|this| this.bg(rgb(surface())))
                                    })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.inspector.show_all = !this.inspector.show_all;
                                        cx.notify();
                                    }))
                                    .child("Show all"),
                            ),
                    ),
            )
            .child(self.secret_field(
                "FILTER",
                &filter,
                "OPENAI…",
                focus == Some(Field::Filter),
                Field::Filter,
                cx,
            ))
            .child(list)
            .child(self.section_label("ADD OR REPLACE"))
            .child(self.secret_field(
                "KEY",
                &key,
                "OPENAI_API_KEY",
                focus == Some(Field::SecretKey),
                Field::SecretKey,
                cx,
            ))
            .child(self.secret_field(
                "VALUE",
                &value,
                "sk-…",
                focus == Some(Field::SecretValue),
                Field::SecretValue,
                cx,
            ))
            .child(
                div()
                    .id("save-secret")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(crate::theme::accent()))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(bg()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(accent_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.add_secret(cx)))
                    .child("Save secret"),
            )
            .into_any_element()
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.composer.is_empty();
        let focused = self.focused;
        let placeholder = if self.viewing_history.is_some() {
            "Stored session — read-only · New chat to continue".to_string()
        } else {
            format!(
                "Message {}",
                self.current_profile()
                    .map(|profile| profile.label().to_string())
                    .unwrap_or_else(|| self.profile_name())
            )
        };

        let input = div().flex_1().min_w_0().child(
            text_area("composer-input")
                .role(gpui::Role::TextInput)
                .aria_label("Message composer")
                .aria_value(SharedString::from(self.composer.clone()))
                .accepts_input(self.viewing_history.is_none())
                .state(self.composer_input.downgrade())
                .placeholder(placeholder)
                .w_full()
                .text_sm()
                .text_color(rgb(text())),
        );

        div()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb_4()
            .bg(rgb(bg()))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .w_full()
                    .max_w(px(READ_W))
                    .mx_auto()
                    .px_2()
                    .py_2()
                    .rounded_xl()
                    .bg(rgb(surface()))
                    .border_1()
                    .when(focused, |this| this.border_color(rgb(accent())))
                    .when(!focused, |this| this.border_color(rgb(border())))
                    .child(
                        div()
                            .id("composer-attach")
                            .w(px(32.))
                            .h(px(32.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(rgb(surface_hi()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(border_hi())))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.pick_attachment(cx)),
                            )
                            .child(div().text_sm().text_color(rgb(text_muted())).child("＋")),
                    )
                    .child(input)
                    .child(
                        div()
                            .id("send")
                            .w(px(32.))
                            .h(px(32.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .cursor_pointer()
                            .when(empty, |this| {
                                this.bg(rgb(surface_hi())).text_color(rgb(text_dim()))
                            })
                            .when(!empty, |this| {
                                this.bg(rgb(accent()))
                                    .hover(|this| this.bg(rgb(accent_hi())))
                            })
                            .on_click(cx.listener(|this, _event, _window, cx| this.submit(cx)))
                            .child(
                                div()
                                    .text_sm()
                                    .when(empty, |this| this.text_color(rgb(text_dim())))
                                    .when(!empty, |this| this.text_color(rgb(bg())))
                                    .child("↑"),
                            ),
                    ),
            )
    }
}

impl HermesApp {
    fn memory_form_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(form) = self.memory.form.clone() else {
            return div().into_any_element();
        };
        let visible = form.visible_indices();
        let mut body = div()
            .id("memory-form")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(SharedString::from(format!("Configure {}", form.label))),
                            )
                            .when(!form.docs_url.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_dim()))
                                        .child(SharedString::from(form.docs_url.clone())),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("Tab moves · Space toggles · Enter saves · Esc cancels"),
                    ),
            );
        for index in visible {
            let field = form.fields[index].clone();
            let focused = form.focus == Some(index);
            let mut input = div()
                .id(SharedString::from(format!("memory-field-{}", field.key)))
                .flex()
                .items_center()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(bg()))
                .border_1()
                .text_xs()
                .cursor_pointer()
                .when(focused, |this| this.border_color(rgb(accent())))
                .when(!focused, |this| this.border_color(rgb(border())))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if let Some(form) = this.memory.form.as_mut() {
                        form.focus_field(index);
                        if !form
                            .fields
                            .get(index)
                            .is_some_and(|field| field.kind.is_text())
                        {
                            form.cycle_focused();
                        }
                    }
                    cx.notify();
                }));
            if field.kind.is_text() {
                let display = if field.text.is_empty()
                    && field.kind == memory::Kind::Secret
                    && field.is_set
                {
                    field.display()
                } else if field.text.is_empty() {
                    field.placeholder.clone()
                } else {
                    field.text.clone()
                };
                input = input.child(
                    div()
                        .text_color(rgb(if field.text.is_empty() {
                            text_dim()
                        } else {
                            text()
                        }))
                        .child(SharedString::from(display)),
                );
            } else {
                input = input
                    .justify_between()
                    .child(SharedString::from(field.display()))
                    .child("click to change");
            }
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().text_color(rgb(text_dim())).child(
                            SharedString::from(format!(
                                "{}{}",
                                field.label,
                                if field.required { " *" } else { "" }
                            )),
                        ))
                        .when(field.kind == memory::Kind::Secret && field.is_set, |this| {
                            this.child(div().text_xs().text_color(rgb(accent())).child("set"))
                        }),
                )
                .child(input);
            if !field.description.is_empty() {
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(field.description.clone())),
                );
            }
            if let (Some(minimum), Some(maximum)) = (field.minimum, field.maximum) {
                let range = match field.step {
                    Some(step) => format!("range {minimum}–{maximum} · step {step}"),
                    None => format!("range {minimum}–{maximum}"),
                };
                row = row.child(
                    div()
                        .text_xs()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(range)),
                );
            }
            body = body.child(row);
        }
        body = body
            .when_some(form.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(surface_hi()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("memory-save")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(accent_hi())))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| {
                                    this.save_memory_config(cx)
                                }),
                            )
                            .child("Save"),
                    )
                    .child(
                        div()
                            .id("memory-cancel")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.memory.form = None;
                                this.memory.status = None;
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(border()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("MEMORY PROVIDER"),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn mcp_tabs(&self, cx: &mut Context<Self>) -> gpui::Div {
        let tab = self.mcp.tab;
        let servers = div()
            .id("mcp-tab-servers")
            .px_3()
            .py_1()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(tab == mcp::Tab::Servers, |this| {
                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
            })
            .when(tab != mcp::Tab::Servers, |this| {
                this.text_color(rgb(text_muted()))
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
            })
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.mcp.tab = mcp::Tab::Servers;
                this.mcp.form = None;
                this.mcp.install = None;
                cx.notify();
            }))
            .child(SharedString::from(format!(
                "Servers · {}",
                self.mcp.rows.len()
            )));
        let catalog = div()
            .id("mcp-tab-catalog")
            .px_3()
            .py_1()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .when(tab == mcp::Tab::Catalog, |this| {
                this.bg(rgb(accent_soft())).text_color(rgb(accent()))
            })
            .when(tab != mcp::Tab::Catalog, |this| {
                this.text_color(rgb(text_muted()))
                    .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
            })
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.mcp.tab = mcp::Tab::Catalog;
                this.mcp.form = None;
                this.mcp.install = None;
                this.load_mcp_catalog(cx);
            }))
            .child("Catalog");
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_5()
            .py_2()
            .border_b_1()
            .border_color(rgb(border()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(servers)
                    .child(catalog),
            )
            .child(
                div()
                    .id("mcp-new")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(accent()))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(bg()))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(accent_hi())))
                    .on_click(cx.listener(|this, _event, _window, cx| this.begin_mcp_create(cx)))
                    .child("＋ Add server"),
            )
    }

    fn mcp_input(
        &self,
        label: &str,
        value: &str,
        placeholder: &str,
        focused: bool,
        field: mcp::Field,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut input = div()
            .id(SharedString::from(format!("mcp-field-{field:?}")))
            .flex()
            .items_center()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(bg()))
            .border_1()
            .text_xs()
            .cursor_pointer()
            .when(focused, |this| this.border_color(rgb(accent())))
            .when(!focused, |this| this.border_color(rgb(border())))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                if let Some(form) = this.mcp.form.as_mut() {
                    form.focus = field;
                }
                cx.notify();
            }));
        if value.is_empty() {
            input = input.child(
                div()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(placeholder.to_string())),
            );
        } else {
            input = input.child(
                div()
                    .text_color(rgb(text()))
                    .child(SharedString::from(value.to_string())),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child(SharedString::from(label.to_string())),
            )
            .child(input)
    }

    fn mcp_form_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(form) = self.mcp.form.clone() else {
            return div().into_any_element();
        };
        let is_edit = form.is_edit();
        let body = div()
            .id("mcp-form")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(if is_edit {
                                "Edit MCP server"
                            } else {
                                "Add MCP server"
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("Tab moves · Enter saves · Esc cancels"),
                    ),
            )
            .child(self.mcp_input(
                "Name",
                &form.name,
                "server-name",
                form.focus == mcp::Field::Name,
                mcp::Field::Name,
                cx,
            ))
            .child(self.mcp_input(
                "URL (HTTP/SSE)",
                &form.url,
                "https://host/mcp",
                form.focus == mcp::Field::Url,
                mcp::Field::Url,
                cx,
            ))
            .child(self.mcp_input(
                "Command (stdio)",
                &form.command,
                "uvx",
                form.focus == mcp::Field::Command,
                mcp::Field::Command,
                cx,
            ))
            .child(self.mcp_input(
                "Arguments",
                &form.args,
                "package==1 --flag",
                form.focus == mcp::Field::Args,
                mcp::Field::Args,
                cx,
            ))
            .child(self.mcp_input(
                "Environment",
                &form.env,
                "KEY=VALUE; OTHER=VALUE",
                form.focus == mcp::Field::Env,
                mcp::Field::Env,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_dim()))
                            .child("HTTP authentication"),
                    )
                    .child(
                        div()
                            .id("mcp-auth")
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(bg()))
                            .border_1()
                            .when(form.focus == mcp::Field::Auth, |this| {
                                this.border_color(rgb(accent()))
                            })
                            .when(form.focus != mcp::Field::Auth, |this| {
                                this.border_color(rgb(border()))
                            })
                            .text_xs()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if let Some(form) = this.mcp.form.as_mut() {
                                    form.focus = mcp::Field::Auth;
                                    form.cycle_auth();
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(match form.auth.as_str() {
                                "header" => "header · bearer token in the profile .env",
                                "oauth" => "oauth · authorize from the host",
                                _ => "none",
                            }))
                            .child("click to cycle"),
                    ),
            )
            .when(form.auth == "header", |this| {
                this.child(self.mcp_input(
                    if is_edit {
                        "Bearer token (blank keeps existing)"
                    } else {
                        "Bearer token"
                    },
                    &form.token,
                    "token",
                    form.focus == mcp::Field::Token,
                    mcp::Field::Token,
                    cx,
                ))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_dim()))
                    .child("HTTP uses URL; stdio uses command, args and env. Arguments are space-separated; env entries are KEY=VALUE separated by semicolons."),
            )
            .when_some(form.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(surface_hi()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("mcp-save")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(accent_hi())))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.save_mcp_form(cx)
                            }))
                            .child(if is_edit { "Save" } else { "Add" }),
                    )
                    .child(
                        div()
                            .id("mcp-cancel")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(surface_hi())).text_color(rgb(text())))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.mcp.form = None;
                                this.mcp.status = None;
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.mcp_tabs(cx))
            .child(body)
            .into_any_element()
    }

    fn mcp_catalog_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("mcp-catalog")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3();
        if self.mcp.catalog_loading {
            list = list.child(self.note("Loading the MCP catalog…"));
        } else if let Some(error) = self.mcp.catalog_error.as_deref() {
            list = list.child(self.note(error));
        } else if let Some(catalog) = &self.mcp.catalog {
            if catalog.entries.is_empty() {
                list = list.child(self.note("The host has no catalog entries."));
            }
            for entry in &catalog.entries {
                let install = entry.clone();
                let requirements = entry
                    .required_env
                    .iter()
                    .map(|field| field.name.clone())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let endpoint = entry
                    .url
                    .clone()
                    .or_else(|| entry.command.clone())
                    .unwrap_or_else(|| "(no endpoint)".into());
                let mut row =
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(bg_elev()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .text_color(rgb(text()))
                                                .child(SharedString::from(entry.name.clone())),
                                        )
                                        .child(
                                            div()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_full()
                                                .bg(rgb(surface()))
                                                .text_xs()
                                                .text_color(rgb(text_dim()))
                                                .child(SharedString::from(format!(
                                                    "{} · auth {}",
                                                    entry.transport, entry.auth_type
                                                ))),
                                        )
                                        .when(entry.needs_install, |this| {
                                            this.child(
                                                div()
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded_full()
                                                    .bg(rgb(accent_soft()))
                                                    .text_xs()
                                                    .text_color(rgb(accent()))
                                                    .child("clone + build"),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(text_muted()))
                                        .child(SharedString::from(entry.description.clone())),
                                )
                                .child(div().text_xs().text_color(rgb(text_dim())).child(
                                    SharedString::from(crate::transcript::clip(&endpoint, 120)),
                                ))
                                .when(!requirements.is_empty(), |this| {
                                    this.child(div().text_xs().text_color(rgb(text_dim())).child(
                                        SharedString::from(format!("requires: {requirements}")),
                                    ))
                                })
                                .when(!entry.post_install.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_dim()))
                                            .child(SharedString::from(entry.post_install.clone())),
                                    )
                                }),
                        );
                if entry.installed {
                    row = row.child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(if entry.enabled {
                                accent()
                            } else {
                                text_muted()
                            }))
                            .child(if entry.enabled {
                                "Installed · On"
                            } else {
                                "Installed · Off"
                            }),
                    );
                } else {
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("mcp-install-{}", entry.name)))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(accent_hi())))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.install_catalog(&install, cx)
                            }))
                            .child(if entry.needs_install {
                                "Install"
                            } else {
                                "Add"
                            }),
                    );
                }
                list = list.child(row);
            }
            if !catalog.diagnostics.is_empty() {
                list = list.child(self.section_label("CATALOG DIAGNOSTICS"));
                for diagnostic in &catalog.diagnostics {
                    list = list.child(self.note(&format!(
                        "{}: {} — {}",
                        diagnostic.name, diagnostic.kind, diagnostic.message
                    )));
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.mcp_tabs(cx))
            .when_some(self.mcp.status.clone(), |this, status| {
                this.child(
                    div()
                        .px_5()
                        .py_2()
                        .bg(rgb(accent_soft()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(status)),
                )
            })
            .child(list)
            .into_any_element()
    }

    fn mcp_install_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(install) = self.mcp.install.clone() else {
            return div().into_any_element();
        };
        let mut body = div()
            .id("mcp-install-form")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(format!("Install {}", install.name))),
            )
            .child(div().text_xs().text_color(rgb(text_dim())).child(
                "These values are written to the profile .env; they never enter config.yaml.",
            ));
        for (index, field) in install.env.iter().enumerate() {
            let mut input = div()
                .id(SharedString::from(format!("mcp-install-env-{index}")))
                .flex()
                .items_center()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(bg()))
                .border_1()
                .text_xs()
                .cursor_pointer()
                .when(install.focus == index, |this| {
                    this.border_color(rgb(accent()))
                })
                .when(install.focus != index, |this| {
                    this.border_color(rgb(border()))
                })
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if let Some(install) = this.mcp.install.as_mut() {
                        install.focus = index;
                    }
                    cx.notify();
                }));
            if field.value.is_empty() {
                input = input.child(
                    div()
                        .text_color(rgb(text_dim()))
                        .child(SharedString::from(field.prompt.clone())),
                );
            } else {
                input = input.child(
                    div()
                        .text_color(rgb(text()))
                        .child(SharedString::from(field.value.clone())),
                );
            }
            body =
                body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().text_color(rgb(text_dim())).child(
                            SharedString::from(format!(
                                "{}{}",
                                field.key,
                                if field.required { " (required)" } else { "" }
                            )),
                        ))
                        .child(input),
                );
        }
        body = body
            .when_some(install.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(surface_hi()))
                        .text_xs()
                        .text_color(rgb(accent()))
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("mcp-install-save")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(accent()))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(bg()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.submit_catalog_install(cx)
                            }))
                            .child("Install"),
                    )
                    .child(
                        div()
                            .id("mcp-install-cancel")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(surface()))
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.mcp.install = None;
                                this.mcp.status = None;
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.mcp_tabs(cx))
            .child(body)
            .into_any_element()
    }
}

impl Render for HermesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modal = self.settings.open
            || self.sessions.open
            || self.rail_search
            || self.inspector.focus.is_some()
            || self.palette.open;
        let target = if modal {
            self.focus.clone()
        } else {
            self.composer_input.focus_handle(cx)
        };
        self.focused = target.is_focused(window);
        self.window_active = window.is_window_active();
        if !self.focused {
            window.focus(&target, cx);
        }

        let palette = if self.palette.open {
            Some(self.palette_overlay(cx).into_any_element())
        } else if self.mention.open && !self.mention_items().is_empty() {
            Some(self.mention_overlay(cx).into_any_element())
        } else {
            None
        };
        let cards = if self.cards.is_empty() {
            None
        } else {
            Some(self.cards_section(cx).into_any_element())
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.focus)
            .bg(rgb(bg()))
            .text_color(rgb(text()))
            .capture_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_key(event, window, cx)
                }),
            )
            .child(self.header(cx))
            // Rail, conversation and inspector share one full-height row; the
            // composer lives inside the centre column so it can never clip the
            // side panes (Grok's all-vertical split).
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(if self.sessions.open {
                        self.sessions_rail(cx).into_any_element()
                    } else {
                        self.sidebar(cx).into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.chat(cx))
                            .when_some(cards, |this, cards| this.child(cards))
                            .child(self.composer(cx)),
                    )
                    .when(self.inspector.open, |this| {
                        this.child(self.inspector_panel(cx))
                    }),
            )
            .child(self.status_bar())
            .when_some(palette, |this, palette| this.child(palette))
            .when(self.settings.open, |this| {
                this.child(self.settings_overlay(cx))
            })
    }
}
