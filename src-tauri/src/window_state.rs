#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn save_window_state_before_exit(app: &tauri::AppHandle) {
    use tauri_plugin_window_state::AppHandleExt;

    let flags = tauri_plugin_window_state::StateFlags::POSITION
        | tauri_plugin_window_state::StateFlags::SIZE
        | tauri_plugin_window_state::StateFlags::MAXIMIZED;
    if let Err(err) = app.save_window_state(flags) {
        log::error!("退出前保存窗口状态失败: {err}");
    } else {
        log::info!("已在退出前保存窗口状态");
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn save_window_state_before_exit(_app: &tauri::AppHandle) {}
