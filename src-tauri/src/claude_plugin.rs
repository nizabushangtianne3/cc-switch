use std::fs;
use std::path::PathBuf;

use crate::error::AppError;

const CLAUDE_DIR: &str = ".claude";
const CLAUDE_CONFIG_FILE: &str = "config.json";

fn claude_dir() -> Result<PathBuf, AppError> {
    // 优先使用设置中的覆盖目录
    if let Some(dir) = crate::settings::get_claude_override_dir() {
        return Ok(dir);
    }
    let home = dirs::home_dir().ok_or_else(|| AppError::Config("无法获取用户主目录".into()))?;
    Ok(home.join(CLAUDE_DIR))
}

pub fn claude_config_path() -> Result<PathBuf, AppError> {
    Ok(claude_dir()?.join(CLAUDE_CONFIG_FILE))
}

pub fn ensure_claude_dir_exists() -> Result<PathBuf, AppError> {
    let dir = claude_dir()?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| AppError::io(&dir, e))?;
    }
    Ok(dir)
}

pub fn read_claude_config() -> Result<Option<String>, AppError> {
    let path = claude_config_path()?;
    if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        Ok(Some(content))
    } else {
        Ok(None)
    }
}

pub fn write_claude_config() -> Result<bool, AppError> {
    // 增量写入：仅设置 primaryApiKey = "any"，保留其它字段
    let path = claude_config_path()?;
    ensure_claude_dir_exists()?;

    // 尝试读取并解析为对象
    let mut obj = match read_claude_config()? {
        Some(existing) => match serde_json::from_str::<serde_json::Value>(&existing) {
            Ok(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
            _ => serde_json::json!({}),
        },
        None => serde_json::json!({}),
    };

    let mut changed = false;
    if let Some(map) = obj.as_object_mut() {
        let cur = map
            .get("primaryApiKey")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if cur != "any" {
            map.insert(
                "primaryApiKey".to_string(),
                serde_json::Value::String("any".to_string()),
            );
            changed = true;
        }
    }

    if changed || !path.exists() {
        let serialized = serde_json::to_string_pretty(&obj)
            .map_err(|e| AppError::JsonSerialize { source: e })?;
        fs::write(&path, format!("{serialized}\n")).map_err(|e| AppError::io(&path, e))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn clear_claude_config() -> Result<bool, AppError> {
    let path = claude_config_path()?;
    if !path.exists() {
        return Ok(false);
    }

    let content = match read_claude_config()? {
        Some(content) => content,
        None => return Ok(false),
    };

    let mut value = match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };

    let obj = match value.as_object_mut() {
        Some(obj) => obj,
        None => return Ok(false),
    };

    if obj.remove("primaryApiKey").is_none() {
        return Ok(false);
    }

    let serialized =
        serde_json::to_string_pretty(&value).map_err(|e| AppError::JsonSerialize { source: e })?;
    fs::write(&path, format!("{serialized}\n")).map_err(|e| AppError::io(&path, e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::settings::{get_settings, update_settings, AppSettings};
    use serde_json::Value;
    use serial_test::serial;
    use std::ffi::OsString;

    /// 把 Claude 配置目录指到临时现场的守卫：`claude_dir()` 优先读设置里的覆盖目录，
    /// 所以换 settings 就够；顺带换 `CC_SWITCH_TEST_HOME`，让设置文件的读写也落进
    /// 临时 HOME，不碰真实用户的 `~/.cc-switch`。
    struct ClaudePluginHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
        previous_settings: AppSettings,
    }

    impl ClaudePluginHome {
        fn new() -> Self {
            // 和 gemini_mcp / mcp::claude 共用一把 HOME 锁；#[serial] 挡住同样要换
            // 全局设置的测试，两把一起拿才不会互相踩。
            let lock = test_home_lock().blocking_lock();
            let temp = tempfile::tempdir().expect("tempdir");
            let previous_home = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
            let previous_settings = get_settings();
            let dir = temp.path().join("claude-override");
            fs::create_dir_all(&dir).expect("create the override dir");
            update_settings(AppSettings {
                claude_config_dir: Some(dir.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .expect("point claude at the test dir");
            Self {
                _lock: lock,
                dir,
                _temp: temp,
                previous_home,
                previous_settings,
            }
        }

        fn config_path(&self) -> PathBuf {
            self.dir.join(CLAUDE_CONFIG_FILE)
        }

        fn write_config(&self, content: &str) {
            fs::write(self.config_path(), content).expect("write the config");
        }

        fn read_config(&self) -> String {
            fs::read_to_string(self.config_path()).expect("the config exists")
        }

        fn parse_config(&self) -> Value {
            serde_json::from_str(&self.read_config()).expect("valid json")
        }
    }

    impl Drop for ClaudePluginHome {
        fn drop(&mut self) {
            // 先还原设置（此刻写入还指向临时 HOME），再恢复 HOME。
            let _ = update_settings(self.previous_settings.clone());
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn write_creates_the_managed_key_and_keeps_the_other_fields() {
        let home = ClaudePluginHome::new();
        home.write_config(r#"{"model": "opus"}"#);

        assert!(write_claude_config().unwrap(), "首次写入算改动");
        let value = home.parse_config();
        assert_eq!(value["primaryApiKey"], "any");
        assert_eq!(value["model"], "opus", "别的字段原样保留");

        // 已经托管：不再写，返回 false。
        assert!(!write_claude_config().unwrap());
    }

    #[test]
    #[serial]
    fn write_creates_the_file_when_nothing_exists() {
        let home = ClaudePluginHome::new();
        assert!(write_claude_config().unwrap());
        assert!(home.config_path().is_file());

        let value = home.parse_config();
        assert_eq!(value.as_object().expect("object").len(), 1);
        assert_eq!(value["primaryApiKey"], "any");
    }

    #[test]
    #[serial]
    fn write_starts_over_when_the_existing_file_is_not_a_json_object() {
        let home = ClaudePluginHome::new();
        home.write_config("not json at all");
        assert!(write_claude_config().unwrap());
        let value = home.parse_config();
        assert_eq!(value.as_object().expect("object").len(), 1);
        assert_eq!(value["primaryApiKey"], "any");

        // 根是数组也同理：整份重写。
        home.write_config("[1, 2, 3]");
        assert!(write_claude_config().unwrap());
        assert_eq!(home.parse_config()["primaryApiKey"], "any");
    }

    #[test]
    #[serial]
    fn clear_removes_the_managed_key_and_keeps_the_rest() {
        let home = ClaudePluginHome::new();
        home.write_config(r#"{"primaryApiKey": "any", "model": "opus"}"#);

        assert!(clear_claude_config().unwrap());
        let value = home.parse_config();
        assert!(value.get("primaryApiKey").is_none());
        assert_eq!(value["model"], "opus");
    }

    #[test]
    #[serial]
    fn clear_reports_nothing_to_do_for_files_it_cannot_manage() {
        let home = ClaudePluginHome::new();
        // 没有文件。
        assert!(!clear_claude_config().unwrap());
        assert!(!home.config_path().exists());

        // 没有 primaryApiKey：原样不动。
        home.write_config(r#"{"model": "opus"}"#);
        assert!(!clear_claude_config().unwrap());
        assert_eq!(home.read_config(), r#"{"model": "opus"}"#);

        // 解析不了 / 根不是对象：也不动。
        home.write_config("not json");
        assert!(!clear_claude_config().unwrap());
        home.write_config("[1]");
        assert!(!clear_claude_config().unwrap());
    }

    #[test]
    #[serial]
    fn ensure_creates_the_override_directory_and_is_idempotent() {
        let home = ClaudePluginHome::new();
        fs::remove_dir_all(&home.dir).expect("remove the dir");

        assert_eq!(ensure_claude_dir_exists().unwrap(), home.dir);
        assert!(home.dir.is_dir());
        assert_eq!(ensure_claude_dir_exists().unwrap(), home.dir);
    }
}
