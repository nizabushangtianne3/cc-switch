//! GitHub Copilot Tauri Commands
//!
//! 提供 Copilot OAuth 认证相关的 Tauri 命令，支持多账号管理。

use crate::proxy::providers::copilot_auth::{
    CopilotAuthManager, CopilotModel, CopilotUsageResponse,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Copilot 认证状态
pub struct CopilotAuthState(pub Arc<RwLock<CopilotAuthManager>>);

// ==================== 模型和使用量 ====================

/// 获取 Copilot 可用模型列表（向后兼容：使用第一个账号）
#[tauri::command]
pub async fn copilot_get_models(
    state: State<'_, CopilotAuthState>,
) -> Result<Vec<CopilotModel>, String> {
    let auth_manager = state.0.read().await;
    auth_manager.fetch_models().await.map_err(|e| e.to_string())
}

/// 获取指定账号的 Copilot 可用模型列表
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_get_models_for_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<Vec<CopilotModel>, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .fetch_models_for_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

/// 获取 Copilot 使用量信息（向后兼容：使用第一个账号）
#[tauri::command]
pub async fn copilot_get_usage(
    state: State<'_, CopilotAuthState>,
) -> Result<CopilotUsageResponse, String> {
    let auth_manager = state.0.read().await;
    auth_manager.fetch_usage().await.map_err(|e| e.to_string())
}

/// 获取指定账号的 Copilot 使用量信息
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_get_usage_for_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<CopilotUsageResponse, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .fetch_usage_for_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}
