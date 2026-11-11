//! `ProviderService` 的单元测试（从 `mod.rs` 拆出）。

use super::*;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use crate::claude_desktop_config::PROFILE_ID;
use crate::config::{get_claude_settings_path, read_json_file, write_json_file};
use crate::database::Database;
use crate::provider::{
    AuthBinding, AuthBindingSource, ClaudeModelConfig, ProviderMeta, UniversalProvider, UsageScript,
};
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use crate::provider::{ClaudeDesktopMode, ClaudeDesktopModelRoute};
use crate::proxy::types::GlobalProxyConfig;
use crate::store::AppState;
use serde_json::json;
use serial_test::serial;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tempfile::TempDir;

struct TempHome {
    #[allow(dead_code)]
    dir: TempDir,
    original_home: Option<String>,
    #[cfg(windows)]
    original_local_app_data: Option<String>,
    original_userprofile: Option<String>,
    original_test_home: Option<String>,
    #[cfg(target_os = "linux")]
    original_xdg_config_home: Option<std::ffi::OsString>,
    // 和 hermes/openclaw/workspace 共用一把 HOME 锁；异步测试里只能 await 拿锁。
    _home_lock: tokio::sync::MutexGuard<'static, ()>,
}

impl TempHome {
    async fn new() -> Self {
        let _home_lock = crate::config::test_home_lock().lock().await;
        Self::build(_home_lock)
    }

    /// 同步测试用：调用方不在 async runtime 里，可以阻塞拿锁（测试体内接着
    /// 自己 `block_on` 的场合走这里，别把它变成 async fn）。
    fn new_sync() -> Self {
        let _home_lock = crate::config::test_home_lock().blocking_lock();
        Self::build(_home_lock)
    }

    fn build(_home_lock: tokio::sync::MutexGuard<'static, ()>) -> Self {
        let dir = TempDir::new().expect("failed to create temp home");
        let original_home = env::var("HOME").ok();
        #[cfg(windows)]
        let original_local_app_data = env::var("LOCALAPPDATA").ok();
        let original_userprofile = env::var("USERPROFILE").ok();
        let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();
        #[cfg(target_os = "linux")]
        let original_xdg_config_home = env::var_os("XDG_CONFIG_HOME");

        env::set_var("HOME", dir.path());
        #[cfg(windows)]
        env::set_var("LOCALAPPDATA", dir.path().join("AppData").join("Local"));
        env::set_var("USERPROFILE", dir.path());
        env::set_var("CC_SWITCH_TEST_HOME", dir.path());
        // Claude Desktop Linux paths follow XDG_CONFIG_HOME; pin them under the temp home.
        #[cfg(target_os = "linux")]
        env::remove_var("XDG_CONFIG_HOME");

        Self {
            dir,
            original_home,
            #[cfg(windows)]
            original_local_app_data,
            original_userprofile,
            original_test_home,
            #[cfg(target_os = "linux")]
            original_xdg_config_home,
            _home_lock,
        }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        match &self.original_home {
            Some(value) => env::set_var("HOME", value),
            None => env::remove_var("HOME"),
        }

        #[cfg(windows)]
        {
            match &self.original_local_app_data {
                Some(value) => env::set_var("LOCALAPPDATA", value),
                None => env::remove_var("LOCALAPPDATA"),
            }
        }

        match &self.original_userprofile {
            Some(value) => env::set_var("USERPROFILE", value),
            None => env::remove_var("USERPROFILE"),
        }

        match &self.original_test_home {
            Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
            None => env::remove_var("CC_SWITCH_TEST_HOME"),
        }

        #[cfg(target_os = "linux")]
        {
            match &self.original_xdg_config_home {
                Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}

#[cfg(windows)]
fn claude_desktop_profile_path(home: &Path) -> PathBuf {
    home.join("AppData")
        .join("Local")
        .join("Claude-3p")
        .join("configLibrary")
        .join(format!("{PROFILE_ID}.json"))
}

#[cfg(target_os = "macos")]
fn claude_desktop_profile_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Claude-3p")
        .join("configLibrary")
        .join(format!("{PROFILE_ID}.json"))
}

#[cfg(target_os = "linux")]
fn claude_desktop_profile_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("Claude-3p")
        .join("configLibrary")
        .join(format!("{PROFILE_ID}.json"))
}

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

fn with_test_home<T>(test: impl FnOnce(&AppState, &Path) -> T) -> T {
    let _guard = test_guard();
    // 私有 test_guard 只挡本文件的测试；再拿共享 HOME 锁，才不会和
    // hermes/openclaw/workspace 等模块的测试互换临时主目录。
    let _home_lock = crate::config::test_home_lock().blocking_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
    let old_home = std::env::var_os("HOME");
    std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
    std::env::set_var("HOME", temp.path());

    let db = Arc::new(Database::memory().expect("in-memory database"));
    let state = AppState::new(db);
    let result = test(&state, temp.path());

    match old_test_home {
        Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
        None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
    }
    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    result
}

fn codex_settings(base_url: &str, api_key: &str) -> Value {
    json!({
        "auth": {
            "OPENAI_API_KEY": api_key
        },
        "config": format!(
            "model_provider = \"custom\"\n\
             [model_providers.custom]\n\
             name = \"custom\"\n\
             base_url = \"{base_url}\"\n\
             wire_api = \"chat\"\n"
        )
    })
}

#[test]
#[serial]
fn effective_current_provider_prefers_a_valid_local_pointer() {
    with_test_home(|state, _home| {
        crate::settings::reload_settings().expect("reload settings");

        let db_current = Provider::with_id(
            "db-current".to_string(),
            "Database Current".to_string(),
            json!({}),
            None,
        );
        let local_current = Provider::with_id(
            "local-current".to_string(),
            "Local Current".to_string(),
            json!({}),
            None,
        );
        state.db.save_provider("claude", &db_current).unwrap();
        state
            .db
            .set_current_provider("claude", "db-current")
            .unwrap();
        state.db.save_provider("claude", &local_current).unwrap();

        let _ = crate::settings::set_current_provider(&AppType::Claude, Some("local-current"));
        let effective =
            crate::settings::get_effective_current_provider(&state.db, &AppType::Claude).unwrap();
        assert_eq!(effective.as_deref(), Some("local-current"));

        let _ = crate::settings::set_current_provider(&AppType::Claude, Some("missing"));
        let fallback =
            crate::settings::get_effective_current_provider(&state.db, &AppType::Claude).unwrap();
        assert_eq!(fallback.as_deref(), Some("db-current"));
    });
}

#[test]
#[serial]
fn codex_editor_view_preserves_draft_and_stored_copilot_metadata() {
    with_test_home(|state, _home| {
        crate::settings::reload_settings().expect("reload settings");
        let settings = json!({
            "auth": {},
            "apiFormat": "openai_chat",
            "config": "model = \"claude-sonnet-5\"\nmodel_provider = \"copilot\"\nmodel_context_window = 400000\n\n[model_providers.copilot]\nbase_url = \"https://api.githubcopilot.com\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n"
        });
        let meta = ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            api_format: Some("openai_chat".to_string()),
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("github_copilot".to_string()),
                account_id: Some("copilot-editor-account".to_string()),
            }),
            ..Default::default()
        };
        let mut stored = Provider::with_id(
            "copilot-editor".to_string(),
            "Copilot".to_string(),
            settings.clone(),
            None,
        );
        stored.meta = Some(meta.clone());
        state
            .db
            .save_provider(AppType::Codex.as_str(), &stored)
            .expect("store Copilot provider");

        for (provider_id, draft_meta) in [(None, Some(&meta)), (Some(stored.id.as_str()), None)] {
            let view = ProviderService::editor_view_with_meta(
                state,
                AppType::Codex,
                &settings,
                None,
                provider_id,
                draft_meta,
            )
            .expect("keyless managed Copilot preview");
            let text = view.settings["config"].as_str().expect("config text");
            let parsed: toml::Value = toml::from_str(text).expect("preview TOML");
            let route = &parsed["model_providers"]["custom"];
            assert_eq!(parsed["model_provider"].as_str(), Some("custom"));
            assert_eq!(route["wire_api"].as_str(), Some("responses"));
            assert!(route.get("requires_openai_auth").is_none());
            assert!(route.get("experimental_bearer_token").is_none());
            assert_eq!(view.settings["auth"], json!({}));
            assert!(!text.contains("copilot-editor-account"));
            assert_eq!(parsed["model_context_window"].as_integer(), Some(400000));
            assert_eq!(
                parsed["web_search"].as_str(),
                Some("disabled"),
                "Copilot must not inherit the generic ProxyChat search policy"
            );
        }

        let cleared_meta = ProviderMeta::default();
        let cleared = ProviderService::editor_view_with_meta(
            state,
            AppType::Codex,
            &settings,
            None,
            Some(&stored.id),
            Some(&cleared_meta),
        )
        .expect("generic keyless preview with explicitly cleared metadata");
        let generic = ProviderService::editor_view(state, AppType::Codex, &settings, None)
            .expect("generic keyless preview without managed metadata");
        // 普通空 Key 草稿也允许预览，不能再用报错区分身份；检查 Copilot 专属策略。
        for (view, reason) in [
            (
                cleared,
                "explicit draft metadata must not fall back to the stored Copilot type",
            ),
            (
                generic,
                "the compatibility wrapper must not infer managed auth from the URL",
            ),
        ] {
            let text = view.settings["config"].as_str().expect("config text");
            let parsed: toml::Value = toml::from_str(text).expect("preview TOML");
            assert!(parsed.get("web_search").is_none(), "{reason}");
            assert_eq!(view.settings["auth"], json!({}));
            assert!(!text.contains("cc-switch-editor-pending-key"));
        }
        assert!(!crate::codex_config::get_codex_config_path().exists());
        assert!(!crate::codex_config::get_codex_model_catalog_path().exists());
    });
}

#[test]
#[serial]
fn codex_editor_metadata_does_not_change_other_app_previews() {
    with_test_home(|state, _home| {
        crate::settings::reload_settings().expect("reload settings");
        let settings = json!({"env": {
            "ANTHROPIC_BASE_URL": "https://relay.example",
            "ANTHROPIC_AUTH_TOKEN": "editor-key",
            "ANTHROPIC_MODEL": "claude-sonnet-5"
        }});
        let expected =
            ProviderService::editor_view(state, AppType::Claude, &settings, None).unwrap();
        let meta = ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            ..Default::default()
        };
        let actual = ProviderService::editor_view_with_meta(
            state,
            AppType::Claude,
            &settings,
            None,
            Some("absent-provider"),
            Some(&meta),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    });
}

fn usage_script_with_credentials(
    api_key: Option<&str>,
    base_url: Option<&str>,
    template_type: Option<&str>,
) -> UsageScript {
    UsageScript {
        enabled: true,
        language: "javascript".to_string(),
        code: "return { remaining: 1, unit: 'USD' };".to_string(),
        timeout: Some(10),
        api_key: api_key.map(str::to_string),
        base_url: base_url.map(str::to_string),
        access_token: None,
        user_id: None,
        template_type: template_type.map(str::to_string),
        auto_query_interval: None,
        coding_plan_provider: None,
        access_key_id: Some("ak-test".to_string()),
        secret_access_key: Some("sk-test".to_string()),
        team_organization_id: None,
        team_project_id: None,
    }
}

fn codex_provider_with_usage(
    id: &str,
    base_url: &str,
    api_key: &str,
    usage_api_key: Option<&str>,
    usage_base_url: Option<&str>,
    template_type: Option<&str>,
) -> Provider {
    let mut provider = Provider::with_id(
        id.to_string(),
        format!("Provider {id}"),
        codex_settings(base_url, api_key),
        None,
    );
    provider.meta = Some(ProviderMeta {
        usage_script: Some(usage_script_with_credentials(
            usage_api_key,
            usage_base_url,
            template_type,
        )),
        ..Default::default()
    });
    provider
}

fn managed_codex_provider(id: &str, account_id: &str) -> Provider {
    let mut provider = Provider::with_id(
        id.to_string(),
        format!("Managed {id}"),
        json!({
            "auth": {},
            "config": ""
        }),
        None,
    );
    provider.category = Some("official".to_string());
    provider.meta = Some(ProviderMeta {
        auth_binding: Some(AuthBinding {
            source: AuthBindingSource::ManagedAccount,
            auth_provider: Some("codex_oauth".to_string()),
            account_id: Some(account_id.to_string()),
        }),
        ..Default::default()
    });
    provider
}

fn openclaw_provider(id: &str) -> Provider {
    Provider {
        id: id.to_string(),
        name: format!("Provider {id}"),
        settings_config: json!({
            "baseUrl": "https://api.deepseek.com",
            "apiKey": "test-key",
            "api": "openai-completions",
            "models": [],
        }),
        website_url: None,
        category: Some("custom".to_string()),
        created_at: Some(1),
        sort_index: Some(0),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

fn hermes_provider(id: &str) -> Provider {
    Provider {
        id: id.to_string(),
        name: format!("Provider {id}"),
        settings_config: json!({
            "api": "openai-chat",
            "base_url": "https://api.example.com/v1",
            "api_key": "test-key",
            "models": {
                "gpt-4o": {
                    "name": "GPT-4o"
                }
            }
        }),
        website_url: None,
        category: Some("custom".to_string()),
        created_at: Some(1),
        sort_index: Some(0),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

fn opencode_provider(id: &str) -> Provider {
    Provider {
        id: id.to_string(),
        name: format!("Provider {id}"),
        settings_config: json!({
            "npm": "@ai-sdk/openai-compatible",
            "name": format!("Provider {id}"),
            "options": {
                "baseURL": "https://api.example.com/v1",
                "apiKey": "test-key"
            },
            "models": {
                "gpt-4o": {
                    "name": "GPT-4o"
                }
            }
        }),
        website_url: None,
        category: Some("custom".to_string()),
        created_at: Some(1),
        sort_index: Some(0),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

fn opencode_omo_provider(id: &str, category: &str) -> Provider {
    let mut settings = serde_json::Map::new();
    settings.insert(
        "agents".to_string(),
        json!({
            "writer": {
                "model": "gpt-4o-mini"
            }
        }),
    );
    if category == "omo" {
        settings.insert(
            "categories".to_string(),
            json!({
                "default": ["writer"]
            }),
        );
    }
    settings.insert(
        "otherFields".to_string(),
        json!({
            "theme": "dark"
        }),
    );

    Provider {
        id: id.to_string(),
        name: format!("Provider {id}"),
        settings_config: Value::Object(settings),
        website_url: None,
        category: Some(category.to_string()),
        created_at: Some(1),
        sort_index: Some(0),
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

fn omo_config_path(home: &Path, category: &str) -> PathBuf {
    home.join(".config").join("opencode").join(match category {
        "omo" => crate::services::omo::STANDARD.preferred_filename,
        "omo-slim" => crate::services::omo::SLIM.preferred_filename,
        other => panic!("unexpected OMO category in test: {other}"),
    })
}

#[test]
#[serial]
fn add_clears_usage_credentials_that_match_provider_config() {
    with_test_home(|state, _| {
        let provider = codex_provider_with_usage(
            "codex-a",
            "https://api.a.example/v1/",
            "sk-a",
            Some(" sk-a "),
            Some(" https://api.a.example/v1/ "),
            None,
        );

        ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

        let saved = state
            .db
            .get_provider_by_id("codex-a", AppType::Codex.as_str())
            .expect("query saved provider")
            .expect("saved provider should exist");
        let script = saved
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script.api_key, None);
        assert_eq!(script.base_url, None);
    });
}

#[test]
#[serial]
fn update_preserves_usage_credentials_that_only_match_previous_config() {
    with_test_home(|state, _| {
        let provider = codex_provider_with_usage(
            "codex-usage-old",
            "https://api.a.example/v1/",
            "sk-a",
            Some("sk-a"),
            Some("https://api.a.example/v1/"),
            None,
        );
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("seed provider with explicit usage credentials");

        let mut updated = provider;
        updated.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");

        ProviderService::update(state, AppType::Codex, None, updated)
            .expect("update provider main credentials");

        let saved = state
            .db
            .get_provider_by_id("codex-usage-old", AppType::Codex.as_str())
            .expect("query updated provider")
            .expect("updated provider should exist");
        let script = saved
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script.api_key.as_deref(), Some("sk-a"));
        assert_eq!(
            script.base_url.as_deref(),
            Some("https://api.a.example/v1/")
        );
        assert_eq!(
            saved.resolve_usage_credentials(&AppType::Codex),
            ("https://api.b.example/v1".to_string(), "sk-b".to_string())
        );
    });
}

#[test]
#[serial]
fn copied_provider_uses_edited_credentials_after_add_clears_mirrored_usage_credentials() {
    with_test_home(|state, _| {
        let copied_provider = codex_provider_with_usage(
            "codex-copy",
            "https://api.a.example/v1/",
            "sk-a",
            Some("sk-a"),
            Some("https://api.a.example/v1/"),
            None,
        );

        ProviderService::add(state, AppType::Codex, copied_provider, false)
            .expect("add copied provider");

        let saved_after_add = state
            .db
            .get_provider_by_id("codex-copy", AppType::Codex.as_str())
            .expect("query copied provider")
            .expect("copied provider should exist");
        let script_after_add = saved_after_add
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");
        assert_eq!(script_after_add.api_key, None);
        assert_eq!(script_after_add.base_url, None);

        let mut edited_provider = saved_after_add.clone();
        edited_provider.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");

        ProviderService::update(state, AppType::Codex, None, edited_provider)
            .expect("edit copied provider credentials");

        let saved_after_update = state
            .db
            .get_provider_by_id("codex-copy", AppType::Codex.as_str())
            .expect("query edited provider")
            .expect("edited provider should exist");
        let script_after_update = saved_after_update
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script_after_update.api_key, None);
        assert_eq!(script_after_update.base_url, None);
        assert_eq!(
            saved_after_update.resolve_usage_credentials(&AppType::Codex),
            ("https://api.b.example/v1".to_string(), "sk-b".to_string())
        );
    });
}

#[test]
#[serial]
fn update_clears_usage_credentials_that_match_current_config() {
    with_test_home(|state, _| {
        let provider = codex_provider_with_usage(
            "codex-current",
            "https://api.a.example/v1",
            "sk-a",
            Some("sk-usage"),
            Some("https://usage.example/api"),
            None,
        );
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("seed provider with distinct usage credentials");

        let mut updated = provider;
        updated.settings_config = codex_settings("https://api.b.example/v1/", "sk-b");
        updated.meta = Some(ProviderMeta {
            usage_script: Some(usage_script_with_credentials(
                Some(" sk-b "),
                Some(" https://api.b.example/v1/ "),
                None,
            )),
            ..Default::default()
        });

        ProviderService::update(state, AppType::Codex, None, updated)
            .expect("update provider with redundant usage credentials");

        let saved = state
            .db
            .get_provider_by_id("codex-current", AppType::Codex.as_str())
            .expect("query updated provider")
            .expect("updated provider should exist");
        let script = saved
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script.api_key, None);
        assert_eq!(script.base_url, None);
    });
}

#[test]
#[serial]
fn add_preserves_distinct_usage_credentials() {
    with_test_home(|state, _| {
        let provider = codex_provider_with_usage(
            "codex-distinct",
            "https://api.main.example/v1",
            "sk-main",
            Some("sk-usage"),
            Some("https://usage.example/api"),
            None,
        );

        ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

        let saved = state
            .db
            .get_provider_by_id("codex-distinct", AppType::Codex.as_str())
            .expect("query saved provider")
            .expect("saved provider should exist");
        let script = saved
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script.api_key.as_deref(), Some("sk-usage"));
        assert_eq!(
            script.base_url.as_deref(),
            Some("https://usage.example/api")
        );
    });
}

#[test]
#[serial]
fn add_does_not_clear_token_plan_credentials() {
    with_test_home(|state, _| {
        let provider = codex_provider_with_usage(
            "codex-token-plan",
            "https://api.plan.example/v1",
            "sk-plan",
            Some("sk-plan"),
            Some("https://api.plan.example/v1"),
            Some("token_plan"),
        );

        ProviderService::add(state, AppType::Codex, provider, false).expect("add provider");

        let saved = state
            .db
            .get_provider_by_id("codex-token-plan", AppType::Codex.as_str())
            .expect("query saved provider")
            .expect("saved provider should exist");
        let script = saved
            .meta
            .as_ref()
            .and_then(|meta| meta.usage_script.as_ref())
            .expect("usage script should remain");

        assert_eq!(script.api_key.as_deref(), Some("sk-plan"));
        assert_eq!(
            script.base_url.as_deref(),
            Some("https://api.plan.example/v1")
        );
        assert_eq!(script.access_key_id.as_deref(), Some("ak-test"));
        assert_eq!(script.secret_access_key.as_deref(), Some("sk-test"));
    });
}

#[test]
fn validate_provider_settings_rejects_missing_auth() {
    let provider = Provider::with_id(
        "codex".into(),
        "Codex".into(),
        json!({ "config": "base_url = \"https://example.com\"" }),
        None,
    );
    let err = ProviderService::validate_provider_settings(&AppType::Codex, &provider)
        .expect_err("missing auth should be rejected");
    assert!(
        err.to_string().contains("auth"),
        "expected auth error, got {err:?}"
    );
}

#[test]
#[serial]
fn add_accepts_multiple_unbound_codex_official_cards() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        state
            .db
            .init_default_official_providers()
            .expect("seed official providers");
        let fixed_id = crate::database::CODEX_OFFICIAL_PROVIDER_ID;
        state
            .db
            .set_current_provider(AppType::Codex.as_str(), fixed_id)
            .expect("set database current");
        crate::settings::set_current_provider(&AppType::Codex, Some(fixed_id))
            .expect("set local current");

        for id in ["follow-login-a", "follow-login-b"] {
            let mut provider = Provider::with_id(
                id.to_string(),
                id.to_string(),
                json!({ "auth": {}, "config": "" }),
                None,
            );
            provider.category = Some("official".to_string());
            ProviderService::add(state, AppType::Codex, provider, false)
                .expect("add unbound Official card");
        }

        let providers = state
            .db
            .get_all_providers(AppType::Codex.as_str())
            .expect("read providers");
        assert!(providers.contains_key("follow-login-a"));
        assert!(providers.contains_key("follow-login-b"));
    });
}

#[test]
#[serial]
fn update_keeps_official_provider_id_when_binding_and_unbinding() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access-token",
                    "managed-user",
                )
                .await
                .expect("seed managed account");
        });

        let provider_id = crate::database::CODEX_OFFICIAL_PROVIDER_ID;
        let mut unbound = Provider::with_id(
            provider_id.to_string(),
            "OpenAI Official".to_string(),
            json!({ "auth": {}, "config": "" }),
            None,
        );
        unbound.category = Some("official".to_string());
        state
            .db
            .save_provider(AppType::Codex.as_str(), &unbound)
            .expect("save unbound card");
        state
            .db
            .set_current_provider(AppType::Codex.as_str(), provider_id)
            .expect("set database current");
        crate::settings::set_current_provider(&AppType::Codex, Some(provider_id))
            .expect("set local current");

        let mut bound = managed_codex_provider(provider_id, "acct-managed");
        bound.name = unbound.name.clone();
        ProviderService::update(state, AppType::Codex, Some(provider_id), bound)
            .expect("bind managed account");

        let saved_bound = state
            .db
            .get_provider_by_id(provider_id, AppType::Codex.as_str())
            .expect("query bound card")
            .expect("bound card should keep its ID");
        assert_eq!(
            ProviderService::managed_codex_oauth_account_id(&saved_bound).as_deref(),
            Some("acct-managed")
        );
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read database current")
                .as_deref(),
            Some(provider_id)
        );
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Codex).as_deref(),
            Some(provider_id)
        );

        // 旧版「统一会话历史」注入进 live、又被回填进行里的形态，保存时剥掉。
        unbound.settings_config["config"] = Value::String("model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"OpenAI\"\nrequires_openai_auth = true\nsupports_websockets = true\nwire_api = \"responses\"\n".to_string());
        ProviderService::update(state, AppType::Codex, Some(provider_id), unbound)
            .expect("unbind managed account");

        let saved_unbound = state
            .db
            .get_provider_by_id(provider_id, AppType::Codex.as_str())
            .expect("query unbound card")
            .expect("unbound card should keep its ID");
        assert!(ProviderService::managed_codex_oauth_account_id(&saved_unbound).is_none());
        assert_eq!(saved_unbound.settings_config["config"], json!(""));
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read database current")
                .as_deref(),
            Some(provider_id)
        );
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Codex).as_deref(),
            Some(provider_id)
        );
    });
}

#[test]
fn extract_gemini_common_config_strips_credentials_keeps_shareable() {
    // Gemini 的共享片段会被 deep-merge 回**其它** Gemini 供应商的 env
    // (旧版切换时合并片段；新版片段冻结只给旧版读)，因此任何凭据都不得进入片段。
    // 之前这里只硬编码跳过 GEMINI_API_KEY/GOOGLE_GEMINI_BASE_URL，而
    // GOOGLE_API_KEY 是 provider.rs 认可的一等 Gemini 凭据 → 会泄露到别的供应商。
    let settings = json!({
        "env": {
            "GEMINI_API_KEY": "g-gem",
            "GOOGLE_API_KEY": "g-legacy-real-key",
            "GOOGLE_GEMINI_BASE_URL": "https://gemini.example",
            "GOOGLE_APPLICATION_CREDENTIALS": "/path/creds.json",
            "SOME_PROXY_AUTH_TOKEN": "tok-proxy",
            // 关键字段归供应商：片段已冻结，收进去会从行里剥掉、再也写不回 live
            "GEMINI_MODEL": "gemini-2.5-pro",
            "GOOGLE_GENAI_USE_VERTEXAI": "true",
            // 可共享的非机密配置必须保留
            "GEMINI_TIMEOUT_MS": "30000"
        }
    });

    let snippet =
        ProviderService::extract_gemini_common_config(&settings).expect("extract should work");
    let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");

    for leaked in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "SOME_PROXY_AUTH_TOKEN",
        "GEMINI_MODEL",
        "GOOGLE_GENAI_USE_VERTEXAI",
    ] {
        assert!(
            value.get(leaked).is_none(),
            "{leaked} must not go into the shared Gemini snippet"
        );
    }
    assert_eq!(
        value.get("GEMINI_TIMEOUT_MS").and_then(|v| v.as_str()),
        Some("30000"),
        "shareable non-secret config must be preserved"
    );
}

/// 造一个「已被污染」的现场：片段里带 A 账号的凭据 + 一个合法可共享键。
#[test]
fn sensitive_key_matcher_covers_common_credential_namings() {
    for key in [
        // 裸 `_KEY`：最常见的写法，却曾被"只枚举 `_API_KEY` 这些子类"漏在外面
        "OPENAI_KEY",
        "GROQ_KEY",
        "XAI_KEY",
        // 不带分隔符的复合写法
        "VOLC_ACCESSKEY",
        "ALIYUN_SECRETKEY",
        "SOME_APITOKEN",
        // personal access token：既不含 TOKEN 也不含 KEY
        "GITHUB_PAT",
        "gitlab_pat",
        // 口令类缩写
        "MYSQL_PWD",
        "DB_PASS",
        "GPG_PASSPHRASE",
        "AWS_CREDS",
        // 发往上游的自定义请求头、Cookie、Authorization
        "ANTHROPIC_CUSTOM_HEADERS",
        "GEMINI_CLI_CUSTOM_HEADERS",
        "headers",
        "UPSTREAM_COOKIE",
        "PROXY_AUTHORIZATION",
    ] {
        assert!(
            ProviderService::is_sensitive_config_key(key),
            "{key} must be treated as a credential"
        );
    }

    // 后缀必须带下划线，不能把正常配置一起卷进来
    for key in [
        "PATH",
        "OLDPWD",
        "GEMINI_COMPAT",
        "SSL_BYPASS",
        "GEMINI_TIMEOUT_MS",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        // 名字带 HEADER 但不是发往上游的凭据：普通开关、用户自己的遥测端点
        "CLAUDE_CODE_ATTRIBUTION_HEADER",
        "OTEL_EXPORTER_OTLP_HEADERS",
    ] {
        assert!(
            !ProviderService::is_sensitive_config_key(key),
            "{key} is ordinary shareable config and must not be stripped"
        );
    }
}

/// 测试夹具：按键排序写一份 `~/.gemini/.env`。
fn write_gemini_env(map: &HashMap<String, String>) -> Result<(), AppError> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let text = keys
        .into_iter()
        .map(|key| format!("{key}={}", map[key]))
        .collect::<Vec<_>>()
        .join("\n");
    crate::gemini_config::write_gemini_env_text_atomic(&text)
}

fn seed_leaked_gemini_state(db: &Arc<Database>) {
    db.set_config_snippet(
        "gemini",
        Some(
            json!({
                "GOOGLE_API_KEY": "key-A-leaked",
                "SOME_PROXY_AUTH_TOKEN": "tok-A-leaked",
                "GEMINI_TIMEOUT_MS": "30000"
            })
            .to_string(),
        ),
    )
    .expect("seed snippet");

    // 受害者 B：泄漏的密钥已经被合并进它的 env
    let victim = Provider::with_id(
        "b".into(),
        "Relay B".into(),
        json!({ "env": {
            "GOOGLE_GEMINI_BASE_URL": "https://relay-b.example",
            "GOOGLE_API_KEY": "key-A-leaked",
            "GEMINI_TIMEOUT_MS": "30000"
        }}),
        None,
    );
    db.save_provider("gemini", &victim).expect("save victim");

    // 供应商 C：自己写了同名键但值不同，不能被误删
    let unrelated = Provider::with_id(
        "c".into(),
        "Own Key C".into(),
        json!({ "env": {
            "GOOGLE_GEMINI_BASE_URL": "https://c.example",
            "GOOGLE_API_KEY": "key-C-owned"
        }}),
        None,
    );
    db.save_provider("gemini", &unrelated).expect("save c");
}

/// Saving the active provider while takeover has never been enabled must
/// rewrite the real live file immediately.
#[tokio::test]
#[serial]
async fn update_current_claude_provider_writes_live_when_proxy_never_enabled() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    let original = Provider::with_id(
        "p1".into(),
        "Claude A".into(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "token-a",
                "ANTHROPIC_BASE_URL": "https://api.old.example"
            }
        }),
        None,
    );
    db.save_provider("claude", &original)
        .expect("save provider");
    db.set_current_provider("claude", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
        .expect("set local current provider");
    write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");

    let mut updated = original;
    updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
        Value::String("https://api.new.example".into());
    ProviderService::update(&state, AppType::Claude, None, updated)
        .expect("update current provider");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"].as_str(),
        Some("https://api.new.example")
    );
}

/// 编辑当前供应商、去掉它的独有字段：live 里 CC Switch 写进去的那个值随之删掉，
/// 用户自己的键不动。
#[tokio::test]
#[serial]
async fn update_current_claude_provider_drops_the_compat_switch_it_no_longer_has() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    let original = Provider::with_id(
        "ds".into(),
        "DeepSeek".into(),
        json!({ "env": {
            "ANTHROPIC_BASE_URL": "https://api.deepseek.example/anthropic",
            "CLAUDE_CODE_DISABLE_ARTIFACT": "1"
        }}),
        None,
    );
    db.save_provider("claude", &original)
        .expect("save provider");
    ProviderService::switch(&state, AppType::Claude, "ds").expect("switch");
    let mut live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    live["env"]["DEBUG"] = json!("1");
    write_json_file(&get_claude_settings_path(), &live).expect("user edit");

    let mut updated = original;
    updated.settings_config["env"]
        .as_object_mut()
        .expect("env")
        .remove("CLAUDE_CODE_DISABLE_ARTIFACT");
    ProviderService::update(&state, AppType::Claude, None, updated).expect("update");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    assert_eq!(
        live,
        json!({ "env": {
            "ANTHROPIC_BASE_URL": "https://api.deepseek.example/anthropic",
            "DEBUG": "1"
        }})
    );
}

/// 文件已经写成目标供应商、指针还没改时崩溃：下次启动按 pending 补完指针，
/// 文件和指针重新一致。
#[tokio::test]
#[serial]
async fn claude_switch_interrupted_before_the_pointer_rolls_forward() {
    use crate::mode::operation::failpoint;

    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    for (id, url) in [("a", "https://a.example"), ("b", "https://b.example")] {
        let provider = Provider::with_id(
            id.into(),
            id.into(),
            json!({ "env": { "ANTHROPIC_BASE_URL": url } }),
            None,
        );
        db.save_provider("claude", &provider)
            .expect("save provider");
    }
    ProviderService::switch(&state, AppType::Claude, "a").expect("switch to a");

    failpoint::crash_at(Some("published:0"));
    let result = ProviderService::switch(&state, AppType::Claude, "b");
    failpoint::crash_at(None);
    assert!(result.is_err(), "the injected crash surfaces");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"],
        json!("https://b.example")
    );
    assert_eq!(
        db.get_current_provider("claude")
            .expect("current")
            .as_deref(),
        Some("a"),
        "the pointer has not moved yet"
    );

    crate::mode::operation::recover_on_startup(&db);
    assert_eq!(
        db.get_current_provider("claude")
            .expect("current")
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        crate::settings::get_current_provider(&AppType::Claude).as_deref(),
        Some("b")
    );
}

/// A stale backup row must be refreshed but must not divert the live write.
#[tokio::test]
#[serial]
async fn update_current_claude_provider_writes_live_when_backup_row_is_stale() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    let original = Provider::with_id(
        "p1".into(),
        "Claude A".into(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "token-a",
                "ANTHROPIC_BASE_URL": "https://api.old.example"
            }
        }),
        None,
    );
    db.save_provider("claude", &original)
        .expect("save provider");
    db.set_current_provider("claude", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
        .expect("set local current provider");
    write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");
    db.save_live_backup(
        "claude",
        &serde_json::to_string(&original.settings_config).expect("serialize backup"),
    )
    .await
    .expect("seed stale backup");
    assert!(!state.proxy_service.is_running().await);

    let mut updated = original.clone();
    updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
        Value::String("https://api.new.example".into());
    ProviderService::update(&state, AppType::Claude, None, updated)
        .expect("update current provider");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"].as_str(),
        Some("https://api.new.example"),
        "a leftover backup row is not proxy mode: the edit goes to live"
    );
}

/// An enabled flag left behind by an interrupted teardown is not enough to
/// suppress a live write when neither placeholder nor backup evidence exists.
#[tokio::test]
#[serial]
async fn update_current_claude_provider_ignores_enabled_flag_without_evidence() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    let original = Provider::with_id(
        "p1".into(),
        "Claude A".into(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "token-a",
                "ANTHROPIC_BASE_URL": "https://api.old.example"
            }
        }),
        None,
    );
    db.save_provider("claude", &original)
        .expect("save provider");
    db.set_current_provider("claude", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
        .expect("set local current provider");
    write_live_for_state(&state, &AppType::Claude, &original).expect("seed live file");
    let mut config = db
        .get_proxy_config_for_app("claude")
        .await
        .expect("read proxy config");
    config.enabled = true;
    db.update_proxy_config_for_app(config)
        .await
        .expect("leave enabled flag set");
    assert!(!state.proxy_service.is_running().await);

    let mut updated = original.clone();
    updated.settings_config["env"]["ANTHROPIC_BASE_URL"] =
        Value::String("https://api.new.example".into());
    ProviderService::update(&state, AppType::Claude, None, updated)
        .expect("update current provider");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    assert_eq!(
        live["env"]["ANTHROPIC_BASE_URL"].as_str(),
        Some("https://api.new.example")
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_removes_leaked_credentials_from_snippet_and_providers() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    // 片段：凭据清掉，可共享配置保留
    let snippet = db
        .get_config_snippet("gemini")
        .expect("read snippet")
        .expect("snippet must still exist");
    let snippet: Value = serde_json::from_str(&snippet).expect("valid json");
    assert!(snippet.get("GOOGLE_API_KEY").is_none());
    assert!(snippet.get("SOME_PROXY_AUTH_TOKEN").is_none());
    assert_eq!(
        snippet.get("GEMINI_TIMEOUT_MS").and_then(Value::as_str),
        Some("30000"),
        "shareable config must survive the scrub"
    );

    // 受害者 B：扩散过去的那一份被清掉
    let providers = db.get_all_providers("gemini").expect("providers");
    let victim_env = &providers["b"].settings_config["env"];
    assert!(
        victim_env.get("GOOGLE_API_KEY").is_none(),
        "leaked key must be removed from the victim provider"
    );
    assert_eq!(
        victim_env.get("GEMINI_TIMEOUT_MS").and_then(Value::as_str),
        Some("30000"),
        "non-credential config must not be touched"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_keeps_a_providers_own_differently_valued_key() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    // 这条最容易写错成「按键名一刀切」：C 自己的密钥值与片段不同，是它自己的凭据
    let providers = db.get_all_providers("gemini").expect("providers");
    assert_eq!(
        providers["c"].settings_config["env"]
            .get("GOOGLE_API_KEY")
            .and_then(Value::as_str),
        Some("key-C-owned"),
        "a provider's own key must not be deleted by name matching"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_audit_records_key_names_but_never_values() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    let audit_text = db
        .get_setting("gemini_common_config_scrub_audit_v1")
        .expect("read audit")
        .expect("an audit record must exist so the deletion is not silent");

    // 值绝不能进这条记录：`settings` 会随 WebDAV/S3 同步上传，留值等于把一次
    // 清除换成一份跨设备扩散、没有界面入口、永不过期的明文副本。
    assert!(
        !audit_text.contains("key-A-leaked") && !audit_text.contains("tok-A-leaked"),
        "the audit record must never carry credential values: {audit_text}"
    );

    // 但必须说清楚删了什么、从哪删的，否则用户只能靠翻日志
    let audit: Value = serde_json::from_str(&audit_text).expect("audit is JSON");
    let removed: Vec<&str> = audit["removedFromSnippet"]
        .as_array()
        .expect("removedFromSnippet array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        removed.contains(&"GOOGLE_API_KEY") && removed.contains(&"SOME_PROXY_AUTH_TOKEN"),
        "every key removed from the snippet must be named: {audit}"
    );
    let victim = audit["providers"]
        .as_array()
        .expect("providers array")
        .iter()
        .find(|entry| entry["id"] == json!("b"))
        .expect("every provider whose config gets rewritten must be recorded");
    assert_eq!(
        victim["removedKeys"],
        json!(["GOOGLE_API_KEY"]),
        "the record must name what was taken from each provider: {audit}"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_never_overwrites_an_existing_audit_record() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    // 上一轮改到一半就中止的情形：完成标记没置位，下次启动会重跑，但那时
    // 读到的"原始状态"已经残缺。无条件覆盖会拿残缺记录盖掉第一轮那份完整的。
    db.set_setting(
        "gemini_common_config_scrub_audit_v1",
        "{\"from\":\"an earlier, complete run\"}",
    )
    .expect("seed an existing audit record");

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    assert_eq!(
        db.get_setting("gemini_common_config_scrub_audit_v1")
            .expect("read audit")
            .as_deref(),
        Some("{\"from\":\"an earlier, complete run\"}"),
        "an audit record from an earlier run must survive a retry"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_cleans_the_live_env_without_a_current_provider() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    // 没有当前供应商——这正是 sync_current_provider_for_app 直接返回 Ok 而
    // 根本不写文件的分支。此时 live 若清不掉，片段又已被清空，下次切换的
    // backfill 就会把残留永久写进受害供应商的配置。
    write_gemini_env(&HashMap::from([
        ("GOOGLE_API_KEY".to_string(), "key-A-leaked".to_string()),
        ("GEMINI_TIMEOUT_MS".to_string(), "30000".to_string()),
        // 只存在于 live 的手工修改：定向删除必须保住它，全量重投影会抹掉
        (
            "HTTPS_PROXY".to_string(),
            "http://127.0.0.1:7890".to_string(),
        ),
    ]))
    .expect("seed live env");

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    let live = crate::gemini_config::read_gemini_env().expect("read live env");
    assert!(
        !live.contains_key("GOOGLE_API_KEY"),
        "the leaked credential must be gone from ~/.gemini/.env: {live:?}"
    );
    assert_eq!(
        live.get("HTTPS_PROXY").map(String::as_str),
        Some("http://127.0.0.1:7890"),
        "a hand-added live-only var must survive targeted removal: {live:?}"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_live_cleanup_preserves_the_rest_of_the_env_file() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    // 这是一次用户没主动触发的启动期清理，不该顺手重写与泄漏无关的内容。
    // read→HashMap→write 的往返会把注释、空行、无法识别的行全丢掉并按键名重排。
    let original = "\
# my own notes
GOOGLE_API_KEY=key-C-owned

GOOGLE_API_KEY=key-A-leaked
this line is not KEY=VALUE at all
GEMINI_TIMEOUT_MS=30000
";
    crate::gemini_config::write_gemini_env_text_atomic(original).expect("seed live env");

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    let raw = std::fs::read_to_string(crate::gemini_config::get_gemini_env_path())
        .expect("read live env");
    assert!(
        !raw.contains("key-A-leaked"),
        "the leaked line must be gone: {raw:?}"
    );
    assert!(
        raw.contains("# my own notes"),
        "comments must survive a targeted removal: {raw:?}"
    );
    assert!(
        raw.contains("this line is not KEY=VALUE at all"),
        "unparseable lines must survive a targeted removal: {raw:?}"
    );
    // 被泄漏值遮住的那条重新生效——正是想要的结果，遮住它的恰恰是泄漏值
    assert_eq!(
        crate::gemini_config::read_gemini_env()
            .expect("read live env")
            .get("GOOGLE_API_KEY")
            .map(String::as_str),
        Some("key-C-owned"),
        "only the matching line may be dropped: {raw:?}"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_aborts_before_clearing_the_snippet_when_the_live_backup_fails() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    // 关代理时这份快照会被原样写回 live。若清不动它却照样清了片段、置了完成标记，
    // 代理一停凭据就复活，而一次性标记保证不会再清第二次。
    db.save_live_backup("gemini", "}not json{")
        .await
        .expect("seed backup");

    let result = ProviderService::scrub_leaked_gemini_common_config(&state).await;
    assert!(
        result.is_err(),
        "a backup that cannot be cleaned must abort the scrub"
    );

    // 片段是「该剥哪些键」的唯一知识来源，中止后必须原样留着，否则下次重试
    // 会因为 poison 为空而直接短路，反倒把标记置上
    let snippet = db
        .get_config_snippet("gemini")
        .expect("read snippet")
        .expect("snippet must still exist");
    assert!(
        snippet.contains("key-A-leaked"),
        "the snippet must be left intact so the next boot can retry: {snippet}"
    );
    assert!(
        db.get_setting("gemini_common_config_credentials_scrubbed_v1")
            .expect("read flag")
            .is_none(),
        "the one-shot flag must not be set when the scrub aborted"
    );
}

#[tokio::test]
#[serial]
async fn scrub_gemini_leaves_no_residue_for_backfill_to_persist() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("scrub must succeed");

    // 顺序陷阱回归：如果只清了片段，切走供应商时 remove_common_config_from_settings
    // 就不再认识这个键，live 里的残留会被 backfill 永久写进供应商配置。
    // 清理必须是原子的——清完之后，任何地方都不该再有那个值。
    let snippet = db
        .get_config_snippet("gemini")
        .expect("read snippet")
        .unwrap_or_default();
    assert!(!snippet.contains("key-A-leaked"));

    for (id, provider) in db.get_all_providers("gemini").expect("providers") {
        assert!(
            !provider
                .settings_config
                .to_string()
                .contains("key-A-leaked"),
            "provider '{id}' still carries the leaked value"
        );
    }
}

#[tokio::test]
#[serial]
async fn scrub_gemini_is_idempotent_and_skips_on_second_run() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");
    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());
    seed_leaked_gemini_state(&db);

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("first run");

    // 第二次必须是 no-op：用户清理后重新填的凭据不能被再抹一遍
    db.set_config_snippet(
        "gemini",
        Some(json!({"GOOGLE_API_KEY": "restored"}).to_string()),
    )
    .expect("user re-adds a value");

    ProviderService::scrub_leaked_gemini_common_config(&state)
        .await
        .expect("second run");

    let snippet = db
        .get_config_snippet("gemini")
        .expect("read snippet")
        .expect("snippet exists");
    assert!(
        snippet.contains("restored"),
        "the one-shot flag must prevent a second scrub: {snippet}"
    );
}

#[test]
fn extract_claude_common_config_strips_all_credentials_keeps_shareable() {
    // env 混入多种凭据（Anthropic/OpenRouter/Google/OpenAI/Gemini + AWS/Vertex）
    // 与可共享配置；顶层混入非标准的 apiKey/api_key 凭据与正常设置。
    let settings = json!({
        "env": {
            "ANTHROPIC_API_KEY": "sk-ant",
            "ANTHROPIC_AUTH_TOKEN": "tok-ant",
            "OPENROUTER_API_KEY": "sk-or",
            "GOOGLE_API_KEY": "g-key",
            "OPENAI_API_KEY": "sk-oai",
            "GEMINI_API_KEY": "g-gem",
            "AWS_ACCESS_KEY_ID": "AKIA",
            "AWS_SECRET_ACCESS_KEY": "secret",
            "AWS_SESSION_TOKEN": "sess",
            "GOOGLE_APPLICATION_CREDENTIALS": "/path/creds.json",
            "AWS_BEARER_TOKEN_BEDROCK": "bedrock-tok",
            "ANTHROPIC_BASE_URL": "https://example.com",
            "ANTHROPIC_MODEL": "claude-x",
            "CLAUDE_CODE_SUBAGENT_MODEL": "gpt-5.4-mini",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "400000",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "400000",
            // 可共享、非机密配置（复数 _TOKENS 不应被误剥）
            "ENABLE_TOOL_SEARCH": "true",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "8192"
        },
        "apiKey": "sk-top",
        "api_key": "sk-top2",
        "theme": "dark",
        "includeCoAuthoredBy": false
    });

    let snippet =
        ProviderService::extract_claude_common_config(&settings).expect("extract should succeed");
    let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");

    // 所有凭据都不得出现在共享片段里
    let env = value.get("env");
    for leaked in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENROUTER_API_KEY",
        "GOOGLE_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AWS_BEARER_TOKEN_BEDROCK",
    ] {
        assert!(
            env.and_then(|e| e.get(leaked)).is_none(),
            "credential {leaked} must not leak into common config"
        );
    }
    assert!(
        value.get("apiKey").is_none() && value.get("api_key").is_none(),
        "top-level credentials must be stripped"
    );

    // 端点/模型（provider-specific 非机密）也应剥掉
    assert!(env.and_then(|e| e.get("ANTHROPIC_BASE_URL")).is_none());
    assert!(env.and_then(|e| e.get("ANTHROPIC_MODEL")).is_none());
    assert!(env
        .and_then(|e| e.get("CLAUDE_CODE_SUBAGENT_MODEL"))
        .is_none());
    assert!(env
        .and_then(|e| e.get("CLAUDE_CODE_MAX_CONTEXT_TOKENS"))
        .is_none());
    assert!(env
        .and_then(|e| e.get("CLAUDE_CODE_AUTO_COMPACT_WINDOW"))
        .is_none());

    // 可共享的非机密配置必须保留（含复数 _TOKENS 不被误剥）
    assert_eq!(
        env.and_then(|e| e.get("ENABLE_TOOL_SEARCH"))
            .and_then(|v| v.as_str()),
        Some("true")
    );
    assert_eq!(
        env.and_then(|e| e.get("CLAUDE_CODE_MAX_OUTPUT_TOKENS"))
            .and_then(|v| v.as_str()),
        Some("8192")
    );
    assert_eq!(value.get("theme").and_then(|v| v.as_str()), Some("dark"));
    assert_eq!(value.get("includeCoAuthoredBy"), Some(&json!(false)));
}

/// 关键字段（协议选择器、Bedrock/Vertex 区域、`/model` 的选择等）不进共享片段；
/// 同在 `CLAUDE_CODE_USE_` 前缀下、与供应商无关的开关照常共享。
#[test]
fn extract_claude_common_config_keeps_key_fields_per_provider() {
    let settings = json!({
        "env": {
            "CLAUDE_CODE_USE_BEDROCK": "1",
            "CLAUDE_CODE_USE_VERTEX": "1",
            "CLAUDE_CODE_SKIP_BEDROCK_AUTH": "1",
            "AWS_REGION": "us-west-2",
            "AWS_PROFILE": "work",
            "CLOUD_ML_REGION": "us-east5",
            "VERTEX_REGION_CLAUDE_4_0_OPUS": "europe-west1",
            "ANTHROPIC_VERTEX_PROJECT_ID": "my-project",
            "ANTHROPIC_SMALL_FAST_MODEL": "haiku",
            "CLAUDE_CODE_SUBAGENT_MODEL_FORCE": "1",
            "CLAUDE_CODE_USE_POWERSHELL_TOOL": "1",
            "CLAUDE_CODE_ATTRIBUTION_HEADER": "0",
            "DISABLE_TELEMETRY": "1"
        },
        "model": "opus",
        "fallbackModel": "sonnet",
        "apiKeyHelper": "~/bin/key.sh",
        "awsAuthRefresh": "aws sso login",
        "hooks": { "Stop": [] },
        "theme": "dark"
    });

    let snippet =
        ProviderService::extract_claude_common_config(&settings).expect("extract should succeed");
    let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");

    assert_eq!(
        value,
        json!({
            "env": {
                "CLAUDE_CODE_USE_POWERSHELL_TOOL": "1",
                "CLAUDE_CODE_ATTRIBUTION_HEADER": "0",
                "DISABLE_TELEMETRY": "1"
            },
            "hooks": { "Stop": [] },
            "theme": "dark"
        })
    );
    assert!(
        snippet.find("CLAUDE_CODE_USE_POWERSHELL_TOOL") < snippet.find("DISABLE_TELEMETRY"),
        "removing keys must not reorder the ones that stay: {snippet}"
    );
}

/// Regression for issue #4272: Fable tier env keys must not enter the shared
/// Claude common-config snippet (same class as haiku/sonnet/opus model pins).
#[test]
fn extract_claude_common_config_strips_fable_model_env_keys() {
    let settings = json!({
        "env": {
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "haiku-mapped",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "Haiku Mapped",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "sonnet-mapped[1M]",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "Sonnet Mapped",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "opus-mapped[1M]",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "Opus Mapped",
            "ANTHROPIC_DEFAULT_FABLE_MODEL": "deepseek-v4-flash[1M]",
            "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME": "deepseek-v4-flash",
            "ANTHROPIC_MODEL": "default-mapped",
            "ENABLE_TOOL_SEARCH": "true"
        },
        "theme": "dark"
    });

    let snippet =
        ProviderService::extract_claude_common_config(&settings).expect("extract should succeed");
    let value: Value = serde_json::from_str(&snippet).expect("snippet is valid JSON");
    let env = value.get("env");

    for stripped in [
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
        "ANTHROPIC_MODEL",
    ] {
        assert!(
            env.and_then(|e| e.get(stripped)).is_none(),
            "provider-specific model key {stripped} must not enter common config"
        );
    }

    assert_eq!(
        env.and_then(|e| e.get("ENABLE_TOOL_SEARCH"))
            .and_then(|v| v.as_str()),
        Some("true")
    );
    assert_eq!(value.get("theme").and_then(|v| v.as_str()), Some("dark"));
}

#[test]
fn extract_codex_common_config_strips_provider_fields_and_injected_artifacts() {
    // 顶层 experimental_bearer_token 模拟无活跃路由时的 fallback 注入；
    // web_search = "disabled" 是 cc-switch 对黑名单网关注入的哨兵；
    // 顶层 wire_api 模拟无 model_provider 时的 fallback 写法；
    // [mcp.servers] 是历史错误格式，sync_all_enabled 清不掉它。
    let config_toml = r#"model_provider = "azure"
model = "gpt-4"
wire_api = "chat"
disable_response_storage = true
model_reasoning_effort = "high"
approval_policy = "on-request"
experimental_bearer_token = "sk-live-secret"
model_catalog_json = "cc-switch-model-catalog.json"
web_search = "disabled"

[agents]
default_subagent_model = "gpt-4-mini"
max_threads = 4

[model_providers.azure]
name = "Azure OpenAI"
base_url = "https://azure.example/v1"
wire_api = "responses"

[mcp_servers.my_server]
base_url = "http://localhost:8080"

[mcp.servers.legacy_server]
command = "legacy-cmd"
"#;

    let settings = json!({ "config": config_toml });
    let extracted = ProviderService::extract_codex_common_config(&settings)
        .expect("extract_codex_common_config should succeed");

    assert!(
        !extracted
            .lines()
            .any(|line| line.trim_start().starts_with("model_provider")),
        "should remove top-level model_provider"
    );
    assert!(
        !extracted
            .lines()
            .any(|line| line.trim_start().starts_with("model =")),
        "should remove top-level model"
    );
    assert!(
        !extracted.contains("[model_providers"),
        "should remove entire model_providers table"
    );
    // MCP 归 DB mcp_servers 表所有，不得进共享片段（含历史错误格式 [mcp.servers]）
    assert!(
        !extracted.contains("mcp_servers") && !extracted.contains("http://localhost:8080"),
        "should strip mcp_servers from the shared snippet, got: {extracted}"
    );
    assert!(
        !extracted.contains("[mcp") && !extracted.contains("legacy-cmd"),
        "should strip the legacy [mcp.servers] form from the shared snippet, got: {extracted}"
    );
    // 顶层 wire_api 是供应商路由语义（model_providers 整表已剥，
    // 剩余任何 wire_api 都意味着泄漏）
    assert!(
        !extracted.contains("wire_api"),
        "should strip top-level wire_api from the shared snippet, got: {extracted}"
    );
    // 注入产物不得进共享片段（bearer token 泄漏为密钥级问题）
    assert!(
        !extracted.contains("experimental_bearer_token") && !extracted.contains("sk-live-secret"),
        "should strip top-level fallback bearer token, got: {extracted}"
    );
    assert!(
        !extracted.contains("model_catalog_json"),
        "should strip catalog projection pointer, got: {extracted}"
    );
    assert!(
        !extracted.contains("web_search"),
        "should strip the cc-switch web_search disabled sentinel, got: {extracted}"
    );
    // 关键字段归供应商（片段已冻结，收进去会从行里剥掉、再也写不回 live）
    for key in [
        "disable_response_storage",
        "model_reasoning_effort",
        "default_subagent_model",
    ] {
        assert!(
            !extracted.contains(key),
            "key field {key} must stay with the provider, got: {extracted}"
        );
    }
    // 真正可共享的键保留
    assert!(
        extracted.contains("approval_policy = \"on-request\"")
            && extracted.contains("max_threads = 4"),
        "shareable keys must survive extraction, got: {extracted}"
    );
}

#[test]
fn extract_codex_common_config_keeps_user_set_web_search() {
    let config_toml = "web_search = \"enabled\"\ndisable_response_storage = true\n";
    let settings = json!({ "config": config_toml });
    let extracted =
        ProviderService::extract_codex_common_config(&settings).expect("extract should succeed");
    assert!(
        extracted.contains("web_search = \"enabled\""),
        "a user-set web_search value is a shareable preference, got: {extracted}"
    );
}

#[tokio::test]
#[serial]
async fn editing_the_routed_claude_provider_rewrites_the_proxy_contract() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());

    let original = Provider::with_id(
        "p1".into(),
        "Claude A".into(),
        json!({
            "env": {
                "ANTHROPIC_API_KEY": "token-a",
                "ANTHROPIC_BASE_URL": "https://api.a.example",
                "ANTHROPIC_MODEL": "model-a"
            }
        }),
        None,
    );
    db.save_provider("claude", &original)
        .expect("save provider");
    db.set_current_provider("claude", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
        .expect("set local current provider");
    write_json_file(
        &get_claude_settings_path(),
        &json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://api.a.example",
                "ANTHROPIC_API_KEY": "token-a",
                "ANTHROPIC_MODEL": "model-a"
            },
            "permissions": { "allow": ["Bash"] }
        }),
    )
    .expect("seed live file");

    db.update_global_proxy_config(GlobalProxyConfig {
        listen_port: 0,
        ..Default::default()
    })
    .await
    .expect("update proxy config");
    crate::mode::controller::enter(&state, &AppType::Claude, false)
        .await
        .expect("enter routing mode");
    let proxy_url = state
        .proxy_service
        .build_proxy_urls()
        .await
        .expect("proxy url")
        .0;

    let updated = Provider::with_id(
        "p1".into(),
        "Claude A".into(),
        json!({
            "env": {
                "ANTHROPIC_API_KEY": "token-updated",
                "ANTHROPIC_BASE_URL": "https://api.updated.example",
                "ANTHROPIC_MODEL": "model-updated"
            }
        }),
        None,
    );
    ProviderService::update(&state, AppType::Claude, None, updated)
        .expect("update routed provider");

    let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
    let env = &live["env"];
    // 行里只配了 `ANTHROPIC_API_KEY`（供上游跳用 x-api-key），本地占位符仍落在
    // `ANTHROPIC_AUTH_TOKEN`，且本地不出现 `ANTHROPIC_API_KEY`（#8104）。
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "PROXY_MANAGED");
    assert_eq!(
        env.get("ANTHROPIC_API_KEY"),
        None,
        "route mode must not present the placeholder as a custom API key"
    );
    assert_eq!(env["ANTHROPIC_BASE_URL"], proxy_url.as_str());
    assert!(env.get("ANTHROPIC_MODEL").is_none(), "{live}");
    assert_eq!(
        env["ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"], "model-updated",
        "the contract's display names follow the edited provider"
    );
    assert_eq!(
        live["permissions"],
        json!({ "allow": ["Bash"] }),
        "user settings in live are left alone"
    );

    state
        .proxy_service
        .stop()
        .await
        .expect("stop proxy service");
}

#[tokio::test]
#[serial]
async fn update_current_codex_provider_refreshes_and_clears_catalog_during_takeover() {
    let _home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    let state = AppState::new(db.clone());

    let mut original = Provider::with_id(
        "p1".into(),
        "Codex A".into(),
        json!({
            "auth": { "OPENAI_API_KEY": "token-a" },
            "config": r#"model_provider = "custom"
model = "old-model"

[model_providers.custom]
name = "Codex A"
base_url = "https://api.a.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#,
            "modelCatalog": {
                "models": [{ "model": "old-model" }]
            }
        }),
        None,
    );
    original.meta = Some(ProviderMeta {
        api_format: Some("openai_responses".into()),
        ..Default::default()
    });
    db.save_provider("codex", &original).expect("save provider");
    db.set_current_provider("codex", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::Codex, Some("p1"))
        .expect("set local current provider");

    db.update_global_proxy_config(GlobalProxyConfig {
        listen_port: 0,
        ..Default::default()
    })
    .await
    .expect("update proxy config");
    crate::mode::controller::enter(&state, &AppType::Codex, false)
        .await
        .expect("enter routing mode");
    assert!(
        state
            .proxy_service
            .live_has_proxy_placeholder(&AppType::Codex),
        "Codex live config should carry the proxy contract"
    );

    let mut updated = original.clone();
    updated.settings_config["config"] = json!(
        r#"model_provider = "custom"
model = "gpt-5.4"

[model_providers.custom]
name = "Codex A"
base_url = "https://api.updated.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#
    );
    updated.settings_config["modelCatalog"] = json!({
        "models": [{ "model": "gpt-5.4", "displayName": "GPT 5.4" }]
    });

    ProviderService::update(&state, AppType::Codex, None, updated.clone())
        .expect("update current Codex provider mapping");

    let catalog_path = crate::codex_config::get_codex_model_catalog_path();
    let catalog: Value = read_json_file(&catalog_path).expect("read generated catalog");
    assert_eq!(catalog["models"][0]["slug"], "gpt-5.4");
    assert_eq!(
        catalog["models"][0]["input_modalities"],
        json!(["text", "image"]),
        "unknown/GPT models must fail open to image input"
    );
    let live_config = fs::read_to_string(crate::codex_config::get_codex_config_path())
        .expect("read Codex config.toml");
    assert!(live_config.contains("model_catalog_json"));

    updated.settings_config["modelCatalog"] = json!({ "models": [] });
    ProviderService::update(&state, AppType::Codex, None, updated)
        .expect("remove current Codex provider mapping");

    let live_config = fs::read_to_string(crate::codex_config::get_codex_config_path())
        .expect("read Codex config.toml after mapping removal");
    assert!(
        !live_config.contains("model_catalog_json"),
        "removing mappings during takeover must clear the stale catalog pointer"
    );

    state
        .proxy_service
        .stop()
        .await
        .expect("stop proxy service");
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[tokio::test]
#[serial]
async fn update_claude_desktop_route_provider_rewrites_profile_in_route_mode() {
    let home = TempHome::new().await;
    crate::settings::reload_settings().expect("reload settings");

    let db = Arc::new(Database::memory().expect("init db"));
    db.update_global_proxy_config(GlobalProxyConfig {
        listen_port: 0,
        ..Default::default()
    })
    .await
    .expect("ephemeral proxy port");
    let state = AppState::new(db.clone());

    let mut original = Provider::with_id(
        "p1".into(),
        "Desktop A".into(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "token-a",
                "ANTHROPIC_BASE_URL": "https://opencode.ai/zen/go"
            }
        }),
        None,
    );
    original.meta = Some(ProviderMeta {
        api_format: Some("openai_chat".into()),
        claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
        claude_desktop_model_routes: std::collections::HashMap::from([(
            "claude-sonnet-4-6".into(),
            ClaudeDesktopModelRoute {
                model: "deepseek-v4-flash".into(),
                label_override: Some("DeepSeek V4 Flash".into()),
                supports_1m: None,
            },
        )]),
        ..Default::default()
    });
    db.save_provider("claude-desktop", &original)
        .expect("save provider");
    db.set_current_provider("claude-desktop", "p1")
        .expect("set current provider");
    crate::settings::set_current_provider(&AppType::ClaudeDesktop, Some("p1"))
        .expect("set local current provider");

    // 路由模式路由到这张映射卡：编辑它要按新行重写指向本地路由的 profile。
    crate::mode::controller::enter_with_route(&state, &AppType::ClaudeDesktop, false, Some("p1"))
        .await
        .expect("enter desktop route mode");
    let gateway_url = crate::claude_desktop_config::proxy_gateway_base_url_from_db(&db)
        .expect("running Desktop gateway");

    let mut updated = Provider::with_id(
        "p1".into(),
        "Desktop A".into(),
        json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "token-updated",
                "ANTHROPIC_BASE_URL": "https://opencode.ai/zen/go"
            }
        }),
        None,
    );
    updated.meta = Some(ProviderMeta {
        api_format: Some("openai_chat".into()),
        claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
        claude_desktop_model_routes: std::collections::HashMap::from([(
            "claude-sonnet-4-6".into(),
            ClaudeDesktopModelRoute {
                model: "deepseek-v4-flash".into(),
                label_override: Some("DeepSeek V4 Flash Updated".into()),
                supports_1m: Some(true),
            },
        )]),
        ..Default::default()
    });

    // 同步命令里取切换锁会阻塞运行时线程：放到阻塞线程池里（同 `switch_provider` 命令）。
    let update_state = state.clone();
    let update_row = updated.clone();
    tokio::task::spawn_blocking(move || {
        ProviderService::update(&update_state, AppType::ClaudeDesktop, None, update_row)
    })
    .await
    .expect("join update")
    .expect("update route provider");

    let profile_path = claude_desktop_profile_path(home.dir.path());
    let profile: Value = read_json_file(&profile_path).expect("read desktop profile");
    assert_eq!(
        profile["inferenceGatewayBaseUrl"],
        json!(gateway_url),
        "desktop profile should stay pointed at the local gateway during takeover"
    );
    assert_eq!(profile["inferenceGatewayAuthScheme"], json!("bearer"));
    assert_eq!(
        profile["inferenceModels"],
        json!([{ "name": "claude-sonnet-4-6", "labelOverride": "DeepSeek V4 Flash Updated", "supports1m": true, "prefer1m": true }]),
        "provider edits should propagate into the Claude Desktop 3P profile during takeover"
    );
    state
        .proxy_service
        .stop()
        .await
        .expect("stop proxy service");
}

#[test]
#[serial]
fn rename_rejects_missing_original_provider() {
    with_test_home(|state, _| {
        let original = openclaw_provider("deepseek");
        ProviderService::add(state, AppType::OpenClaw, original.clone(), false)
            .expect("seed db-only provider");

        let mut renamed = original;
        renamed.id = "deepseek-copy".to_string();

        let err =
            ProviderService::update(state, AppType::OpenClaw, Some("missing-provider"), renamed)
                .expect_err("stale originalId should be rejected");

        assert!(
            err.to_string().contains("Original provider"),
            "expected missing original provider error, got {err:?}"
        );
        assert!(
            state
                .db
                .get_provider_by_id("deepseek-copy", AppType::OpenClaw.as_str())
                .expect("query renamed provider")
                .is_none(),
            "rename must not create a new row when originalId is stale"
        );
    });
}

#[test]
#[serial]
fn db_only_additive_update_survives_live_config_parse_errors() {
    with_test_home(|state, home| {
        let provider = openclaw_provider("deepseek");
        ProviderService::add(state, AppType::OpenClaw, provider, false)
            .expect("seed db-only provider");

        let stored = state
            .db
            .get_provider_by_id("deepseek", AppType::OpenClaw.as_str())
            .expect("query stored provider")
            .expect("provider should exist");
        assert_eq!(
            stored
                .meta
                .as_ref()
                .and_then(|meta| meta.live_config_managed),
            Some(false),
            "db-only provider should be marked as not live-managed"
        );

        let openclaw_dir = home.join(".openclaw");
        fs::create_dir_all(&openclaw_dir).expect("create openclaw dir");
        fs::write(openclaw_dir.join("openclaw.json"), "{ invalid json5")
            .expect("write malformed config");

        let mut updated = stored;
        updated.name = "DeepSeek Edited".to_string();
        updated.meta.get_or_insert_with(ProviderMeta::default);

        ProviderService::update(state, AppType::OpenClaw, None, updated)
            .expect("db-only update should ignore live parse errors");

        let saved = state
            .db
            .get_provider_by_id("deepseek", AppType::OpenClaw.as_str())
            .expect("query updated provider")
            .expect("updated provider should exist");
        assert_eq!(saved.name, "DeepSeek Edited");
    });
}

#[test]
#[serial]
fn sync_current_provider_for_app_skips_db_only_opencode_provider() {
    with_test_home(|state, _| {
        let provider = opencode_provider("db-only-opencode");
        ProviderService::add(state, AppType::OpenCode, provider.clone(), false)
            .expect("seed db-only opencode provider");

        ProviderService::sync_current_provider_for_app(state, AppType::OpenCode)
            .expect("sync additive opencode providers");

        let live_providers =
            crate::opencode_config::get_providers().expect("read opencode providers after sync");
        assert!(
            !live_providers.contains_key(&provider.id),
            "db-only opencode provider should not be written to live during sync"
        );
    });
}

#[test]
#[serial]
fn sync_current_provider_for_app_skips_db_only_openclaw_provider() {
    with_test_home(|state, _| {
        let provider = openclaw_provider("db-only-openclaw");
        ProviderService::add(state, AppType::OpenClaw, provider.clone(), false)
            .expect("seed db-only openclaw provider");

        ProviderService::sync_current_provider_for_app(state, AppType::OpenClaw)
            .expect("sync additive openclaw providers");

        let live_providers =
            crate::openclaw_config::get_providers().expect("read openclaw providers after sync");
        assert!(
            !live_providers.contains_key(&provider.id),
            "db-only openclaw provider should not be written to live during sync"
        );
    });
}

#[test]
#[serial]
fn sync_current_provider_for_app_preserves_legacy_live_opencode_provider() {
    with_test_home(|state, _| {
        let provider = opencode_provider("legacy-opencode");
        crate::opencode_config::set_provider(&provider.id, provider.settings_config.clone())
            .expect("seed opencode live provider");
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &provider)
            .expect("seed legacy opencode provider in db");

        let mut updated = provider.clone();
        updated.settings_config["options"]["apiKey"] = Value::String("updated-key".to_string());
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &updated)
            .expect("update legacy opencode provider in db");

        ProviderService::sync_current_provider_for_app(state, AppType::OpenCode)
            .expect("sync legacy opencode provider");

        let live_providers =
            crate::opencode_config::get_providers().expect("read opencode providers");
        assert_eq!(
            live_providers
                .get(&provider.id)
                .and_then(|config| config.get("options"))
                .and_then(|options| options.get("apiKey")),
            Some(&Value::String("updated-key".to_string())),
            "legacy provider that already exists in live should still be synced"
        );
    });
}

#[test]
#[serial]
fn sync_current_provider_for_app_restores_legacy_opencode_provider_after_live_reset() {
    with_test_home(|state, _| {
        let provider = opencode_provider("legacy-opencode-reset");
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &provider)
            .expect("seed legacy opencode provider in db");

        ProviderService::sync_current_provider_for_app(state, AppType::OpenCode)
            .expect("sync legacy opencode provider after reset");

        let live_providers =
            crate::opencode_config::get_providers().expect("read opencode providers");
        assert!(
            live_providers.contains_key(&provider.id),
            "legacy opencode provider should be restored when live config is reset"
        );
    });
}

#[test]
#[serial]
fn sync_current_provider_for_app_restores_legacy_openclaw_provider_after_live_reset() {
    with_test_home(|state, _| {
        let mut provider = openclaw_provider("legacy-openclaw-reset");
        provider.settings_config["models"] = json!([
            {
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4"
            }
        ]);
        state
            .db
            .save_provider(AppType::OpenClaw.as_str(), &provider)
            .expect("seed legacy openclaw provider in db");

        ProviderService::sync_current_provider_for_app(state, AppType::OpenClaw)
            .expect("sync legacy openclaw provider after reset");

        let live_providers =
            crate::openclaw_config::get_providers().expect("read openclaw providers");
        assert!(
            live_providers.contains_key(&provider.id),
            "legacy openclaw provider should be restored when live config is reset"
        );
    });
}

#[test]
#[serial]
fn add_first_managed_codex_with_reauth_required_account_is_rejected() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_access_token("acct-legacy", "managed-token", None)
                .await
                .expect("seed legacy account without id_token");
        });
        let provider = managed_codex_provider("managed-legacy", "acct-legacy");

        let error = ProviderService::add(state, AppType::Codex, provider.clone(), false)
            .expect_err("reauth-required account must not be written to live auth");
        assert!(
            error.to_string().contains("id_token"),
            "backend should require re-login even if the frontend gate is bypassed: {error}"
        );
        assert!(state
            .db
            .get_provider_by_id(&provider.id, AppType::Codex.as_str())
            .expect("query provider")
            .is_none());
        assert!(!crate::codex_config::get_codex_auth_path().exists());
    });
}

#[test]
#[serial]
fn switch_from_managed_codex_official_to_unbound_clears_live_without_backfilling_token() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-token",
                    "managed-user",
                )
                .await
                .expect("seed managed Codex OAuth account");
        });

        let mut managed = Provider::with_id(
            "managed-official".to_string(),
            "Managed Official".to_string(),
            json!({
                "auth": {},
                "config": ""
            }),
            None,
        );
        managed.category = Some("official".to_string());
        managed.meta = Some(ProviderMeta {
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some("acct-managed".to_string()),
            }),
            ..Default::default()
        });

        let mut unbound = Provider::with_id(
            "unbound-official".to_string(),
            "Unbound Official".to_string(),
            json!({
                "auth": {},
                "config": ""
            }),
            None,
        );
        unbound.category = Some("official".to_string());

        state
            .db
            .save_provider(AppType::Codex.as_str(), &managed)
            .expect("save managed provider");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &unbound)
            .expect("save unbound provider");

        ProviderService::switch(state, AppType::Codex, "managed-official")
            .expect("switch to managed official");
        let live_auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
            .expect("read managed live auth");
        assert_eq!(
            live_auth
                .pointer("/tokens/access_token")
                .and_then(Value::as_str),
            Some("managed-token"),
            "managed switch should write the selected ChatGPT token to live auth"
        );

        // Simulate a bare Codex CLI self-refresh. The app marker still
        // describes the pre-refresh write, while both access and refresh
        // token material on disk have rotated.
        let rotated_id_token = crate::codex_config::test_codex_id_token("managed-user");
        let rotated_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
            "acct-managed",
            "cli-rotated-access",
            Some(&rotated_id_token),
            "cli-rotated-refresh",
            "2099-01-02T03:04:05Z",
        );
        write_json_file(
            &crate::codex_config::get_codex_auth_path(),
            &rotated_live_auth,
        )
        .expect("simulate Codex CLI token rotation");

        ProviderService::switch(state, AppType::Codex, "unbound-official")
            .expect("switch to unbound official");

        assert!(
            !crate::codex_config::get_codex_auth_path().exists(),
            "switching to an unbound official provider should clear the recorded managed live auth"
        );
        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-managed")
            )
            .as_deref(),
            Some("cli-rotated-refresh"),
            "switch-away must adopt the CLI-rotated refresh token before deleting live auth"
        );

        let saved_managed = state
            .db
            .get_provider_by_id("managed-official", AppType::Codex.as_str())
            .expect("query managed provider")
            .expect("managed provider should exist");
        assert_eq!(
            saved_managed.settings_config.get("auth"),
            Some(&json!({})),
            "switch-away backfill must not persist the managed access token into provider storage"
        );
    });
}

#[test]
#[serial]
fn managed_codex_switch_adopts_outgoing_cli_rotation_before_account_or_key_overwrite() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-a", "managed-access-a", "user-a")
                .await
                .expect("seed account A");
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-b", "managed-access-b", "user-b")
                .await
                .expect("seed account B");
        });

        let provider_a = managed_codex_provider("managed-a", "acct-a");
        let provider_b = managed_codex_provider("managed-b", "acct-b");
        let mut third_party = Provider::with_id(
            "third-party".to_string(),
            "Third Party".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "sk-third-party" },
                "config": r#"model_provider = "third"
[model_providers.third]
name = "Third"
base_url = "https://third.example/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        third_party.category = Some("custom".to_string());
        for provider in [&provider_a, &provider_b, &third_party] {
            state
                .db
                .save_provider(AppType::Codex.as_str(), provider)
                .expect("save provider");
        }

        ProviderService::switch(state, AppType::Codex, &provider_a.id).expect("activate managed A");
        let id_token_a = crate::codex_config::test_codex_id_token("user-a");
        write_json_file(
            &crate::codex_config::get_codex_auth_path(),
            &crate::codex_config::codex_managed_oauth_auth_value(
                "acct-a",
                "cli-access-a1",
                Some(&id_token_a),
                "cli-refresh-a1",
                "2099-01-02T00:00:00Z",
            ),
        )
        .expect("rotate account A live auth");

        ProviderService::switch(state, AppType::Codex, &provider_b.id)
            .expect("switch managed A to managed B");
        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-a")
            )
            .as_deref(),
            Some("cli-refresh-a1"),
            "A's CLI generation must be adopted before B overwrites auth.json"
        );
        let live_b: Value =
            read_json_file(&crate::codex_config::get_codex_auth_path()).expect("read B auth");
        assert_eq!(
            live_b.pointer("/tokens/account_id").and_then(Value::as_str),
            Some("acct-b")
        );

        let id_token_b = crate::codex_config::test_codex_id_token("user-b");
        write_json_file(
            &crate::codex_config::get_codex_auth_path(),
            &crate::codex_config::codex_managed_oauth_auth_value(
                "acct-b",
                "cli-access-b1",
                Some(&id_token_b),
                "cli-refresh-b1",
                "2099-01-03T00:00:00Z",
            ),
        )
        .expect("rotate account B live auth");

        ProviderService::switch(state, AppType::Codex, &third_party.id)
            .expect("switch managed B to API-key provider");
        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-b")
            )
            .as_deref(),
            Some("cli-refresh-b1"),
            "B's CLI generation must be adopted before the third-party switch removes auth.json"
        );
        assert!(
            !crate::codex_config::get_codex_auth_path().exists(),
            "third-party switches are config-only: auth.json is removed"
        );
        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read third-party config");
        assert!(
            live_config.contains("experimental_bearer_token = \"sk-third-party\""),
            "the third-party key rides in config.toml; got:\n{live_config}"
        );
    });
}

#[test]
#[serial]
fn managed_codex_direct_update_adopts_outgoing_cli_rotation_and_commits_target_binding() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-a", "managed-access-a", "user-a")
                .await
                .expect("seed account A");
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-b", "managed-access-b", "user-b")
                .await
                .expect("seed account B");
        });

        let provider = managed_codex_provider("managed-official-a", "acct-a");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("save managed official provider");
        ProviderService::switch(state, AppType::Codex, &provider.id)
            .expect("activate managed account A");
        assert!(
            tauri::async_runtime::block_on(state.db.get_live_backup(AppType::Codex.as_str()))
                .expect("read initial live backup")
                .is_none(),
            "direct update precondition requires no takeover backup"
        );

        let id_token_a = crate::codex_config::test_codex_id_token("user-a");
        write_json_file(
            &crate::codex_config::get_codex_auth_path(),
            &crate::codex_config::codex_managed_oauth_auth_value(
                "acct-a",
                "cli-access-a1",
                Some(&id_token_a),
                "cli-refresh-a1",
                "2099-03-01T00:00:00Z",
            ),
        )
        .expect("simulate account A CLI rotation");

        let mut updated = provider.clone();
        updated.name = "OpenAI Official B".to_string();
        updated
            .meta
            .as_mut()
            .and_then(|meta| meta.auth_binding.as_mut())
            .expect("managed binding")
            .account_id = Some("acct-b".to_string());

        ProviderService::update(state, AppType::Codex, None, updated.clone())
            .expect("directly update managed binding from A to B");

        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-a")
            )
            .as_deref(),
            Some("cli-refresh-a1"),
            "direct update must adopt A's CLI generation before overwriting live auth"
        );
        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::Codex.as_str())
            .expect("read updated provider")
            .expect("updated provider exists");
        assert_eq!(saved.name, updated.name);
        assert_eq!(
            saved
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
                .as_deref(),
            Some("acct-b")
        );

        let live_b: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
            .expect("read account B live auth");
        assert_eq!(
            live_b.pointer("/tokens/account_id").and_then(Value::as_str),
            Some("acct-b")
        );
        assert!(
            crate::codex_config::codex_auth_matches_recorded_managed_oauth(&live_b, "acct-b")
                .expect("check account B marker"),
            "clearing outgoing account A must not remove account B's marker"
        );
        assert!(
            tauri::async_runtime::block_on(state.db.get_live_backup(AppType::Codex.as_str()))
                .expect("read live backup after direct update")
                .is_none(),
            "direct update must not create a takeover backup"
        );
    });
}

#[test]
#[serial]
fn same_account_managed_codex_update_rejects_equal_timestamp_refresh_conflict() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access",
                    "managed-user",
                )
                .await
                .expect("seed managed account");
        });

        let provider = managed_codex_provider("managed-same-account", "acct-managed");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("save managed provider");
        ProviderService::switch(state, AppType::Codex, &provider.id)
            .expect("activate managed provider");

        // Different refresh material at the exact manager generation
        // timestamp is ambiguous at millisecond precision. A same-account
        // update has no outgoing-account guard, so its managed bundle
        // preflight itself must refuse to overwrite this CLI generation.
        tauri::async_runtime::block_on(
            state
                .codex_oauth_manager
                .test_set_token_updated_at_ms("acct-managed", 1_700_000_000_000),
        );
        let id_token = crate::codex_config::test_codex_id_token("managed-user");
        let cli_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
            "acct-managed",
            "cli-access-r1",
            Some(&id_token),
            "cli-refresh-r1",
            "2023-11-14T22:13:20Z",
        );
        write_json_file(&crate::codex_config::get_codex_auth_path(), &cli_live_auth)
            .expect("seed equal-timestamp CLI generation");

        let mut updated = provider.clone();
        updated.name = "Managed updated".to_string();
        let error = ProviderService::update(state, AppType::Codex, None, updated)
            .expect_err("ambiguous same-account generation must block the live write");
        assert!(
            error
                .to_string()
                .contains("无法安全判断 refresh token 新旧"),
            "update should explain the safe-write rejection: {error}"
        );

        let live_after: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
            .expect("read preserved CLI auth");
        assert_eq!(
            live_after, cli_live_auth,
            "same-account managed update must not overwrite ambiguous CLI token material"
        );
        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-managed")
            )
            .as_deref(),
            Some("test-refresh-token"),
            "ambiguous CLI material must not replace the manager generation either"
        );
        assert_eq!(
            state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("read provider after rejected update")
                .expect("provider remains present")
                .name,
            provider.name,
            "rejected preflight must leave the provider row unchanged"
        );
    });
}

#[test]
#[serial]
fn switch_away_rejects_legacy_refresh_conflict_on_every_retry() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-legacy", "managed-access", "legacy-user")
                .await
                .expect("seed managed account");
        });

        let managed = managed_codex_provider("managed-legacy", "acct-legacy");
        let mut unbound = Provider::with_id(
            "unbound-official".to_string(),
            "Unbound Official".to_string(),
            json!({
                "auth": {},
                "config": ""
            }),
            None,
        );
        unbound.category = Some("official".to_string());
        for provider in [&managed, &unbound] {
            state
                .db
                .save_provider(AppType::Codex.as_str(), provider)
                .expect("save provider");
        }
        ProviderService::switch(state, AppType::Codex, &managed.id)
            .expect("activate managed provider");

        tauri::async_runtime::block_on(
            state
                .codex_oauth_manager
                .test_set_token_updated_at_ms("acct-legacy", 0),
        );
        let id_token = crate::codex_config::test_codex_id_token("legacy-user");
        let cli_live_auth = crate::codex_config::codex_managed_oauth_auth_value(
            "acct-legacy",
            "cli-access-r1",
            Some(&id_token),
            "cli-refresh-r1",
            "2023-11-14T22:13:20Z",
        );
        write_json_file(&crate::codex_config::get_codex_auth_path(), &cli_live_auth)
            .expect("seed CLI generation against legacy manager state");

        for attempt in 1..=2 {
            let error = ProviderService::switch(state, AppType::Codex, &unbound.id)
                .expect_err("legacy conflict must block every switch-away retry");
            assert!(
                error
                    .to_string()
                    .contains("无法安全判断 refresh token 新旧"),
                "attempt {attempt} should remain ambiguous: {error}"
            );
            let live_after: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read preserved CLI auth");
            assert_eq!(
                live_after, cli_live_auth,
                "attempt {attempt} must not overwrite or delete the CLI generation"
            );
            assert_eq!(
                state
                    .db
                    .get_current_provider(AppType::Codex.as_str())
                    .expect("read current provider")
                    .as_deref(),
                Some(managed.id.as_str()),
                "attempt {attempt} must not commit the target provider"
            );
        }

        assert_eq!(
            tauri::async_runtime::block_on(
                state
                    .codex_oauth_manager
                    .test_refresh_token_for_account("acct-legacy")
            )
            .as_deref(),
            Some("test-refresh-token"),
            "ambiguous legacy retries must keep manager material unchanged"
        );
    });
}

#[test]
#[serial]
fn codex_auth_center_remove_and_logout_clear_live_credentials_and_marker() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access",
                    "managed-user",
                )
                .await
                .expect("seed managed account");
        });
        let provider = managed_codex_provider("managed-auth-center", "acct-managed");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("save managed provider");
        ProviderService::switch(state, AppType::Codex, &provider.id)
            .expect("activate managed provider");
        assert!(crate::codex_config::get_codex_auth_path().exists());
        assert!(crate::codex_config::codex_managed_oauth_live_auth_marker_exists());

        tauri::async_runtime::block_on(state.codex_oauth_manager.remove_account("acct-managed"))
            .expect("remove managed account");
        assert!(
            !crate::codex_config::get_codex_auth_path().exists(),
            "removing the active account must delete its refreshable live auth"
        );
        assert!(
            !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
            "removing the active account must delete its marker"
        );
        assert_eq!(
            state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("read provider")
                .and_then(|provider| provider.meta)
                .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
                .as_deref(),
            Some("acct-managed"),
            "the binding is retained so re-login with the same account can recover it"
        );

        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access-2",
                    "managed-user",
                )
                .await
                .expect("re-login managed account");
        });
        ProviderService::switch(state, AppType::Codex, &provider.id)
            .expect("reactivate managed provider after re-login");
        assert!(crate::codex_config::get_codex_auth_path().exists());

        tauri::async_runtime::block_on(state.codex_oauth_manager.clear_auth())
            .expect("logout all managed accounts");
        assert!(!crate::codex_config::get_codex_auth_path().exists());
        assert!(!crate::codex_config::codex_managed_oauth_live_auth_marker_exists());
        assert!(
            tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts()).is_empty()
        );
    });
}

#[test]
#[serial]
fn deleted_codex_account_can_rebind_or_switch_in_either_mode() {
    for mode in ["direct", "proxy"] {
        for rebind in [true, false] {
            with_test_home(|state, _| {
                crate::settings::reload_settings().unwrap();
                let runtime = tauri::async_runtime::handle();
                let token = crate::codex_config::test_codex_id_token("same-user");
                runtime
                    .block_on(
                        state
                            .codex_oauth_manager
                            .add_test_account_with_workspace_and_access_token(
                                "old-local-id",
                                "workspace",
                                "old-access",
                                Some(&token),
                            ),
                    )
                    .unwrap();
                runtime.block_on(async {
                    let mut config = state.db.get_global_proxy_config().await.unwrap();
                    config.listen_port = 0;
                    state.db.update_global_proxy_config(config).await.unwrap();
                });
                let current = managed_codex_provider("current", "old-local-id");
                state.db.save_provider("codex", &current).unwrap();
                ProviderService::switch(state, AppType::Codex, &current.id).unwrap();
                if mode == "proxy" {
                    runtime
                        .block_on(crate::mode::controller::enter(
                            state,
                            &AppType::Codex,
                            false,
                        ))
                        .unwrap();
                }
                runtime
                    .block_on(remove_codex_account_leaving_dangling_bindings(
                        state,
                        "old-local-id",
                    ))
                    .unwrap();
                let restarted = (mode == "direct").then(|| AppState::new(state.db.clone()));
                let state = restarted.as_ref().unwrap_or(state);
                // Ordinary login creates a new local ID, even for the same user/workspace.
                runtime
                    .block_on(
                        state
                            .codex_oauth_manager
                            .add_test_account_with_workspace_and_access_token(
                                "new-local-id",
                                "workspace",
                                "new-access",
                                Some(&token),
                            ),
                    )
                    .unwrap();
                assert_eq!(
                    ProviderService::managed_codex_oauth_account_id(
                        &state
                            .db
                            .get_provider_by_id("current", "codex")
                            .unwrap()
                            .unwrap()
                    )
                    .as_deref(),
                    Some("old-local-id")
                );

                // 绑定已失效：直连下进入路由要报错让用户重新绑定，客户端文件不动。
                if mode == "direct" {
                    let error = runtime
                        .block_on(crate::mode::controller::enter(
                            state,
                            &AppType::Codex,
                            false,
                        ))
                        .unwrap_err();
                    assert!(error.contains("选择账号"), "{error}");
                    assert!(!crate::mode::current::is_proxy(&AppType::Codex));
                    assert!(!state
                        .proxy_service
                        .live_has_proxy_placeholder(&AppType::Codex));
                }

                let target = managed_codex_provider(
                    if rebind { "current" } else { "target" },
                    "new-local-id",
                );
                if rebind {
                    ProviderService::update(state, AppType::Codex, None, target.clone()).unwrap();
                } else {
                    state.db.save_provider("codex", &target).unwrap();
                    ProviderService::switch(state, AppType::Codex, &target.id).unwrap();
                }
                let auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
                    .unwrap_or_else(|error| panic!("{mode}, rebind={rebind}: {error}"));
                assert_eq!(
                    auth["tokens"]["access_token"], "new-access",
                    "{mode}, rebind={rebind}"
                );
                assert_eq!(
                    crate::mode::current::provider_for(
                        &state.db,
                        &AppType::Codex,
                        crate::mode::current::Purpose::InUse,
                    )
                    .unwrap()
                    .as_deref(),
                    Some(target.id.as_str())
                );

                if mode == "proxy" && !rebind {
                    // 路由换了，直连指针仍是失效的那家：在路由模式下改它的绑定只存行，
                    // 退出路由时再写回。
                    ProviderService::update(
                        state,
                        AppType::Codex,
                        None,
                        managed_codex_provider("current", "new-local-id"),
                    )
                    .unwrap();
                }
                runtime.block_on(async {
                    if mode == "direct" {
                        crate::mode::controller::enter(state, &AppType::Codex, false)
                            .await
                            .unwrap();
                    }
                    crate::mode::controller::exit(state, &AppType::Codex)
                        .await
                        .unwrap();
                    assert!(!state.proxy_service.is_running().await);
                });
                let restored: Value =
                    read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
                assert_eq!(
                    restored["tokens"]["access_token"], "new-access",
                    "leaving routing mode must keep the replacement account ({mode}, rebind={rebind})"
                );
            });
        }
    }
}

/// 删号但不解绑供应商：模拟旧版删号留下的悬空行。解绑失败、别的设备同步来的行
/// 同样会悬空，恢复路径仍要能处理。
async fn remove_codex_account_leaving_dangling_bindings(
    state: &AppState,
    account_id: &str,
) -> Result<(), String> {
    let _switch_guard = state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    state
        .codex_oauth_manager
        .remove_account(account_id)
        .await
        .map_err(|error| error.to_string())
}

fn seed_codex_recovery_accounts(state: &AppState) {
    tauri::async_runtime::block_on(async {
        for (id, user) in [("acct-old", "old-user"), ("acct-new", "new-user")] {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(id, "access", user)
                .await
                .expect("seed account");
        }
    });
}

#[test]
#[serial]
fn codex_account_removal_and_logout_unbind_current_and_inactive_cards() {
    for logout_all in [false, true] {
        with_test_home(|state, _| {
            crate::settings::reload_settings().unwrap();
            seed_codex_recovery_accounts(state);
            let current = managed_codex_provider("current", "acct-old");
            let mut legacy = managed_codex_provider("legacy", "acct-old");
            legacy.category = None;
            legacy.notes = Some("keep notes".into());
            legacy.settings_config["auth"] = json!({"tokens": {"refresh_token": "stale-snapshot"}});
            legacy.settings_config["model"] = json!("keep-model");
            let other = managed_codex_provider("other", "acct-new");
            // 从另一台设备同步来的行：绑的是那台设备的本地账号，本机删号或登出都不能动它，
            // 否则解绑会随云同步回去，弄坏那台设备上正常的绑定。
            let foreign = managed_codex_provider("foreign", "acct-other-device");
            let mut unrelated = managed_codex_provider("unrelated", "acct-old");
            unrelated
                .meta
                .as_mut()
                .unwrap()
                .auth_binding
                .as_mut()
                .unwrap()
                .source = AuthBindingSource::ProviderConfig;
            for provider in [&current, &legacy, &other, &foreign, &unrelated] {
                state.db.save_provider("codex", provider).unwrap();
            }
            state.db.save_provider("claude", &current).unwrap();
            state
                .db
                .conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE providers SET meta = json_set(meta, '$.futureField', 'keep') WHERE id = 'legacy'",
                    [],
                )
                .unwrap();
            ProviderService::switch(state, AppType::Codex, &current.id).unwrap();
            if logout_all {
                tauri::async_runtime::block_on(
                    crate::commands::logout_codex_oauth_with_switch_lock(state),
                )
                .unwrap();
            } else {
                tauri::async_runtime::block_on(
                    crate::commands::remove_codex_oauth_account_with_switch_lock(state, "acct-old"),
                )
                .unwrap();
            }
            for id in ["current", "legacy"] {
                let saved = state.db.get_provider_by_id(id, "codex").unwrap().unwrap();
                assert!(saved.meta.unwrap().auth_binding.is_none());
                assert_eq!(saved.settings_config["auth"], json!({}));
                assert_eq!(saved.category.as_deref(), Some("official"));
            }
            let saved_legacy = state
                .db
                .get_provider_by_id("legacy", "codex")
                .unwrap()
                .unwrap();
            assert_eq!(saved_legacy.notes, legacy.notes);
            assert_eq!(saved_legacy.settings_config["model"], "keep-model");
            let future: String = state
                .db
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT json_extract(meta, '$.futureField') FROM providers WHERE id = 'legacy'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(future, "keep");
            let bound_to = |id: &str, app: &str| {
                state
                    .db
                    .get_provider_by_id(id, app)
                    .unwrap()
                    .unwrap()
                    .meta
                    .unwrap()
                    .managed_account_id_for("codex_oauth")
            };
            assert_eq!(bound_to("other", "codex").is_none(), logout_all);
            assert_eq!(
                bound_to("foreign", "codex").as_deref(),
                Some("acct-other-device")
            );
            assert!(state
                .db
                .get_provider_by_id("unrelated", "codex")
                .unwrap()
                .unwrap()
                .meta
                .unwrap()
                .auth_binding
                .is_some());
            assert!(state
                .db
                .get_provider_by_id("current", "claude")
                .unwrap()
                .unwrap()
                .meta
                .unwrap()
                .auth_binding
                .is_some());
            assert_eq!(
                crate::settings::get_current_provider(&AppType::Codex).as_deref(),
                Some("current")
            );
            assert!(!crate::codex_config::get_codex_auth_path().exists());
            assert!(!crate::codex_config::codex_managed_oauth_live_auth_marker_exists());
            ProviderService::switch(state, AppType::Codex, "legacy").unwrap();
            assert!(
                !crate::codex_config::get_codex_auth_path().exists(),
                "unbound legacy card must not resurrect stored credentials"
            );
            assert!(state
                .db
                .unbind_codex_managed_accounts(&["acct-old".to_string()])
                .unwrap()
                .is_empty());
        });
    }
}

#[test]
#[serial]
fn codex_failed_batch_unbinding_rolls_back_and_leaves_a_recovery_path() {
    for rebind in [false, true] {
        with_test_home(|state, _| {
            crate::settings::reload_settings().unwrap();
            seed_codex_recovery_accounts(state);
            let current = managed_codex_provider("a-current", "acct-old");
            let second = managed_codex_provider("z-second", "acct-old");
            let target = managed_codex_provider("target", "acct-new");
            for provider in [&current, &second, &target] {
                state.db.save_provider("codex", provider).unwrap();
            }
            ProviderService::switch(state, AppType::Codex, &current.id).unwrap();
            state
                .db
                .conn
                .lock()
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER reject_unbind BEFORE UPDATE OF meta ON providers
                 WHEN OLD.id = 'z-second' AND json_extract(NEW.meta, '$.authBinding') IS NULL
                 BEGIN SELECT RAISE(FAIL, 'injected unbind failure'); END;",
                )
                .unwrap();
            let error = tauri::async_runtime::block_on(
                crate::commands::remove_codex_oauth_account_with_switch_lock(state, "acct-old"),
            )
            .unwrap_err();
            assert!(error.contains("账号已删除") && error.contains("解绑失败"));
            for id in [&current.id, &second.id] {
                assert_eq!(
                    state
                        .db
                        .get_provider_by_id(id, "codex")
                        .unwrap()
                        .unwrap()
                        .meta
                        .unwrap()
                        .managed_account_id_for("codex_oauth")
                        .as_deref(),
                    Some("acct-old")
                );
            }
            assert!(
                !tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts())
                    .iter()
                    .any(|account| account.id == "acct-old")
            );
            if rebind {
                let replacement = managed_codex_provider(&current.id, "acct-new");
                ProviderService::update(state, AppType::Codex, None, replacement).unwrap();
            } else {
                ProviderService::switch(state, AppType::Codex, &target.id).unwrap();
            }
            let auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
            assert_eq!(auth["tokens"]["account_id"], "acct-new");
        });
    }
}

#[test]
#[serial]
fn deleted_codex_account_recovers_after_persisted_startup() {
    for exit_state in ["detached", "crashed"] {
        let _guard = test_guard();
        let _home = TempHome::new_sync();
        crate::settings::reload_settings().unwrap();
        let runtime = tauri::async_runtime::handle();
        let db = Arc::new(Database::memory().unwrap());
        let state = AppState::new(db.clone());
        let token = crate::codex_config::test_codex_id_token("same-user");
        runtime.block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_workspace_and_access_token(
                    "old-local-id",
                    "workspace",
                    "old-access",
                    Some(&token),
                )
                .await
                .unwrap();
            let mut config = state.db.get_global_proxy_config().await.unwrap();
            config.listen_port = 0;
            state.db.update_global_proxy_config(config).await.unwrap();
        });
        let current = managed_codex_provider("current", "old-local-id");
        state.db.save_provider("codex", &current).unwrap();
        ProviderService::switch(&state, AppType::Codex, "current").unwrap();
        runtime.block_on(async {
            crate::mode::controller::enter(&state, &AppType::Codex, false)
                .await
                .unwrap();
            remove_codex_account_leaving_dangling_bindings(&state, "old-local-id")
                .await
                .unwrap();
            state
                .codex_oauth_manager
                .add_test_account_with_workspace_and_access_token(
                    "new-local-id",
                    "workspace",
                    "new-access",
                    Some(&token),
                )
                .await
                .unwrap();
        });
        // Model a later CLI login. Startup recovery must leave it intact.
        let native_auth = crate::codex_config::codex_managed_oauth_auth_value(
            "native-workspace",
            "native-access",
            Some(&token),
            "native-refresh",
            "2026-09-14T00:00:00Z",
        );
        write_json_file(&crate::codex_config::get_codex_auth_path(), &native_auth).unwrap();
        runtime.block_on(async {
            if exit_state == "detached" {
                // 直连供应商的账号没了写不出来：只清掉占位符，登录不动。
                crate::mode::controller::detach_all(&state).await;
                assert!(!state
                    .proxy_service
                    .live_has_proxy_placeholder(&AppType::Codex));
            } else {
                // 崩溃：客户端仍指着代理，只是监听没了。
                state.proxy_service.stop().await.unwrap();
                assert!(state
                    .proxy_service
                    .live_has_proxy_placeholder(&AppType::Codex));
            }
            assert!(crate::mode::current::is_proxy(&AppType::Codex));
        });
        drop(state);

        crate::settings::reload_settings().unwrap();
        // 内存库跨重启沿用同一实例；要验证的持久化状态（账号库、live-state、
        // settings、auth.json）都在磁盘上。
        let restarted = AppState::new(db);
        assert_eq!(
            ProviderService::managed_codex_oauth_account_id(
                &restarted
                    .db
                    .get_provider_by_id("current", "codex")
                    .unwrap()
                    .unwrap()
            )
            .as_deref(),
            Some("old-local-id"),
        );
        runtime.block_on(async {
            let accounts = restarted.codex_oauth_manager.list_accounts().await;
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].id, "new-local-id");
            let store_path = crate::config::get_app_config_dir().join("codex_oauth_auth.json");
            let persisted_accounts = fs::read(&store_path).unwrap();
            // Match setup's ordering: extract common config from the direct live
            // file before re-attaching routing mode.
            ProviderService::scrub_leaked_gemini_common_config(&restarted)
                .await
                .unwrap();
            crate::initialize_common_config_snippets(&restarted);
            crate::mode::controller::startup(&restarted).await;
            // 路由的账号失效，接不上就退回直连。
            assert!(
                !crate::mode::current::is_proxy(&AppType::Codex),
                "{exit_state}"
            );
            assert!(
                !restarted
                    .db
                    .get_proxy_config_for_app("codex")
                    .await
                    .unwrap()
                    .enabled
            );
            assert!(!restarted.proxy_service.is_running().await);
            assert!(!restarted
                .proxy_service
                .live_has_proxy_placeholder(&AppType::Codex));
            assert_eq!(fs::read(&store_path).unwrap(), persisted_accounts);
            assert_eq!(
                read_json_file::<Value>(&crate::codex_config::get_codex_auth_path()).unwrap(),
                native_auth
            );
            // Stub the network refresh only after proving the account survived on disk.
            restarted
                .codex_oauth_manager
                .test_cache_access_token("new-local-id", "new-access")
                .await;
        });
        let target = managed_codex_provider("current", "new-local-id");
        ProviderService::update(&restarted, AppType::Codex, None, target).unwrap();
        runtime.block_on(async {
            crate::mode::controller::enter(&restarted, &AppType::Codex, false)
                .await
                .unwrap();
            assert!(
                restarted
                    .db
                    .get_proxy_config_for_app("codex")
                    .await
                    .unwrap()
                    .enabled
            );
            assert!(restarted
                .proxy_service
                .live_has_proxy_placeholder(&AppType::Codex));
            crate::mode::controller::exit(&restarted, &AppType::Codex)
                .await
                .unwrap();
            assert!(!restarted.proxy_service.is_running().await);
        });
        let auth: Value = read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
        assert_eq!(auth["tokens"]["access_token"], "new-access", "{exit_state}");
    }
}

#[test]
#[serial]
fn codex_auth_center_removal_waits_for_provider_switch_lock() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access",
                    "managed-user",
                )
                .await
                .expect("seed managed account");
        });

        let switch_guard = tauri::async_runtime::block_on(
            state
                .proxy_service
                .lock_switch_for_app(AppType::Codex.as_str()),
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                started_tx.send(()).expect("signal removal start");
                let result = tauri::async_runtime::block_on(
                    crate::commands::remove_codex_oauth_account_with_switch_lock(
                        state,
                        "acct-managed",
                    ),
                );
                done_tx.send(result).expect("send removal result");
            });
            started_rx.recv().expect("wait for removal task");
            assert!(
                done_rx
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .is_err(),
                "Auth Center removal must wait while a provider transaction owns the Codex lock"
            );
            drop(switch_guard);
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("removal should finish after lock release")
                .expect("remove managed account");
        });
        assert!(
            tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts()).is_empty()
        );
    });
}

#[test]
#[serial]
fn codex_auth_center_logout_waits_for_provider_switch_lock() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-access",
                    "managed-user",
                )
                .await
                .expect("seed managed account");
        });
        let provider = managed_codex_provider("managed-logout-lock", "acct-managed");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider)
            .expect("save managed provider");
        ProviderService::switch(state, AppType::Codex, &provider.id)
            .expect("activate managed provider");
        assert!(crate::codex_config::get_codex_auth_path().exists());
        assert!(crate::codex_config::codex_managed_oauth_live_auth_marker_exists());

        let switch_guard = tauri::async_runtime::block_on(
            state
                .proxy_service
                .lock_switch_for_app(AppType::Codex.as_str()),
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                started_tx.send(()).expect("signal logout start");
                let result = tauri::async_runtime::block_on(
                    crate::commands::logout_codex_oauth_with_switch_lock(state),
                );
                done_tx.send(result).expect("send logout result");
            });
            started_rx.recv().expect("wait for logout task");
            assert!(
                done_rx
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .is_err(),
                "Auth Center logout must wait while a provider transaction owns the Codex lock"
            );
            drop(switch_guard);
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("logout should finish after lock release")
                .expect("logout managed accounts");
        });

        assert!(
            tauri::async_runtime::block_on(state.codex_oauth_manager.list_accounts()).is_empty()
        );
        assert!(
            !crate::codex_config::get_codex_auth_path().exists(),
            "logout must clear the active managed live auth"
        );
        assert!(
            !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
            "logout must clear the managed live auth marker"
        );
    });
}

#[test]
#[serial]
fn switch_to_managed_codex_official_with_unresolvable_account_keeps_current_unchanged() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");

        // 基线：一个普通第三方 provider，可正常切换，作为初始 current。
        // config 必须带自定义 provider 表：config-only 切换要求 key 有
        // provider 级落点。
        let mut baseline = Provider::with_id(
            "baseline".to_string(),
            "Baseline".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "sk-baseline" },
                "config": "model_provider = \"baseline\"\n[model_providers.baseline]\nbase_url = \"https://baseline.example/v1\"\n"
            }),
            None,
        );
        baseline.category = Some("custom".to_string());

        // 托管 official provider，绑定一个 manager 中不存在的账号：切换预检
        // 取 token 必然失败。
        let mut managed = Provider::with_id(
            "managed-official".to_string(),
            "Managed Official".to_string(),
            json!({ "auth": {}, "config": "" }),
            None,
        );
        managed.category = Some("official".to_string());
        managed.meta = Some(ProviderMeta {
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some("acct-missing".to_string()),
            }),
            ..Default::default()
        });

        state
            .db
            .save_provider(AppType::Codex.as_str(), &baseline)
            .expect("save baseline");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &managed)
            .expect("save managed");

        ProviderService::switch(state, AppType::Codex, "baseline").expect("switch to baseline");

        // 切到绑定了不存在账号的托管 provider：预检失败 → 返回 Err。
        let result = ProviderService::switch(state, AppType::Codex, "managed-official");
        assert!(
            result.is_err(),
            "switch must fail when the managed OAuth token cannot be resolved"
        );

        // current 必须仍是 baseline：预检在提交 current 之前失败，不留下
        // 「DB/UI 指向新 provider、但 live 仍是旧 provider」的不一致状态。
        let current = crate::settings::get_effective_current_provider(&state.db, &AppType::Codex)
            .expect("read current");
        assert_eq!(
            current.as_deref(),
            Some("baseline"),
            "a failed managed switch must not move current off the previous provider"
        );
    });
}

#[test]
fn native_opencode_common_config_excludes_provider_credentials() {
    let config = json!({
        "settings": {"baseURL": "https://native.example", "apiKey": "test", "timeout": 1000},
        "body": {"metadata": {"keep": true}}
    });
    let common: Value =
        serde_json::from_str(&ProviderService::extract_opencode_common_config(&config).unwrap())
            .unwrap();
    assert_eq!(
        common,
        json!({"settings": {"timeout": 1000}, "body": {"metadata": {"keep": true}}})
    );
}

#[test]
#[serial]
fn native_opencode_provider_keeps_source_format_through_database_and_sync() {
    use crate::provider::OpenCodeConfigFormat;
    for native in [
        json!({}),
        json!({"models": {"alias": {"modelID": "upstream", "limit": {"input": 10000}}}}),
        json!({
            "package": "@opencode/ai/providers/anthropic",
            "settings": {"baseURL": "https://native.example", "apiKey": "test"},
            "headers": {"X-Tenant": "example"},
            "body": {"metadata": {"keep": true}},
            "models": {"model": {"capabilities": {"tools": true, "input": ["text"], "output": ["text"]}, "variants": [
                {"id": "low", "settings": {"reasoningEffort": "low"}},
                {"id": "high", "settings": {"reasoningEffort": "high"}}
            ]}}
        }),
    ] {
        with_test_home(|state, _| {
            // Also exercise upgrading an existing DB entry with no source metadata.
            let existing = Provider::with_id(
                "anthropic".into(),
                "Existing name".into(),
                native.clone(),
                None,
            );
            state.db.save_provider("opencode", &existing).unwrap();
            crate::opencode_config::set_provider_with_format(
                "anthropic",
                native.clone(),
                OpenCodeConfigFormat::V2,
            )
            .unwrap();
            assert_eq!(import_opencode_providers_from_live(state).unwrap(), 1);
            assert_eq!(import_opencode_providers_from_live(state).unwrap(), 0);
            let mut saved = state
                .db
                .get_provider_by_id("anthropic", "opencode")
                .unwrap()
                .unwrap();
            assert_eq!(saved.name, "Existing name");
            assert_eq!(saved.settings_config, native);
            assert_eq!(
                saved.meta.as_ref().unwrap().opencode_config_format,
                Some(OpenCodeConfigFormat::V2)
            );

            // Source metadata must survive removal and re-enabling even for {}.
            crate::opencode_config::remove_provider("anthropic").unwrap();
            live::write_live_snapshot(&AppType::OpenCode, &saved).unwrap();
            assert_eq!(
                crate::opencode_config::read_opencode_config().unwrap()["providers"]["anthropic"],
                native
            );
            saved.settings_config["name"] = json!("Edited native provider");
            let expected = saved.settings_config.clone();
            ProviderService::update(state, AppType::OpenCode, None, saved).unwrap();
            let config = crate::opencode_config::read_opencode_config().unwrap();
            assert_eq!(config["providers"]["anthropic"], expected);
            assert!(config.get("provider").is_none());

            let mut full = Provider::with_id(
                "anthropic".into(),
                "Full config".into(),
                json!({"providers": {"anthropic": expected}}),
                None,
            );
            live::write_live_snapshot(&AppType::OpenCode, &full).unwrap();
            let before =
                std::fs::read(crate::opencode_config::get_opencode_config_path().unwrap()).unwrap();
            full.settings_config = json!({"providers": {"other": {}}});
            assert!(live::write_live_snapshot(&AppType::OpenCode, &full).is_err());
            assert_eq!(
                std::fs::read(crate::opencode_config::get_opencode_config_path().unwrap()).unwrap(),
                before
            );
        });
    }
}

#[test]
#[serial]
fn malformed_native_opencode_provider_allows_legacy_import_update_and_sync() {
    use crate::provider::OpenCodeConfigFormat;
    for native in [
        json!({"package": false}),
        json!({"models": {"m": {"variants": {}}}}),
        json!({"settings": {"timeout": "1000"}}),
        json!({"models": {"m": {"limit": {"input": "1000"}}}}),
        json!({"models": {"m": {"capabilities": {"tools": true}}}}),
    ] {
        for existing in [false, true] {
            with_test_home(|state, _| {
                let provider = opencode_provider("shared");
                if existing {
                    state.db.save_provider("opencode", &provider).unwrap();
                }
                let mut expected = json!({
                    "provider": {"shared": provider.settings_config},
                    "providers": {"shared": native},
                    "model": "shared/gpt-4o"
                });
                write_json_file(
                    &crate::opencode_config::get_opencode_config_path().unwrap(),
                    &expected,
                )
                .unwrap();
                import_opencode_providers_from_live(state).unwrap();
                let mut saved = state
                    .db
                    .get_provider_by_id("shared", "opencode")
                    .unwrap()
                    .unwrap();
                assert_eq!(saved.settings_config, provider.settings_config, "{native}");
                assert_ne!(
                    saved
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.opencode_config_format),
                    Some(OpenCodeConfigFormat::V2)
                );
                assert_eq!(import_opencode_providers_from_live(state).unwrap(), 0);

                saved.settings_config["options"]["apiKey"] = json!("fake-new");
                ProviderService::update(state, AppType::OpenCode, None, saved.clone()).unwrap();
                expected["provider"]["shared"] = saved.settings_config.clone();
                assert_eq!(
                    crate::opencode_config::read_opencode_config().unwrap(),
                    expected
                );
                assert_eq!(
                    state
                        .db
                        .get_provider_by_id("shared", "opencode")
                        .unwrap()
                        .unwrap()
                        .settings_config,
                    saved.settings_config
                );

                saved.settings_config["options"]["apiKey"] = json!("fake-synced");
                state.db.save_provider("opencode", &saved).unwrap();
                ProviderService::sync_current_provider_for_app(state, AppType::OpenCode).unwrap();
                expected["provider"]["shared"] = saved.settings_config;
                assert_eq!(
                    crate::opencode_config::read_opencode_config().unwrap(),
                    expected
                );
                assert_eq!(import_opencode_providers_from_live(state).unwrap(), 0);
            });
        }
    }
}

#[test]
#[serial]
fn invalid_native_opencode_edit_is_rejected_before_saving() {
    use crate::provider::OpenCodeConfigFormat;
    with_test_home(|state, _| {
        let native = json!({"settings": {"baseURL": "https://a.example"}});
        crate::opencode_config::set_provider_with_format(
            "native",
            native.clone(),
            OpenCodeConfigFormat::V2,
        )
        .unwrap();
        import_opencode_providers_from_live(state).unwrap();
        let mut edited = state
            .db
            .get_provider_by_id("native", "opencode")
            .unwrap()
            .unwrap();
        // OpenCode requires tools, input and output together.
        edited.settings_config = json!({"settings": {"baseURL": "https://b.example"}, "models": {"m": {"capabilities": {"tools": true}}}});

        let err = ProviderService::update(state, AppType::OpenCode, None, edited.clone())
            .unwrap_err()
            .to_string();
        assert!(err.contains("models.m.capabilities"), "{err}");
        edited.id = "added".into();
        assert!(ProviderService::add(state, AppType::OpenCode, edited, true).is_err());

        let saved = state
            .db
            .get_provider_by_id("native", "opencode")
            .unwrap()
            .unwrap();
        assert_eq!(saved.settings_config, native);
        assert!(state
            .db
            .get_provider_by_id("added", "opencode")
            .unwrap()
            .is_none());
        assert_eq!(
            crate::opencode_config::read_opencode_config().unwrap()["providers"]["native"],
            native
        );
    });
}

#[test]
#[serial]
fn full_opencode_config_writes_legacy_entry_behind_invalid_native() {
    with_test_home(|_, _| {
        let legacy =
            json!({"npm": "@ai-sdk/openai", "options": {"apiKey": "v1"}, "models": {"m": {}}});
        let full = Provider::with_id(
            "shared".into(),
            "Full config".into(),
            json!({"provider": {"shared": legacy}, "providers": {"shared": {"package": false}}}),
            None,
        );
        live::write_live_snapshot(&AppType::OpenCode, &full).unwrap();
        let config = crate::opencode_config::read_opencode_config().unwrap();
        assert_eq!(config["provider"]["shared"], legacy);
        assert!(config.get("providers").is_none());
    });
}

#[test]
#[serial]
fn opencode_provider_roundtrip_preserves_fields_on_import_and_update() {
    with_test_home(|state, _| {
        let mut provider = opencode_provider("roundtrip-opencode");
        provider.settings_config["api"] = json!("https://api.example.com/v1");
        provider.settings_config["env"] = json!(["EXAMPLE_API_KEY"]);
        provider.settings_config["models"]["gpt-4o"]["limit"] = json!({
            "input": 120000,
            "context": 128000,
            "output": 8000
        });
        crate::opencode_config::set_provider(&provider.id, provider.settings_config.clone())
            .expect("seed opencode live provider");

        assert_eq!(import_opencode_providers_from_live(state).unwrap(), 1);
        let mut saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
            .unwrap()
            .expect("imported provider");
        assert_eq!(saved.settings_config, provider.settings_config);
        assert_eq!(import_opencode_providers_from_live(state).unwrap(), 0);

        saved.settings_config["options"]["apiKey"] = json!("updated-key");
        let expected = saved.settings_config.clone();
        ProviderService::update(state, AppType::OpenCode, None, saved)
            .expect("update imported provider");
        let live = crate::opencode_config::get_providers().unwrap();
        assert_eq!(live[&provider.id], expected);
    });
}

#[test]
#[serial]
fn opencode_provider_roundtrip_preserves_fields_on_live_write() {
    with_test_home(|_, _| {
        let mut provider = opencode_provider("write-opencode");
        provider.settings_config["api"] = json!("https://api.example.com/v1");
        provider.settings_config["env"] = json!(["EXAMPLE_API_KEY"]);
        provider.settings_config["models"]["gpt-4o"]["limit"] = json!({
            "input": 120000,
            "context": 128000,
            "output": 8000
        });
        let expected = provider.settings_config.clone();

        // Exercise the full-config fragment extraction as well as the writer.
        provider.settings_config = json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": { provider.id.clone(): expected }
        });
        live::write_live_snapshot(&AppType::OpenCode, &provider).expect("write opencode provider");

        let live = crate::opencode_config::get_providers().unwrap();
        assert_eq!(live[&provider.id], expected);
    });
}

#[test]
#[serial]
fn import_opencode_providers_from_live_marks_provider_as_live_managed() {
    with_test_home(|state, _| {
        let provider = opencode_provider("imported-opencode");
        crate::opencode_config::set_provider(&provider.id, provider.settings_config.clone())
            .expect("seed opencode live provider");

        let imported = import_opencode_providers_from_live(state)
            .expect("import opencode providers from live");
        assert_eq!(imported, 1);

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
            .expect("query imported opencode provider")
            .expect("imported opencode provider should exist");
        assert_eq!(
            saved
                .meta
                .as_ref()
                .and_then(|meta| meta.live_config_managed),
            Some(true),
            "providers imported from live should be treated as live-managed"
        );
    });
}

#[test]
#[serial]
fn import_opencode_providers_from_live_updates_existing_provider_from_live() {
    with_test_home(|state, _| {
        let provider = opencode_provider("existing-opencode");
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &provider)
            .expect("seed existing opencode provider");

        let mut live_settings = provider.settings_config.clone();
        live_settings.as_object_mut().unwrap().remove("name");
        live_settings["npm"] = Value::String("@ai-sdk/anthropic".to_string());
        live_settings["models"]["gpt-4o"]["name"] = Value::String("Claude Sonnet".to_string());
        crate::opencode_config::set_provider(&provider.id, live_settings)
            .expect("seed edited live opencode provider");

        let updated = import_opencode_providers_from_live(state)
            .expect("import opencode providers from live");
        assert_eq!(updated, 1);

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
            .expect("query updated opencode provider")
            .expect("opencode provider should exist");
        assert_eq!(saved.name, provider.name);
        assert_eq!(saved.settings_config["npm"], json!("@ai-sdk/anthropic"));
        assert_eq!(
            saved.settings_config["models"]["gpt-4o"]["name"],
            json!("Claude Sonnet")
        );
    });
}
#[test]
#[serial]
fn import_openclaw_providers_from_live_marks_provider_as_live_managed() {
    with_test_home(|state, _| {
        let mut provider = openclaw_provider("imported-openclaw");
        provider.settings_config["models"] = json!([
            {
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4"
            }
        ]);
        crate::openclaw_config::set_provider(&provider.id, provider.settings_config.clone())
            .expect("seed openclaw live provider");

        let imported = import_openclaw_providers_from_live(state)
            .expect("import openclaw providers from live");
        assert_eq!(imported, 1);

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenClaw.as_str())
            .expect("query imported openclaw provider")
            .expect("imported openclaw provider should exist");
        assert_eq!(
            saved
                .meta
                .as_ref()
                .and_then(|meta| meta.live_config_managed),
            Some(true),
            "providers imported from live should be treated as live-managed"
        );
    });
}

#[test]
#[serial]
fn import_openclaw_providers_from_live_updates_existing_provider_from_live() {
    with_test_home(|state, _| {
        let mut provider = openclaw_provider("existing-openclaw");
        provider.settings_config["models"] = json!([
            {
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4"
            }
        ]);
        state
            .db
            .save_provider(AppType::OpenClaw.as_str(), &provider)
            .expect("seed existing openclaw provider");

        let mut live_settings = provider.settings_config.clone();
        live_settings["baseUrl"] = Value::String("https://api.example.com/v1".to_string());
        live_settings["models"][0]["name"] = Value::String("Claude Sonnet 4.1".to_string());
        crate::openclaw_config::set_provider(&provider.id, live_settings)
            .expect("seed edited live openclaw provider");

        let updated = import_openclaw_providers_from_live(state)
            .expect("import openclaw providers from live");
        assert_eq!(updated, 1);

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenClaw.as_str())
            .expect("query updated openclaw provider")
            .expect("openclaw provider should exist");
        assert_eq!(saved.name, provider.name);
        assert_eq!(
            saved.settings_config["baseUrl"],
            json!("https://api.example.com/v1")
        );
        assert_eq!(
            saved.settings_config["models"][0]["name"],
            json!("Claude Sonnet 4.1")
        );
    });
}

#[test]
#[serial]
fn import_hermes_providers_from_live_updates_existing_provider_from_live() {
    with_test_home(|state, _| {
        let provider = hermes_provider("existing-hermes");
        state
            .db
            .save_provider(AppType::Hermes.as_str(), &provider)
            .expect("seed existing hermes provider");

        let mut live_settings = provider.settings_config.clone();
        live_settings["base_url"] = Value::String("https://api.hermes.example/v1".to_string());
        live_settings["models"]["gpt-4o"]["name"] = Value::String("GPT-4o Updated".to_string());
        crate::hermes_config::set_provider(&provider.id, live_settings)
            .expect("seed edited live hermes provider");

        let updated =
            import_hermes_providers_from_live(state).expect("import hermes providers from live");
        assert_eq!(updated, 1);

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::Hermes.as_str())
            .expect("query updated hermes provider")
            .expect("hermes provider should exist");
        assert_eq!(saved.name, provider.name);
        assert_eq!(
            saved.settings_config["base_url"],
            json!("https://api.hermes.example/v1")
        );
        // models are denormalized from YAML dict to UI-friendly array by
        // get_providers(), so access by index rather than dict key
        assert_eq!(
            saved.settings_config["models"][0]["name"],
            json!("GPT-4o Updated")
        );
        assert_eq!(saved.settings_config["models"][0]["id"], json!("gpt-4o"));
    });
}

#[test]
#[serial]
fn legacy_additive_provider_still_errors_on_live_config_parse_failure() {
    with_test_home(|state, home| {
        let provider = openclaw_provider("legacy-provider");
        state
            .db
            .save_provider(AppType::OpenClaw.as_str(), &provider)
            .expect("seed legacy provider without live_config_managed marker");

        let openclaw_dir = home.join(".openclaw");
        fs::create_dir_all(&openclaw_dir).expect("create openclaw dir");
        fs::write(openclaw_dir.join("openclaw.json"), "{ invalid json5")
            .expect("write malformed config");

        let mut updated = provider;
        updated.name = "Legacy Edited".to_string();

        let err = ProviderService::update(state, AppType::OpenClaw, None, updated)
            .expect_err("legacy providers should still surface live parse errors");
        assert!(
            err.to_string().contains("Failed to parse OpenClaw config"),
            "expected parse error, got {err:?}"
        );
    });
}

#[test]
#[serial]
fn update_persists_non_current_omo_variants_in_database() {
    with_test_home(|state, _| {
        for category in ["omo", "omo-slim"] {
            let provider = opencode_omo_provider(&format!("{category}-provider"), category);
            state
                .db
                .save_provider(AppType::OpenCode.as_str(), &provider)
                .unwrap_or_else(|err| panic!("seed {category} provider: {err}"));

            let mut updated = provider.clone();
            updated.name = format!("Updated {category}");
            updated.settings_config["agents"]["writer"]["model"] =
                Value::String(format!("{category}-next-model"));

            ProviderService::update(state, AppType::OpenCode, None, updated)
                .unwrap_or_else(|err| panic!("update {category} provider: {err}"));

            let saved = state
                .db
                .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
                .unwrap_or_else(|err| panic!("query updated {category} provider: {err}"))
                .unwrap_or_else(|| panic!("{category} provider should exist"));

            assert_eq!(saved.name, format!("Updated {category}"));
            assert_eq!(
                saved.settings_config["agents"]["writer"]["model"],
                Value::String(format!("{category}-next-model")),
                "{category} updates should persist in the database"
            );
        }
    });
}

#[test]
#[serial]
fn update_current_omo_variant_rewrites_config_from_saved_provider() {
    with_test_home(|state, home| {
        for category in ["omo", "omo-slim"] {
            let provider = opencode_omo_provider(&format!("{category}-current"), category);
            state
                .db
                .save_provider(AppType::OpenCode.as_str(), &provider)
                .unwrap_or_else(|err| panic!("seed current {category} provider: {err}"));
            state
                .db
                .set_omo_provider_current(AppType::OpenCode.as_str(), &provider.id, category)
                .unwrap_or_else(|err| panic!("set current {category} provider: {err}"));

            let mut updated = provider.clone();
            updated.name = format!("Current {category} updated");
            updated.settings_config["agents"]["writer"]["model"] =
                Value::String(format!("{category}-saved-model"));
            updated.settings_config["otherFields"]["theme"] =
                Value::String(format!("{category}-light"));

            ProviderService::update(state, AppType::OpenCode, None, updated)
                .unwrap_or_else(|err| panic!("update current {category} provider: {err}"));

            let saved = state
                .db
                .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
                .unwrap_or_else(|err| panic!("query current {category} provider: {err}"))
                .unwrap_or_else(|| panic!("current {category} provider should exist"));
            assert_eq!(saved.name, format!("Current {category} updated"));

            let written = fs::read_to_string(omo_config_path(home, category))
                .unwrap_or_else(|err| panic!("read written {category} config: {err}"));
            let written_json: Value = serde_json::from_str(&written)
                .unwrap_or_else(|err| panic!("parse written {category} config: {err}"));

            assert_eq!(
                written_json["agents"]["writer"]["model"],
                Value::String(format!("{category}-saved-model")),
                "{category} config should be written from the saved provider state"
            );
            assert_eq!(
                written_json["theme"],
                Value::String(format!("{category}-light")),
                "{category} top-level config should reflect updated otherFields"
            );
        }
    });
}

#[test]
#[serial]
fn update_current_omo_variant_does_not_persist_database_when_file_write_fails() {
    with_test_home(|state, home| {
        let provider = opencode_omo_provider("omo-current", "omo");
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &provider)
            .unwrap_or_else(|err| panic!("seed current omo provider: {err}"));
        state
            .db
            .set_omo_provider_current(AppType::OpenCode.as_str(), &provider.id, "omo")
            .unwrap_or_else(|err| panic!("set current omo provider: {err}"));

        let config_dir = home.join(".config").join("opencode");
        fs::create_dir_all(config_dir.parent().expect("config dir parent"))
            .expect("create .config dir");
        fs::write(&config_dir, "not a directory").expect("block opencode config dir");

        let mut updated = provider.clone();
        updated.name = "Current omo updated".to_string();
        updated.settings_config["agents"]["writer"]["model"] =
            Value::String("omo-saved-model".to_string());

        ProviderService::update(state, AppType::OpenCode, None, updated)
            .expect_err("update should fail when current omo file write fails");

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
            .unwrap_or_else(|err| panic!("query current omo provider: {err}"))
            .unwrap_or_else(|| panic!("current omo provider should exist"));

        assert_eq!(saved.name, provider.name);
        assert_eq!(
            saved.settings_config["agents"]["writer"]["model"],
            provider.settings_config["agents"]["writer"]["model"],
            "database should remain unchanged when file write fails"
        );
    });
}

#[test]
#[serial]
fn update_current_omo_variant_rolls_back_file_when_plugin_sync_fails() {
    with_test_home(|state, home| {
        let provider = opencode_omo_provider("omo-current", "omo");
        state
            .db
            .save_provider(AppType::OpenCode.as_str(), &provider)
            .unwrap_or_else(|err| panic!("seed current omo provider: {err}"));
        state
            .db
            .set_omo_provider_current(AppType::OpenCode.as_str(), &provider.id, "omo")
            .unwrap_or_else(|err| panic!("set current omo provider: {err}"));

        let config_path = omo_config_path(home, "omo");
        fs::create_dir_all(config_path.parent().expect("omo config parent"))
            .expect("create omo config dir");
        let previous_content = serde_json::to_string_pretty(&json!({
            "theme": "legacy-live-theme",
            "agents": {
                "writer": {
                    "model": "legacy-live-model"
                }
            },
            "categories": {
                "default": ["writer"]
            }
        }))
        .expect("serialize previous config");
        fs::write(&config_path, &previous_content).expect("seed previous omo config");

        let opencode_config_path = home.join(".config").join("opencode").join("opencode.json");
        fs::write(&opencode_config_path, "{ invalid json").expect("seed malformed opencode");

        let mut updated = provider.clone();
        updated.name = "Current omo updated".to_string();
        updated.settings_config["agents"]["writer"]["model"] =
            Value::String("omo-saved-model".to_string());
        updated.settings_config["otherFields"]["theme"] = Value::String("omo-light".to_string());

        ProviderService::update(state, AppType::OpenCode, None, updated)
            .expect_err("update should fail when plugin sync fails");

        let saved = state
            .db
            .get_provider_by_id(&provider.id, AppType::OpenCode.as_str())
            .unwrap_or_else(|err| panic!("query current omo provider: {err}"))
            .unwrap_or_else(|| panic!("current omo provider should exist"));

        assert_eq!(saved.name, provider.name);
        assert_eq!(
            saved.settings_config["agents"]["writer"]["model"],
            provider.settings_config["agents"]["writer"]["model"],
            "database should remain unchanged when plugin sync fails"
        );

        let written =
            fs::read_to_string(&config_path).expect("read rolled back omo config content");
        assert_eq!(
            written, previous_content,
            "OMO config should roll back to its previous on-disk contents"
        );
    });
}

#[test]
#[serial]
fn universal_provider_icon_is_validated_on_save_and_sync() {
    with_test_home(|state, _home| {
        let mut universal = UniversalProvider::new(
            "icon".into(),
            "Icon".into(),
            "custom".into(),
            "https://api.example".into(),
            "key".into(),
        );
        universal.apps.claude = true;
        // 只有文件头、没有像素数据的「PNG」
        universal.icon =
            Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAGAAAABgCAYAAAA=".into());
        assert!(ProviderService::upsert_universal(state, universal.clone()).is_err());
        assert!(state.db.get_universal_provider("icon").unwrap().is_none());

        // 绕过保存直接写进库的坏图标，同步到应用时也不会复制给子供应商
        state.db.save_universal_provider(&universal).unwrap();
        assert!(ProviderService::sync_universal_to_apps(state, "icon").is_err());
        assert!(state
            .db
            .get_provider_by_id("universal-claude-icon", "claude")
            .unwrap()
            .is_none());
    });
}

#[test]
#[serial]
fn sync_universal_to_apps_preserves_child_metadata() {
    with_test_home(|state, _home| {
        let mut universal = UniversalProvider::new(
            "metadata".into(),
            "Original".into(),
            "custom".into(),
            "https://old.example".into(),
            "old-key".into(),
        );
        universal.apps.claude = true;
        universal.apps.codex = true;
        universal.apps.gemini = true;
        universal.meta = Some(
            serde_json::from_value(json!({
                "usage_script": {"enabled": false, "language": "javascript", "code": "parent"}
            }))
            .unwrap(),
        );
        state.db.save_universal_provider(&universal).unwrap();
        ProviderService::sync_universal_to_apps(state, &universal.id).unwrap();

        let mut expected = Vec::new();
        for (index, app) in ["claude", "codex", "gemini"].iter().enumerate() {
            let id = format!("universal-{app}-metadata");
            let mut child = state.db.get_provider_by_id(&id, app).unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&child.meta).unwrap(),
                serde_json::to_value(&universal.meta).unwrap()
            );
            child.meta = Some(
                serde_json::from_value(json!({
                    "usage_script": {"enabled": true, "language": "javascript", "code": app,
                        "apiKey": "usage-only-key", "autoQueryInterval": 15},
                    "commonConfigEnabled": false,
                    "endpointAutoSelect": true
                }))
                .unwrap(),
            );
            child.created_at = Some(123 + index as i64);
            child.sort_index = Some(10 + index);
            child.settings_config["local_setting"] = json!(app);
            state.db.save_provider(app, &child).unwrap();
            state
                .db
                .add_custom_endpoint(app, &id, "https://extra.example")
                .unwrap();
            expected.push(child);
        }

        universal.name = "Updated".into();
        universal.base_url = "https://new.example".into();
        universal.api_key = "new-key".into();
        universal.notes = Some("shared note".into());
        universal.models = serde_json::from_value(json!({
            "claude": {"model": "claude-new"},
            "codex": {"model": "codex-new", "reasoningEffort": "low"},
            "gemini": {"model": "gemini-new"}
        }))
        .unwrap();
        // Both absent and present parent metadata must not overwrite child settings.
        for parent_meta in [None, universal.meta.clone()] {
            universal.meta = parent_meta;
            state.db.save_universal_provider(&universal).unwrap();
            ProviderService::sync_universal_to_apps(state, &universal.id).unwrap();
            for (app, before) in ["claude", "codex", "gemini"].iter().zip(&expected) {
                let after = state
                    .db
                    .get_provider_by_id(&before.id, app)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&after.meta).unwrap(),
                    serde_json::to_value(&before.meta).unwrap(),
                    "{app}"
                );
                assert_eq!(after.created_at, before.created_at, "{app}");
                assert_eq!(after.sort_index, before.sort_index, "{app}");
                assert_eq!(after.name, "Updated");
                assert_eq!(after.notes.as_deref(), Some("shared note"));
                assert_eq!(after.settings_config["local_setting"], json!(app));
                let generated = match *app {
                    "claude" => universal.to_claude_provider(),
                    "codex" => universal.to_codex_provider(),
                    _ => universal.to_gemini_provider(),
                }
                .unwrap();
                let mut expected_settings = before.settings_config.clone();
                ProviderService::merge_json(&mut expected_settings, &generated.settings_config);
                assert_eq!(after.settings_config, expected_settings);
                assert_eq!(
                    state.db.get_all_providers(app).unwrap()[&before.id]
                        .meta
                        .as_ref()
                        .unwrap()
                        .custom_endpoints
                        .len(),
                    1
                );
            }
        }
    });
}

#[test]
#[serial]
fn sync_universal_to_apps_reprojects_current_child_to_live() {
    with_test_home(|state, _home| {
        let mut universal = UniversalProvider::new(
            "shared".to_string(),
            "Shared Relay".to_string(),
            "custom".to_string(),
            "https://api.new.example".to_string(),
            "new-key".to_string(),
        );
        universal.apps.claude = true;
        universal.models.claude = Some(ClaudeModelConfig {
            model: Some("claude-sonnet-4".to_string()),
            ..Default::default()
        });
        state
            .db
            .save_universal_provider(&universal)
            .expect("save universal provider");

        let child = universal
            .to_claude_provider()
            .expect("claude child provider");
        state
            .db
            .save_provider("claude", &child)
            .expect("seed child provider");
        state
            .db
            .set_current_provider("claude", &child.id)
            .expect("set current child");
        crate::settings::set_current_provider(&AppType::Claude, Some(&child.id))
            .expect("set local current child");

        let mut old_live = child.settings_config;
        old_live["env"]["ANTHROPIC_BASE_URL"] =
            Value::String("https://api.old.example".to_string());
        write_json_file(&get_claude_settings_path(), &old_live).expect("seed old live");

        ProviderService::sync_universal_to_apps(state, "shared").expect("sync universal provider");

        let live: Value = read_json_file(&get_claude_settings_path()).expect("read live");
        assert_eq!(
            live["env"]["ANTHROPIC_BASE_URL"].as_str(),
            Some("https://api.new.example")
        );
        assert_eq!(
            live["env"]["ANTHROPIC_AUTH_TOKEN"].as_str(),
            Some("new-key")
        );
    });
}

#[test]
#[serial]
fn add_first_managed_codex_with_missing_account_leaves_no_provider_or_live_state() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        let provider = managed_codex_provider("managed-missing", "acct-missing");
        let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
            .expect("capture empty Codex live state");

        ProviderService::add(state, AppType::Codex, provider.clone(), false)
            .expect_err("missing managed account should fail before add commits");

        assert!(
            state
                .db
                .get_provider_by_id(&provider.id, AppType::Codex.as_str())
                .expect("query failed managed add")
                .is_none(),
            "failed preflight must not leave an orphan provider row"
        );
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read current after failed add"),
            None
        );
        assert_eq!(
            crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture Codex live after failed add"),
            live_before,
            "failed preflight must not mutate Codex live files"
        );
    });
}

#[test]
#[serial]
fn add_first_managed_codex_current_failure_rolls_forward_on_recovery() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity(
                    "acct-managed",
                    "managed-token",
                    "managed-user",
                )
                .await
                .expect("seed managed Codex OAuth account");
        });

        let provider = managed_codex_provider("managed-first", "acct-managed");
        let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
            .expect("capture empty Codex live state");
        {
            let conn = state.db.conn.lock().expect("lock database");
            conn.execute_batch(
                "CREATE TRIGGER reject_first_managed_current_update
                 BEFORE UPDATE OF is_current ON providers
                 WHEN NEW.app_type = 'codex'
                   AND NEW.id = 'managed-first'
                   AND NEW.is_current = 1
                 BEGIN
                   SELECT RAISE(ABORT, 'forced first managed Codex current failure');
                 END;",
            )
            .expect("install first-current failure trigger");
        }

        let error = ProviderService::add(state, AppType::Codex, provider.clone(), false)
            .expect_err("DB current failure surfaces");
        assert!(
            error
                .to_string()
                .contains("forced first managed Codex current failure"),
            "add should surface the DB current failure, got: {error}"
        );
        // 文件已经发布：行留着，pending 等着补完指针，live 不回滚。
        assert!(state
            .db
            .get_provider_by_id(&provider.id, AppType::Codex.as_str())
            .expect("query provider")
            .is_some());
        assert!(crate::mode::operation::has_pending(AppType::Codex.as_str()));
        assert_ne!(
            crate::codex_config::CodexLiveStateSnapshot::capture().expect("capture live"),
            live_before
        );

        state
            .db
            .conn
            .lock()
            .expect("lock database")
            .execute_batch("DROP TRIGGER reject_first_managed_current_update;")
            .expect("drop trigger");
        crate::mode::operation::recover_on_startup(&state.db);
        assert!(!crate::mode::operation::has_pending(
            AppType::Codex.as_str()
        ));
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read current after recovery")
                .as_deref(),
            Some(provider.id.as_str())
        );
    });
}

#[test]
#[serial]
fn missing_codex_account_preserves_native_login_but_corrupt_store_blocks_recovery() {
    for takeover in [false, true] {
        for corrupt in [false, true] {
            with_test_home(|state, _| {
                crate::settings::reload_settings().unwrap();
                crate::settings::update_settings(crate::settings::AppSettings {
                    preserve_codex_official_auth_on_switch: true,
                    ..Default::default()
                })
                .unwrap();
                let runtime = tauri::async_runtime::handle();
                runtime
                    .block_on(
                        state
                            .codex_oauth_manager
                            .add_test_account_with_user_identity("old", "access", "user"),
                    )
                    .unwrap();
                let current = managed_codex_provider("current", "old");
                state.db.save_provider("codex", &current).unwrap();
                ProviderService::switch(state, AppType::Codex, "current").unwrap();
                let mut auth: Value =
                    read_json_file(&crate::codex_config::get_codex_auth_path()).unwrap();
                if takeover {
                    runtime.block_on(async {
                        // 端口 0 启动后实际端口会记进库，重开的 AppState 不启动代理
                        // 也能算出契约地址。
                        let mut config = state.db.get_global_proxy_config().await.unwrap();
                        config.listen_port = 0;
                        state.db.update_global_proxy_config(config).await.unwrap();
                        crate::mode::controller::enter(state, &AppType::Codex, false)
                            .await
                            .unwrap();
                    });
                }
                runtime
                    .block_on(remove_codex_account_leaving_dangling_bindings(state, "old"))
                    .unwrap();
                // A stale marker must not claim a later native login of the same user.
                auth["tokens"]["refresh_token"] = json!("native-rotated-token");
                write_json_file(&crate::codex_config::get_codex_auth_path(), &auth).unwrap();
                crate::codex_config::record_codex_managed_oauth_live_auth(&auth, "old").unwrap();
                if corrupt {
                    fs::write(
                        crate::config::get_app_config_dir().join("codex_oauth_auth.json"),
                        "{broken",
                    )
                    .unwrap();
                }
                let restarted = AppState::new(state.db.clone());
                let target = Provider::with_id(
                    "target".into(),
                    "Third party".into(),
                    codex_settings("https://example.test/v1", "sk-target"),
                    None,
                );
                state.db.save_provider("codex", &target).unwrap();
                let before = crate::codex_config::CodexLiveStateSnapshot::capture().unwrap();
                let result = ProviderService::switch(&restarted, AppType::Codex, "target");
                if corrupt {
                    assert!(result.is_err());
                    assert_eq!(
                        crate::codex_config::CodexLiveStateSnapshot::capture().unwrap(),
                        before
                    );
                    assert_eq!(
                        crate::mode::current::provider_for(
                            &state.db,
                            &AppType::Codex,
                            crate::mode::current::Purpose::InUse,
                        )
                        .unwrap()
                        .as_deref(),
                        Some("current")
                    );
                } else {
                    result.unwrap();
                    assert!(
                        !crate::codex_config::codex_managed_oauth_live_auth_marker_exists(),
                        "missing account must relinquish ownership"
                    );
                }
                assert_eq!(
                    read_json_file::<Value>(&crate::codex_config::get_codex_auth_path()).unwrap(),
                    auth
                );
            });
        }
    }
}

#[test]
#[serial]
fn managed_codex_switch_db_current_failure_rolls_forward_on_recovery() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-managed-a", "managed-token-a", "user-a")
                .await
                .expect("seed first managed Codex OAuth account");
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-managed-b", "managed-token-b", "user-b")
                .await
                .expect("seed second managed Codex OAuth account");
        });

        let managed_provider = |id: &str, account_id: &str, model: &str| {
            let mut provider = Provider::with_id(
                id.to_string(),
                format!("Managed {id}"),
                json!({
                    "auth": {},
                    "config": format!("model = \"{model}\"\n"),
                    "modelCatalog": {
                        "models": [{ "model": model }]
                    }
                }),
                None,
            );
            provider.category = Some("official".to_string());
            provider.meta = Some(ProviderMeta {
                auth_binding: Some(AuthBinding {
                    source: AuthBindingSource::ManagedAccount,
                    auth_provider: Some("codex_oauth".to_string()),
                    account_id: Some(account_id.to_string()),
                }),
                ..Default::default()
            });
            provider
        };

        let provider_a = managed_provider("managed-a", "acct-managed-a", "gpt-5.4-managed-a");
        let provider_b = managed_provider("managed-b", "acct-managed-b", "gpt-5.4-managed-b");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider_a)
            .expect("save first managed provider");
        state
            .db
            .save_provider(AppType::Codex.as_str(), &provider_b)
            .expect("save second managed provider");

        ProviderService::switch(state, AppType::Codex, &provider_a.id)
            .expect("activate first managed provider");
        let auth_before: Value = read_json_file(&crate::codex_config::get_codex_auth_path())
            .expect("read first managed auth");
        assert!(
            crate::codex_config::get_codex_config_path().exists(),
            "baseline must include config.toml"
        );
        assert!(
            crate::codex_config::get_codex_model_catalog_path().exists(),
            "baseline must include the generated model catalog"
        );
        assert!(
            crate::codex_config::codex_auth_matches_recorded_managed_oauth(
                &auth_before,
                "acct-managed-a",
            )
            .expect("check first managed auth marker"),
            "baseline must include a marker owned by the first managed account"
        );
        let live_before = crate::codex_config::CodexLiveStateSnapshot::capture()
            .expect("capture auth/config/catalog/marker before failed switch");

        {
            let conn = state.db.conn.lock().expect("lock database");
            conn.execute_batch(
                "CREATE TRIGGER reject_managed_b_current_update
                 BEFORE UPDATE OF is_current ON providers
                 WHEN NEW.app_type = 'codex'
                   AND NEW.id = 'managed-b'
                   AND NEW.is_current = 1
                 BEGIN
                   SELECT RAISE(ABORT, 'forced managed Codex current failure');
                 END;",
            )
            .expect("install current-provider failure trigger");
        }

        let error = ProviderService::switch(state, AppType::Codex, &provider_b.id)
            .expect_err("DB current failure should abort managed switch");
        assert!(
            error
                .to_string()
                .contains("forced managed Codex current failure"),
            "switch should surface the DB commit failure, got: {error}"
        );

        // 文件已经发布，只是指针没落定：不回滚，pending 等着补完。
        assert_ne!(
            crate::codex_config::CodexLiveStateSnapshot::capture().expect("capture live"),
            live_before
        );
        let auth_after: Value =
            read_json_file(&crate::codex_config::get_codex_auth_path()).expect("read managed auth");
        assert!(
            crate::codex_config::codex_auth_matches_recorded_managed_oauth(
                &auth_after,
                "acct-managed-b",
            )
            .expect("check managed marker"),
            "auth.json and the marker belong to the target account"
        );
        assert!(crate::mode::operation::has_pending(AppType::Codex.as_str()));

        state
            .db
            .conn
            .lock()
            .expect("lock database")
            .execute_batch("DROP TRIGGER reject_managed_b_current_update;")
            .expect("drop trigger");
        crate::mode::operation::recover_on_startup(&state.db);
        assert!(!crate::mode::operation::has_pending(
            AppType::Codex.as_str()
        ));
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Codex).as_deref(),
            Some(provider_b.id.as_str())
        );
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read DB current after recovery")
                .as_deref(),
            Some(provider_b.id.as_str())
        );
    });
}

#[test]
#[serial]
fn managed_codex_update_rechecks_current_after_waiting_for_switch_lock() {
    with_test_home(|state, _| {
        crate::settings::reload_settings().expect("reload settings");
        tauri::async_runtime::block_on(async {
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-managed-a", "managed-token-a", "user-a")
                .await
                .expect("seed managed account A");
            state
                .codex_oauth_manager
                .add_test_account_with_user_identity("acct-managed-b", "managed-token-b", "user-b")
                .await
                .expect("seed managed account B");
        });

        let mut official = Provider::with_id(
            "managed-official-a".to_string(),
            "OpenAI Official".to_string(),
            json!({ "auth": {}, "config": "model = \"gpt-5.4\"\n" }),
            None,
        );
        official.category = Some("official".to_string());
        official.meta = Some(ProviderMeta {
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some("acct-managed-a".to_string()),
            }),
            ..Default::default()
        });
        state
            .db
            .save_provider(AppType::Codex.as_str(), &official)
            .expect("save official A");
        state
            .db
            .set_current_provider(AppType::Codex.as_str(), &official.id)
            .expect("set official current");
        crate::settings::set_current_provider(&AppType::Codex, Some(&official.id))
            .expect("set local official current");

        let mut third_party = Provider::with_id(
            "third-party-current".to_string(),
            "Third Party".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "sk-third" },
                "config": r#"model_provider = "third"
[model_providers.third]
name = "Third"
base_url = "https://third.example/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        third_party.category = Some("custom".to_string());
        state
            .db
            .save_provider(AppType::Codex.as_str(), &third_party)
            .expect("save third party");

        let mut updated = official.clone();
        updated
            .meta
            .as_mut()
            .and_then(|meta| meta.auth_binding.as_mut())
            .expect("managed binding")
            .account_id = Some("acct-managed-b".to_string());

        let switch_guard = tauri::async_runtime::block_on(
            state
                .proxy_service
                .lock_switch_for_app(AppType::Codex.as_str()),
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (update_result, live_after_switch) = std::thread::scope(|scope| {
            let updater = scope.spawn(move || {
                started_tx.send(()).expect("signal updater start");
                ProviderService::update(state, AppType::Codex, None, updated)
            });
            started_rx.recv().expect("wait for updater");

            // This emulates a switch that already owns the per-app lock and
            // commits a different current target before the queued update is
            // allowed to inspect current/existing state.
            state
                .db
                .set_current_provider(AppType::Codex.as_str(), &third_party.id)
                .expect("switch DB current to third party");
            crate::settings::set_current_provider(&AppType::Codex, Some(third_party.id.as_str()))
                .expect("switch local current to third party");
            write_live_for_state(state, &AppType::Codex, &third_party)
                .expect("write third-party live");
            let live_after_switch = crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture third-party live");

            drop(switch_guard);
            let result = updater.join().expect("join managed updater");
            (result, live_after_switch)
        });

        update_result.expect("save queued non-current managed row");
        assert_eq!(
            state
                .db
                .get_current_provider(AppType::Codex.as_str())
                .expect("read DB current")
                .as_deref(),
            Some(third_party.id.as_str())
        );
        assert_eq!(
            crate::codex_config::CodexLiveStateSnapshot::capture()
                .expect("capture live after queued update"),
            live_after_switch,
            "queued provider edit must not rewrite the newly switched current live"
        );
        let saved_official = state
            .db
            .get_provider_by_id(&official.id, AppType::Codex.as_str())
            .expect("read saved official")
            .expect("official exists");
        assert_eq!(
            saved_official
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for("codex_oauth")),
            Some("acct-managed-b".to_string())
        );
    });
}
