//! Settings-UI commands for what the assistant remembers: listing and
//! searching, editing, pinning, marking sensitive, deleting, viewing superseded
//! history, erasing everything, and the on/off switch. They work on the store
//! whether or not memory is switched on, so the list stays reviewable
//! while it is off. Each change emits `memories-changed` so an open list
//! refreshes.

use std::sync::Mutex;

use fleet_snowfluff_ai::{
    memory::{MemoryEdit, MemoryError, MemoryRecord, MemoryService},
    AiSettings,
};
use serde::Deserialize;
use tauri::{AppHandle, Emitter, State};

use crate::{ai_config_store, memory_runtime};

pub const MEMORIES_CHANGED_EVENT: &str = "memories-changed";

/// Tells an open Settings window to reload the list.
pub fn notify_changed(app: &AppHandle) { let _ = app.emit(MEMORIES_CHANGED_EVENT, ()); }

/// The edits the Settings row can make. A missing field is left as it is.
#[derive(Debug, Default, Deserialize)]
pub struct MemoryEditRequest {
    pub text: Option<String>,
    pub tags: Option<Vec<String>>,
    pub importance: Option<u8>,
    pub pinned: Option<bool>,
    pub sensitive: Option<bool>,
}

impl MemoryEditRequest {
    fn into_edit(self) -> MemoryEdit {
        MemoryEdit {
            // Blank text would leave an empty memory; the row keeps its old text instead.
            text: self.text.map(|text| text.trim().to_string()).filter(|text| !text.is_empty()),
            kind: None,
            tags: self.tags,
            importance: self.importance,
            pinned: self.pinned,
            sensitive: self.sensitive,
        }
    }
}

pub(crate) async fn list(
    service: &dyn MemoryService,
    query: Option<String>,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    let query = query.as_deref().map(str::trim).filter(|query| !query.is_empty());
    service.list(query).await
}

pub(crate) async fn update(
    service: &dyn MemoryService,
    id: &str,
    request: MemoryEditRequest,
) -> Result<Option<MemoryRecord>, MemoryError> {
    service.edit(id, request.into_edit()).await
}

#[tauri::command]
pub async fn list_memories(
    app: AppHandle,
    query: Option<String>,
) -> Result<Vec<MemoryRecord>, String> {
    list(memory_runtime::store(&app).as_ref(), query).await.map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn list_memory_history(app: AppHandle) -> Result<Vec<MemoryRecord>, String> {
    memory_runtime::store(&app).history().await.map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn update_memory(
    app: AppHandle,
    id: String,
    request: MemoryEditRequest,
) -> Result<Option<MemoryRecord>, String> {
    let updated = update(memory_runtime::store(&app).as_ref(), &id, request)
        .await
        .map_err(|err| err.to_string())?;
    notify_changed(&app);
    Ok(updated)
}

#[tauri::command]
pub async fn delete_memory(app: AppHandle, id: String) -> Result<bool, String> {
    let deleted = memory_runtime::store(&app).forget(&id).await.map_err(|err| err.to_string())?;
    notify_changed(&app);
    Ok(deleted)
}

/// Erases every memory, superseded history included. Returns how many there
/// were.
#[tauri::command]
pub async fn erase_all_memories(app: AppHandle) -> Result<usize, String> {
    let count = memory_runtime::store(&app).erase_all().await.map_err(|err| err.to_string())?;
    notify_changed(&app);
    Ok(count)
}

#[tauri::command]
pub fn get_memory_enabled(ai_settings: State<Mutex<AiSettings>>) -> bool {
    ai_settings.lock().unwrap().memory_enabled
}

/// Turning memory off leaves what is stored untouched; turning it on again uses
/// it again.
#[tauri::command]
pub fn set_memory_enabled(app: AppHandle, ai_settings: State<Mutex<AiSettings>>, enabled: bool) {
    let mut settings = ai_settings.lock().unwrap();
    settings.memory_enabled = enabled;
    ai_config_store::save(&app, &settings);
}

#[cfg(test)]
mod tests {
    use fleet_snowfluff_ai::memory::{fake::InMemoryMemory, Audience, NewMemory};

    use super::*;

    #[tokio::test]
    async fn the_list_filters_by_text_and_ignores_a_blank_query() {
        let service = InMemoryMemory::new();
        service.add(NewMemory::told("Likes green tea")).await.unwrap();
        service.add(NewMemory::told("Has a cat")).await.unwrap();
        assert_eq!(list(&service, None).await.unwrap().len(), 2);
        assert_eq!(list(&service, Some("   ".into())).await.unwrap().len(), 2);
        let found = list(&service, Some(" TEA ".into())).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Likes green tea");
    }

    #[tokio::test]
    async fn an_edit_changes_only_the_fields_sent() {
        let service = InMemoryMemory::new();
        let memory = service.add(NewMemory::told("Likes tea")).await.unwrap();
        let request = MemoryEditRequest { pinned: Some(true), ..Default::default() };
        let edited = update(&service, &memory.id, request).await.unwrap().unwrap();
        assert!(edited.pinned);
        assert_eq!(edited.text, "Likes tea");
        let request = MemoryEditRequest { sensitive: Some(true), ..Default::default() };
        update(&service, &memory.id, request).await.unwrap();
        assert!(service.get(&memory.id, Audience::Cloud).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn blank_text_leaves_the_old_text_and_a_missing_id_reports_none() {
        let service = InMemoryMemory::new();
        let memory = service.add(NewMemory::told("Likes tea")).await.unwrap();
        let request = MemoryEditRequest { text: Some("   ".into()), ..Default::default() };
        let edited = update(&service, &memory.id, request).await.unwrap().unwrap();
        assert_eq!(edited.text, "Likes tea");
        let none = update(&service, "missing", MemoryEditRequest::default()).await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn the_history_lists_replaced_memories_and_erase_all_clears_it_too() {
        let service = InMemoryMemory::new();
        let old = service.add(NewMemory::told("Lives in Taichung")).await.unwrap();
        service.supersede(&old.id, NewMemory::told("Lives in Taipei")).await.unwrap();
        assert_eq!(service.history().await.unwrap().len(), 1);
        assert_eq!(service.erase_all().await.unwrap(), 2);
        assert!(service.history().await.unwrap().is_empty());
        assert!(list(&service, None).await.unwrap().is_empty());
    }
}
