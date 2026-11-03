//! Claude Code 官方订阅账号的状态与专用命令（加号、列表、删号走授权中心的通用 `auth_*` 命令）。

use std::sync::Arc;

use crate::services::claude_account::ClaudeAccountManager;

pub struct ClaudeAccountState(pub Arc<ClaudeAccountManager>);

/// 浏览器回不到本机时，把落地页地址或登录页上的代码交给正在等待的 `claude auth login`。
#[tauri::command(rename_all = "camelCase")]
pub async fn claude_account_submit_paste(
    flow_id: String,
    pasted: String,
    state: tauri::State<'_, ClaudeAccountState>,
) -> Result<(), String> {
    state
        .0
        .submit_paste(&flow_id, &pasted)
        .await
        .map_err(|error| error.to_string())
}

/// 绑了账号的 Claude 官方卡的额度：按那个账号自己的登录查，带上它的套餐。
/// 结果按账号写进托盘缓存（同 Codex 托管账号）：卡片和托盘悬停查到的是同一份。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_claude_account_quota(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, crate::store::AppState>,
    account_id: String,
) -> Result<crate::services::subscription::ClaudeAccountQuota, String> {
    let account_id = account_id.trim().to_string();
    let result = crate::services::subscription::get_claude_account_quota(
        crate::config::get_app_config_dir(),
        account_id.clone(),
    )
    .await;
    if let Ok(quota) = &result {
        app_state
            .usage_cache
            .put_claude_account(account_id, quota.quota.clone());
        crate::tray::schedule_tray_refresh(&app);
    }
    result
}

/// Claude Code 默认登录的套餐（没绑账号的官方卡显示）；没登录或凭据里没写时为 `None`。
#[tauri::command]
pub async fn get_claude_default_plan(
) -> Result<Option<crate::services::claude_account::AccountPlan>, String> {
    tokio::task::spawn_blocking(crate::services::subscription::read_claude_default_plan)
        .await
        .map_err(|error| error.to_string())
}

/// 切卡时切换 Claude Code 登录的状态。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLoginSwitchStatus {
    /// 切到绑号卡时会切换登录（没被挡住）
    pub active: bool,
    /// 不切换登录的原因：`"wsl"`（Claude 配置目录在 WSL 里，账号功能不可用）或
    /// `"overrideDir"`（设了别的 Claude 配置目录覆盖）
    pub blocked: Option<String>,
    /// Claude Code 现在登录的账号 key（按 `~/.claude.json` 的资料）
    pub live: Option<String>,
}

fn login_switch_status() -> ClaudeLoginSwitchStatus {
    use crate::services::claude_account::snapshot;
    let blocked = snapshot::blocked_reason();
    ClaudeLoginSwitchStatus {
        active: blocked.is_none(),
        blocked: blocked.map(str::to_string),
        live: snapshot::default_owner(&snapshot::store::Slot::default_slot()),
    }
}

#[tauri::command]
pub async fn get_claude_login_switch() -> Result<ClaudeLoginSwitchStatus, String> {
    tokio::task::spawn_blocking(login_switch_status)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::settings::{get_settings, update_settings, AppSettings};
    use serde_json::json;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// HOME 和全局设置换成测试现场：默认槽位的 `.claude.json` 落进临时目录，
    /// 目录覆盖按需打开。
    struct ClaudeAccountTestHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        home: PathBuf,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
        previous_settings: AppSettings,
    }

    impl ClaudeAccountTestHome {
        fn new() -> Self {
            // 和 gemini_mcp / mcp::claude 共用一把 HOME 锁；#[serial] 挡住同样要换
            // 全局设置的测试。
            let lock = test_home_lock().blocking_lock();
            let temp = tempfile::tempdir().expect("tempdir");
            let previous_home = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
            let previous_settings = get_settings();
            update_settings(AppSettings::default()).expect("clear settings for the test");
            Self {
                _lock: lock,
                home: temp.path().to_path_buf(),
                _temp: temp,
                previous_home,
                previous_settings,
            }
        }

        fn write_home_profile(&self, value: serde_json::Value) {
            std::fs::write(
                self.home.join(".claude.json"),
                serde_json::to_string_pretty(&value).expect("serialize"),
            )
            .expect("write the home profile");
        }

        fn set_override_dir(&self) -> PathBuf {
            let dir = self.home.join("claude-override");
            std::fs::create_dir_all(&dir).expect("create the override dir");
            update_settings(AppSettings {
                claude_config_dir: Some(dir.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .expect("point claude at the override dir");
            dir
        }
    }

    impl Drop for ClaudeAccountTestHome {
        fn drop(&mut self) {
            let _ = update_settings(self.previous_settings.clone());
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn the_switch_is_active_with_nothing_blocking_it() {
        // 只为隔离现场，不断言具体路径。
        let _home = ClaudeAccountTestHome::new();

        let status = login_switch_status();
        assert!(status.active);
        assert_eq!(status.blocked, None);
        assert_eq!(status.live, None, "没有资料就不知道登的是谁");
    }

    #[test]
    #[serial]
    fn an_override_dir_blocks_the_switch_but_the_live_owner_still_comes_from_home() {
        let home = ClaudeAccountTestHome::new();
        home.set_override_dir();
        home.write_home_profile(json!({"oauthAccount": {"accountUuid": "acct-1"}}));

        let status = login_switch_status();
        assert!(!status.active, "设了覆盖目录就不切登录");
        assert_eq!(status.blocked.as_deref(), Some("overrideDir"));
        // 账号 key 是 <accountUuid>:<organizationUuid>，没有组织时后半为空。
        assert_eq!(status.live.as_deref(), Some("acct-1:"));
    }
}
