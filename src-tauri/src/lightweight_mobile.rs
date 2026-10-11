pub fn enter_lightweight_mode(_app: &tauri::AppHandle) -> Result<(), String> {
    Err("移动端暂不支持轻量模式".to_string())
}

pub fn exit_lightweight_mode(_app: &tauri::AppHandle) -> Result<(), String> {
    Err("移动端暂不支持轻量模式".to_string())
}

pub fn is_lightweight_mode() -> bool {
    false
}
