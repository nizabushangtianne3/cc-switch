use tauri::State;

use crate::services::omo::{OmoLocalFileData, SLIM, STANDARD};
use crate::services::OmoService;
use crate::store::AppState;

#[tauri::command]
pub async fn read_omo_local_file() -> Result<OmoLocalFileData, String> {
    OmoService::read_local_file(&STANDARD).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_current_omo_provider_id(state: State<'_, AppState>) -> Result<String, String> {
    let provider_id = state
        .db
        .get_current_omo_provider_id("opencode", "omo")
        .map_err(|e| e.to_string())?;
    Ok(provider_id.unwrap_or_default())
}

#[tauri::command]
pub async fn disable_current_omo(state: State<'_, AppState>) -> Result<(), String> {
    state
        .db
        .clear_omo_provider_current_by_category("opencode", "omo")
        .map_err(|e| e.to_string())?;
    OmoService::delete_config_file(&STANDARD).map_err(|e| e.to_string())?;
    Ok(())
}

// ── OMO Slim commands ───────────────────────────────────────

#[tauri::command]
pub async fn read_omo_slim_local_file() -> Result<OmoLocalFileData, String> {
    OmoService::read_local_file(&SLIM).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_current_omo_slim_provider_id(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let provider_id = state
        .db
        .get_current_omo_provider_id("opencode", "omo-slim")
        .map_err(|e| e.to_string())?;
    Ok(provider_id.unwrap_or_default())
}

#[tauri::command]
pub async fn disable_current_omo_slim(state: State<'_, AppState>) -> Result<(), String> {
    state
        .db
        .clear_omo_provider_current_by_category("opencode", "omo-slim")
        .map_err(|e| e.to_string())?;
    OmoService::delete_config_file(&SLIM).map_err(|e| e.to_string())?;
    Ok(())
}
