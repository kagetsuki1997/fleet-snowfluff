//! The app side of conversation memory: where the store lives, who it may be
//! shown to, the tools the agent gets, and the after-reply work that saves what
//! the user asked to remember or corrected.
//!
//! Like compaction, this is best effort and off the hot path. A missing or
//! unreadable store, a model that cannot be reached, or the switch being off
//! never fails a chat: it just has no memory that turn.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use fleet_snowfluff_ai::{
    memory::{
        background::{self, ProviderTextModel},
        store::SqliteMemory,
        Audience, MemoryService,
    },
    AiSettings, ProfileKey, ProviderCredentials, Tool,
};
use tauri::{AppHandle, Manager};

use crate::{ai_commands, chat_log_store, memory_commands, persona_store};

/// The one memory database, opened lazily on first use.
pub struct MemoryState {
    store: Arc<SqliteMemory>,
}

/// Registers the store as managed state. Nothing is created on disk until
/// something uses it.
pub fn start(app: &AppHandle) {
    let path = app
        .path()
        .app_data_dir()
        .map(|dir| dir.join("memory").join("memory.db"))
        .unwrap_or_else(|_| std::env::temp_dir().join("fleet-snowfluff-memory.db"));
    app.manage(MemoryState { store: Arc::new(SqliteMemory::at(path)) });
}

/// The store itself, whatever the switch says: the Settings list must keep
/// working while memory is off.
pub fn store(app: &AppHandle) -> Arc<SqliteMemory> { Arc::clone(&app.state::<MemoryState>().store) }

pub fn is_enabled(app: &AppHandle) -> bool {
    app.state::<Mutex<AiSettings>>().lock().unwrap().memory_enabled
}

/// The store for use in a request, or `None` while memory is switched off.
pub fn service_if_enabled(app: &AppHandle) -> Option<Arc<dyn MemoryService>> {
    is_enabled(app).then(|| store(app) as Arc<dyn MemoryService>)
}

pub fn audience_for(profile: ProfileKey) -> Audience { Audience::for_profile(profile) }

/// The memory tools for the top-level agent, or none while memory is off.
pub fn top_level_tools(app: &AppHandle, profile: ProfileKey) -> Vec<Arc<dyn Tool>> {
    let Some(service) = service_if_enabled(app) else { return Vec::new() };
    let persona_name = Some(persona_store::load(app).persona.name);
    fleet_snowfluff_ai::memory::tools::all_tools(service, audience_for(profile), persona_name)
}

/// Only the two read tools, for the mixed-mode local attempt (always the local
/// audience).
pub fn local_read_tools(app: &AppHandle) -> Vec<Arc<dyn Tool>> {
    service_if_enabled(app)
        .map(|service| fleet_snowfluff_ai::memory::tools::read_tools(service, Audience::Local))
        .unwrap_or_default()
}

/// Called after a reply is recorded. Looks for a request to remember something
/// or a correction in the user's message and saves it when the assistant did
/// not already (`tool_saved`). Runs in the background; the user's next message
/// never waits on it.
pub fn reply_done(app: &AppHandle, session_path: &Path, tool_saved: bool) {
    let Some(service) = service_if_enabled(app) else { return };
    // The assistant may have saved something itself during the reply.
    if tool_saved {
        memory_commands::notify_changed(app);
    }
    let Some(user_message) = last_user_message(session_path) else { return };
    if !background::has_remember_cue(&user_message)
        && !background::has_correction_cue(&user_message)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(model) = background_model(&app) else { return };
        let persona = persona_store::load(&app).persona.name;
        let persona = Some(persona.as_str());
        if let Some(outcome) = background::remember_after_reply(
            &model,
            service.as_ref(),
            &user_message,
            tool_saved,
            persona,
        )
        .await
        {
            log::debug!("remembered after the reply: {outcome:?}");
        }
        if let Some(outcome) =
            background::correct_after_reply(&model, service.as_ref(), &user_message, persona).await
        {
            log::debug!("corrected after the reply: {outcome:?}");
        }
        memory_commands::notify_changed(&app);
    });
}

fn last_user_message(session_path: &Path) -> Option<String> {
    chat_log_store::read_session(session_path)
        .into_iter()
        .rev()
        .find(|entry| entry.role == fleet_snowfluff_ai::LogRole::User)
        .map(|entry| entry.content)
}

/// The model that does background memory work: the same choice and the same
/// conditions as for compaction (the local model when enabled, else the default
/// profile; nothing while AI is off or a cloud disclosure is unacknowledged).
pub fn background_model(app: &AppHandle) -> Option<ProviderTextModel> {
    let (settings, credentials) = {
        let settings = app.state::<Mutex<AiSettings>>().lock().unwrap().clone();
        let credentials = app.state::<Mutex<ProviderCredentials>>().lock().unwrap().clone();
        (settings, credentials)
    };
    if !settings.ai_enabled {
        return None;
    }
    let profile = settings.background_profile().cloned()?;
    if !ai_commands::disclosure_ok(&settings, Some(profile.key())) {
        return None;
    }
    let provider = ai_commands::build_provider(
        &credentials,
        &profile,
        None,
        &settings.claude_code_tool_access,
    );
    Some(ProviderTextModel::new(provider))
}
