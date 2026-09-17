//! The small status-bubble webview anchored to `pets[0]`
//! (`ai-chat`'s "Status bubble"): hidden normally, shown while a
//! generation is pending or an unread result exists. A separate tiny
//! window from the chat window, reusing the same `index.html` entry
//! point and branching on window label -- the bubble's own content
//! (`...`, the success/failure glyphs) is rendered entirely client-side
//! by polling `get_chat_state`, the same command the chat window
//! itself uses; this module's only job is *when* and *where* the
//! window exists.

use std::sync::Mutex;

use tauri::{AppHandle, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::{chat_commands::ChatRuntimeState, manager::PetManager};

pub const BUBBLE_WINDOW_LABEL: &str = "status-bubble";
const BUBBLE_WIDTH: f64 = 180.0;
const BUBBLE_HEIGHT: f64 = 36.0;

const BUBBLE_GAP: f64 = 8.0;

/// Beside the pet's right edge, vertically centered on it -- `anchor_w`/
/// `anchor_h` are the pet's own current on-screen size (`PetWindow::size`),
/// not a guess, so this tracks however big the sprite actually is right
/// now rather than a fixed offset that only happened to look right at
/// one scale.
fn bubble_position(
    anchor_x: f64,
    anchor_y: f64,
    anchor_w: u32,
    anchor_h: u32,
) -> LogicalPosition<f64> {
    LogicalPosition::new(
        anchor_x + anchor_w as f64 + BUBBLE_GAP,
        anchor_y + (anchor_h as f64 - BUBBLE_HEIGHT) / 2.0,
    )
}

fn show_at(app: &AppHandle, anchor_x: f64, anchor_y: f64, anchor_w: u32, anchor_h: u32) {
    let position = bubble_position(anchor_x, anchor_y, anchor_w, anchor_h);
    if let Some(window) = app.get_webview_window(BUBBLE_WINDOW_LABEL) {
        window.set_position(position).ok();
        window.show().ok();
        return;
    }

    let result =
        WebviewWindowBuilder::new(app, BUBBLE_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
            .inner_size(BUBBLE_WIDTH, BUBBLE_HEIGHT)
            .position(position.x, position.y)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .transparent(true)
            .resizable(false)
            .focused(false)
            .build();

    if let Err(err) = result {
        log::error!("failed to create status bubble window: {err}");
    }
}

fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(BUBBLE_WINDOW_LABEL) {
        window.hide().ok();
    }
}

/// Shows or hides the bubble to match current chat state -- call
/// whenever "is a generation pending" or "is there an unread result"
/// could have changed (`chat_commands::on_chat_activity_changed`
/// already does this alongside the matching pause recompute).
pub fn sync(app: &AppHandle) {
    let chat_state = app.state::<ChatRuntimeState>();
    let should_show = chat_state.is_pending() || chat_state.unread().is_some();

    if should_show {
        let rect = app.state::<Mutex<PetManager>>().lock().unwrap().primary_pet_rect();
        if let Some((x, y, w, h)) = rect {
            show_at(app, x, y, w, h);
        }
    } else {
        hide(app);
    }
}

/// Repositions the bubble if it currently exists -- called from the
/// pet tick loop's drag-position path (`ai-chat`'s "Status bubble
/// tracks manual drag"), the one case `pets[0]` can move at all while
/// the pause this bubble's visibility implies is in effect (the pet's
/// own wander state machine never runs while paused).
pub fn reposition(app: &AppHandle, anchor_x: f64, anchor_y: f64, anchor_w: u32, anchor_h: u32) {
    if let Some(window) = app.get_webview_window(BUBBLE_WINDOW_LABEL) {
        window.set_position(bubble_position(anchor_x, anchor_y, anchor_w, anchor_h)).ok();
    }
}
