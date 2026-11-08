#![allow(non_snake_case)]

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::app_config::AppType;
use crate::codex_config;
use crate::config::{self, get_claude_settings_path};
use std::str::FromStr;

#[tauri::command]
pub async fn get_claude_code_config_path() -> Result<String, String> {
    Ok(get_claude_settings_path().to_string_lossy().to_string())
}

#[tauri::command]
pub async fn get_config_dir(app: String) -> Result<String, String> {
    let dir = match AppType::from_str(&app).map_err(|e| e.to_string())? {
        AppType::Claude => config::get_claude_config_dir(),
        AppType::ClaudeDesktop => {
            crate::claude_desktop_config::get_config_library_path().map_err(|e| e.to_string())?
        }
        AppType::Codex => codex_config::get_codex_config_dir(),
        AppType::Gemini => crate::gemini_config::get_gemini_dir(),
        AppType::GrokBuild => crate::grok_config::get_grok_config_dir(),
        AppType::OpenCode => crate::opencode_config::get_opencode_dir(),
        AppType::OpenClaw => crate::openclaw_config::get_openclaw_dir(),
        AppType::Hermes => crate::hermes_config::get_hermes_dir(),
        AppType::Pi => crate::pi_config::get_pi_agent_dir().map_err(|e| e.to_string())?,
        AppType::Dsh => crate::dsh_config::data_dir(),
        AppType::Kimi => crate::kimi_config::data_dir(),
        AppType::Omp => crate::omp_config::get_omp_agent_dir().map_err(|e| e.to_string())?,
        AppType::Mcode => crate::mcode_config::config_path()
            .parent()
            .unwrap()
            .to_path_buf(),
        AppType::Agy => crate::agy_config::agy_dir(),
    };

    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn open_config_folder(handle: AppHandle, app: String) -> Result<bool, String> {
    let config_dir = match AppType::from_str(&app).map_err(|e| e.to_string())? {
        AppType::Claude => config::get_claude_config_dir(),
        AppType::ClaudeDesktop => {
            crate::claude_desktop_config::get_config_library_path().map_err(|e| e.to_string())?
        }
        AppType::Codex => codex_config::get_codex_config_dir(),
        AppType::Gemini => crate::gemini_config::get_gemini_dir(),
        AppType::GrokBuild => crate::grok_config::get_grok_config_dir(),
        AppType::OpenCode => crate::opencode_config::get_opencode_dir(),
        AppType::OpenClaw => crate::openclaw_config::get_openclaw_dir(),
        AppType::Hermes => crate::hermes_config::get_hermes_dir(),
        AppType::Pi => crate::pi_config::get_pi_agent_dir().map_err(|e| e.to_string())?,
        AppType::Dsh => crate::dsh_config::data_dir(),
        AppType::Kimi => crate::kimi_config::data_dir(),
        AppType::Omp => crate::omp_config::get_omp_agent_dir().map_err(|e| e.to_string())?,
        AppType::Mcode => crate::mcode_config::config_path()
            .parent()
            .unwrap()
            .to_path_buf(),
        AppType::Agy => crate::agy_config::agy_dir(),
    };

    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }

    handle
        .opener()
        .open_path(config_dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| format!("打开文件夹失败: {e}"))?;

    Ok(true)
}

#[tauri::command]
pub async fn pick_directory(
    app: AppHandle,
    #[allow(non_snake_case)] defaultPath: Option<String>,
) -> Result<Option<String>, String> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let initial = defaultPath
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());

        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut builder = app.dialog().file();
            if let Some(path) = initial {
                builder = builder.set_directory(path);
            }
            builder.blocking_pick_folder()
        })
        .await
        .map_err(|e| format!("弹出目录选择器失败: {e}"))?;

        match result {
            Some(file_path) => {
                let resolved = file_path
                    .simplified()
                    .into_path()
                    .map_err(|e| format!("解析选择的目录失败: {e}"))?;
                Ok(Some(resolved.to_string_lossy().to_string()))
            }
            None => Ok(None),
        }
    }
}

#[tauri::command]
pub async fn get_app_config_path() -> Result<String, String> {
    let config_path = config::get_app_config_path();
    Ok(config_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn open_app_config_folder(handle: AppHandle) -> Result<bool, String> {
    let config_dir = config::get_app_config_dir();

    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }

    handle
        .opener()
        .open_path(config_dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| format!("打开文件夹失败: {e}"))?;

    Ok(true)
}

/// 在文件管理器中定位应用诊断日志 cc-switch.log；文件还没生成时打开日志目录
#[tauri::command]
pub async fn reveal_app_log_file() -> Result<bool, String> {
    let log_dir = crate::panic_hook::get_log_dir();
    let log_file = log_dir.join("cc-switch.log");

    let target = if log_file.exists() {
        log_file
    } else {
        std::fs::create_dir_all(&log_dir).map_err(|e| format!("创建目录失败: {e}"))?;
        log_dir
    };

    tauri_plugin_opener::reveal_item_in_dir(&target).map_err(|e| format!("打开文件夹失败: {e}"))?;

    Ok(true)
}
