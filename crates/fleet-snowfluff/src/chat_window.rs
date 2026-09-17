//! Opens (or focuses) the global chat window (`ai-chat`'s "Global chat
//! window" -- one instance regardless of pet instance count).
//! Deliberately reuses `settings_window.rs`'s exact singleton pattern
//! and, per that module's own warning about `WebviewUrl::App` only
//! being reliable for the literal string `"index.html"`, loads that
//! same entry point rather than a separate HTML file -- the frontend
//! branches on the window's own label (`getCurrentWindow().label`) to
//! decide whether to render the settings UI or the chat UI.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const CHAT_WINDOW_LABEL: &str = "chat";

pub fn open_or_focus_chat(app: &AppHandle, title: &str) {
    if let Some(window) = app.get_webview_window(CHAT_WINDOW_LABEL) {
        window.show().ok();
        window.set_focus().ok();
        return;
    }

    let result =
        WebviewWindowBuilder::new(app, CHAT_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(420.0, 560.0)
            .resizable(true)
            .build();

    if let Err(err) = result {
        log::error!("failed to open chat window: {err}");
    }
}
