pub fn show(window: &tauri::WebviewWindow) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let _ = window.show();
    }
}

pub fn hide(window: &tauri::Window) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let _ = window.hide();
    }
}

pub fn hide_webview(window: &tauri::WebviewWindow) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let _ = window.hide();
    }
}

pub fn unminimize(window: &tauri::WebviewWindow) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let _ = window.unminimize();
    }
}

pub fn set_focus(window: &tauri::WebviewWindow) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let _ = window.set_focus();
    }
}
