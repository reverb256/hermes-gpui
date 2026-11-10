mod app;
mod backend;
mod computers;
mod cron;
mod hub_client;
mod inspector;
mod ipc;
mod kanban;
mod keys;
mod mcp;
mod memory;
mod moa;
mod models;
mod oauth;
mod omarchy;
mod sessions;
mod settings;
mod skills;
mod status;
mod theme;
mod transcript;
mod tray;
mod usage;
mod vault;
mod voice;

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_ce_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};
use hermes_api::HostConfig;

fn main() {
    // The hub device socket uses wss://; install a provider before rustls is
    // asked to infer one from a feature graph that can contain both providers.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermes_desktop=info,hermes_api=info,hermes_rpc=info".into()),
        )
        .init();

    let host = HostConfig::from_env();
    let args: Vec<String> = std::env::args().collect();

    // Read-only snapshot for the Omarchy bar widget.
    if args.iter().any(|arg| arg == "--status-json")
        || (args.get(1).is_some_and(|arg| arg == "status")
            && args.iter().any(|arg| arg == "--json"))
    {
        println!("{}", status::read_json());
        return;
    }

    // Install the `theme-set` hook that repaints without waiting for a poll.
    if args.iter().any(|arg| arg == "--install-omarchy-hooks") {
        match omarchy::install_hooks() {
            Ok(path) => println!("installed {}", path.display()),
            Err(message) => {
                eprintln!("cannot install Omarchy hooks: {message}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Local facts for support requests; no host contact.
    if args.iter().any(|arg| arg == "--diagnostics") {
        println!("{}", omarchy::diagnostics());
        return;
    }

    // Headless verification: REST, WS handshake, session creation, then exit.
    if std::env::args().any(|arg| arg == "--probe") {
        let report = tokio::runtime::Runtime::new()
            .expect("build tokio runtime")
            .block_on(backend::probe(&host));
        let mut failed = false;
        for line in &report {
            println!("{line}");
            failed |= line.contains("FAILED");
        }
        std::process::exit(i32::from(failed));
    }

    let toggle = std::env::args().any(|arg| arg == "--toggle");
    let listener = match ipc::claim() {
        ipc::Claim::Primary(listener) => Some(listener),
        ipc::Claim::Secondary => {
            let request = if toggle {
                ipc::Request::Toggle
            } else {
                ipc::Request::Show
            };
            if !ipc::forward(request) {
                tracing::error!("another Hermes instance owns the socket but did not answer");
                std::process::exit(1);
            }
            return;
        }
        ipc::Claim::Unavailable => None,
    };

    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
        let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Hermes".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            // Close hides to the tray instead of quitting, so a running turn
            // keeps streaming; Quit lives on the tray menu and in the palette.
            window.on_window_should_close(cx, |window, _cx| {
                window.minimize_window();
                false
            });
            cx.new(|cx| app::HermesApp::new(host.clone(), listener, cx))
        })
        .expect("open Hermes window");

        cx.activate(true);
    });
}
