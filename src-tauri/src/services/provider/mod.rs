//! Provider service module
//!
//! Handles provider CRUD operations, switching, and configuration management.

pub(crate) mod agy_direct;
pub(crate) mod claude_direct;
mod claude_editor;
pub(crate) mod claude_official;
pub(crate) mod codex_client_catalog;
pub(crate) mod codex_direct;
mod codex_editor;
mod codex_login;
pub(crate) mod codex_official_models;
pub(crate) mod codex_terminal;
pub(crate) mod copilot_direct;
mod editor_toml;
mod endpoints;
mod gemini_auth;
pub(crate) mod gemini_direct;
mod gemini_editor;
pub(crate) mod grok_direct;
mod grok_editor;
pub(crate) mod icon;
mod live;
mod omp;
#[cfg(test)]
mod opencode_tests;
mod pi;
mod usage;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta, UsageResult};
use crate::services::mcp::McpService;
use crate::settings::CustomEndpoint;
use crate::store::AppState;

// Re-export sub-module functions for external access
pub use live::{
    import_default_config, import_hermes_providers_from_live, import_openclaw_providers_from_live,
    import_opencode_providers_from_live, read_live_settings,
    should_import_default_config_on_startup, sync_current_to_live,
};

pub fn import_pi_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    pi::import_from_live(state)
}

pub fn import_omp_providers_from_live(state: &AppState) -> Result<usize, AppError> {
    omp::import_from_live(state)
}

pub(crate) use claude_editor::{switch_drops as claude_switch_drops, SwitchDrops};
pub use claude_editor::{EditorSave, EditorView};

// Internal re-exports (pub(crate))
pub(crate) use live::{
    initialize_dsh_profiles, provider_exists_in_live_config, sync_additive_app_to_live,
    write_live_for_state, LiveSyncOutcome,
};

// Internal re-exports
use live::{
    remove_hermes_provider_from_live, remove_openclaw_provider_from_live,
    remove_opencode_provider_from_live,
};
use usage::validate_usage_script;

/// Codex official providers are safe to select during takeover: Codex keeps
/// ownership of the active ChatGPT login and the proxy only forwards the
/// authenticated request. Other apps' official providers retain the block.
/// Claude Desktop 直连模式下用不了要经路由的卡。
fn desktop_needs_routing_error() -> AppError {
    AppError::localized(
        "switch.desktop_needs_routing",
        "这个供应商要经本地路由才能用（模型映射或托管账号），请先把 Claude Desktop 切到路由模式",
        "This provider only works through local routing (model mapping or a managed account); switch Claude Desktop to the routing mode first",
    )
}

pub fn official_provider_supports_proxy_takeover(app_type: &AppType, provider: &Provider) -> bool {
    matches!(app_type, AppType::Codex)
        && crate::proxy::providers::is_codex_official_provider(provider)
}

/// 统一会话开关变更后，立即按新开关状态重写当前官方 Codex 供应商的选路（关键字段），
/// 使开关即时生效，无需等下一次切换。当前供应商非官方（或不存在）时为 no-op：开关只
/// 影响官方直连的选路。代理模式下 live 是代理契约，不受这个开关影响。
pub fn reapply_current_codex_official_live(state: &AppState) -> Result<bool, AppError> {
    let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &AppType::Codex)?;
    let current_id = ProviderService::current(state, AppType::Codex)?;
    if current_id.is_empty() {
        return Ok(false);
    }
    let provider = state
        .db
        .get_provider_by_id(&current_id, AppType::Codex.as_str())?;
    let Some(provider) = provider else {
        return Ok(false);
    };
    if !codex_direct::is_official(&provider) {
        return Ok(false);
    }
    live::sync_live_for_provider_respecting_mode(state, &AppType::Codex, &provider, None)?;
    Ok(true)
}

/// 新版不再读通用配置片段，但旧设备经云同步拿到新建的行时仍按这个标记合并片段；不写的
/// 话，旧版切到它会把 hooks、MCP 等共享设置整份抹掉。
fn keep_common_config_for_old_versions(provider: &mut Provider) {
    provider
        .meta
        .get_or_insert_with(Default::default)
        .common_config_enabled = Some(true);
}

/// 编辑器保存的是新增的供应商，还是已有的。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorSaveKind {
    Add,
    Update,
}

impl EditorSaveKind {
    /// 这次保存要不要把关键字段也换进 live：直连模式下编辑的是当前供应商，或者新增的是
    /// 第一个供应商。代理模式下 live 的关键字段是代理契约，只写全局改动。
    fn writes_key_fields(
        self,
        state: &AppState,
        app_type: &AppType,
        mode: &crate::mode::state::ModeState,
        id: &str,
    ) -> Result<bool, AppError> {
        if mode.is_proxy() {
            return Ok(false);
        }
        let direct = crate::mode::current::provider_for(
            &state.db,
            app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        Ok(match self {
            Self::Add => direct.is_none(),
            Self::Update => direct.as_deref() == Some(id),
        })
    }
}

/// Provider business logic service
pub struct ProviderService;

/// Native provider lists refresh saved rows as well as reading them. Keep the
/// entire snapshot/readback under the same per-app lock as provider mutations,
/// so an in-flight list cannot restore a deleted row or overwrite a newer edit.
fn lock_native_provider_operations(
    state: &AppState,
    app_type: &AppType,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    matches!(app_type, AppType::Mcode | AppType::Kimi | AppType::Dsh).then(|| {
        futures::executor::block_on(state.proxy_service.lock_switch_for_app(app_type.as_str()))
    })
}

/// Result of a provider switch operation, including any non-fatal warnings
#[derive(Debug, serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub warnings: Vec<String>,
    /// Present only when this operation selected a local proxy target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<crate::mode::controller::RouteSwitchResult>,
    /// Codex only: this switch moved new sessions to the other history bucket
    /// while unified history is off, so newer Codex locks the old bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_history_split: Option<crate::codex_history_migration::CodexHistorySplitHint>,
}

// ============================================================================
// 单元测试（`ProviderService` 与部分自由函数）拆到 ./tests.rs
// ============================================================================
#[path = "tests.rs"]
#[cfg(test)]
mod tests;

impl ProviderService {
    pub(crate) fn managed_codex_oauth_account_id(provider: &Provider) -> Option<String> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }

    fn normalize_provider_if_claude(app_type: &AppType, provider: &mut Provider) {
        if matches!(app_type, AppType::Claude) {
            let mut v = provider.settings_config.clone();
            if normalize_claude_models_in_value(&mut v) {
                provider.settings_config = v;
            }
        }
    }

    /// Check whether a provider exists in live config, tolerating parse errors
    /// only for providers that are explicitly marked as DB-only.
    fn check_live_config_exists(
        app_type: &AppType,
        provider_id: &str,
        live_config_managed: Option<bool>,
    ) -> Result<bool, AppError> {
        if live_config_managed == Some(false) {
            Ok(provider_exists_in_live_config(app_type, provider_id).unwrap_or(false))
        } else {
            provider_exists_in_live_config(app_type, provider_id)
        }
    }

    fn provider_live_config_managed(provider: &Provider) -> Option<bool> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.live_config_managed)
    }

    fn set_provider_live_config_managed(provider: &mut Provider, managed: bool) {
        provider
            .meta
            .get_or_insert_with(Default::default)
            .live_config_managed = Some(managed);
    }

    fn normalize_usage_script_credential_overrides(app_type: &AppType, provider: &mut Provider) {
        let current_credentials = provider.resolve_usage_credentials(app_type);

        let Some(usage_script) = provider
            .meta
            .as_mut()
            .and_then(|meta| meta.usage_script.as_mut())
        else {
            return;
        };

        if usage_script.template_type.as_deref() == Some("token_plan") {
            return;
        }

        if usage_script.api_key.as_deref().is_some_and(|api_key| {
            Self::should_clear_usage_api_key_override(api_key, &current_credentials)
        }) {
            usage_script.api_key = None;
        }

        if usage_script.base_url.as_deref().is_some_and(|base_url| {
            Self::should_clear_usage_base_url_override(base_url, &current_credentials)
        }) {
            usage_script.base_url = None;
        }
    }

    fn should_clear_usage_api_key_override(
        script_api_key: &str,
        current_credentials: &(String, String),
    ) -> bool {
        let candidate = script_api_key.trim();
        if candidate.is_empty() {
            return true;
        }

        let matches_provider_key = |api_key: &str| {
            let api_key = api_key.trim();
            !api_key.is_empty() && api_key == candidate
        };

        matches_provider_key(&current_credentials.1)
    }

    fn should_clear_usage_base_url_override(
        script_base_url: &str,
        current_credentials: &(String, String),
    ) -> bool {
        let candidate = Self::normalize_usage_base_url_for_compare(script_base_url);
        if candidate.is_empty() {
            return true;
        }

        let matches_provider_base_url = |base_url: &str| {
            let base_url = Self::normalize_usage_base_url_for_compare(base_url);
            !base_url.is_empty() && base_url == candidate
        };

        matches_provider_base_url(&current_credentials.0)
    }

    fn normalize_usage_base_url_for_compare(base_url: &str) -> String {
        base_url.trim().trim_end_matches('/').to_string()
    }

    /// List all providers for an app type
    pub fn list(
        state: &AppState,
        app_type: AppType,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type == AppType::Dsh {
            // A profile can appear while CC Switch is running. Keep the provider list usable
            // when a native lock or an invalid profile prevents the initial sync; retry later.
            if let Err(error) = initialize_dsh_profiles(state) {
                log::warn!("Initializing DeepSeek Harness profiles failed: {error}");
            }
        }
        if app_type == AppType::Pi {
            return pi::list(state);
        }
        if app_type == AppType::Omp {
            return omp::list(state);
        }
        if matches!(app_type, AppType::Mcode | AppType::Kimi | AppType::Dsh) {
            let native = if matches!(app_type, AppType::Dsh) {
                crate::dsh_config::get_providers()?
            } else if app_type == AppType::Kimi {
                crate::kimi_config::get_providers()?
            } else {
                crate::mcode_config::get_providers()?
            };
            let mut saved = state.db.get_all_providers(app_type.as_str())?;
            let nicknames = if app_type == AppType::Kimi {
                crate::services::kimi_oauth::account_nicknames()
            } else {
                Default::default()
            };
            for (id, config) in &native {
                if !saved.contains_key(id) {
                    let name = config.get("name").and_then(Value::as_str).unwrap_or(id);
                    saved.insert(
                        id.clone(),
                        Provider::with_id(id.clone(), name.into(), config.clone(), None),
                    );
                }
            }
            for (id, provider) in &mut saved {
                let mut changed = false;
                if let Some(config) = native.get(id) {
                    if let Some(name) = config.get("name").and_then(Value::as_str) {
                        if provider.name != name {
                            provider.name = name.into();
                            changed = true;
                        }
                    }
                    if provider.settings_config != *config {
                        provider.settings_config = config.clone();
                        changed = true;
                    }
                }
                let managed = native.contains_key(id);
                if Self::provider_live_config_managed(provider) != Some(managed) {
                    Self::set_provider_live_config_managed(provider, managed);
                    changed = true;
                }
                if app_type == AppType::Kimi {
                    // 归一化可能改写身份字段，Kimi 一律回写
                    crate::kimi_config::normalize_provider_identity(
                        provider,
                        nicknames.get(id).map(String::as_str),
                    );
                    changed = true;
                }
                if changed {
                    state.db.save_provider(app_type.as_str(), provider)?;
                }
            }
            return Ok(saved);
        }
        state.db.get_all_providers(app_type.as_str())
    }

    /// Get current provider ID
    ///
    /// 使用有效的当前供应商 ID（验证过存在性）。
    /// 优先从本地 settings 读取，验证后 fallback 到数据库的 is_current 字段。
    /// 这确保了云同步场景下多设备可以独立选择供应商，且返回的 ID 一定有效。
    ///
    /// 对于累加模式应用（OpenCode, OpenClaw），不存在"当前供应商"概念，直接返回空字符串。
    pub fn current(state: &AppState, app_type: AppType) -> Result<String, AppError> {
        if app_type == AppType::Kimi {
            return crate::kimi_config::current_provider();
        }
        // Additive mode apps have no "current" provider concept
        if app_type.is_additive_mode() {
            return Ok(String::new());
        }
        // 代理模式下界面上的「当前」是代理路由到的那家。
        crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::InUse,
        )
        .map(|opt| opt.unwrap_or_default())
    }

    fn save_native_provider(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
        write_live: bool,
        create: bool,
    ) -> Result<bool, AppError> {
        if matches!(app_type, AppType::Dsh) {
            return if write_live {
                crate::dsh_config::save_and_commit(
                    &provider.id,
                    Some(provider.settings_config.clone()),
                    create,
                    || {
                        state.db.save_provider(app_type.as_str(), provider)?;
                        Ok(true)
                    },
                )
            } else {
                state.db.save_provider(app_type.as_str(), provider)?;
                Ok(true)
            };
        }
        if app_type == &AppType::Kimi {
            return if write_live {
                crate::kimi_config::save_and_commit(
                    &provider.id,
                    Some(provider.settings_config.clone()),
                    create,
                    || {
                        state.db.save_provider("kimi", provider)?;
                        Ok(true)
                    },
                )
            } else {
                state.db.save_provider("kimi", provider)?;
                Ok(true)
            };
        }
        let previous = state.db.get_provider_by_id(&provider.id, "mcode")?;
        state.db.save_provider("mcode", provider)?;
        if write_live {
            let config = provider.settings_config.clone();
            let written = if create {
                crate::mcode_config::add_provider(&provider.id, config)
            } else {
                crate::mcode_config::set_provider(&provider.id, config)
            };
            if let Err(error) = written {
                match previous {
                    Some(previous) => state.db.save_provider("mcode", &previous)?,
                    None => state.db.delete_provider("mcode", &provider.id)?,
                }
                return Err(error);
            }
        }
        Ok(true)
    }

    /// 新增供应商之前的规范化和校验，`add` 和 `add_without_switching` 共用。
    fn prepare_new_provider(app_type: &AppType, provider: &mut Provider) -> Result<(), AppError> {
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(app_type, provider);
        Self::validate_provider_settings(app_type, provider)?;
        Self::normalize_usage_script_credential_overrides(app_type, provider);
        if matches!(app_type, AppType::Claude | AppType::Codex | AppType::Gemini) {
            keep_common_config_for_old_versions(provider);
        }
        Ok(())
    }

    /// 只把供应商存进列表，不写客户端配置、不动当前指针（切换式应用）。
    ///
    /// `add` 在还没有当前供应商时会让新的这家直接生效；深链接导入时用户关掉了「导入后立即
    /// 切换」，就不能这样，交给用户之后自己切换。
    pub fn add_without_switching(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
    ) -> Result<bool, AppError> {
        if app_type.is_additive_mode() {
            return Self::add(state, app_type, provider, false);
        }
        let mut provider = provider;
        Self::prepare_new_provider(&app_type, &mut provider)?;
        // 和切换互斥：等着的切换不能看到只存了一半的行
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        state.db.save_provider(app_type.as_str(), &provider)?;
        Ok(true)
    }

    /// Add a new provider
    pub fn add(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
    ) -> Result<bool, AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type == AppType::Kimi
            && crate::kimi_config::is_official_provider(&provider.settings_config)
        {
            return Err(AppError::InvalidInput(
                "Manage Kimi official accounts in the Authorization Center".into(),
            ));
        }
        if app_type == AppType::Pi {
            return pi::add(state, provider, add_to_live);
        }

        if app_type == AppType::Omp {
            return omp::add(state, provider, add_to_live);
        }

        let mut provider = provider;
        Self::prepare_new_provider(&app_type, &mut provider)?;
        if app_type.is_additive_mode() {
            Self::set_provider_live_config_managed(&mut provider, add_to_live);
        }

        if matches!(app_type, AppType::Codex) {
            return Self::add_codex(state, provider);
        }

        if matches!(app_type, AppType::Mcode | AppType::Kimi | AppType::Dsh) {
            // The key is user-chosen, and saving would overwrite an existing row or live node.
            // The per-app lock also serializes native list refreshes and other mutations;
            // the live write repeats the check under MCode's file lock.
            if state
                .db
                .get_provider_by_id(&provider.id, app_type.as_str())?
                .is_some()
                || if matches!(app_type, AppType::Dsh) {
                    crate::dsh_config::provider_key_exists(&provider.id)?
                } else if app_type == AppType::Kimi {
                    crate::kimi_config::provider_key_exists(&provider.id)?
                } else {
                    crate::mcode_config::provider_key_exists(&provider.id)?
                }
            {
                return Err(AppError::InvalidInput(format!(
                    "{} provider key '{}' already exists",
                    app_type.as_str(),
                    provider.id
                )));
            }
            return Self::save_native_provider(state, &app_type, &provider, add_to_live, true);
        }

        // 还没有当前供应商时这一家会被写进 live：和进入代理、切换互斥（同 `update`）。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;

        // 累加式写 live 失败（如配置文件解析不了）时要撤回这一行，先记下原来的
        let existing_row = if app_type.is_additive_mode() {
            state
                .db
                .get_provider_by_id(&provider.id, app_type.as_str())?
        } else {
            None
        };

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        // Additive mode apps (OpenCode, OpenClaw, Hermes): optionally write to live config.
        if app_type.is_additive_mode() {
            // OMO / OMO Slim providers use exclusive mode and write to dedicated config file.
            if matches!(app_type, AppType::OpenCode)
                && matches!(provider.category.as_deref(), Some("omo") | Some("omo-slim"))
            {
                // Do not auto-enable newly added OMO / OMO Slim providers.
                // Users must explicitly switch/apply an OMO provider to activate it.
                return Ok(true);
            }
            if !add_to_live {
                return Ok(true);
            }
            return Self::keep_row_if_written(
                state,
                &app_type,
                &provider.id,
                existing_row.as_ref(),
                write_live_for_state(state, &app_type, &provider),
            );
        }

        // For other apps: Check if sync is needed (if this is current provider, or no current provider)
        let current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        if current.is_none() {
            // 第一个供应商同样只写关键字段，不覆盖用户已有的配置文件。
            match app_type {
                AppType::Claude => {
                    claude_direct::switch_to(state.db.as_ref(), None, &provider)?;
                    return Ok(true);
                }
                AppType::Gemini => {
                    gemini_direct::switch_to(state.db.as_ref(), &provider)?;
                    return Ok(true);
                }
                // Antigravity、Copilot CLI 的卡只用来打开终端，添加不写任何文件。
                AppType::Agy | AppType::Copilot => return Ok(true),
                AppType::GrokBuild => {
                    grok_direct::switch_to(state.db.as_ref(), None, &provider)?;
                    return Ok(true);
                }
                AppType::ClaudeDesktop => {
                    // 要经路由的卡直连写不进去，路由模式下配置文件归路由那家：只存行。
                    if !crate::mode::current::is_proxy(&app_type)
                        && !crate::claude_desktop_config::needs_routing(&provider)
                    {
                        crate::claude_desktop_config::write_direct(
                            state.db.as_ref(),
                            crate::mode::state::op::SWITCH,
                            Some(&provider),
                            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
                        )?;
                    }
                    return Ok(true);
                }
                _ => {}
            }
            // No current provider, set as current and sync. Managed Codex adds
            // use the transactional path above because token resolution can fail.
            state
                .db
                .set_current_provider(app_type.as_str(), &provider.id)?;
            write_live_for_state(state, &app_type, &provider)?;
        }

        Ok(true)
    }

    /// 新增 Codex 供应商。还没有当前供应商时，它就是第一个：行、live 和指针一起提交，
    /// live 只写关键字段，不覆盖用户已有的 config.toml。写 live 失败时撤回刚存的行，
    /// 免得留下一个看得见却用不了的供应商。
    fn add_codex(state: &AppState, provider: Provider) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        // 和切换互斥：等着的切换不能看到只存了一半的托管账号绑定。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?;
        if current.is_some() {
            state.db.save_provider(app_type.as_str(), &provider)?;
            return Ok(true);
        }

        // 行有问题（会把官方登录发给第三方等）在存行之前就拒绝。
        codex_direct::preflight(state.db.as_ref(), &provider)?;
        let previous = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        state.db.save_provider(app_type.as_str(), &provider)?;
        let written = codex_direct::write_direct(
            state.db.as_ref(),
            &state.codex_oauth_manager,
            crate::mode::state::op::SWITCH,
            codex_direct::Owner::None,
            Some(&provider),
            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
        );
        if let Err(error) = written {
            // 文件已经发布、只是落定状态失败时，pending 会在下次操作或启动时补完指针，
            // 行要留着；还没发布就失败，撤回刚存的行。
            if crate::mode::operation::has_pending(AppType::Codex.as_str()) {
                return Err(error);
            }
            let rollback = match &previous {
                Some(previous) => state.db.save_provider(app_type.as_str(), previous),
                None => state.db.delete_provider(app_type.as_str(), &provider.id),
            };
            if let Err(rollback) = rollback {
                return Err(AppError::Message(format!(
                    "新增首个 Codex 供应商失败: {error}; 恢复供应商数据同时失败: {rollback}"
                )));
            }
            return Err(error);
        }
        Ok(true)
    }

    /// 供应商编辑器底部配置的显示内容（Claude Code、Codex、Gemini CLI、Grok Build）：切到
    /// 这个供应商之后配置文件会是什么样，以及行里不随切换生效的字段。`category` 用来认出
    /// 官方卡。
    pub fn editor_view(
        state: &AppState,
        app_type: AppType,
        settings_config: &Value,
        category: Option<&str>,
    ) -> Result<EditorView, AppError> {
        Self::editor_view_with_meta(state, app_type, settings_config, category, None, None)
    }

    /// Codex previews retain managed-provider identity. Explicit draft metadata,
    /// including an empty value, takes precedence over the stored card's metadata.
    pub fn editor_view_with_meta(
        state: &AppState,
        app_type: AppType,
        settings_config: &Value,
        category: Option<&str>,
        provider_id: Option<&str>,
        meta: Option<&ProviderMeta>,
    ) -> Result<EditorView, AppError> {
        match app_type {
            AppType::Claude => claude_editor::view(state, settings_config),
            AppType::Codex => {
                let stored_meta = match (meta, provider_id) {
                    (None, Some(id)) => state
                        .db
                        .get_provider_by_id(id, AppType::Codex.as_str())?
                        .and_then(|provider| provider.meta),
                    _ => None,
                };
                codex_editor::view(
                    state,
                    settings_config,
                    category,
                    meta.or(stored_meta.as_ref()),
                )
            }
            AppType::Gemini => gemini_editor::view(state, settings_config, category),
            AppType::GrokBuild => grok_editor::view(state, settings_config, category),
            other => Err(AppError::InvalidInput(format!(
                "{} 的编辑器还不支持按关键字段显示",
                other.as_str()
            ))),
        }
    }

    /// 编辑已有供应商时给 [`Self::editor_view`] 的 `category`：按库里那一行，用切换时同一个
    /// 判断认官方卡。Gemini 还按合作方标记和名字认出旧版没有 `category` 的 Google 卡，Codex
    /// 还认固定 id 和托管账号；只看 `category` 的话，这些卡预览里的登录方式和切换写的不同。
    /// 没有 `provider_id`（新增）或行不存在时原样用 `category`。
    pub fn editor_category(
        state: &AppState,
        app_type: &AppType,
        provider_id: Option<&str>,
        category: Option<String>,
    ) -> Result<Option<String>, AppError> {
        let Some(id) = provider_id else {
            return Ok(category);
        };
        let Some(row) = state.db.get_provider_by_id(id, app_type.as_str())? else {
            return Ok(category);
        };
        let official = match app_type {
            AppType::Codex => codex_direct::is_official(&row),
            AppType::Gemini => gemini_direct::is_official(&row),
            AppType::Agy => agy_direct::is_official(&row),
            AppType::Copilot => copilot_direct::is_official(&row),
            _ => false,
        };
        Ok(if official {
            Some("official".to_string())
        } else {
            category
        })
    }

    /// 从编辑器新增供应商。Claude Code、Codex、Gemini CLI、Grok Build 按关键字段拆开保存
    /// （见各自的 `*_editor`），其余应用和 `add` 一样。
    pub fn add_from_editor(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        add_to_live: bool,
        editor: Option<EditorSave>,
    ) -> Result<bool, AppError> {
        match (app_type, editor) {
            (AppType::Claude, Some(editor)) => {
                Self::add_claude_from_editor(state, provider, editor)
            }
            (AppType::Codex, Some(editor)) => {
                Self::save_codex_from_editor(state, provider, editor, EditorSaveKind::Add)
            }
            (app_type @ (AppType::Gemini | AppType::GrokBuild), Some(editor)) => {
                Self::save_gemini_or_grok_from_editor(
                    state,
                    app_type,
                    provider,
                    editor,
                    EditorSaveKind::Add,
                )
            }
            (app_type, _) => Self::add(state, app_type, provider, add_to_live),
        }
    }

    /// 从编辑器保存供应商。Claude Code、Codex、Gemini CLI、Grok Build 按关键字段拆开保存
    /// （见各自的 `*_editor`），其余应用和 `update` 一样。
    pub fn update_from_editor(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
        editor: Option<EditorSave>,
    ) -> Result<bool, AppError> {
        match (app_type, editor) {
            (AppType::Claude, Some(editor))
                if original_id.is_none_or(|original| original == provider.id) =>
            {
                Self::update_claude_from_editor(state, provider, editor)
            }
            (AppType::Codex, Some(editor))
                if original_id.is_none_or(|original| original == provider.id) =>
            {
                Self::save_codex_from_editor(state, provider, editor, EditorSaveKind::Update)
            }
            (app_type @ (AppType::Gemini | AppType::GrokBuild), Some(editor))
                if original_id.is_none_or(|original| original == provider.id) =>
            {
                Self::save_gemini_or_grok_from_editor(
                    state,
                    app_type,
                    provider,
                    editor,
                    EditorSaveKind::Update,
                )
            }
            (app_type, _) => Self::update(state, app_type, original_id, provider),
        }
    }

    /// 从编辑器新增或保存 Codex 供应商：关键字段、独有字段存回行，其余改动作为全局设置
    /// 写进 live（三方比较）。直连模式下编辑当前供应商、或新增第一个供应商时，关键字段在
    /// 同一次写入里换进 live；代理模式下编辑路由那家，全局设置写完后按新行重写代理契约。
    fn save_codex_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
        kind: EditorSaveKind,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let mut provider = provider;
        let plan = codex_editor::plan_save(
            existing.as_ref().map(|row| &row.settings_config),
            &provider.settings_config,
            &editor.base,
            &match (existing.as_ref(), editor.draft.as_ref()) {
                (Some(row), _) => codex_editor::Origin::row(&row.settings_config)?,
                (None, Some(draft)) => codex_editor::Origin::row(draft)?,
                (None, None) => codex_editor::Origin::Live(codex_editor::live_exclusive(state)?),
            },
            codex_direct::is_official(&provider),
            provider.uses_proxy_injected_oauth(),
            editor.on_conflict,
        )?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        if kind == EditorSaveKind::Add {
            keep_common_config_for_old_versions(&mut provider);
        }

        let mode = crate::mode::current::mode_state(&app_type);
        let key_fields = kind.writes_key_fields(state, &app_type, &mode, &provider.id)?;

        state.db.save_provider(app_type.as_str(), &provider)?;
        let written = if key_fields {
            codex_editor::write_live(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                &plan.edits,
                codex_editor::KeyFields::Direct {
                    prev: existing.as_ref(),
                    target: &provider,
                    set_pointer: kind == EditorSaveKind::Add,
                },
            )
        } else {
            codex_editor::write_live(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                &plan.edits,
                codex_editor::KeyFields::None,
            )
            .and_then(|()| Self::resync_proxy_for_saved_row(state, &app_type, &provider))
        };
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// 从编辑器新增或保存 Gemini CLI、Grok Build 供应商：关键字段存回行，其余改动作为全局
    /// 设置写进 live（三方比较）。直连模式下编辑当前供应商、或新增第一个供应商时，关键
    /// 字段在同一次写入里换进 live；代理模式下编辑路由那家，全局设置写完后按新行重写代理
    /// 契约。
    fn save_gemini_or_grok_from_editor(
        state: &AppState,
        app_type: AppType,
        provider: Provider,
        editor: EditorSave,
        kind: EditorSaveKind,
    ) -> Result<bool, AppError> {
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let stored = existing.as_ref().map(|row| &row.settings_config);
        let mut provider = provider;
        enum Edits {
            Gemini(gemini_editor::GeminiEdits),
            Grok(editor_toml::TomlEdits),
        }
        let edits = if matches!(app_type, AppType::Gemini) {
            let plan = gemini_editor::plan_save(
                stored,
                &provider.settings_config,
                &editor.base,
                editor.on_conflict,
            )?;
            provider.settings_config = plan.row_settings;
            Edits::Gemini(plan.edits)
        } else {
            let plan = grok_editor::plan_save(
                stored,
                &provider.settings_config,
                &editor.base,
                grok_direct::is_official(&provider),
                editor.on_conflict,
            )?;
            provider.settings_config = plan.row_settings;
            Edits::Grok(plan.edits)
        };
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        if kind == EditorSaveKind::Add && matches!(app_type, AppType::Gemini) {
            keep_common_config_for_old_versions(&mut provider);
        }

        let mode = crate::mode::current::mode_state(&app_type);
        let key_fields = kind.writes_key_fields(state, &app_type, &mode, &provider.id)?;
        let set_pointer = kind == EditorSaveKind::Add;

        state.db.save_provider(app_type.as_str(), &provider)?;
        let written = match &edits {
            Edits::Gemini(edits) => gemini_editor::write_live(
                state.db.as_ref(),
                edits,
                key_fields.then_some(&provider),
                set_pointer,
            ),
            Edits::Grok(edits) => grok_editor::write_live(
                state.db.as_ref(),
                edits,
                key_fields.then_some((existing.as_ref(), &provider)),
                set_pointer,
            ),
        }
        .and_then(|()| Self::resync_proxy_for_saved_row(state, &app_type, &provider));
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// 编辑器保存先存行、再写 live（`written`）。写 live 失败时：文件已经发布、只是落定
    /// 状态失败的，pending 会在下次操作或启动时补完，行要留着；还没发布就失败（没有
    /// pending），撤回刚存的行。
    fn keep_row_if_written(
        state: &AppState,
        app_type: &AppType,
        provider_id: &str,
        existing: Option<&Provider>,
        written: Result<(), AppError>,
    ) -> Result<bool, AppError> {
        let Err(error) = written else {
            return Ok(true);
        };
        if crate::mode::operation::has_pending(app_type.as_str()) {
            return Err(error);
        }
        let rollback = match existing {
            Some(existing) => state.db.save_provider(app_type.as_str(), existing),
            None => state.db.delete_provider(app_type.as_str(), provider_id),
        };
        if let Err(rollback) = rollback {
            log::warn!(
                "恢复 {} 供应商 '{provider_id}' 失败: {rollback}",
                app_type.as_str()
            );
        }
        Err(error)
    }

    /// 代理模式下存好了这一行：它是代理路由或在 Stack 名单里时按新行重写代理契约，契约没变
    /// 就不碰客户端文件（见 [`crate::mode::controller::resync_saved_row_locked`]）。调用方
    /// 持有这个应用的切换锁。
    fn resync_proxy_for_saved_row(
        state: &AppState,
        app_type: &AppType,
        provider: &Provider,
    ) -> Result<(), AppError> {
        futures::executor::block_on(crate::mode::controller::resync_saved_row_locked(
            state, app_type, provider,
        ))
        .map_err(AppError::Message)
    }

    fn add_claude_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Claude;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let mut provider = provider;
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        let plan = claude_editor::plan_save(None, &provider.settings_config, &editor.base)?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);
        keep_common_config_for_old_versions(&mut provider);

        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let first = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?
        .is_none();
        state.db.save_provider(app_type.as_str(), &provider)?;

        let key_fields = first.then_some(claude_editor::KeyFieldWrite {
            prev: None,
            target: &provider,
            set_pointer: true,
        });
        let written =
            claude_editor::write_live(state.db.as_ref(), &plan, editor.on_conflict, key_fields);
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// 和其他编辑器一样先存行、再写 live：写 live 失败且没有 pending 时撤回行；已经发布的
    /// 由 pending 补完，行和 live 不会对不上。
    fn update_claude_from_editor(
        state: &AppState,
        provider: Provider,
        editor: EditorSave,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Claude;
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing = state
            .db
            .get_provider_by_id(&provider.id, app_type.as_str())?;
        let mut provider = provider;
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        let plan = claude_editor::plan_save(
            existing.as_ref().map(|row| &row.settings_config),
            &provider.settings_config,
            &editor.base,
        )?;
        provider.settings_config = plan.row_settings.clone();
        Self::validate_provider_settings(&app_type, &provider)?;
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);

        let mode = crate::mode::current::mode_state(&app_type);
        // 代理模式下 live 的关键字段是代理契约，这里只写全局改动；编辑的是代理路由那家
        // 或 Stack 名单里的一家时，写完按新行重写契约（契约没变就不动）。直连指针那家在
        // 退出代理时写回。
        let key_fields = EditorSaveKind::Update
            .writes_key_fields(state, &app_type, &mode, &provider.id)?
            .then_some(claude_editor::KeyFieldWrite {
                prev: existing.as_ref(),
                target: &provider,
                set_pointer: false,
            });

        state.db.save_provider(app_type.as_str(), &provider)?;
        let written =
            claude_editor::write_live(state.db.as_ref(), &plan, editor.on_conflict, key_fields)
                .and_then(|()| Self::resync_proxy_for_saved_row(state, &app_type, &provider));
        Self::keep_row_if_written(state, &app_type, &provider.id, existing.as_ref(), written)
    }

    /// Update a provider
    pub fn update(
        state: &AppState,
        app_type: AppType,
        original_id: Option<&str>,
        provider: Provider,
    ) -> Result<bool, AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        let mut provider = provider;
        if app_type == AppType::Kimi {
            // Official cards come from signing in. Their models, name and notes are
            // editable; the connection stays as signed in, and no edit may turn
            // another card into an official one.
            let id = original_id.unwrap_or(&provider.id).to_string();
            match Self::kimi_official_connection(state, &id)? {
                Some(_) if provider.id != id => {
                    return Err(AppError::InvalidInput(
                        "Kimi official account keys cannot be changed".into(),
                    ));
                }
                Some(connection) => provider.settings_config["provider"] = connection,
                None if crate::kimi_config::is_official_provider(&provider.settings_config) => {
                    return Err(AppError::InvalidInput(
                        "Manage Kimi official accounts in the Authorization Center".into(),
                    ));
                }
                None => {}
            }
        }
        if app_type == AppType::Pi {
            return pi::update(state, original_id, provider);
        }

        if app_type == AppType::Omp {
            return omp::update(state, original_id, provider);
        }

        let original_id = original_id.unwrap_or(provider.id.as_str()).to_string();
        let provider_id_changed = original_id != provider.id;
        // 读旧行、判断谁是当前供应商、存行、写 live 都在切换锁里：进入代理和切换都等这次
        // 保存写完，这里也不会在读完模式之后、写 live 之前被它们改掉模式（否则直连的关键
        // 字段会盖掉刚写的代理契约）。Codex 还要在锁里读旧行，看它是不是托管账号的行。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;
        let existing_provider = state
            .db
            .get_provider_by_id(&original_id, app_type.as_str())?;
        // Normalize Claude model keys
        Self::normalize_provider_if_claude(&app_type, &mut provider);
        Self::validate_provider_settings(&app_type, &provider)?;
        if matches!(app_type, AppType::Codex) && provider.category.as_deref() == Some("official") {
            crate::codex_config::strip_codex_unified_session_bucket_from_settings(
                &mut provider.settings_config,
            )?;
        }
        Self::normalize_usage_script_credential_overrides(&app_type, &mut provider);

        if provider_id_changed {
            if !app_type.is_additive_mode() {
                return Err(AppError::Message(
                    "Only additive-mode providers support changing provider key".to_string(),
                ));
            }

            let Some(existing_provider) = existing_provider else {
                return Err(AppError::Message(format!(
                    "Original provider '{}' does not exist in app '{}'",
                    original_id,
                    app_type.as_str()
                )));
            };

            // OMO / OMO Slim providers are activated via a dedicated current-state mechanism
            // (set_omo_provider_current) that is NOT captured by provider_exists_in_live_config,
            // which only checks opencode.json. A rename would orphan that current-state marker
            // and silently break subsequent OMO file syncs. Block it unconditionally.
            if matches!(app_type, AppType::OpenCode)
                && matches!(
                    existing_provider.category.as_deref(),
                    Some("omo") | Some("omo-slim")
                )
            {
                return Err(AppError::Message(
                    "Provider key cannot be changed for OMO/OMO Slim providers".to_string(),
                ));
            }

            let original_in_live = Self::check_live_config_exists(
                &app_type,
                &original_id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if original_in_live {
                return Err(AppError::Message(
                    "Provider key cannot be changed after the provider has been added to the app config"
                        .to_string(),
                ));
            }

            let next_id_in_live = Self::check_live_config_exists(
                &app_type,
                &provider.id,
                Self::provider_live_config_managed(&existing_provider),
            )?;
            if state
                .db
                .get_provider_by_id(&provider.id, app_type.as_str())?
                .is_some()
                || next_id_in_live
            {
                return Err(AppError::Message(format!(
                    "Provider '{}' already exists in app '{}'",
                    provider.id,
                    app_type.as_str()
                )));
            }

            Self::set_provider_live_config_managed(&mut provider, false);
            let was_current = crate::mode::current::local_direct_pointer(&app_type).as_deref()
                == Some(original_id.as_str());
            state.db.save_provider(app_type.as_str(), &provider)?;
            state.db.delete_provider(app_type.as_str(), &original_id)?;

            if was_current {
                crate::settings::set_current_provider(&app_type, Some(provider.id.as_str()))?;
            }

            return Ok(true);
        }

        // Additive mode apps (OpenCode, OpenClaw): only sync to live when the provider
        // already exists in live config. Editing a DB-only provider must not auto-add it.
        if app_type.is_additive_mode() {
            let omo_variant = if matches!(app_type, AppType::OpenCode) {
                match provider.category.as_deref() {
                    Some("omo") => Some(&crate::services::omo::STANDARD),
                    Some("omo-slim") => Some(&crate::services::omo::SLIM),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(variant) = omo_variant {
                let is_current = state.db.is_omo_provider_current(
                    app_type.as_str(),
                    &provider.id,
                    variant.category,
                )?;
                if is_current {
                    crate::services::OmoService::write_provider_config_to_file(&provider, variant)?;
                }
                if let Err(err) = state.db.save_provider(app_type.as_str(), &provider) {
                    if is_current {
                        if let Err(rollback_err) =
                            crate::services::OmoService::write_config_to_file(state, variant)
                        {
                            log::warn!(
                                "Failed to roll back {} config after DB save error: {}",
                                variant.label,
                                rollback_err
                            );
                        }
                    }
                    return Err(err);
                }
                return Ok(true);
            }
            let live_config_managed = Self::check_live_config_exists(
                &app_type,
                &provider.id,
                Self::provider_live_config_managed(&provider).or_else(|| {
                    existing_provider
                        .as_ref()
                        .and_then(Self::provider_live_config_managed)
                }),
            )?;
            Self::set_provider_live_config_managed(&mut provider, live_config_managed);

            if matches!(app_type, AppType::Mcode | AppType::Kimi | AppType::Dsh) {
                return Self::save_native_provider(
                    state,
                    &app_type,
                    &provider,
                    live_config_managed,
                    false,
                );
            }

            // Save to database after live-config presence is resolved so parse errors
            // do not report failure after already mutating DB state.
            state.db.save_provider(app_type.as_str(), &provider)?;

            if !live_config_managed {
                return Ok(true);
            }
            write_live_for_state(state, &app_type, &provider)?;
            return Ok(true);
        }

        // For other apps: 是否是直连指针那家。代理模式下是不是代理路由或 Stack 名单里的一家，
        // 由同步 live 的那一步判断。
        let mode = crate::mode::current::mode_state(&app_type);
        let is_direct_current = crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::Direct,
        )?
        .as_deref()
            == Some(provider.id.as_str());

        if matches!(app_type, AppType::Codex) {
            return Self::update_codex(
                state,
                &provider,
                existing_provider.as_ref(),
                is_direct_current,
            );
        }
        // Desktop 直连模式下正在用的这张卡改成了要经路由的（模型映射、托管账号）：直连写不
        // 进去，存了行也同步不到 Desktop。先进入路由模式再改。
        if matches!(app_type, AppType::ClaudeDesktop)
            && is_direct_current
            && !mode.is_proxy()
            && crate::claude_desktop_config::needs_routing(&provider)
        {
            return Err(desktop_needs_routing_error());
        }

        // Save to database
        state.db.save_provider(app_type.as_str(), &provider)?;

        if is_direct_current || mode.is_proxy() {
            let outcome = live::sync_live_for_provider_respecting_mode(
                state,
                &app_type,
                &provider,
                existing_provider.as_ref(),
            )?;
            if outcome == LiveSyncOutcome::WroteLive {
                // MCP is stored in the database and projected after a successful
                // live write. Keep the failure best-effort so the provider save
                // itself is not reported as failed when MCP projection can retry.
                if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
                    log::warn!(
                        "保存供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}"
                    );
                }
            }
        }

        Ok(true)
    }

    /// 保存 Codex 供应商。调用方持有这个应用的切换锁。
    ///
    /// - 直连模式下编辑直连那家：先存行，再只替换 live 里的关键字段和独有字段（换托管
    ///   账号时先采纳、再清掉旧账号的登录）；写 live 失败就把行恢复原样。
    /// - 代理模式下先存行，编辑的是路由那家或 Stack 名单里的一家时按新行重写代理契约（见
    ///   [`Self::resync_proxy_for_saved_row`]），写失败同样把行恢复原样。
    /// - 其余只存行。
    fn update_codex(
        state: &AppState,
        provider: &Provider,
        existing: Option<&Provider>,
        is_direct_current: bool,
    ) -> Result<bool, AppError> {
        let app_type = AppType::Codex;
        let mode = crate::mode::current::mode_state(&app_type);
        if !mode.is_proxy() && !is_direct_current {
            state.db.save_provider(app_type.as_str(), provider)?;
            return Ok(true);
        }
        if !mode.is_proxy() {
            codex_direct::preflight(state.db.as_ref(), provider)?;
        }

        state.db.save_provider(app_type.as_str(), provider)?;
        let written = if mode.is_proxy() {
            Self::resync_proxy_for_saved_row(state, &app_type, provider)
        } else {
            codex_direct::write_direct(
                state.db.as_ref(),
                &state.codex_oauth_manager,
                crate::mode::state::op::APPLY,
                existing.map_or(codex_direct::Owner::None, codex_direct::Owner::Provider),
                Some(provider),
                crate::mode::state::PendingTarget::default(),
            )
            .map(|_| ())
        };
        if let Err(error) = written {
            if crate::mode::operation::has_pending(AppType::Codex.as_str()) {
                return Err(error);
            }
            if let Some(existing) = existing {
                if let Err(rollback) = state.db.save_provider(app_type.as_str(), existing) {
                    return Err(AppError::Message(format!(
                        "更新 Codex 供应商失败: {error}; 恢复供应商数据同时失败: {rollback}"
                    )));
                }
            }
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) fn update_pi_usage_script(
        state: &AppState,
        app_type: AppType,
        id: &str,
        script: crate::provider::UsageScript,
    ) -> Result<bool, AppError> {
        pi::update_usage_script(state, app_type, id, script)
    }

    /// Delete a provider
    ///
    /// 直连指针或代理路由指着的供应商不能删（见 `mode::current::is_referenced`）。
    /// 对于累加模式应用（OpenCode, OpenClaw），可以随时删除任意供应商，同时从 live 配置中移除。
    pub fn delete(state: &AppState, app_type: AppType, id: &str) -> Result<(), AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type == AppType::Pi {
            return pi::delete(state, id);
        }
        if matches!(app_type, AppType::Dsh) {
            return crate::dsh_config::save_and_commit(id, None, false, || {
                state.db.delete_provider(app_type.as_str(), id)
            });
        }
        if app_type == AppType::Kimi {
            Self::ensure_kimi_provider_editable(id)?;
            let saved = state.db.get_provider_by_id(id, "kimi")?;
            crate::kimi_config::save_and_commit(id, None, false, || {
                state.db.delete_provider("kimi", id)
            })?;
            // The official node is already gone from Kimi (otherwise editing is
            // refused above); its CC Switch-owned credential goes with the card.
            if let Some(saved) = saved {
                if let Err(error) =
                    crate::services::kimi_oauth::forget_removed_profile(id, &saved.settings_config)
                {
                    log::warn!("[Kimi] 删除卡片后清理凭据失败: {error}");
                }
            }
            return Ok(());
        }

        if app_type == AppType::Omp {
            return omp::delete(state, id);
        }

        // Additive mode apps - no current provider concept
        if app_type.is_additive_mode() {
            // Single DB read shared across all additive-mode sub-paths below.
            let existing = state.db.get_provider_by_id(id, app_type.as_str())?;

            if matches!(app_type, AppType::OpenCode) {
                let provider_category = existing.as_ref().and_then(|p| p.category.clone());
                let omo_variant = match provider_category.as_deref() {
                    Some("omo") => Some(&crate::services::omo::STANDARD),
                    Some("omo-slim") => Some(&crate::services::omo::SLIM),
                    _ => None,
                };
                if let Some(variant) = omo_variant {
                    let was_current = state.db.is_omo_provider_current(
                        app_type.as_str(),
                        id,
                        variant.category,
                    )?;
                    state.db.delete_provider(app_type.as_str(), id)?;
                    if was_current {
                        crate::services::OmoService::delete_config_file(variant)?;
                    }
                    return Ok(());
                }
            }

            // Non-OMO path for both OpenCode and OpenClaw:
            // remove from live first (atomicity), then DB.
            //
            // Use check_live_config_exists rather than trusting the flag alone: the flag
            // can be stale (Some(false) for a provider that was written to live before the
            // live_config_managed flip was introduced). check_live_config_exists reads the
            // actual file when the flag is Some(false), so it handles historical data correctly.
            let live_managed = existing
                .as_ref()
                .and_then(Self::provider_live_config_managed);
            if Self::check_live_config_exists(&app_type, id, live_managed)? {
                match app_type {
                    AppType::OpenCode => remove_opencode_provider_from_live(id)?,
                    AppType::OpenClaw => remove_openclaw_provider_from_live(id)?,
                    AppType::Hermes => remove_hermes_provider_from_live(id)?,
                    AppType::Mcode => crate::mcode_config::remove_provider(id)?,
                    AppType::Kimi => crate::kimi_config::remove_provider(id)?,
                    AppType::Dsh => crate::dsh_config::remove_provider(id)?,
                    _ => {}
                }
            }
            state.db.delete_provider(app_type.as_str(), id)?;
            return Ok(());
        }

        // For other apps: 本地记录、DB、代理路由任何一处指着它都不能删。Antigravity 没有
        // 「启用」，旧版留下的直连指针不拦删除。
        if !matches!(app_type, AppType::Agy | AppType::Copilot)
            && crate::mode::current::is_referenced(&state.db, &app_type, id)?
        {
            return Err(AppError::Message(
                "无法删除当前正在使用的供应商".to_string(),
            ));
        }

        // Stack 名单里的先移出（和客户端文件同一个操作提交，key 留在登记簿里），成功了再删行。
        // 删行失败时它已经不在名单里，重新加入即可。
        if crate::mode::stack::is_member(&app_type, id)? {
            futures::executor::block_on(crate::mode::controller::set_stack_member(
                state, &app_type, id, false,
            ))
            .map_err(|error| AppError::Message(error.message))?;
        }

        state.db.delete_provider(app_type.as_str(), id)?;
        Ok(())
    }

    /// Kimi「添加」：把卡片写进 config.toml，但不改 default_model（「切换」才改）。
    /// 其他累加式应用的添加就是 `switch`，不走这里。
    pub fn add_to_live_config(
        state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<(), AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type != AppType::Kimi {
            return Err(AppError::InvalidInput(format!(
                "{} adds providers through switch",
                app_type.as_str()
            )));
        }
        let mut provider = state
            .db
            .get_provider_by_id(id, "kimi")?
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;
        crate::services::kimi_oauth::ensure_profile_signed_in(&provider.settings_config)?;
        Self::set_provider_live_config_managed(&mut provider, true);
        crate::kimi_config::add_and_commit(id, provider.settings_config.clone(), || {
            state.db.save_provider("kimi", &provider)
        })
    }

    /// Remove provider from live config only (for additive mode apps like OpenCode, OpenClaw)
    ///
    /// Does NOT delete from database - provider remains in the list.
    /// This is used when user wants to "remove" a provider from active config
    /// but keep it available for future use.
    pub fn remove_from_live_config(
        state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<(), AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type == AppType::Pi {
            return pi::remove(state, id);
        }
        if matches!(app_type, AppType::Dsh) {
            return crate::dsh_config::save_and_commit(id, None, false, || {
                if let Some(mut provider) = state.db.get_provider_by_id(id, app_type.as_str())? {
                    Self::set_provider_live_config_managed(&mut provider, false);
                    state.db.save_provider(app_type.as_str(), &provider)?;
                }
                Ok(())
            });
        }
        if app_type == AppType::Kimi {
            // An official account leaves Kimi's config but keeps its login and card,
            // so「添加」can put it back.
            return crate::kimi_config::save_and_commit(id, None, false, || {
                if let Some(mut provider) = state.db.get_provider_by_id(id, "kimi")? {
                    Self::set_provider_live_config_managed(&mut provider, false);
                    state.db.save_provider("kimi", &provider)?;
                }
                Ok(())
            });
        }

        if app_type == AppType::Omp {
            return omp::remove(state, id);
        }

        // 一次读取供应商行：OpenCode 用来判断 OMO 变体，收尾用来更新 liveConfigManaged。
        let provider = state.db.get_provider_by_id(id, app_type.as_str())?;

        match app_type {
            AppType::OpenCode => {
                let provider_category = provider.as_ref().and_then(|p| p.category.clone());

                let omo_variant = match provider_category.as_deref() {
                    Some("omo") => Some(&crate::services::omo::STANDARD),
                    Some("omo-slim") => Some(&crate::services::omo::SLIM),
                    _ => None,
                };
                if let Some(variant) = omo_variant {
                    state
                        .db
                        .clear_omo_provider_current(app_type.as_str(), id, variant.category)?;
                    let still_has_current = state
                        .db
                        .get_current_omo_provider("opencode", variant.category)?
                        .is_some();
                    if still_has_current {
                        crate::services::OmoService::write_config_to_file(state, variant)?;
                    } else {
                        crate::services::OmoService::delete_config_file(variant)?;
                    }
                } else {
                    remove_opencode_provider_from_live(id)?;
                }
            }
            AppType::OpenClaw => {
                remove_openclaw_provider_from_live(id)?;
            }
            AppType::Hermes => {
                remove_hermes_provider_from_live(id)?;
            }
            AppType::Mcode => crate::mcode_config::remove_provider(id)?,
            AppType::Kimi => crate::kimi_config::remove_provider(id)?,
            AppType::Dsh => crate::dsh_config::remove_provider(id)?,
            _ => {
                return Err(AppError::Message(format!(
                    "App {} does not support remove from live config",
                    app_type.as_str()
                )));
            }
        }

        if let Some(mut provider) = provider {
            if Self::provider_live_config_managed(&provider) != Some(false) {
                Self::set_provider_live_config_managed(&mut provider, false);
                state.db.save_provider(app_type.as_str(), &provider)?;
            }
        }

        Ok(())
    }

    /// The signed-in connection node of a Kimi official card: Kimi's live node
    /// first, else the saved card (an official card removed from Kimi's config).
    fn kimi_official_connection(state: &AppState, id: &str) -> Result<Option<Value>, AppError> {
        if let Some(live) = crate::kimi_config::get_providers()?.get(id) {
            return Ok(
                crate::kimi_config::is_official_provider(live).then(|| live["provider"].clone())
            );
        }
        Ok(state
            .db
            .get_provider_by_id(id, "kimi")?
            .filter(|saved| crate::kimi_config::is_official_provider(&saved.settings_config))
            .map(|saved| saved.settings_config["provider"].clone()))
    }

    /// Deleting an official account that is still in Kimi's config goes through
    /// the Authorization Center (it also deletes the login); remove it first.
    fn ensure_kimi_provider_editable(id: &str) -> Result<(), AppError> {
        let native = crate::kimi_config::get_providers()?;
        if native
            .get(id)
            .is_some_and(crate::kimi_config::is_official_provider)
        {
            return Err(AppError::InvalidInput(
                "Manage Kimi official accounts in the Authorization Center".into(),
            ));
        }
        Ok(())
    }

    /// 切换供应商。
    ///
    /// - 代理模式：只换代理路由，直连指针不变；契约没变时客户端文件不读也不写。
    /// - 直连模式：切换式应用只替换客户端文件里的关键字段（不回填），文件和指针在同一个
    ///   操作里提交；累加式应用按各自的规则写入。
    pub fn switch(state: &AppState, app_type: AppType, id: &str) -> Result<SwitchResult, AppError> {
        let app = app_type.as_str().to_string();
        let result = Self::switch_inner(state, app_type, id);
        match &result {
            Ok(result) if result.warnings.is_empty() => {
                log::info!("[SWITCH] {app} 切到 {id} 完成");
            }
            Ok(result) => {
                log::warn!(
                    "[SWITCH] {app} 切到 {id} 完成，有警告: {:?}",
                    result.warnings
                );
            }
            // 规则拒绝：什么都没改。
            Err(error @ AppError::Localized { key, .. })
                if *key == "switch.official_blocked_by_proxy"
                    || *key == "agy.switch.unsupported"
                    || *key == "copilot.switch.unsupported" =>
            {
                log::info!("[SWITCH] {app} 拒绝切到 {id}: {error}");
            }
            Err(error) => log::error!(
                "[SWITCH] {app} 切到 {id} 失败: {}",
                crate::error_for_log(&error.to_string())
            ),
        }
        result
    }

    fn switch_inner(
        state: &AppState,
        app_type: AppType,
        id: &str,
    ) -> Result<SwitchResult, AppError> {
        let _native_guard = lock_native_provider_operations(state, &app_type);
        if app_type == AppType::Pi {
            return pi::enable(state, id);
        }
        // Antigravity 的卡只作用于它打开的终端，没有「启用」（见 agy_config 模块说明）。
        if app_type == AppType::Agy {
            return Err(AppError::localized(
                "agy.switch.unsupported",
                "Antigravity 的卡片不需要启用，用「打开终端」启动即可",
                "Antigravity cards are not enabled; use Open terminal to start one",
            ));
        }
        // Copilot CLI 同理：自带模型只能经进程环境传入（见 copilot_config 模块说明）。
        if app_type == AppType::Copilot {
            return Err(AppError::localized(
                "copilot.switch.unsupported",
                "GitHub Copilot CLI 的卡片不需要启用，用「打开终端」启动即可",
                "GitHub Copilot CLI cards are not enabled; use Open terminal to start one",
            ));
        }

        if app_type == AppType::Omp {
            return omp::enable(state, id);
        }

        // Check if provider exists
        let mut provider = state
            .db
            .get_provider_by_id(id, app_type.as_str())?
            .ok_or_else(|| AppError::Message(format!("供应商 {id} 不存在")))?;

        if matches!(app_type, AppType::Dsh) {
            let mut dsh_provider = provider.clone();
            Self::set_provider_live_config_managed(&mut dsh_provider, true);
            crate::dsh_config::save_and_commit(
                id,
                Some(dsh_provider.settings_config.clone()),
                false,
                || state.db.save_provider(app_type.as_str(), &dsh_provider),
            )?;
            return Ok(SwitchResult::default());
        }
        if app_type == AppType::Kimi {
            crate::services::kimi_oauth::ensure_profile_signed_in(&provider.settings_config)?;
            Self::set_provider_live_config_managed(&mut provider, true);
            crate::kimi_config::switch_and_commit(
                id,
                provider.settings_config.clone(),
                |config| {
                    provider.settings_config = config;
                    state.db.save_provider("kimi", &provider)
                },
            )?;
            return Ok(SwitchResult::default());
        }

        // OMO providers are switched through their own exclusive path.
        if matches!(app_type, AppType::OpenCode) && provider.category.as_deref() == Some("omo") {
            return Self::switch_normal(state, app_type, id, &provider);
        }

        // OMO Slim providers are switched through their own exclusive path.
        if matches!(app_type, AppType::OpenCode) && provider.category.as_deref() == Some("omo-slim")
        {
            return Self::switch_normal(state, app_type, id, &provider);
        }

        // 切换和进入 / 退出代理都会改客户端文件和指针。按应用串行，拿到锁、补完上一次
        // 没做完的写入之后再读模式和指针：刚进入代理的应用不会被一次直连写入覆盖，上次
        // 失败后重试也按补完后的指针删上一家的独有字段。
        let _switch_guard = crate::mode::controller::lock_settled_blocking(state, &app_type)?;

        if crate::mode::current::is_proxy(&app_type) {
            // 代理模式：只换代理路由，直连指针不变。契约没变时客户端文件不读也不写；
            // 变了在同一个操作里先改写客户端，再发布路由。
            if provider.category.as_deref() == Some("official")
                && !official_provider_supports_proxy_takeover(&app_type, &provider)
            {
                return Err(AppError::localized(
                    "switch.official_blocked_by_proxy",
                    "路由模式下不能切换到官方供应商，使用代理访问官方 API 可能导致账号被封禁。请先退出路由模式，或选择第三方供应商。",
                    "Cannot switch to an official provider in routing mode. Using a proxy with official APIs may cause account bans.",
                ));
            }
            log::info!(
                "[SWITCH] {} 代理模式：路由 {} → {}",
                app_type.as_str(),
                crate::mode::current::mode_state(&app_type)
                    .proxy_route
                    .as_deref()
                    .unwrap_or("（无）"),
                id
            );
            let route = futures::executor::block_on(crate::mode::controller::switch_route_locked(
                state, &app_type, &provider,
            ))
            .map_err(|e| AppError::Message(format!("切换路由失败: {e}")))?;
            // MCP 不随路由变：客户端文件没按直连重写。
            return Ok(SwitchResult {
                route: Some(route),
                ..Default::default()
            });
        }

        // Normal mode: full switch with Live config write
        Self::switch_normal(state, app_type, id, &provider)
    }

    /// Normal switch flow (non-proxy mode)
    fn switch_normal(
        state: &AppState,
        app_type: AppType,
        id: &str,
        provider: &Provider,
    ) -> Result<SwitchResult, AppError> {
        if app_type.is_additive_mode() {
            log::info!("[SWITCH] {} 写入 {id}（累加式应用）", app_type.as_str());
        } else {
            let previous = crate::mode::current::provider_for(
                &state.db,
                &app_type,
                crate::mode::current::Purpose::Direct,
            )
            .ok()
            .flatten();
            log::info!(
                "[SWITCH] {} 直连：{} → {id}，写客户端配置",
                app_type.as_str(),
                previous.as_deref().unwrap_or("（无）")
            );
        }

        if matches!(app_type, AppType::Claude) {
            return Self::switch_claude_direct(state, provider);
        }
        if matches!(app_type, AppType::Codex) {
            return Self::switch_codex_direct(state, provider);
        }
        if matches!(app_type, AppType::Gemini) {
            gemini_direct::switch_to(state.db.as_ref(), provider)?;
            return Ok(SwitchResult::default());
        }
        if matches!(app_type, AppType::GrokBuild) {
            return Self::switch_grok_direct(state, provider);
        }
        if matches!(app_type, AppType::ClaudeDesktop) {
            return Self::switch_desktop_direct(state, provider);
        }

        // OMO ↔ OMO Slim are mutually exclusive; activating one removes the other's config file.
        if matches!(app_type, AppType::OpenCode) {
            let omo_pair = match provider.category.as_deref() {
                Some("omo") => Some((&crate::services::omo::STANDARD, &crate::services::omo::SLIM)),
                Some("omo-slim") => {
                    Some((&crate::services::omo::SLIM, &crate::services::omo::STANDARD))
                }
                _ => None,
            };
            if let Some((enable, disable)) = omo_pair {
                state
                    .db
                    .set_omo_provider_current(app_type.as_str(), id, enable.category)?;
                crate::services::OmoService::write_config_to_file(state, enable)?;
                let _ = crate::services::OmoService::delete_config_file(disable);
                return Ok(SwitchResult::default());
            }
        }

        let mut result = SwitchResult::default();

        // Additive mode apps skip setting is_current (no such concept).
        if !app_type.is_additive_mode() {
            crate::settings::set_current_provider(&app_type, Some(id))?;
            state.db.set_current_provider(app_type.as_str(), id)?;
        }

        // 写 live（Claude Desktop、累加式应用；切换式应用在上面各自的分支里写完了）。
        write_live_for_state(state, &app_type, provider)?;

        // Hermes is additive, so "switching" doesn't overwrite a live config file
        // — we instead update the top-level `model:` section to point at this
        // provider's first declared model. Without this, clicking "switch" would
        // only shuffle entries in custom_providers[] while Hermes keeps using
        // whatever `model.provider` was set before.
        if matches!(app_type, AppType::Hermes) {
            if let Err(e) =
                crate::hermes_config::apply_switch_defaults(&provider.id, &provider.settings_config)
            {
                log::warn!(
                    "Failed to update Hermes model defaults after switching to '{}': {e}",
                    provider.id
                );
                result
                    .warnings
                    .push(format!("hermes_model_defaults_failed:{}", provider.id));
            }
        }

        // For additive-mode providers that were DB-only (live_config_managed == Some(false)),
        // flip the flag to true now that the provider has been successfully written to the live
        // file. This ensures sync_all_providers_to_live() will include it on future syncs.
        //
        // If persisting the marker fails, roll back the just-written live config so we don't leave
        // the provider in a silent inconsistent state (present in live, but still marked DB-only).
        if app_type.is_additive_mode() && Self::provider_live_config_managed(provider) != Some(true)
        {
            let mut updated = provider.clone();
            Self::set_provider_live_config_managed(&mut updated, true);
            if let Err(e) = state.db.save_provider(app_type.as_str(), &updated) {
                let rollback_result = match app_type {
                    AppType::OpenCode => remove_opencode_provider_from_live(&provider.id),
                    AppType::OpenClaw => remove_openclaw_provider_from_live(&provider.id),
                    AppType::Hermes => remove_hermes_provider_from_live(&provider.id),
                    AppType::Mcode => crate::mcode_config::remove_provider(&provider.id),
                    AppType::Kimi => crate::kimi_config::remove_provider(&provider.id),
                    AppType::Dsh => crate::dsh_config::remove_provider(&provider.id),
                    _ => Ok(()),
                };

                match rollback_result {
                    Ok(()) => {
                        return Err(AppError::Message(format!(
                            "Failed to persist live_config_managed for '{}' after writing live config; live changes were rolled back: {e}",
                            provider.id
                        )));
                    }
                    Err(rollback_err) => {
                        return Err(AppError::Message(format!(
                            "Failed to persist live_config_managed for '{}' after writing live config: {e}; additionally failed to roll back live config: {rollback_err}",
                            provider.id
                        )));
                    }
                }
            }
        }

        // 切换重写了目标应用的 live，只重投影该应用的 MCP（Grok Build 的
        // [mcp_servers] 与 live 同文件，整体替换后必须补回；其余应用的
        // MCP 文件独立于 live，投影是幂等维护）。不用全量 sync_all_enabled：
        // 无关应用的 live 损坏（如 ~/.claude.json 坏 JSON）不该阻断切换。
        // 走到这里 DB is_current 与 live 都已落盘，切换事实上已成功；
        // 投影失败上抛会让前端报"切换失败"制造分裂假象，故降级为警告
        // （MCP 投影可自愈：下次切换 / 任一 MCP 启停都会重新投影）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &app_type) {
            log::warn!("切换供应商后重投影 {app_type:?} MCP 失败（将在下次同步时自愈）: {err}");
        }

        Ok(result)
    }

    /// Claude Desktop 直连切换：配置文件和指针在同一个操作里提交。模型映射卡、托管 OAuth 要经
    /// 本地路由，直连写不进去（界面上先弹「需要路由」）。
    fn switch_desktop_direct(
        state: &AppState,
        provider: &Provider,
    ) -> Result<SwitchResult, AppError> {
        if crate::claude_desktop_config::needs_routing(provider) {
            return Err(desktop_needs_routing_error());
        }
        crate::claude_desktop_config::write_direct(
            state.db.as_ref(),
            crate::mode::state::op::SWITCH,
            Some(provider),
            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
        )?;
        Ok(SwitchResult::default())
    }

    /// Claude Code 直连切换：只替换关键字段和独有字段，文件和指针在同一个操作里提交。
    ///
    /// 不回填、不同步通用配置片段：用户在 live 里的改动本来就留在原处。发布前的失败
    /// （解析不了、并发冲突）什么都不改；开始发布后由 pending 保证前滚补完。
    fn switch_claude_direct(
        state: &AppState,
        provider: &Provider,
    ) -> Result<SwitchResult, AppError> {
        let current_id = crate::mode::current::provider_for(
            &state.db,
            &AppType::Claude,
            crate::mode::current::Purpose::Direct,
        )?;
        let prev = match current_id.as_deref() {
            Some(id) => state.db.get_provider_by_id(id, AppType::Claude.as_str())?,
            None => None,
        };
        claude_direct::switch_to(state.db.as_ref(), prev.as_ref(), provider)?;

        // MCP 在 ~/.claude.json，和 settings.json 无关；重投影是幂等维护，失败只记警告
        // （切换已经提交，下次同步会自愈）。
        if let Err(err) = McpService::sync_enabled_for_app(state, &AppType::Claude) {
            log::warn!("切换供应商后重投影 claude MCP 失败（将在下次同步时自愈）: {err}");
        }
        Ok(SwitchResult::default())
    }

    /// Codex 直连切换：`config.toml` 只替换关键字段和独有字段；`auth.json`、模型目录、
    /// 托管账号标记和指针在同一个操作里提交。
    ///
    /// 不回填、不同步通用配置片段、不补回 MCP：用户在 live 里的改动（含 `[mcp_servers]`）
    /// 本来就留在原处。行有问题（会把官方登录发给第三方、带 Key 却没地方放）时在写任何
    /// 东西之前报错，指针也不动。
    fn switch_codex_direct(
        state: &AppState,
        provider: &Provider,
    ) -> Result<SwitchResult, AppError> {
        let current_id = crate::mode::current::provider_for(
            &state.db,
            &AppType::Codex,
            crate::mode::current::Purpose::Direct,
        )?;
        let owner_provider = match current_id.as_deref() {
            Some(id) => state.db.get_provider_by_id(id, AppType::Codex.as_str())?,
            None => None,
        };
        let owner = owner_provider
            .as_ref()
            .map_or(codex_direct::Owner::None, codex_direct::Owner::Provider);
        codex_direct::write_direct(
            state.db.as_ref(),
            &state.codex_oauth_manager,
            crate::mode::state::op::SWITCH,
            owner,
            Some(provider),
            crate::mode::state::PendingTarget::pointer(Some(provider.id.clone())),
        )?;

        let mut result = SwitchResult::default();
        // 保留登录关闭时切到第三方要删掉 auth.json。删不掉（只读目录、被占用）不让切换
        // 失败：配置和指针都已提交，但要让用户看到官方登录还在盘上。
        if !codex_direct::is_official(provider)
            && !crate::settings::preserve_codex_official_auth_on_switch()
            && crate::codex_config::get_codex_auth_path().exists()
        {
            log::warn!("Codex auth.json still present after a preservation-off third-party switch");
            result
                .warnings
                .push("codex_auth_cleanup_failed".to_string());
        }
        Ok(result)
    }

    /// Grok Build 直连切换：`models.default` 和模型表换成目标的，按写入记录删掉上一家
    /// 的表；文件、指针和写入记录在同一个操作里提交。不回填、不补回 MCP。
    fn switch_grok_direct(state: &AppState, provider: &Provider) -> Result<SwitchResult, AppError> {
        let current_id = crate::mode::current::provider_for(
            &state.db,
            &AppType::GrokBuild,
            crate::mode::current::Purpose::Direct,
        )?;
        let live_owner = match current_id.as_deref() {
            Some(id) => state
                .db
                .get_provider_by_id(id, AppType::GrokBuild.as_str())?,
            None => None,
        };
        grok_direct::switch_to(state.db.as_ref(), live_owner.as_ref(), provider)?;
        Ok(SwitchResult::default())
    }

    /// Sync current provider to live configuration (re-export)
    pub fn sync_current_to_live(state: &AppState) -> Result<(), AppError> {
        sync_current_to_live(state)
    }

    pub fn sync_current_provider_for_app(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return sync_additive_app_to_live(state, &app_type);
        }
        // 没有正在用的那家、或者在代理模式（客户端文件没按直连重写）时不重投影 MCP。
        let outcome = live::sync_current_provider_for_app_respecting_mode(state, &app_type)?;
        if outcome != Some(LiveSyncOutcome::WroteLive) {
            return Ok(());
        }
        McpService::sync_enabled_for_app(state, &app_type)
    }

    pub fn migrate_legacy_common_config_usage(
        state: &AppState,
        app_type: AppType,
        legacy_snippet: &str,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() || legacy_snippet.trim().is_empty() {
            return Ok(());
        }

        let providers = state.db.get_all_providers(app_type.as_str())?;

        for provider in providers.values() {
            if provider
                .meta
                .as_ref()
                .and_then(|meta| meta.common_config_enabled)
                .is_some()
            {
                continue;
            }

            if !live::provider_uses_common_config(&app_type, provider, Some(legacy_snippet)) {
                continue;
            }

            let mut updated_provider = provider.clone();
            updated_provider
                .meta
                .get_or_insert_with(Default::default)
                .common_config_enabled = Some(true);

            match live::remove_common_config_from_settings(
                &app_type,
                &updated_provider.settings_config,
                legacy_snippet,
            ) {
                Ok(settings) => updated_provider.settings_config = settings,
                Err(err) => {
                    log::warn!(
                        "Failed to normalize legacy common config for {} provider '{}': {err}",
                        app_type.as_str(),
                        updated_provider.id
                    );
                }
            }

            state
                .db
                .save_provider(app_type.as_str(), &updated_provider)?;
        }

        Ok(())
    }

    pub fn migrate_legacy_common_config_usage_if_needed(
        state: &AppState,
        app_type: AppType,
    ) -> Result<(), AppError> {
        if app_type.is_additive_mode() {
            return Ok(());
        }

        let Some(snippet) = state.db.get_config_snippet(app_type.as_str())? else {
            return Ok(());
        };

        if snippet.trim().is_empty() {
            return Ok(());
        }

        Self::migrate_legacy_common_config_usage(state, app_type, &snippet)
    }

    /// Extract common config snippet from current provider
    ///
    /// Extracts the current provider's configuration and removes provider-specific fields
    /// (API keys, model settings, endpoints) to create a reusable common config snippet.
    pub fn extract_common_config_snippet(
        state: &AppState,
        app_type: AppType,
    ) -> Result<String, AppError> {
        // Get current provider
        let current_id = Self::current(state, app_type)?;
        if current_id.is_empty() {
            return Err(AppError::Message("No current provider".to_string()));
        }

        let provider = state
            .db
            .get_provider_by_id(&current_id, app_type.as_str())?
            .ok_or_else(|| AppError::Message(format!("Provider {current_id} not found")))?;

        match app_type {
            AppType::Claude => Self::extract_claude_common_config(&provider.settings_config),
            AppType::ClaudeDesktop => Ok(String::new()),
            AppType::Codex => Self::extract_codex_common_config(&provider.settings_config),
            AppType::Gemini => Self::extract_gemini_common_config(&provider.settings_config),
            AppType::GrokBuild => Ok(String::new()),
            AppType::OpenCode => Self::extract_opencode_common_config(&provider.settings_config),
            AppType::OpenClaw => Self::extract_openclaw_common_config(&provider.settings_config),
            AppType::Hermes => Ok(String::new()), // Hermes doesn't use common config snippets
            AppType::Pi
            | AppType::Mcode
            | AppType::Kimi
            | AppType::Dsh
            | AppType::Omp
            | AppType::Agy
            | AppType::Copilot => Ok(String::new()),
        }
    }

    /// Extract common config snippet from a config value (e.g. editor content).
    pub fn extract_common_config_snippet_from_settings(
        app_type: AppType,
        settings_config: &Value,
    ) -> Result<String, AppError> {
        match app_type {
            AppType::Claude => Self::extract_claude_common_config(settings_config),
            AppType::ClaudeDesktop => Ok(String::new()),
            AppType::Codex => Self::extract_codex_common_config(settings_config),
            AppType::Gemini => Self::extract_gemini_common_config(settings_config),
            AppType::GrokBuild => Ok(String::new()),
            AppType::OpenCode => Self::extract_opencode_common_config(settings_config),
            AppType::OpenClaw => Self::extract_openclaw_common_config(settings_config),
            AppType::Hermes => Ok(String::new()), // Hermes doesn't use common config snippets
            AppType::Pi
            | AppType::Mcode
            | AppType::Kimi
            | AppType::Dsh
            | AppType::Omp
            | AppType::Agy
            | AppType::Copilot => Ok(String::new()),
        }
    }

    /// 判断一个 env / 顶层配置键名是否为凭据/机密：凡命中一律不得写入共享的
    /// 通用配置片段。**故意从严**——多剥一个非机密键只是它不被共享（可恢复的小
    /// 不便），漏剥一个凭据则会把密钥注入到每个供应商（不可恢复的泄漏）。因此用
    /// 模式匹配覆盖整类，而非枚举具体名字（枚举永远会漏掉下一个 `*_API_KEY`）。
    ///
    /// 覆盖：Anthropic / OpenRouter / Google / OpenAI / Gemini 等 `*_API_KEY`
    /// （Claude provider 的凭据见 `Provider::resolve_usage_credentials`，确实支持
    /// `OPENROUTER_API_KEY` / `GOOGLE_API_KEY` 等回退）、各类 `*_AUTH_TOKEN` /
    /// 单数 `*_TOKEN`、AWS Bedrock / Vertex 凭据、通用 secret / password /
    /// 私钥命名，以及发往上游的自定义请求头、Cookie、Authorization。
    pub(crate) fn is_sensitive_config_key(name: &str) -> bool {
        let upper = name.to_ascii_uppercase();

        // 单数 `_TOKEN` 命中 AWS_SESSION_TOKEN 等，但**不**误伤复数 `_TOKENS`
        // （CLAUDE_CODE_MAX_OUTPUT_TOKENS / MAX_THINKING_TOKENS 是正常可共享配置）。
        const SENSITIVE_SUFFIXES: &[&str] = &[
            // 裸 `_KEY` 是最常见的凭据写法（OPENAI_KEY / GROQ_KEY / XAI_KEY…），
            // 必须单列：只枚举 `_API_KEY` / `_ACCESS_KEY` 这些子类，等于把最普通
            // 的那一种漏在外面。下面几条 `_*_KEY` 被它蕴含，保留是为了说明覆盖面。
            "_KEY",
            "_API_KEY",
            "_ACCESS_KEY",
            "_ACCESS_KEY_ID",
            "_KEY_ID",
            "_PRIVATE_KEY",
            // 不带分隔符的复合写法各走各的后缀：`_KEY` 够不着 `..._APIKEY`
            // （倒数第四个字符是 I 不是下划线）。VOLC_ACCESSKEY 是火山引擎文档
            // 里的正式变量名，本仓库就实现了火山 AK/SK 用量查询。
            "_APIKEY",
            "_ACCESSKEY",
            "_SECRETKEY",
            "_APITOKEN",
            "_AUTH_TOKEN",
            "_TOKEN",
            // GITHUB_PAT / GITLAB_PAT 等 personal access token 的惯用写法，
            // 既不含 TOKEN 也不含 KEY，前面每一条规则都够不着。
            "_PAT",
            // 口令类的常见缩写。`_PASS` 不会误伤 `*_BYPASS`（那个以 `_BYPASS`
            // 结尾），`_PWD` 也不会误伤 shell 的 PWD / OLDPWD。
            "_PWD",
            "_PASS",
            "_PASSPHRASE",
            "_CREDS",
            // 发往上游的自定义请求头（ANTHROPIC_CUSTOM_HEADERS、
            // GEMINI_CLI_CUSTOM_HEADERS）：常见写法是 `Authorization: Bearer …`
            // 或 `Cookie: …`，整串就是凭据。只认 `_CUSTOM_HEADERS`，不按 HEADER
            // 一刀切：CLAUDE_CODE_ATTRIBUTION_HEADER 是普通开关，
            // OTEL_EXPORTER_OTLP_HEADERS 发往用户自己的遥测端点，都应照常共享。
            "_CUSTOM_HEADERS",
        ];
        const SENSITIVE_EXACT: &[&str] = &[
            "APIKEY",
            "API_KEY",
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "CREDENTIALS",
            "HEADERS",
        ];
        // contains：覆盖 AWS_SECRET_ACCESS_KEY / *_CLIENT_SECRET /
        // GOOGLE_APPLICATION_CREDENTIALS / AWS_BEARER_TOKEN_BEDROCK 等变体。
        const SENSITIVE_CONTAINS: &[&str] = &[
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "CREDENTIAL",
            "PRIVATE_KEY",
            "BEARER_TOKEN",
            "COOKIE",
            "AUTHORIZATION",
        ];

        SENSITIVE_EXACT.contains(&upper.as_str())
            || SENSITIVE_SUFFIXES.iter().any(|s| upper.ends_with(s))
            || SENSITIVE_CONTAINS.iter().any(|c| upper.contains(c))
    }

    /// Claude `env` 里的键能否进通用配置片段。
    ///
    /// 片段会合并进每一家勾选了它的供应商，所以只能放与供应商无关的设置。关键字段
    /// （请求发到哪、凭什么鉴权、哪个模型名、哪种协议）一旦进了片段，就会跟着切换带给
    /// 下一家：从 Bedrock 切到官方后，官方的 live 里还留着 `CLAUDE_CODE_USE_BEDROCK=1`，
    /// Claude Code 继续走 Bedrock。凭据另由 `is_sensitive_config_key` 统一剥离。
    fn claude_env_key_is_shared(key: &str) -> bool {
        // Context limits follow the actual upstream model. Sharing these
        // across providers can cap GPT/Kimi to the wrong window and make
        // Claude Code compact too early or miss the upstream limit.
        const UPSTREAM_WINDOW_KEYS: &[&str] = &[
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        ];

        !crate::live::floor::claude_floor_env(key)
            && !UPSTREAM_WINDOW_KEYS.contains(&key)
            && !Self::is_sensitive_config_key(key)
    }

    /// Claude 顶层键能否进通用配置片段，口径同 [`Self::claude_env_key_is_shared`]。
    /// `model` 是 `/model` 保存的选择，属于当时那一家。
    fn claude_top_key_is_shared(key: &str) -> bool {
        !crate::live::floor::claude_floor_top(key) && !Self::is_sensitive_config_key(key)
    }

    /// Extract common config for Claude (JSON format)
    fn extract_claude_common_config(settings: &Value) -> Result<String, AppError> {
        let mut config = settings.clone();

        if let Some(obj) = config.as_object_mut() {
            if let Some(Value::Object(env)) = obj.get_mut("env") {
                env.retain(|key, _| Self::claude_env_key_is_shared(key));
            }
            obj.retain(|key, value| match key.as_str() {
                "env" => !value.as_object().is_some_and(|env| env.is_empty()),
                _ => Self::claude_top_key_is_shared(key),
            });
        }

        // Check if result is empty
        if config.as_object().is_none_or(|obj| obj.is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Extract common config for Codex (TOML format)
    fn extract_codex_common_config(settings: &Value) -> Result<String, AppError> {
        // Codex config is stored as { "auth": {...}, "config": "toml string" }
        let config_toml = settings
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if config_toml.is_empty() {
            return Ok(String::new());
        }

        let mut doc = config_toml
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| AppError::Message(format!("TOML parse error: {e}")))?;

        // 关键字段（选路、模型名、推理档位等，见 `live::floor`）归供应商，不进片段：
        // 片段已冻结，收进去的值会从行里被迁移剥掉、再也写不回 live。
        let root = doc.as_table_mut();
        for key in crate::live::floor::CODEX_FLOOR_TOP {
            root.remove(key);
        }
        for path in crate::live::floor::CODEX_FLOOR_NESTED {
            let [parent, key] = path else { continue };
            if let Some(table) = root
                .get_mut(parent)
                .and_then(|item| item.as_table_like_mut())
            {
                table.remove(key);
                if table.is_empty() {
                    root.remove(parent);
                }
            }
        }

        // Remove entire model_providers table (provider-specific configuration)
        root.remove("model_providers");

        // MCP 服务器归 DB mcp_servers 表所有：进了共享片段会绕过按应用的
        // 启用状态被合并进所有勾选通用配置的供应商，且在通用配置编辑框里
        // 显示为一份"重复"的 MCP 配置。
        root.remove("mcp_servers");
        // 历史错误格式 [mcp.servers] 一并剥离（与 strip_codex_mcp_servers_from_settings
        // 一致）：sync_all_enabled 只管理 [mcp_servers.*]，legacy 形态一旦进了
        // 片段就会被合并进所有供应商，且没有任何同步路径能清掉这个孤儿。
        if let Some(mcp_tbl) = root
            .get_mut("mcp")
            .and_then(|item| item.as_table_like_mut())
        {
            mcp_tbl.remove("servers");
            if mcp_tbl.is_empty() {
                root.remove("mcp");
            }
        }

        // cc-switch 写 live 时注入的产物一律不进共享片段：
        // - experimental_bearer_token 正常写在 [model_providers.<id>] 内（上面
        //   整表已剥），但无活跃路由 / 内建保留 id / 路由表缺失三种 fallback
        //   会落在顶层——不剥等于把 API 密钥写进共享片段。
        root.remove("experimental_bearer_token");
        // - model_catalog_json 指向按供应商生成的 catalog 投影文件（DB 为 SSOT）。
        root.remove("model_catalog_json");
        // - web_search 只剥 cc-switch 注入的 "disabled" 哨兵；用户手设的其它值
        //   属于可共享偏好，保留。
        if root
            .get(crate::codex_config::CODEX_WEB_SEARCH_FIELD)
            .and_then(|item| item.as_str())
            == Some(crate::codex_config::CODEX_WEB_SEARCH_DISABLED)
        {
            root.remove(crate::codex_config::CODEX_WEB_SEARCH_FIELD);
        }

        // Clean up multiple empty lines (keep at most one blank line).
        let mut cleaned = String::new();
        let mut blank_run = 0usize;
        for line in doc.to_string().lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run <= 1 {
                    cleaned.push('\n');
                }
                continue;
            }
            blank_run = 0;
            cleaned.push_str(line);
            cleaned.push('\n');
        }

        Ok(cleaned.trim().to_string())
    }

    /// Extract common config for Gemini (JSON format)
    ///
    /// Extracts `.env` values while excluding provider-specific credentials:
    /// - GOOGLE_GEMINI_BASE_URL
    /// - GEMINI_API_KEY
    fn extract_gemini_common_config(settings: &Value) -> Result<String, AppError> {
        let env = settings.get("env").and_then(|v| v.as_object());

        let mut snippet = serde_json::Map::new();
        if let Some(env) = env {
            for (key, value) in env {
                // 关键字段（地址、凭据、模型名、Vertex 选择器）归供应商，不进片段：
                // 片段已冻结，新版不再合并它，收进去的值会从行里被迁移剥掉、再也写不回
                // live（首启导入的 `GEMINI_MODEL` 就是这样丢的）。凭据另交给
                // `is_sensitive_config_key` 统一模式匹配（与 Claude 提取器一致），
                // 兜住关键字段清单以外的 `*_API_KEY`、`*_TOKEN`。
                if crate::live::floor::gemini_floor_env(key) || Self::is_sensitive_config_key(key) {
                    continue;
                }
                let Value::String(v) = value else {
                    continue;
                };
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    snippet.insert(key.to_string(), Value::String(trimmed.to_string()));
                }
            }
        }

        if snippet.is_empty() {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&Value::Object(snippet))
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// 一次性清理：把历史泄漏进 Gemini 共享片段的凭据从所有存储位置抹掉。
    ///
    /// 背景：`extract_gemini_common_config` 曾只剥离两个固定键名，`GOOGLE_API_KEY`
    /// 等一等凭据会进入共享片段，再在切换时被深合并进
    /// **其它** Gemini 供应商的 env，随请求发往对方的 base_url。
    ///
    /// 光修提取器不够：Gemini 的片段一旦生成就**永不自动重提取**（启动期
    /// auto-extract 与导入后补提取都要求 `snippet.is_none()`，切换时的回写又只对
    /// Claude / Codex 生效），所以存量片段会一直带着密钥继续注入。
    ///
    /// 两个关键约束：
    ///
    /// 1. **不能只清片段**。合并与剥离是一对靠「值相等」严格抵消的操作：切走供应商时
    ///    `remove_common_config_from_settings` 依据片段内容把注入的键删掉。片段里一旦
    ///    没了这个键，backfill 就会把 live 中残留的密钥原样写进受害供应商的
    ///    `settings_config`——泄漏从瞬时污染变成永久污染。所以片段、各供应商配置、
    ///    live 文件必须一起清。
    /// 2. **按值相等定向删除，不按键名一刀切**。复用 `remove_common_config_from_settings`
    ///    可以只清掉扩散出去的那一份，保留某个供应商自己写的、值不同的同名键。
    ///
    /// 步骤顺序本身是安全属性的一部分：**清片段必须排在最后**。片段是
    /// `remove_common_config_from_settings` 唯一的"该剥哪些键"来源，一旦清空，任何
    /// 残留（live 文件里的、下一轮重试要处理的）都再也无法被识别和剥离。所以所有
    /// 可能失败的步骤都排在它前面，失败即带错返回，让下次启动能原样重来。
    ///
    /// 清理后部分供应商会显示缺少 API Key，需用户重填——这是正确行为：那把密钥本就
    /// 不属于它们。（受害者原有的同名键在合并时已被覆盖，无法恢复。）动手前会往
    /// settings 的 `gemini_common_config_scrub_audit_v1` 写一条审计记录，内容是
    /// **键名与受影响的供应商 id，不含值**：`settings` 会随 WebDAV/S3 同步上传，
    /// 而这里处理的正是必须销毁的凭据，留值等于把一次清除换成一份跨设备扩散、
    /// 没有界面入口、永不过期的明文副本。
    pub async fn scrub_leaked_gemini_common_config(state: &AppState) -> Result<(), AppError> {
        const FLAG: &str = "gemini_common_config_credentials_scrubbed_v1";
        const AUDIT_KEY: &str = "gemini_common_config_scrub_audit_v1";
        let app = AppType::Gemini;

        if state.db.get_bool_flag(FLAG).unwrap_or(false) {
            return Ok(());
        }

        let Some(snippet_text) = state.db.get_config_snippet(app.as_str())? else {
            state.db.set_setting(FLAG, "true")?;
            return Ok(());
        };

        // 片段解析不了就不动它，只标记完成——乱改用户数据比留着更糟
        let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(&snippet_text) else {
            state.db.set_setting(FLAG, "true")?;
            return Ok(());
        };

        let mut poison = serde_json::Map::new();
        let mut clean = serde_json::Map::new();
        for (key, value) in entries {
            if Self::is_sensitive_config_key(&key) {
                poison.insert(key, value);
            } else {
                clean.insert(key, value);
            }
        }

        if poison.is_empty() {
            state.db.set_setting(FLAG, "true")?;
            return Ok(());
        }

        log::warn!(
            "检测到 {} 个凭据键残留在 Gemini 通用配置片段中，开始一次性清理",
            poison.len()
        );

        let poison_keys: Vec<String> = poison.keys().cloned().collect();
        let poison_value = Value::Object(poison);
        let poison_text = serde_json::to_string(&poison_value)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))?;

        // 1) 先算出各供应商清理后的配置，但**先不落库**
        let providers = state.db.get_all_providers(app.as_str())?;
        let mut pending: Vec<(String, Provider, Value)> = Vec::new();
        for (id, provider) in providers {
            let cleaned = match live::remove_common_config_from_settings(
                &app,
                &provider.settings_config,
                &poison_text,
            ) {
                Ok(cleaned) => cleaned,
                Err(err) => {
                    log::warn!("清理供应商 '{id}' 的泄漏凭据失败: {err}");
                    continue;
                }
            };
            if cleaned != provider.settings_config {
                pending.push((id, provider, cleaned));
            }
        }

        // 2) 落库前留一份审计记录：**只记键名与受影响的供应商，不记值**。
        //
        //    「按值相等定向删除」在一种合法场景下也会命中：用户有意在多个供应商里
        //    复用同一把 key。所以必须留下"删了什么、从哪删的"，否则用户只能靠翻
        //    日志。但不能留值——`settings` 表不在 `SYNC_SKIP_TABLES` 里，会随
        //    WebDAV/S3 同步上传，而这里处理的恰恰是必须销毁的泄漏凭据：留值等于
        //    把一次清除换成一份没有界面入口、永不过期、还会跨设备扩散的明文副本。
        //    密钥本来就该轮换，可恢复性不值这个代价。
        let removed_env_keys = |before: &Value, after: &Value| -> Vec<String> {
            let before_env = before.get("env").and_then(Value::as_object);
            let after_env = after.get("env").and_then(Value::as_object);
            match (before_env, after_env) {
                (Some(before_env), Some(after_env)) => before_env
                    .keys()
                    .filter(|key| !after_env.contains_key(*key))
                    .cloned()
                    .collect(),
                (Some(before_env), None) => before_env.keys().cloned().collect(),
                _ => Vec::new(),
            }
        };
        let audit = serde_json::json!({
            "removedFromSnippet": poison_keys,
            "providers": pending
                .iter()
                .map(|(id, provider, cleaned)| serde_json::json!({
                    "id": id,
                    "removedKeys": removed_env_keys(&provider.settings_config, cleaned),
                }))
                .collect::<Vec<_>>(),
        });
        let audit_text = serde_json::to_string(&audit)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))?;
        // 只在没有记录时写。provider 的写入不是一个事务（每次 save_provider 各自
        // 提交），上一轮可能改到一半就中止；此时完成标记没置位，下次启动会重跑，
        // 而重跑看到的"原始状态"已经残缺。无条件 INSERT OR REPLACE 会拿这份残缺
        // 记录盖掉第一轮那份完整的。
        if state.db.get_setting(AUDIT_KEY)?.is_none() {
            state.db.set_setting(AUDIT_KEY, &audit_text)?;
        }

        // 3) 各供应商 settings_config：按值相等定向删除扩散出去的副本
        for (id, provider, cleaned) in pending {
            let mut updated = provider;
            updated.settings_config = cleaned;
            state.db.save_provider(app.as_str(), &updated)?;
            log::info!("已从 Gemini 供应商 '{id}' 中清除泄漏的共享凭据");
        }

        // 4) 旧版接管留下的 live 快照（备份行）里也可能有一份副本。这一步的失败**必须
        //    传播**：
        //
        //    新版启动时不回放这份快照（转存到本机文件后删除），但同一次启动里它排在
        //    这里之后；在那之前降级的话，旧版停代理时会把快照原样写回 `~/.gemini/.env`。
        //    若它仍带毒而我们照样清了片段、置了完成标记，凭据就会复活，而一次性标记又
        //    保证不会再清第二次。
        //
        //    带错返回是安全的失败方式：调用方（lib.rs:1189）只记 warn 不中断启动，
        //    片段和标记都原样留着，下次启动照原样重来。
        if let Some(backup) = state.db.get_live_backup(app.as_str()).await? {
            let original: Value = serde_json::from_str(&backup.original_config)
                .map_err(|e| AppError::Message(format!("解析 Gemini 代理接管备份失败: {e}")))?;
            let cleaned = live::remove_common_config_from_settings(&app, &original, &poison_text)?;
            if cleaned != original {
                let text = serde_json::to_string(&cleaned)
                    .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))?;
                state.db.save_live_backup(app.as_str(), &text).await?;
                log::info!("已从 Gemini 代理接管备份中清除泄漏的共享凭据");
            }
        }

        // 5) `~/.gemini/.env`：**定向**删除，且必须在清片段之前做，失败即中止。
        //
        //    为什么不用 `sync_current_provider_for_app` 重投影：它在没有当前供应商
        //    时直接返回 Ok 而根本不写文件，泄漏值会原样留在 live 里；等片段被清空
        //    之后，下次切换时 `remove_common_config_from_settings` 再也认不出这个
        //    键，backfill 就把它永久写进受害供应商的配置——正是本函数开头说的那个
        //    顺序陷阱，只是由"没修"变成"修了一半更糟"。定向删除还顺带保住了只存在
        //    于 live、与供应商无关的手工 env（重投影会把它们抹掉）。
        //
        //    删除走 `remove_gemini_env_entries` 的**保序**实现而不是 read→HashMap→
        //    write 往返：后者会顺手抹掉注释、空行和无法识别的行，并按键名重排整个
        //    文件。全量投影时那无所谓，但这里是一次用户没主动触发的启动期清理，不该
        //    连带改写与泄漏无关的内容。
        //
        //    失败就带着错误返回：片段此刻还留着毒键，完成标记也没置位，下次启动能
        //    照原样重来。清片段是不可逆的一步，必须排在所有会失败的步骤之后。
        let poison_env: HashMap<String, String> = poison_value
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|text| (key.clone(), text.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if crate::gemini_config::remove_gemini_env_entries(&poison_env)? {
            log::info!("已从 ~/.gemini/.env 中清除泄漏的共享凭据");
        }

        // 6) 片段本身：保留可共享的部分。全部清空时删行而不是写 "{}"——留着空行会让
        //    should_auto_extract_config_snippet 永远为 false，用户的合法共享配置再也
        //    重建不回来。同理绝不置 cleared 标记。
        if clean.is_empty() {
            state.db.set_config_snippet(app.as_str(), None)?;
        } else {
            let cleaned_snippet = serde_json::to_string_pretty(&Value::Object(clean))
                .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))?;
            state
                .db
                .set_config_snippet(app.as_str(), Some(cleaned_snippet))?;
        }

        state.db.set_setting(FLAG, "true")?;
        log::info!("Gemini 通用配置凭据清理完成");
        Ok(())
    }

    /// Extract common config for OpenCode (JSON format)
    fn extract_opencode_common_config(settings: &Value) -> Result<String, AppError> {
        // OpenCode uses a different config structure with npm, options, models
        // For common config, we exclude provider-specific fields like apiKey
        let mut config = settings.clone();

        // Remove provider-specific fields
        if let Some(obj) = config.as_object_mut() {
            for key in ["options", "settings"] {
                if let Some(options) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
                    options.remove("apiKey");
                    options.remove("baseURL");
                }
            }
            // Keep npm and models as they might be common
        }

        if config.is_null() || (config.is_object() && config.as_object().unwrap().is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Extract common config for OpenClaw (JSON format)
    fn extract_openclaw_common_config(settings: &Value) -> Result<String, AppError> {
        // OpenClaw uses a different config structure with baseUrl, apiKey, api, models
        // For common config, we exclude provider-specific fields like apiKey
        let mut config = settings.clone();

        // Remove provider-specific fields
        if let Some(obj) = config.as_object_mut() {
            obj.remove("apiKey");
            obj.remove("baseUrl");
            // Keep api and models as they might be common
        }

        if config.is_null() || (config.is_object() && config.as_object().unwrap().is_empty()) {
            return Ok("{}".to_string());
        }

        serde_json::to_string_pretty(&config)
            .map_err(|e| AppError::Message(format!("Serialization failed: {e}")))
    }

    /// Import default configuration from live files (re-export)
    ///
    /// Returns `Ok(true)` if imported, `Ok(false)` if skipped.
    pub fn import_default_config(state: &AppState, app_type: AppType) -> Result<bool, AppError> {
        import_default_config(state, app_type)
    }

    pub fn should_import_default_config_on_startup(
        state: &AppState,
        app_type: &AppType,
    ) -> Result<bool, AppError> {
        should_import_default_config_on_startup(state, app_type)
    }

    /// Read current live settings (re-export)
    pub fn read_live_settings(app_type: AppType) -> Result<Value, AppError> {
        read_live_settings(app_type)
    }

    /// Get custom endpoints list (re-export)
    pub fn get_custom_endpoints(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
    ) -> Result<Vec<CustomEndpoint>, AppError> {
        endpoints::get_custom_endpoints(state, app_type, provider_id)
    }

    /// Add custom endpoint (re-export)
    pub fn add_custom_endpoint(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        url: String,
    ) -> Result<(), AppError> {
        endpoints::add_custom_endpoint(state, app_type, provider_id, url)
    }

    /// Remove custom endpoint (re-export)
    pub fn remove_custom_endpoint(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        url: String,
    ) -> Result<(), AppError> {
        endpoints::remove_custom_endpoint(state, app_type, provider_id, url)
    }

    /// Update provider sort order
    pub fn update_sort_order(
        state: &AppState,
        app_type: AppType,
        updates: Vec<ProviderSortUpdate>,
    ) -> Result<bool, AppError> {
        let updates: Vec<(String, usize)> = updates
            .into_iter()
            .map(|update| (update.id, update.sort_index))
            .collect();
        state
            .db
            .update_provider_sort_indexes(app_type.as_str(), &updates)?;
        Ok(true)
    }

    /// Query provider usage (re-export)
    pub async fn query_usage(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
    ) -> Result<UsageResult, AppError> {
        usage::query_usage(state, app_type, provider_id).await
    }

    /// Test usage script (re-export)
    #[allow(clippy::too_many_arguments)]
    pub async fn test_usage_script(
        state: &AppState,
        app_type: AppType,
        provider_id: &str,
        script_code: &str,
        timeout: u64,
        api_key: Option<&str>,
        base_url: Option<&str>,
        access_token: Option<&str>,
        user_id: Option<&str>,
        template_type: Option<&str>,
    ) -> Result<UsageResult, AppError> {
        usage::test_usage_script(
            state,
            app_type,
            provider_id,
            script_code,
            timeout,
            api_key,
            base_url,
            access_token,
            user_id,
            template_type,
        )
        .await
    }

    fn validate_provider_settings(app_type: &AppType, provider: &Provider) -> Result<(), AppError> {
        icon::validate_icon(provider.icon.as_deref())?;
        match app_type {
            AppType::Claude => {
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.claude.settings.not_object",
                        "Claude 配置必须是 JSON 对象",
                        "Claude configuration must be a JSON object",
                    ));
                }
            }
            AppType::ClaudeDesktop => {
                crate::claude_desktop_config::validate_provider(provider)?;
            }
            AppType::Codex => {
                let settings = provider.settings_config.as_object().ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.settings.not_object",
                        "Codex 配置必须是 JSON 对象",
                        "Codex configuration must be a JSON object",
                    )
                })?;

                let auth = settings.get("auth").ok_or_else(|| {
                    AppError::localized(
                        "provider.codex.auth.missing",
                        format!("供应商 {} 缺少 auth 配置", provider.id),
                        format!("Provider {} is missing auth configuration", provider.id),
                    )
                })?;
                if !auth.is_object() {
                    return Err(AppError::localized(
                        "provider.codex.auth.not_object",
                        format!("供应商 {} 的 auth 配置必须是 JSON 对象", provider.id),
                        format!(
                            "Provider {} auth configuration must be a JSON object",
                            provider.id
                        ),
                    ));
                }

                if let Some(config_value) = settings.get("config") {
                    if !(config_value.is_string() || config_value.is_null()) {
                        return Err(AppError::localized(
                            "provider.codex.config.invalid_type",
                            "Codex config 字段必须是字符串",
                            "Codex config field must be a string",
                        ));
                    }
                    if let Some(cfg_text) = config_value.as_str() {
                        crate::codex_config::validate_config_toml(cfg_text)?;
                    }
                }
            }
            AppType::Gemini => {
                use crate::gemini_config::validate_gemini_settings;
                validate_gemini_settings(&provider.settings_config)?
            }
            AppType::Agy => {
                use crate::gemini_config::validate_gemini_settings;
                validate_gemini_settings(&provider.settings_config)?
            }
            AppType::Copilot => {
                crate::copilot_config::validate_settings(&provider.settings_config)?
            }
            AppType::GrokBuild => {
                let settings = provider.settings_config.as_object().ok_or_else(|| {
                    AppError::localized(
                        "provider.grokbuild.settings.not_object",
                        "Grok Build 配置必须是 JSON 对象",
                        "Grok Build configuration must be a JSON object",
                    )
                })?;
                let config = settings
                    .get("config")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AppError::localized(
                            "provider.grokbuild.config.missing",
                            "Grok Build 配置缺少 config 字段",
                            "Grok Build configuration is missing the config field",
                        )
                    })?;
                if provider.category.as_deref() == Some("official") {
                    // 官方条目走 Grok CLI 自带 OAuth：空 config 合法，
                    // 其余内容只要求 TOML 语法合法。
                    crate::grok_config::validate_config_toml_syntax(config)?;
                } else {
                    crate::grok_config::validate_config_toml(config)?;
                }
            }
            AppType::OpenCode => {
                // OpenCode uses a different config structure: { npm, options, models }
                // Basic validation - must be an object
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.opencode.settings.not_object",
                        "OpenCode 配置必须是 JSON 对象",
                        "OpenCode configuration must be a JSON object",
                    ));
                }
                // The row is saved before the live write, which would refuse an invalid
                // native declaration only after the database already holds it.
                if !matches!(provider.category.as_deref(), Some("omo") | Some("omo-slim")) {
                    let (fragment, format) = crate::opencode_config::provider_fragment(
                        &provider.id,
                        &provider.settings_config,
                        provider.opencode_config_format(),
                    )?;
                    if format == crate::provider::OpenCodeConfigFormat::V2 {
                        crate::opencode_config::validate_native_provider(&provider.id, fragment)?;
                    }
                }
            }
            AppType::OpenClaw => {
                // OpenClaw uses config structure: { baseUrl, apiKey, api, models }
                // Basic validation - must be an object
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.openclaw.settings.not_object",
                        "OpenClaw 配置必须是 JSON 对象",
                        "OpenClaw configuration must be a JSON object",
                    ));
                }
            }
            AppType::Hermes => {
                // Hermes: accept any JSON object for now
                if !provider.settings_config.is_object() {
                    return Err(AppError::localized(
                        "provider.hermes.settings.not_object",
                        "Hermes 配置必须是 JSON 对象",
                        "Hermes configuration must be a JSON object",
                    ));
                }
            }
            AppType::Dsh => {
                crate::dsh_config::validate_provider(&provider.id, &provider.settings_config)?;
            }
            AppType::Kimi => {
                crate::kimi_config::validate_provider(&provider.id, &provider.settings_config)?
            }
            AppType::Mcode => {
                crate::mcode_config::validate_provider(&provider.id, &provider.settings_config)?
            }
            AppType::Pi => {
                crate::pi_config::validate_provider_node(&provider.id, &provider.settings_config)?;
            }
            AppType::Omp => {
                crate::omp_config::validate_provider(&provider.id, &provider.settings_config)?;
            }
        }

        // Validate and clean UsageScript configuration (common for all app types)
        if let Some(meta) = &provider.meta {
            if let Some(usage_script) = &meta.usage_script {
                validate_usage_script(usage_script)?;
            }
        }

        Ok(())
    }
}

/// Normalize Claude model keys in a JSON value
///
/// Reads old key (ANTHROPIC_SMALL_FAST_MODEL), writes new keys (DEFAULT_*), and deletes old key.
pub(crate) fn normalize_claude_models_in_value(settings: &mut Value) -> bool {
    let mut changed = false;
    let env = match settings.get_mut("env").and_then(|v| v.as_object_mut()) {
        Some(obj) => obj,
        None => return changed,
    };

    let model = env
        .get("ANTHROPIC_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let small_fast = env
        .get("ANTHROPIC_SMALL_FAST_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let current_haiku = env
        .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_sonnet = env
        .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let current_opus = env
        .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let target_haiku = current_haiku
        .or_else(|| small_fast.clone())
        .or_else(|| model.clone());
    let target_sonnet = current_sonnet
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());
    let target_opus = current_opus
        .or_else(|| model.clone())
        .or_else(|| small_fast.clone());

    if env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none() {
        if let Some(v) = target_haiku {
            env.insert(
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none() {
        if let Some(v) = target_sonnet {
            env.insert(
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                Value::String(v),
            );
            changed = true;
        }
    }
    if env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none() {
        if let Some(v) = target_opus {
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(), Value::String(v));
            changed = true;
        }
    }

    if env.remove("ANTHROPIC_SMALL_FAST_MODEL").is_some() {
        changed = true;
    }

    changed
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSortUpdate {
    pub id: String,
    #[serde(rename = "sortIndex")]
    pub sort_index: usize,
}

// ============================================================================
// 统一供应商（Universal Provider）服务方法
// ============================================================================

use crate::provider::UniversalProvider;
use std::collections::HashMap;

impl ProviderService {
    /// 获取所有统一供应商
    pub fn list_universal(
        state: &AppState,
    ) -> Result<HashMap<String, UniversalProvider>, AppError> {
        state.db.get_all_universal_providers()
    }

    /// 获取单个统一供应商
    pub fn get_universal(
        state: &AppState,
        id: &str,
    ) -> Result<Option<UniversalProvider>, AppError> {
        state.db.get_universal_provider(id)
    }

    /// 添加或更新统一供应商（不自动同步，需手动调用 sync_universal_to_apps）
    pub fn upsert_universal(
        state: &AppState,
        provider: UniversalProvider,
    ) -> Result<bool, AppError> {
        icon::validate_icon(provider.icon.as_deref())?;
        // 保存统一供应商
        state.db.save_universal_provider(&provider)?;

        Ok(true)
    }

    /// 删除统一供应商
    pub fn delete_universal(state: &AppState, id: &str) -> Result<bool, AppError> {
        // 获取统一供应商（用于删除生成的子供应商）
        let provider = state.db.get_universal_provider(id)?;

        // 删除统一供应商
        state.db.delete_universal_provider(id)?;

        // 删除生成的子供应商
        if let Some(p) = provider {
            if p.apps.claude {
                let claude_id = format!("universal-claude-{id}");
                let _ = state.db.delete_provider("claude", &claude_id);
            }
            if p.apps.codex {
                let codex_id = format!("universal-codex-{id}");
                let _ = state.db.delete_provider("codex", &codex_id);
            }
            if p.apps.gemini {
                let gemini_id = format!("universal-gemini-{id}");
                let _ = state.db.delete_provider("gemini", &gemini_id);
            }
        }

        Ok(true)
    }

    /// 同步统一供应商到各应用
    pub fn sync_universal_to_apps(state: &AppState, id: &str) -> Result<bool, AppError> {
        let provider = state
            .db
            .get_universal_provider(id)?
            .ok_or_else(|| AppError::Message(format!("统一供应商 {id} 不存在")))?;
        // 子供应商经 save_provider 直接入库，不走保存校验，图标在这里把关
        icon::validate_icon(provider.icon.as_deref())?;

        // Keep DB and live projections in sync independently per application:
        // one broken config file must not prevent the other two apps from being
        // updated, but it must still be reported instead of returning success.
        let mut live_failures = Vec::new();

        // 同步到 Claude
        if let Some(mut claude_provider) = provider.to_claude_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&claude_provider.id, "claude")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &claude_provider.settings_config);
                claude_provider.settings_config = merged;
                // 已有子供应商的应用专属配置与排序不属于统一供应商管理的字段。
                claude_provider.meta = existing.meta;
                claude_provider.created_at = existing.created_at;
                claude_provider.sort_index = existing.sort_index;
            }
            state.db.save_provider("claude", &claude_provider)?;
            Self::project_universal_child_to_live(
                state,
                AppType::Claude,
                &claude_provider.id,
                &mut live_failures,
            );
        } else {
            // 如果禁用了 Claude，删除对应的子供应商
            let claude_id = format!("universal-claude-{id}");
            let _ = state.db.delete_provider("claude", &claude_id);
        }

        // 同步到 Codex
        if let Some(mut codex_provider) = provider.to_codex_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&codex_provider.id, "codex")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &codex_provider.settings_config);
                codex_provider.settings_config = merged;
                // 已有子供应商的应用专属配置与排序不属于统一供应商管理的字段。
                codex_provider.meta = existing.meta;
                codex_provider.created_at = existing.created_at;
                codex_provider.sort_index = existing.sort_index;
            }
            state.db.save_provider("codex", &codex_provider)?;
            Self::project_universal_child_to_live(
                state,
                AppType::Codex,
                &codex_provider.id,
                &mut live_failures,
            );
        } else {
            let codex_id = format!("universal-codex-{id}");
            let _ = state.db.delete_provider("codex", &codex_id);
        }

        // 同步到 Gemini
        if let Some(mut gemini_provider) = provider.to_gemini_provider() {
            // 合并已有配置
            if let Some(existing) = state.db.get_provider_by_id(&gemini_provider.id, "gemini")? {
                let mut merged = existing.settings_config.clone();
                Self::merge_json(&mut merged, &gemini_provider.settings_config);
                gemini_provider.settings_config = merged;
                // 已有子供应商的应用专属配置与排序不属于统一供应商管理的字段。
                gemini_provider.meta = existing.meta;
                gemini_provider.created_at = existing.created_at;
                gemini_provider.sort_index = existing.sort_index;
            }
            state.db.save_provider("gemini", &gemini_provider)?;
            Self::project_universal_child_to_live(
                state,
                AppType::Gemini,
                &gemini_provider.id,
                &mut live_failures,
            );
        } else {
            let gemini_id = format!("universal-gemini-{id}");
            let _ = state.db.delete_provider("gemini", &gemini_id);
        }

        if live_failures.is_empty() {
            Ok(true)
        } else {
            Err(AppError::Message(format!(
                "统一供应商已保存到数据库，但以下应用的配置文件未能写入，仍是旧内容：{}。请重试同步，或切换一次该应用的供应商。",
                live_failures.join("、")
            )))
        }
    }

    /// Re-project a generated universal child only when it is the effective
    /// current provider for that app. Failures are collected by the caller so
    /// the other applications can continue syncing.
    fn project_universal_child_to_live(
        state: &AppState,
        app_type: AppType,
        child_id: &str,
        failures: &mut Vec<String>,
    ) {
        // 正在用的那家（代理模式下是代理路由）才需要重投影。
        let is_current = match crate::mode::current::provider_for(
            &state.db,
            &app_type,
            crate::mode::current::Purpose::InUse,
        ) {
            Ok(current) => current.as_deref() == Some(child_id),
            Err(err) => {
                log::warn!(
                    "读取 {} 当前供应商失败，跳过统一供应商的 live 重投影: {err}",
                    app_type.as_str()
                );
                failures.push(app_type.as_str().to_string());
                return;
            }
        };
        if !is_current {
            return;
        }

        if let Err(err) = Self::sync_current_provider_for_app(state, app_type) {
            log::warn!(
                "统一供应商同步后重写 {} live 配置失败: {err}",
                app_type.as_str()
            );
            failures.push(app_type.as_str().to_string());
        }
    }

    /// 递归合并 JSON：base 为底，patch 覆盖同名字段
    fn merge_json(base: &mut serde_json::Value, patch: &serde_json::Value) {
        use serde_json::Value;

        match (base, patch) {
            (Value::Object(base_map), Value::Object(patch_map)) => {
                for (k, v_patch) in patch_map {
                    match base_map.get_mut(k) {
                        Some(v_base) => Self::merge_json(v_base, v_patch),
                        None => {
                            base_map.insert(k.clone(), v_patch.clone());
                        }
                    }
                }
            }
            // 其它类型：直接覆盖
            (base_val, patch_val) => {
                *base_val = patch_val.clone();
            }
        }
    }
}
