mod app;
mod backend;
mod transcript;
mod tray;

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use hermes_api::HostConfig;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermes_desktop=info,hermes_api=info,hermes_rpc=info".into()),
        )
        .init();

    let host = HostConfig::from_env();

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Hermes".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(options, |_window, cx| {
            cx.new(|cx| app::HermesApp::new(host.clone(), cx))
        })
        .expect("open Hermes window");

        cx.activate(true);
    });
}
