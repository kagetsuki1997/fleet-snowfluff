//! Ties the chat window's open/pending-generation activity to the
//! existing global pause mechanism (`ai-chat`'s "Pause during chat
//! activity"): pets stay paused for as long as the chat window is
//! open, or a generation is pending, whichever is longer, and whatever
//! pause state existed before that period started is restored once
//! both conditions clear.

use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::{chat_commands::ChatRuntimeState, chat_window, manager::PetManager};

#[derive(Default)]
pub struct ChatPauseState {
    /// `Some(previous_paused_value)` while chat activity is forcing a
    /// pause; `None` when it isn't currently doing so.
    prior_pause: Mutex<Option<bool>>,
}

/// Recomputes whether chat activity should currently be forcing a
/// pause, and applies or reverts it as needed. Call this whenever
/// either condition could have changed: the chat window opens/closes,
/// or a generation starts/stops being pending.
pub fn recompute(app: &AppHandle) {
    let chat_open = app.get_webview_window(chat_window::CHAT_WINDOW_LABEL).is_some();
    let chat_state = app.state::<ChatRuntimeState>();
    // An unread result also keeps pets paused -- otherwise `pets[0]`
    // could wander off between a generation finishing (chat window
    // closed) and the user eventually noticing the bubble, which would
    // reopen exactly the "bubble needs continuous position tracking"
    // problem this design avoids everywhere else.
    let should_force_pause = chat_open || chat_state.is_pending() || chat_state.unread().is_some();

    let pause_state = app.state::<ChatPauseState>();
    let mut prior = pause_state.prior_pause.lock().unwrap();
    let manager_state = app.state::<Mutex<PetManager>>();

    if should_force_pause {
        if prior.is_none() {
            let mut manager = manager_state.lock().unwrap();
            *prior = Some(manager.paused);
            manager.set_paused(true);
        }
    } else if let Some(previous) = prior.take() {
        manager_state.lock().unwrap().set_paused(previous);
    }
}
