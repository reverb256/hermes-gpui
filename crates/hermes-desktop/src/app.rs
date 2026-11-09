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
use crate::keys::{self, Action, Chord, Focus};
use crate::transcript::{Line, Role, append_streaming};
use crate::tray::{self, TrayStatus};

const BG: u32 = 0x0f1014;
const PANEL: u32 = 0x16181f;
const BORDER: u32 = 0x24262f;
const TEXT: u32 = 0xe4e6ee;
const MUTED: u32 = 0x8b90a0;
const ACCENT: u32 = 0x7aa2f7;
const USER: u32 = 0x9ece6a;
const REASONING: u32 = 0xbb9af7;
const WARN: u32 = 0xe0af68;
const DANGER: u32 = 0xf7768e;

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
        };

        app.pump(events, cx);
        app.pump_tray(cx);
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
        self.lines
            .push(Line::new(Role::System, format!("Switched to {name}")));
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
            .px_4()
            .py_2()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("HERMES"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(SharedString::from(self.host.base_url.clone())),
            )
            .child(
                div()
                    .id("connection")
                    .text_xs()
                    .text_color(rgb(color))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _event, _window, cx| this.refresh(cx)))
                    .child(SharedString::from(format!("● {label}"))),
            )
            .when(!model.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(SharedString::from(model)),
                )
            })
            .when(self.busy, |this| {
                this.child(div().text_xs().text_color(rgb(WARN)).child("working…"))
            })
            .when(!self.cards.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(WARN))
                        .child(format!("{} waiting on you", self.cards.len())),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("attach")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .hover(|this| this.text_color(rgb(TEXT)))
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
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .hover(|this| this.text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.dispatch(Action::OpenPalette, cx)
                    }))
                    .child("⌘K commands"),
            )
            .child(
                div()
                    .id("new-chat")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(BORDER))
                    .text_xs()
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(0x2d3140)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.new_chat(cx)))
                    .child("＋ New chat"),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.profile_name();

        div()
            .flex()
            .flex_col()
            .w(px(232.))
            .h_full()
            .bg(rgb(PANEL))
            .border_r_1()
            .border_color(rgb(BORDER))
            .py_2()
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("BOTS ({})", self.profiles.len())),
            )
            .child(
                div()
                    .id("profiles")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .children(self.profiles.iter().enumerate().map(|(index, profile)| {
                        let selected = profile.name == current;
                        let name = profile.name.clone();
                        let subtitle = profile
                            .model
                            .clone()
                            .or_else(|| profile.provider.clone())
                            .unwrap_or_default();
                        let meta = format!("{} skills", profile.skill_count);

                        div()
                            .id(SharedString::from(format!("profile-{}", profile.name)))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .px_3()
                            .py_2()
                            .cursor_pointer()
                            .when(selected, |this| this.bg(rgb(BORDER)))
                            .hover(|this| this.bg(rgb(0x1d2029)))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.select_profile(name.clone(), cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                                            .child(SharedString::from(profile.label().to_string())),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(SharedString::from(format!("{}", index + 1))),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(SharedString::from(meta))
                                    .when(!subtitle.is_empty(), |this| {
                                        this.child(SharedString::from(subtitle))
                                    }),
                            )
                    })),
            )
            .when(!self.attachments.is_empty(), |this| {
                this.child(div().px_3().pt_2().text_xs().text_color(rgb(MUTED)).child(
                    SharedString::from(format!("attached: {}", self.attachments.join(", "))),
                ))
            })
            .when_some(self.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .mt_auto()
                        .px_3()
                        .pt_2()
                        .text_xs()
                        .text_color(rgb(DANGER))
                        .child(SharedString::from(error.clone())),
                )
            })
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .p_8()
            .h_full()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("What should we get done?"),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "Talking to {} · Enter sends · @ mentions · Ctrl+K commands",
                self.profile_name()
            )))
            .children(SUGGESTIONS.iter().map(|suggestion| {
                let text = suggestion.to_string();
                div()
                    .id(SharedString::from(format!("suggestion-{suggestion}")))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(PANEL)))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.use_suggestion(&text.clone(), cx)
                    }))
                    .child(SharedString::from(*suggestion))
            }))
    }

    fn chat(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.has_conversation() {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .p_5()
                .children(self.lines.iter().map(|line| self.render_line(line)))
                .into_any_element()
        } else {
            self.empty_state(cx).into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .child(
                div()
                    .id("transcript")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }

    fn render_line(&self, line: &Line) -> gpui::AnyElement {
        let (speaker, color) = match line.role {
            Role::User => ("you", USER),
            Role::Assistant => ("hermes", TEXT),
            Role::Reasoning => ("thinking", REASONING),
            Role::System => ("·", MUTED),
        };

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

        let mut block = div().flex().flex_col().gap_1().w_full();
        for part in text.split('\n') {
            block = block.child(
                div()
                    .text_sm()
                    .text_color(rgb(color))
                    .child(SharedString::from(part.to_string())),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(color))
                    .child(SharedString::from(speaker)),
            )
            .child(block)
            .into_any_element()
    }

    fn cards_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
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
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(WARN))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(WARN))
                    .child("APPROVAL NEEDED"),
            )
            .child(div().text_sm().child(SharedString::from(body)))
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(SharedString::from(detail)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("allow-once-{id}")))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(ACCENT))
                            .text_xs()
                            .text_color(rgb(BG))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(0x8bb0f8)))
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
                                .bg(rgb(BORDER))
                                .text_xs()
                                .text_color(rgb(USER))
                                .cursor_pointer()
                                .hover(|this| this.bg(rgb(0x2d3140)))
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
                            .bg(rgb(BORDER))
                            .text_xs()
                            .text_color(rgb(DANGER))
                            .cursor_pointer()
                            .hover(|this| this.bg(rgb(0x2d3140)))
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.approve(&id_deny, ApprovalChoice::Deny, cx)
                            }))
                            .child("Deny"),
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
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(BORDER)))
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
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(ACCENT))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(ACCENT))
                    .child("QUESTION"),
            )
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(request.question.clone())),
            )
            .child(choices)
            .child(
                div()
                    .flex()
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
                                .text_color(rgb(BG))
                                .cursor_pointer()
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
                            .bg(rgb(BORDER))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                this.dismiss_card(&id_skip, cx)
                            }))
                            .child("Skip"),
                    ),
            )
            .into_any_element()
    }

    fn palette_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.palette_items();
        let selected = self.palette.index.min(items.len().saturating_sub(1));

        div()
            .flex()
            .flex_col()
            .gap_1()
            .mx_4()
            .mb_2()
            .p_2()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(ACCENT))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(rgb(if self.palette.query.is_empty() {
                        MUTED
                    } else {
                        TEXT
                    }))
                    .child(SharedString::from(if self.palette.query.is_empty() {
                        "Type a command…".to_string()
                    } else {
                        self.palette.query.clone()
                    })),
            )
            .children(items.into_iter().enumerate().map(|(index, item)| {
                div()
                    .id(SharedString::from(format!("palette-{index}")))
                    .flex()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(index == selected, |this| this.bg(rgb(BORDER)))
                    .hover(|this| this.bg(rgb(0x1d2029)))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.palette.index = index;
                        this.accept_palette(cx);
                    }))
                    .child(SharedString::from(item.label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(SharedString::from(item.hint)),
                    )
            }))
    }

    fn mention_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.mention_items();
        let selected = self.mention.index.min(items.len().saturating_sub(1));

        div()
            .flex()
            .flex_col()
            .gap_1()
            .mx_4()
            .mb_2()
            .p_2()
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(REASONING))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(SharedString::from(format!(
                        "@{} · Enter to mention",
                        self.mention.query
                    ))),
            )
            .children(items.into_iter().enumerate().map(|(index, item)| {
                div()
                    .id(SharedString::from(format!("mention-{index}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(index == selected, |this| this.bg(rgb(BORDER)))
                    .hover(|this| this.bg(rgb(0x1d2029)))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.mention.index = index;
                        this.accept_mention(cx);
                    }))
                    .child(SharedString::from(format!("@{item}")))
            }))
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.composer.is_empty();
        let focused = self.focused;

        let mut input = div().flex().flex_col().gap_0p5().flex_1();
        if empty {
            input = input.child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("Message Hermes…  ·  Shift+Enter for a new line"),
            );
        } else {
            for part in self.composer.split('\n') {
                input = input.child(div().text_sm().child(SharedString::from(part.to_string())));
            }
        }

        div()
            .flex()
            .items_end()
            .gap_2()
            .px_4()
            .py_3()
            .bg(rgb(PANEL))
            .border_t_1()
            .border_color(rgb(BORDER))
            .when(!focused, |this| this.border_color(rgb(WARN)))
            .child(
                div()
                    .flex_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .text_color(rgb(TEXT))
                    .child(input),
            )
            .child(
                div()
                    .id("send")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(empty, |this| this.bg(rgb(BORDER)).text_color(rgb(MUTED)))
                    .when(!empty, |this| this.bg(rgb(ACCENT)).text_color(rgb(BG)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.submit(cx)))
                    .child("Send ↵"),
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
                    .overflow_hidden()
                    .child(self.sidebar(cx))
                    .child(self.chat(cx)),
            )
            .children(cards)
            .children(palette)
            .child(self.composer(cx))
    }
}
