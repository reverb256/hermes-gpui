//! The Hermes window: bot sidebar, streaming transcript, approval and clarify
//! cards, command palette, @-mentions and composer.
//!
//! Interaction design mirrors Grok Bot's: no modal dialogs, everything reachable
//! from the keyboard, and blocking questions show up as cards you answer.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Context, FocusHandle, Focusable, KeyDownEvent, Render, ScrollHandle, SharedString, Window, div,
    px, rgb,
};
use hermes_api::{ActiveProfile, HostConfig, Profile};
use hermes_rpc::ConnectionState;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::backend::{
    ApprovalChoice, ApprovalRequest, Backend, BackendEvent, ClarifyRequest, Command,
};
use crate::inspector::{self, Field, Inspector, Tab};
use crate::keys::{self, Action, Chord, Focus};
use crate::theme::*;
use crate::transcript::{Line, Role, append_streaming, clip};
use crate::tray::{self, TrayStatus};

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
    palette: Palette,
    mention: Mention,
    cards: Vec<Card>,
    attachments: Vec<String>,
    connection: ConnectionState,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
    focused: bool,
    scroll: ScrollHandle,
    tray: TrayStatus,
    tray_state: (bool, String),
    inspector: Inspector,
    rail_search: bool,
    rail_filter: String,
}

impl Focusable for HermesApp {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HermesApp {
    pub fn new(host: HostConfig, cx: &mut Context<Self>) -> Self {
        let (backend, events) = Backend::spawn(host.clone());
        let tray = tray::spawn();

        let mut app = Self {
            host,
            backend,
            profiles: Vec::new(),
            active: None,
            routines: Vec::new(),
            session_id: None,
            lines: Vec::new(),
            composer: String::new(),
            palette: Palette::default(),
            mention: Mention::default(),
            cards: Vec::new(),
            attachments: Vec::new(),
            connection: ConnectionState::Connecting,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
            focused: false,
            scroll: ScrollHandle::new(),
            tray,
            tray_state: (false, String::new()),
            inspector: Inspector::default(),
            rail_search: false,
            rail_filter: String::new(),
        };

        app.pump(events, cx);
        app.pump_tray(cx);
        // QA hook: open the inspector (optionally on a given tab) at launch so
        // a screenshot run can review the panel without input automation.
        if std::env::var("HERMES_OPEN_INSPECTOR").is_ok() {
            app.inspector.tab = match std::env::var("HERMES_INSPECTOR_TAB").ok().as_deref() {
                Some("connectors") => Tab::Connectors,
                Some("secrets") => Tab::Secrets,
                _ => Tab::Look,
            };
            app.toggle_inspector(cx);
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
                if this.update(cx, |app, cx| app.sync_tray(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
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
            BackendEvent::MessageStart => self.busy = true,
            BackendEvent::MessageComplete => self.busy = false,
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
                self.cards.push(Card {
                    id: request.id.clone(),
                    kind: CardKind::Approval(request),
                });
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
            BackendEvent::InspectorStatus(text) => self.inspector.status = Some(text),
            BackendEvent::Attached { name } => {
                self.attachments.push(name.clone());
                self.lines
                    .push(Line::new(Role::System, format!("attached {name}")));
                self.scroll.scroll_to_bottom();
            }
        }
        self.sync_tray(cx);
        cx.notify();
    }

    // ---- keyboard -------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.rail_search && self.route_rail_typing(event, cx) {
            return;
        }
        if self.route_inspector_typing(event, cx) {
            return;
        }
        let keystroke = &event.keystroke;
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
        self.mention = Mention::default();
        cx.notify();
    }

    // ---- actions --------------------------------------------------------

    fn submit(&mut self, cx: &mut Context<Self>) {
        let prompt = self.composer.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.composer.clear();
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
        self.lines.clear();
        self.cards.clear();
        self.attachments.clear();
        self.busy = false;
        self.palette = Palette::default();
        self.mention = Mention::default();
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
        if let Some(card) = self.cards.iter().find(|card| card.id == id) {
            if let CardKind::Approval(request) = &card.kind {
                self.backend.send(Command::Approve {
                    request: request.clone(),
                    choice,
                });
            }
        }
        self.resolve_card(id, cx);
    }

    fn answer(&mut self, id: &str, answer: String, cx: &mut Context<Self>) {
        if let Some(card) = self.cards.iter().find(|card| card.id == id) {
            if let CardKind::Clarify(request) = &card.kind {
                self.backend.send(Command::Answer {
                    request: request.clone(),
                    answer,
                });
            }
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
        cx.notify();
    }

    // ---- rendering ------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, color) = match self.connection {
            ConnectionState::Open => ("connected", USER),
            ConnectionState::Connecting => ("connecting", WARN),
            ConnectionState::Closed => ("offline", DANGER),
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
            .bg(rgb(BG_ELEV))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().text_color(rgb(ACCENT)).child("◆"))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(TEXT))
                            .child("Hermes"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_DIM))
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
                    .bg(rgb(SURFACE))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(SURFACE_HI)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.refresh(cx)))
                    .child(div().text_xs().text_color(rgb(color)).child("●"))
                    .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(label)),
            )
            .when(!model.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(SURFACE))
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
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
                        .bg(rgb(ACCENT_SOFT))
                        .child(div().text_xs().text_color(rgb(ACCENT)).child("◐"))
                        .child(div().text_xs().text_color(rgb(ACCENT)).child("working")),
                )
            })
            .when(!self.cards.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(WARN_SOFT))
                        .text_xs()
                        .text_color(rgb(WARN))
                        .child(format!("{} awaiting you", self.cards.len())),
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
                        this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                    })
                    .when(!self.inspector.open, |this| {
                        this.text_color(rgb(TEXT_MUTED))
                            .hover(|this| this.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
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
                    .text_color(rgb(TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.pick_attachment(cx)))
                    .child("＋ Attach"),
            )
            .child(
                div()
                    .id("palette-hint")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.dispatch(Action::OpenPalette, cx)
                    }))
                    .child("⌘K"),
            )
            .child(
                div()
                    .id("new-chat")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(SURFACE_HI))
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(BORDER_HI)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.new_chat(cx)))
                    .child("New chat"),
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
            .bg(rgb(BG_ELEV))
            .border_r_1()
            .border_color(rgb(BORDER))
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
                            .text_color(rgb(TEXT_DIM))
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
                                    .bg(rgb(SURFACE))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(rgb(SURFACE_HI)))
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.rail_search = !this.rail_search;
                                        if !this.rail_search {
                                            this.rail_filter.clear();
                                        }
                                        cx.notify();
                                    }))
                                    .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child("⌕")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_DIM))
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
                        .bg(rgb(SURFACE))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .text_xs()
                        .when(self.rail_filter.is_empty(), |this| {
                            this.text_color(rgb(TEXT_DIM)).child("Search bots…")
                        })
                        .when(!self.rail_filter.is_empty(), |this| {
                            this.text_color(rgb(TEXT))
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
                        let (glyph, accent) = inspector::look_from_meta(profile.ui_meta.as_ref());
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
                            .when(selected, |this| this.bg(rgb(SURFACE)))
                            .when(!selected, |this| this.hover(|this| this.bg(rgb(SURFACE))))
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
                                    .bg(rgb(ACCENT_SOFT))
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
                                                        this.text_color(rgb(ACCENT))
                                                    })
                                                    .when(!selected, |this| {
                                                        this.text_color(rgb(TEXT))
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
                                                this.text_color(rgb(TEXT_MUTED))
                                            })
                                            .when(!has_history, |this| {
                                                this.text_color(rgb(TEXT_DIM))
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
                        .border_color(rgb(BORDER))
                        .mt_2()
                        .px_4()
                        .pt_2()
                        .text_xs()
                        .text_color(rgb(TEXT_DIM))
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
                        .bg(rgb(DANGER_SOFT))
                        .text_xs()
                        .text_color(rgb(DANGER))
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
                    .border_color(rgb(BORDER))
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
                                    .bg(rgb(SURFACE_HI))
                                    .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child("◍")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .min_w_0()
                                    .child(div().text_xs().text_color(rgb(TEXT)).child("You"))
                                    .child(
                                        div().text_xs().text_color(rgb(TEXT_DIM)).child(
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
                            .bg(rgb(SURFACE))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(SURFACE_HI)))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.toggle_inspector(cx)),
                            )
                            .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child("⊞")),
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
                            .bg(rgb(ACCENT_SOFT))
                            .child(div().text_lg().text_color(rgb(ACCENT)).child("◆")),
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
                                    .text_color(rgb(TEXT))
                                    .child("What should we get done?"),
                            )
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(
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
                                .border_color(rgb(BORDER))
                                .bg(rgb(SURFACE))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(SURFACE_HI)).border_color(rgb(BORDER_HI)))
                                .on_click(cx.listener(move |this, _event, _window, cx| {
                                    this.use_suggestion(&text.clone(), cx)
                                }))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(TEXT))
                                        .child(SharedString::from(*suggestion)),
                                )
                                .child(div().text_xs().text_color(rgb(TEXT_DIM)).child("↵"))
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(TEXT_DIM))
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
                .h_full()
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
            .h_full()
            .overflow_y_scroll()
            .bg(rgb(BG))
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
                    .children(self.lines.iter().map(|line| self.render_line(line))),
            )
            .into_any_element()
    }

    fn render_line(&self, line: &Line) -> gpui::AnyElement {
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
                        .bg(rgb(USER))
                        .text_sm()
                        .text_color(rgb(BG))
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
            Role::Assistant => {
                let (bot_glyph, _) = inspector::look_from_meta(
                    self.current_profile()
                        .and_then(|profile| profile.ui_meta.as_ref()),
                );
                let bot_name = self
                    .current_profile()
                    .map(|profile| profile.label().to_string())
                    .unwrap_or_else(|| self.profile_name());
                let mut body = div().flex().flex_col().gap_1();
                for part in text.split('\n') {
                    body = body.child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .child(SharedString::from(part.to_string())),
                    );
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
                                    .bg(rgb(ACCENT_SOFT))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(ACCENT))
                                            .child(SharedString::from(bot_glyph)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(SharedString::from(format!("Messaged {bot_name}"))),
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
                            .bg(rgb(SURFACE))
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
                        .text_color(rgb(REASONING))
                        .child("THINKING"),
                )
                .child(
                    div()
                        .pl_3()
                        .border_l_2()
                        .border_color(rgb(REASONING_SOFT))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(text.split('\n').map(|part| {
                            div()
                                .text_xs()
                                .text_color(rgb(REASONING))
                                .child(SharedString::from(part.to_string()))
                        })),
                )
                .into_any_element(),
            Role::System => div()
                .flex()
                .items_start()
                .gap_2()
                .w_full()
                .child(div().text_xs().text_color(rgb(TEXT_DIM)).child("·"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(rgb(TEXT_DIM))
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
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
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
                            .bg(rgb(WARN_SOFT))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(WARN))
                            .child("APPROVAL"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_DIM))
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
                    .bg(rgb(BG))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .child(SharedString::from(body)),
            )
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
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
                            .bg(rgb(ACCENT))
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(BG))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(ACCENT_HI)))
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
                                .bg(rgb(SURFACE_HI))
                                .text_xs()
                                .text_color(rgb(USER))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(BORDER_HI)))
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
                            .bg(rgb(SURFACE_HI))
                            .text_xs()
                            .text_color(rgb(DANGER))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(DANGER_SOFT)))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.approve(&id_deny, ApprovalChoice::Deny, cx)
                            }))
                            .child("Deny"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_DIM))
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
                    .bg(rgb(BG))
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(SURFACE_HI)).text_color(rgb(ACCENT)))
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
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(ACCENT_SOFT))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(ACCENT))
                    .child("QUESTION"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT))
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
                                .bg(rgb(ACCENT))
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(BG))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(ACCENT_HI)))
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
                            .bg(rgb(SURFACE_HI))
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(BORDER_HI)).text_color(rgb(TEXT)))
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
                                .text_color(rgb(TEXT_DIM))
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
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER_HI))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(div().text_sm().text_color(rgb(ACCENT)).child("›"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .when(self.palette.query.is_empty(), |this| {
                                this.text_color(rgb(TEXT_DIM))
                            })
                            .when(!self.palette.query.is_empty(), |this| {
                                this.text_color(rgb(TEXT))
                            })
                            .child(SharedString::from(if self.palette.query.is_empty() {
                                "Type a command…".to_string()
                            } else {
                                self.palette.query.clone()
                            })),
                    )
                    .child(div().text_xs().text_color(rgb(TEXT_DIM)).child("esc")),
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
                            .when(index == selected, |this| this.bg(rgb(ACCENT_SOFT)))
                            .when(index != selected, |this| {
                                this.hover(|this| this.bg(rgb(SURFACE_HI)))
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
                                    .when(index == selected, |this| this.text_color(rgb(ACCENT)))
                                    .when(index != selected, |this| this.text_color(rgb(TEXT)))
                                    .child(SharedString::from(item.label)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_DIM))
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
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER_HI))
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
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(REASONING))
                            .child(SharedString::from(format!("@{}", self.mention.query))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_DIM))
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
                                this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                            })
                            .when(index != selected, |this| {
                                this.text_color(rgb(TEXT))
                                    .hover(|this| this.bg(rgb(SURFACE_HI)))
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
            .text_color(rgb(TEXT_DIM))
            .child(SharedString::from(text.to_string()))
    }

    fn note(&self, text: &str) -> gpui::Div {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(SURFACE))
            .text_xs()
            .text_color(rgb(TEXT_DIM))
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
                    .text_color(rgb(TEXT_DIM))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
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
            .bg(rgb(BG_ELEV))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(HEADER_H))
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(TEXT_DIM))
                            .child("BOT INSPECTOR"),
                    )
                    .child(
                        div()
                            .id("close-inspector")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
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
                    .border_color(rgb(BORDER))
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
                                this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                            })
                            .when(!selected, |this| {
                                this.text_color(rgb(TEXT_MUTED))
                                    .hover(|this| this.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
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
                        .bg(rgb(SURFACE))
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
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
            .bg(rgb(SURFACE_HI))
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
                                .text_color(rgb(TEXT))
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
                                        .bg(rgb(SURFACE))
                                        .text_xs()
                                        .text_color(rgb(TEXT_MUTED))
                                        .cursor_pointer()
                                        .hover(|this| {
                                            this.bg(rgb(SURFACE_HI)).text_color(rgb(TEXT))
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
                                            .text_color(rgb(DANGER))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(rgb(DANGER_SOFT)))
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
                            .bg(rgb(SURFACE))
                            .text_sm()
                            .when(selected, |this| {
                                this.border_1()
                                    .border_color(rgb(accent))
                                    .text_color(rgb(accent))
                            })
                            .when(!selected, |this| this.text_color(rgb(TEXT_MUTED)))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(SURFACE_HI)))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.inspector.glyph = value.clone();
                                cx.notify();
                            }))
                            .child(*candidate)
                    })),
            )
            .child(self.section_label("ACCENT"))
            .child(div().flex().items_center().gap_2().children(
                inspector::ACCENT_CHOICES.iter().map(|choice| {
                    let selected = *choice == accent;
                    let value = *choice;
                    div()
                        .id(SharedString::from(format!("accent-{value:06x}")))
                        .w(px(22.))
                        .h(px(22.))
                        .rounded_full()
                        .bg(rgb(value))
                        .when(selected, |this| this.border_2().border_color(rgb(TEXT)))
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
                    .bg(rgb(ACCENT))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(BG))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(ACCENT_HI)))
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
                    .bg(rgb(SURFACE))
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
                                    .text_color(rgb(TEXT))
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_DIM))
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
                                this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                            })
                            .when(!enabled, |this| {
                                this.bg(rgb(SURFACE_HI)).text_color(rgb(TEXT_DIM))
                            })
                            .hover(|this| this.bg(rgb(BORDER_HI)))
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
                    .bg(rgb(SURFACE))
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
                                    .text_color(rgb(TEXT))
                                    .child(SharedString::from(label)),
                            )
                            .child(div().text_xs().text_color(rgb(TEXT_DIM)).child(
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
                                this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                            })
                            .when(!enabled, |this| {
                                this.bg(rgb(SURFACE_HI)).text_color(rgb(TEXT_DIM))
                            })
                            .hover(|this| this.bg(rgb(BORDER_HI)))
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
                    .bg(rgb(SURFACE))
                    .border_1()
                    .when(focused, |this| this.border_color(rgb(ACCENT)))
                    .when(!focused, |this| this.border_color(rgb(BORDER)))
                    .text_xs()
                    .cursor_pointer()
                    .when(value.is_empty(), |this| {
                        this.text_color(rgb(TEXT_DIM))
                            .child(SharedString::from(placeholder.to_string()))
                    })
                    .when(!value.is_empty(), |this| {
                        this.text_color(rgb(TEXT))
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
                    .bg(rgb(SURFACE))
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
                                    .text_color(rgb(TEXT))
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_DIM))
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
                                .text_color(rgb(DANGER))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(DANGER_SOFT)))
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
                            .id("show-all-secrets")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(self.inspector.show_all, |this| {
                                this.bg(rgb(ACCENT_SOFT)).text_color(rgb(ACCENT))
                            })
                            .when(!self.inspector.show_all, |this| {
                                this.text_color(rgb(TEXT_MUTED))
                                    .hover(|this| this.bg(rgb(SURFACE)))
                            })
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.inspector.show_all = !this.inspector.show_all;
                                cx.notify();
                            }))
                            .child("Show all"),
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
                    .bg(rgb(ACCENT))
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(BG))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(ACCENT_HI)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.add_secret(cx)))
                    .child("Save secret"),
            )
            .into_any_element()
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.composer.is_empty();
        let focused = self.focused;

        let mut input = div().flex().flex_col().gap_1().flex_1().min_w_0();
        if empty {
            input = input.child(div().text_sm().text_color(rgb(TEXT_DIM)).child(
                SharedString::from(format!(
                        "Message {}",
                        self.current_profile()
                            .map(|profile| profile.label().to_string())
                            .unwrap_or_else(|| self.profile_name())
                    )),
            ));
        } else {
            for part in self.composer.split('\n') {
                input = input.child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(SharedString::from(part.to_string())),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb_4()
            .bg(rgb(BG))
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
                    .bg(rgb(SURFACE))
                    .border_1()
                    .when(focused, |this| this.border_color(rgb(ACCENT)))
                    .when(!focused, |this| this.border_color(rgb(BORDER)))
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
                            .bg(rgb(SURFACE_HI))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(BORDER_HI)))
                            .on_click(
                                cx.listener(|this, _event, _window, cx| this.pick_attachment(cx)),
                            )
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child("＋")),
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
                                this.bg(rgb(SURFACE_HI)).text_color(rgb(TEXT_DIM))
                            })
                            .when(!empty, |this| {
                                this.bg(rgb(ACCENT)).hover(|this| this.bg(rgb(ACCENT_HI)))
                            })
                            .on_click(cx.listener(|this, _event, _window, cx| this.submit(cx)))
                            .child(
                                div()
                                    .text_sm()
                                    .when(empty, |this| this.text_color(rgb(TEXT_DIM)))
                                    .when(!empty, |this| this.text_color(rgb(BG)))
                                    .child("↑"),
                            ),
                    ),
            )
    }
}

impl Render for HermesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focused = self.focus.is_focused(window);
        if !self.focused {
            window.focus(&self.focus, cx);
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
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _window, cx| this.on_key(event, cx)),
            )
            .child(self.header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.sidebar(cx))
                    .child(self.chat(cx))
                    .when(self.inspector.open, |this| {
                        this.child(self.inspector_panel(cx))
                    }),
            )
            .when_some(cards, |this, cards| this.child(cards))
            .when_some(palette, |this, palette| this.child(palette))
            .child(self.composer(cx))
    }
}
