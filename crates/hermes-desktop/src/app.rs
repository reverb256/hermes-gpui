//! The Hermes window: profile sidebar, streaming transcript, composer.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Context, FocusHandle, Focusable, KeyDownEvent, Render, ScrollHandle, SharedString, Window, div,
    px, rgb,
};
use hermes_api::{ActiveProfile, HostConfig, Profile};
use hermes_rpc::ConnectionState;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::backend::{Backend, BackendEvent, Command};
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

pub struct HermesApp {
    host: HostConfig,
    backend: Backend,
    profiles: Vec<Profile>,
    active: Option<ActiveProfile>,
    session_id: Option<String>,
    lines: Vec<Line>,
    composer: String,
    connection: ConnectionState,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
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
            session_id: None,
            lines: vec![Line::new(Role::System, "Connecting to Hermes…")],
            composer: String::new(),
            connection: ConnectionState::Connecting,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
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

    fn on_backend_event(&mut self, event: BackendEvent, cx: &mut Context<Self>) {
        match event {
            BackendEvent::Connection(state) => {
                self.connection = state.clone();
                if state == ConnectionState::Open {
                    self.lines.clear();
                    self.lines.push(Line::new(
                        Role::System,
                        format!("Connected to {}", self.host.base_url),
                    ));
                } else if state == ConnectionState::Closed {
                    self.busy = false;
                }
            }
            BackendEvent::Profiles(profiles) => self.profiles = profiles,
            BackendEvent::ActiveProfile(active) => self.active = Some(active),
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
            }
            BackendEvent::Reasoning { text } => {
                append_streaming(&mut self.lines, Role::Reasoning, &text)
            }
            BackendEvent::MessageStart => self.busy = true,
            BackendEvent::MessageComplete => self.busy = false,
            BackendEvent::ToolActivity(text) => {
                self.lines.push(Line::new(Role::System, text));
            }
            BackendEvent::Notice(text) => {
                self.lines.push(Line::new(Role::System, text));
            }
            BackendEvent::Error(error) => {
                self.error = Some(error.clone());
                self.lines
                    .push(Line::new(Role::System, format!("⚠ {error}")));
                self.busy = false;
            }
        }
        self.sync_tray(cx);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.control || keystroke.modifiers.platform {
            return;
        }

        match keystroke.key.as_str() {
            "enter" => self.submit(cx),
            "backspace" => {
                self.composer.pop();
                cx.notify();
            }
            "escape" => {
                if self.busy {
                    self.backend.send(Command::Interrupt);
                }
            }
            _ => {
                if let Some(text) = keystroke.key_char.as_ref() {
                    self.composer.push_str(text);
                    cx.notify();
                }
            }
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let prompt = self.composer.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.composer.clear();
        self.lines.push(Line::new(Role::User, prompt.clone()));
        self.lines.push(Line::new(Role::Assistant, String::new()));
        self.busy = true;
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
        self.busy = false;
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, color) = match self.connection {
            ConnectionState::Open => ("connected", USER),
            ConnectionState::Connecting => ("connecting", WARN),
            ConnectionState::Closed => ("offline", 0xf7768e),
        };

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
            .when_some(self.active.as_ref(), |this, active| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(rgb(ACCENT))
                        .child(SharedString::from(format!("◆ {}", active.current))),
                )
            })
            .when(self.busy, |this| {
                this.child(div().text_xs().text_color(rgb(WARN)).child("working…"))
            })
            .child(div().flex_1())
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
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("PROFILES ({})", self.profiles.len())),
            )
            .children(self.profiles.iter().map(|profile| {
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
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                            .child(SharedString::from(profile.label().to_string())),
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
            }))
            .when_some(self.error.as_ref(), |this, error| {
                this.child(
                    div()
                        .mt_auto()
                        .px_3()
                        .pt_2()
                        .text_xs()
                        .text_color(rgb(0xf7768e))
                        .child(SharedString::from(error.clone())),
                )
            })
    }

    fn select_profile(&mut self, name: String, cx: &mut Context<Self>) {
        if self.profile_name() == name {
            return;
        }
        self.host.profile = Some(name.clone());
        self.session_id = None;
        self.backend.send(Command::SwitchProfile(name.clone()));
        self.lines.push(Line::new(
            Role::System,
            format!("Switched to profile {name}"),
        ));
        cx.notify();
    }

    fn chat(&self) -> impl IntoElement {
        let body = if self.lines.is_empty() {
            div()
                .p_6()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("No messages yet. Type below and press Enter.")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .p_5()
                .children(self.lines.iter().map(|line| {
                    let (prefix, color) = match line.role {
                        Role::User => ("you", USER),
                        Role::Assistant => ("hermes", TEXT),
                        Role::Reasoning => ("thinking", REASONING),
                        Role::System => ("·", MUTED),
                    };
                    let text = if line.text.is_empty() {
                        "…".to_string()
                    } else {
                        line.text.clone()
                    };

                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(color))
                                .child(SharedString::from(prefix)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(color))
                                .child(SharedString::from(text)),
                        )
                }))
                .into_any_element()
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

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .bg(rgb(PANEL))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(if self.composer.is_empty() {
                        MUTED
                    } else {
                        TEXT
                    }))
                    .child(SharedString::from(if self.composer.is_empty() {
                        "Message Hermes… (Enter to send, Esc to interrupt)".to_string()
                    } else {
                        self.composer.clone()
                    })),
            )
            .child(
                div()
                    .id("send")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(BORDER))
                    .text_sm()
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(0x2d3140)))
                    .on_click(cx.listener(|this, _event, _window, cx| this.submit(cx)))
                    .child("Send"),
            )
    }
}

impl Render for HermesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        if !focused {
            window.focus(&self.focus, cx);
        }

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
                    .child(self.chat()),
            )
            .child(self.composer(cx))
    }
}
