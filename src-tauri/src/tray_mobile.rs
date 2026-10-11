use crate::app_config::AppType;
use crate::error::AppError;
use crate::store::AppState;
use serde::Serialize;

pub const TRAY_ID: &str = "cc-switch";
pub const TRAY_APPS: [AppType; 5] = [
    AppType::Claude,
    AppType::ClaudeDesktop,
    AppType::Codex,
    AppType::Gemini,
    AppType::GrokBuild,
];

pub struct Menu;
#[derive(Serialize)]
pub struct TrayNavigation;
pub struct Image;

pub fn mark_startup_settled(_app: &tauri::AppHandle) {}

#[tauri::command]
pub fn tray_app_page_seen(_app: tauri::AppHandle, _app_type: String) {}

pub fn note_tray_click(_button: TrayButton) {}

pub enum TrayButton {
    Left,
    Right,
}

pub fn create_tray_menu(_app: &tauri::AppHandle, _state: &AppState) -> Result<Menu, AppError> {
    Ok(Menu)
}

pub fn base_tray_icon(_app: &tauri::AppHandle) -> Option<(Image, bool)> {
    None
}

pub fn refresh_tray_menu(_app: &tauri::AppHandle) {}

pub fn update_menu(_app: &tauri::AppHandle, _state: &AppState) {}

pub fn refresh_tray_if_problems_changed(_app: &tauri::AppHandle) {}

pub fn schedule_tray_status_check(_app: &tauri::AppHandle) {}

#[tauri::command]
pub fn take_tray_navigation() -> Option<TrayNavigation> {
    None
}

pub fn show_main_window(_app: &tauri::AppHandle) {}

pub fn handle_profile_tray_event(_app: &tauri::AppHandle, _event_id: &str) -> bool {
    false
}

pub fn handle_provider_tray_event(_app: &tauri::AppHandle, _event_id: &str) -> bool {
    false
}

pub fn apply_tray_policy(_app: &tauri::AppHandle, _dock_visible: bool) {}

pub fn handle_tray_menu_event(_app: &tauri::AppHandle, _event_id: &str) {}

pub fn schedule_tray_refresh(_app: &tauri::AppHandle) {}

pub async fn refresh_all_usage_in_tray(_app: &tauri::AppHandle) {}
