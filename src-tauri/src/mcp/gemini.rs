//! Gemini MCP 同步和导入模块

use serde_json::Value;
use std::collections::HashMap;

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::validate_server_spec;

fn should_sync_gemini_mcp() -> bool {
    // Gemini 未安装/未初始化时：~/.gemini 目录不存在。
    // 按用户偏好：目录缺失时跳过写入/删除，不创建任何文件或目录。
    crate::gemini_config::get_gemini_dir().exists()
}

/// 将 config.json 中 Gemini 的 enabled==true 项写入 Gemini MCP 配置
pub fn sync_enabled_to_gemini(config: &MultiAppConfig) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    let enabled = super::collect_enabled_servers(&config.mcp.gemini);
    crate::gemini_mcp::set_mcp_servers_map(&enabled)
}

/// 从 Gemini MCP 配置导入到统一结构（v3.7.0+）
/// 已存在的服务器将启用 Gemini 应用，不覆盖其他字段和应用状态
pub fn import_from_gemini(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let map = crate::gemini_mcp::read_mcp_servers_map()?;
    if map.is_empty() {
        return Ok(0);
    }

    // 确保新结构存在
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for (id, spec) in map.iter() {
        // 校验：单项失败不中止，收集错误继续处理
        if let Err(e) = validate_server_spec(spec) {
            log::warn!("跳过无效 MCP 服务器 '{id}': {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            // 已存在：仅启用 Gemini 应用
            if !existing.apps.gemini {
                existing.apps.gemini = true;
                changed += 1;
                log::info!("MCP 服务器 '{id}' 已启用 Gemini 应用");
            }
        } else {
            // 新建服务器：默认仅启用 Gemini
            servers.insert(
                id.clone(),
                McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    server: spec.clone(),
                    apps: McpApps {
                        agy: false,
                        claude: false,
                        codex: false,
                        gemini: true,
                        grokbuild: false,
                        opencode: false,
                        hermes: false,
                        mcode: false,
                        kimi: false,
                        copilot: false,
                        dsh: false,
                        pi: false,
                        omp: false,
                    },
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                },
            );
            changed += 1;
            log::info!("导入新 MCP 服务器 '{id}'");
        }
    }

    if !errors.is_empty() {
        log::warn!("导入完成，但有 {} 项失败: {:?}", errors.len(), errors);
    }

    Ok(changed)
}

/// 将单个 MCP 服务器同步到 Gemini live 配置
pub fn sync_single_server_to_gemini(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    // 读取现有的 MCP 配置
    let mut current = crate::gemini_mcp::read_mcp_servers_map()?;

    // 添加/更新当前服务器
    current.insert(id.to_string(), server_spec.clone());

    // 写回
    crate::gemini_mcp::set_mcp_servers_map(&current)
}

/// 从 Gemini live 配置中移除单个 MCP 服务器
pub fn remove_server_from_gemini(id: &str) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    // 读取现有的 MCP 配置
    let mut current = crate::gemini_mcp::read_mcp_servers_map()?;

    // 移除指定服务器
    current.remove(id);

    // 写回
    crate::gemini_mcp::set_mcp_servers_map(&current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::McpConfig;
    use crate::config::test_home_lock;
    use crate::settings::{get_settings, update_settings, AppSettings};
    use serde_json::json;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    /// HOME 和全局设置换成测试现场：`~/.gemini/settings.json` 落进临时目录。
    struct GeminiMcpHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        home: PathBuf,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
        previous_settings: AppSettings,
    }

    impl GeminiMcpHome {
        fn new() -> Self {
            // 和 gemini_mcp / mcp::claude 共用一把 HOME 锁；#[serial] 挡住同样要换
            // 全局设置的测试，两把一起拿才不会互相踩。
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

        fn gemini_dir(&self) -> PathBuf {
            self.home.join(".gemini")
        }

        fn settings_path(&self) -> PathBuf {
            self.gemini_dir().join("settings.json")
        }

        /// 造出「已安装过 Gemini」的现场：目录和配置文件都在。
        fn installed(&self) {
            fs::create_dir_all(self.gemini_dir()).expect("create the gemini dir");
        }

        fn write_settings(&self, value: Value) {
            self.installed();
            fs::write(
                self.settings_path(),
                serde_json::to_string_pretty(&value).expect("serialize"),
            )
            .expect("write the gemini settings");
        }

        fn read_settings(&self) -> Value {
            let text = fs::read_to_string(self.settings_path()).expect("the settings exist");
            serde_json::from_str(&text).expect("valid json")
        }
    }

    impl Drop for GeminiMcpHome {
        fn drop(&mut self) {
            // 先还原设置（此刻写入还指向临时 HOME），再恢复 HOME。
            let _ = update_settings(self.previous_settings.clone());
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn server(id: &str, spec: Value, gemini: bool) -> McpServer {
        McpServer {
            id: id.to_string(),
            name: id.to_string(),
            server: spec,
            apps: McpApps {
                gemini,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    #[serial]
    fn sync_skips_writes_when_the_gemini_dir_is_missing() {
        let home = GeminiMcpHome::new();
        assert!(!should_sync_gemini_mcp());

        let mut legacy = HashMap::new();
        legacy.insert(
            "one".to_string(),
            json!({"enabled": true, "server": {"command": "python"}}),
        );
        let mut config = MultiAppConfig::default();
        config.mcp.gemini = McpConfig { servers: legacy };

        sync_enabled_to_gemini(&config).expect("skipping is not an error");
        assert!(!home.settings_path().exists(), "不该替用户创建文件");

        remove_server_from_gemini("one").expect("skipping is not an error");
        assert!(!home.settings_path().exists(), "也不该创建文件");
    }

    #[test]
    #[serial]
    fn import_reads_nothing_without_the_gemini_dir() {
        let home = GeminiMcpHome::new();
        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_gemini(&mut config).unwrap(), 0);
        let servers = config.mcp.servers.expect("空配置本来就带着统一存储");
        assert!(servers.is_empty(), "没有文件就不该写入任何服务器");
        assert!(!home.settings_path().exists());
    }

    #[test]
    #[serial]
    fn import_reports_broken_json() {
        let home = GeminiMcpHome::new();
        home.installed();
        fs::write(home.settings_path(), "{ not json").expect("write broken json");

        let mut config = MultiAppConfig::default();
        assert!(import_from_gemini(&mut config).is_err());
    }

    #[test]
    #[serial]
    fn import_adds_new_servers_with_only_gemini_enabled() {
        let home = GeminiMcpHome::new();
        home.write_settings(json!({
            "mcpServers": {
                "local": {"command": "python", "args": ["server.py"]},
                "remote": {"httpUrl": "https://example.com/mcp"}
            }
        }));

        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_gemini(&mut config).unwrap(), 2);

        let servers = config.mcp.servers.expect("the unified store appears");
        assert_eq!(servers.len(), 2);
        let local = &servers["local"];
        assert_eq!(local.id, "local");
        assert_eq!(local.name, "local");
        assert_eq!(
            local.server,
            // 读取时按字段名补上了传输类型。
            json!({"command": "python", "args": ["server.py"], "type": "stdio"})
        );
        assert!(local.apps.gemini);
        assert!(!local.apps.claude);
        assert!(!local.apps.codex);

        // httpUrl 已被 gemini_mcp 抬成统一结构里的 url + type。
        let remote = &servers["remote"];
        assert_eq!(remote.server["url"], "https://example.com/mcp");
        assert_eq!(remote.server["type"], "http");
    }

    #[test]
    #[serial]
    fn import_enables_an_existing_server_without_touching_its_fields() {
        let home = GeminiMcpHome::new();
        home.write_settings(json!({"mcpServers": {"shared": {"command": "python"}}}));

        let existing = server(
            "shared",
            json!({"command": "python", "args": ["old.py"]}),
            false,
        );
        let mut existing = existing;
        existing.apps.claude = true;
        existing.name = "Kept Name".to_string();
        existing.description = Some("kept".to_string());
        let mut config = MultiAppConfig::default();
        config.mcp.servers = Some(HashMap::from([("shared".to_string(), existing)]));

        assert_eq!(import_from_gemini(&mut config).unwrap(), 1);

        // 再导入一次：已经开着，不再计数。
        assert_eq!(import_from_gemini(&mut config).unwrap(), 0);

        let servers = config.mcp.servers.expect("the store stays");
        let shared = &servers["shared"];
        assert!(shared.apps.gemini, "给这个现有服务器打开 Gemini");
        assert!(shared.apps.claude, "别的应用状态不受影响");
        assert_eq!(shared.name, "Kept Name");
        assert_eq!(shared.description.as_deref(), Some("kept"));
        assert_eq!(
            shared.server,
            json!({"command": "python", "args": ["old.py"]}),
            "spec 不被导入内容覆盖"
        );
    }

    #[test]
    #[serial]
    fn import_skips_invalid_specs_but_keeps_the_valid_ones() {
        let home = GeminiMcpHome::new();
        home.write_settings(json!({
            "mcpServers": {
                "ok": {"command": "python"},
                "no-command": {"type": "stdio"},
                "not-an-object": 42
            }
        }));

        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_gemini(&mut config).unwrap(), 1);

        let servers = config.mcp.servers.expect("the unified store appears");
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("ok"));
    }

    #[test]
    #[serial]
    fn sync_enabled_writes_only_the_servers_enabled_for_gemini() {
        let home = GeminiMcpHome::new();
        home.installed();

        let mut legacy = HashMap::new();
        legacy.insert(
            "on".to_string(),
            json!({"enabled": true, "server": {"command": "python", "args": ["on.py"]}}),
        );
        legacy.insert(
            "off".to_string(),
            json!({"enabled": false, "server": {"command": "python"}}),
        );
        let mut config = MultiAppConfig::default();
        config.mcp.gemini = McpConfig { servers: legacy };

        sync_enabled_to_gemini(&config).expect("sync the enabled ones");

        let map = home.read_settings()["mcpServers"]
            .as_object()
            .expect("mcpServers written")
            .clone();
        assert_eq!(map.len(), 1);
        assert_eq!(map["on"]["command"], "python");
        assert_eq!(map["on"]["args"], json!(["on.py"]));
        assert!(map["on"].get("type").is_none(), "Gemini 不认 type 字段");
        assert!(!map.contains_key("off"));
    }

    #[test]
    #[serial]
    fn sync_single_server_merges_without_dropping_the_others() {
        let home = GeminiMcpHome::new();
        home.write_settings(json!({"mcpServers": {"keep": {"command": "python"}}}));

        sync_single_server_to_gemini(
            &MultiAppConfig::default(),
            "added",
            &json!({"command": "python", "args": ["added.py"]}),
        )
        .expect("merge the new server in");

        let written = home.read_settings();
        assert_eq!(written["mcpServers"]["keep"]["command"], "python");
        assert_eq!(written["mcpServers"]["added"]["args"], json!(["added.py"]));
    }

    #[test]
    #[serial]
    fn remove_server_drops_only_the_named_one() {
        let home = GeminiMcpHome::new();
        home.write_settings(json!({
            "mcpServers": {
                "gone": {"command": "python"},
                "stay": {"command": "python"}
            }
        }));

        remove_server_from_gemini("gone").expect("remove the named server");
        let written = home.read_settings();
        assert!(!written["mcpServers"]
            .as_object()
            .expect("mcpServers stays an object")
            .contains_key("gone"));
        assert!(written["mcpServers"]["stay"].is_object());

        // 删一个不存在的：其余的原地不动。
        remove_server_from_gemini("ghost").expect("removing a stranger is fine");
        let again = home.read_settings();
        assert_eq!(again, written);
    }
}
