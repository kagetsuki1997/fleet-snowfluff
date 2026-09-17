//! Opens (or focuses) the global chat window (`ai-chat`'s "Global chat
//! window" -- one instance regardless of pet instance count).
//! Deliberately reuses `settings_window.rs`'s exact singleton pattern
//! and, per that module's own warning about `WebviewUrl::App` only
//! being reliable for the literal string `"index.html"`, loads that
//! same entry point rather than a separate HTML file -- the frontend
//! branches on the window's own label (`getCurrentWindow().label`) to
//! decide whether to render the settings UI or the chat UI.

use fleet_snowfluff_core::ForeignWindowRect;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::{chat_commands::ChatRuntimeState, chat_pause, status_bubble};

pub const CHAT_WINDOW_LABEL: &str = "chat";

/// The chat window's own rect in logical pixels, if it's currently
/// open -- used so a paused pet can dock against the chat window
/// itself the same way it already docks against whichever foreign app
/// window is in the foreground (`ai-chat`'s "Pause docks to the chat
/// window"). The platform `foreground_window()` queries this project
/// already has deliberately exclude every window owned by this app's
/// own process (including this one), so docking to it needs this
/// separate, explicit path rather than relying on those.
pub fn logical_rect(app: &AppHandle) -> Option<ForeignWindowRect> {
    let window = app.get_webview_window(CHAT_WINDOW_LABEL)?;
    let scale = window.scale_factor().ok()?;
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let left = position.x as f64 / scale;
    let top = position.y as f64 / scale;
    Some(ForeignWindowRect {
        left,
        top,
        right: left + size.width as f64 / scale,
        bottom: top + size.height as f64 / scale,
    })
}

/// Brings the chat window to the front if it's open but not the
/// focused window -- any click on a pet should do this while the chat
/// window is up, not just the double-click that opens it fresh
/// (`ai-chat`'s chat window is meant to stay reachable at a glance
/// while pets keep receiving clicks around it). A no-op if the window
/// isn't open at all, or is already focused.
pub fn focus_if_unfocused(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CHAT_WINDOW_LABEL) {
        if !window.is_focused().unwrap_or(true) {
            window.set_focus().ok();
        }
    }
}

pub fn open_or_focus_chat(app: &AppHandle, title: &str) {
    if let Some(window) = app.get_webview_window(CHAT_WINDOW_LABEL) {
        window.show().ok();
        window.set_focus().ok();
        return;
    }

    let result = WebviewWindowBuilder::new(app, CHAT_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(420.0, 560.0)
            .resizable(true)
            // Lets the personalization opacity setting fade the whole
            // window (ai-chat's "Chat window opacity") -- CSS opacity
            // on the page content only shows the desktop through it if
            // the window itself allows per-pixel alpha.
            .transparent(true)
            .build();

    match result {
        Ok(window) => {
            let app_handle = app.clone();
            window.on_window_event(move |event| match event {
                // The window closing lifts the pause/hides the bubble
                // unless a still-pending generation (or an unread
                // result) is keeping them up regardless -- `recompute`/
                // `sync` read that state themselves rather than
                // assuming closing always means "done".
                WindowEvent::Destroyed => {
                    chat_pause::recompute(&app_handle);
                    status_bubble::sync(&app_handle);
                }
                // Regaining focus is what clears an unread indicator
                // (`ai-chat`'s "Status bubble" -- the unread state is
                // specifically about *not yet seen*, not merely "chat
                // window exists"), including the case where a reply
                // arrived while this window sat open but unfocused.
                WindowEvent::Focused(true) => {
                    let chat_state = app_handle.state::<ChatRuntimeState>();
                    if chat_state.unread().is_some() {
                        chat_state.clear_unread();
                        chat_pause::recompute(&app_handle);
                        status_bubble::sync(&app_handle);
                    }
                }
                _ => {}
            });
            chat_pause::recompute(app);
            status_bubble::sync(app);
        }
        Err(err) => log::error!("failed to open chat window: {err}"),
    }
}
