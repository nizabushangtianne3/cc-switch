//! Claude MCP 同步和导入模块

use serde_json::Value;
use std::collections::HashMap;

use crate::app_config::{McpApps, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::validate_server_spec;

fn should_sync_claude_mcp() -> bool {
    // Claude 未安装/未初始化时：通常 ~/.claude 目录与 ~/.claude.json 都不存在。
    // 按用户偏好：此时跳过写入/删除，不创建任何文件或目录。
    crate::config::get_claude_config_dir().exists() || crate::config::get_claude_mcp_path().exists()
}

/// 将 config.json 中 enabled==true 的项投影写入 ~/.claude.json
pub fn sync_enabled_to_claude(config: &MultiAppConfig) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    let enabled = super::collect_enabled_servers(&config.mcp.claude);
    crate::claude_mcp::set_mcp_servers_map(&enabled)
}

/// 从 ~/.claude.json 导入 mcpServers 到统一结构（v3.7.0+）
/// 已存在的服务器将启用 Claude 应用，不覆盖其他字段和应用状态
pub fn import_from_claude(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let text_opt = crate::claude_mcp::read_mcp_json()?;
    let Some(text) = text_opt else { return Ok(0) };

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::McpValidation(format!("解析 ~/.claude.json 失败: {e}")))?;
    let Some(map) = v.get("mcpServers").and_then(|x| x.as_object()) else {
        return Ok(0);
    };

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
            // 已存在：仅启用 Claude 应用
            if !existing.apps.claude {
                existing.apps.claude = true;
                changed += 1;
                log::info!("MCP 服务器 '{id}' 已启用 Claude 应用");
            }
        } else {
            // 新建服务器：默认仅启用 Claude
            servers.insert(
                id.clone(),
                McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    server: spec.clone(),
                    apps: McpApps {
                        agy: false,
                        claude: true,
                        codex: false,
                        gemini: false,
                        grokbuild: false,
                        opencode: false,
                        hermes: false,
                        mcode: false,
                        kimi: false,
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

/// 将单个 MCP 服务器同步到 Claude live 配置
pub fn sync_single_server_to_claude(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    // 读取现有的 MCP 配置
    let current = crate::claude_mcp::read_mcp_servers_map()?;

    // 创建新的 HashMap，包含现有的所有服务器 + 当前要同步的服务器
    let mut updated = current;
    updated.insert(id.to_string(), server_spec.clone());

    // 写回
    crate::claude_mcp::set_mcp_servers_map(&updated)
}

/// 从 Claude live 配置中移除单个 MCP 服务器
pub fn remove_server_from_claude(id: &str) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    // 读取现有的 MCP 配置
    let mut current = crate::claude_mcp::read_mcp_servers_map()?;

    // 移除指定服务器
    current.remove(id);

    // 写回
    crate::claude_mcp::set_mcp_servers_map(&current)
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

    /// HOME 和全局设置一起换成测试现场：`~/.claude.json` 与 `~/.claude` 落进临时
    /// 目录，退出时连设置一起还原。命令统一用 `python`：它在 Windows 包装名单外，
    /// 断言不必沾染 `cmd /c` 的细节。
    struct ClaudeMcpHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        home: PathBuf,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
        previous_settings: AppSettings,
    }

    impl ClaudeMcpHome {
        fn new() -> Self {
            // 和 openclaw_config / workspace 共用一把 HOME 锁；#[serial] 挡住同样要
            // 换全局设置的测试，两把一起拿才不会互相踩。
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

        fn mcp_path(&self) -> PathBuf {
            self.home.join(".claude.json")
        }

        fn claude_dir(&self) -> PathBuf {
            self.home.join(".claude")
        }

        fn write_mcp_file(&self, value: Value) {
            fs::write(
                self.mcp_path(),
                serde_json::to_string_pretty(&value).expect("serialize"),
            )
            .expect("write the claude mcp file");
        }

        fn read_mcp_file(&self) -> Value {
            let text = fs::read_to_string(self.mcp_path()).expect("the claude mcp file exists");
            serde_json::from_str(&text).expect("valid json")
        }
    }

    impl Drop for ClaudeMcpHome {
        fn drop(&mut self) {
            // 先还原设置（此刻写入还指向临时 HOME），再恢复 HOME。
            let _ = update_settings(self.previous_settings.clone());
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn server(id: &str, spec: Value, claude: bool) -> McpServer {
        McpServer {
            id: id.to_string(),
            name: id.to_string(),
            server: spec,
            apps: McpApps {
                claude,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    #[serial]
    fn sync_skips_every_write_when_claude_is_not_installed() {
        let home = ClaudeMcpHome::new();
        assert!(!should_sync_claude_mcp());

        let mut config = MultiAppConfig::default();
        config.mcp.servers = Some(HashMap::from([(
            "one".to_string(),
            server("one", json!({"command": "python"}), true),
        )]));
        sync_enabled_to_claude(&config).expect("skipping is not an error");
        assert!(!home.mcp_path().exists(), "不该替用户创建文件");

        remove_server_from_claude("one").expect("skipping is not an error");
        assert!(!home.mcp_path().exists(), "也不该创建文件");
    }

    #[test]
    #[serial]
    fn sync_runs_once_the_claude_config_dir_exists() {
        let home = ClaudeMcpHome::new();
        assert!(!should_sync_claude_mcp());

        fs::write(home.mcp_path(), "{}").expect("write the mcp file");
        assert!(should_sync_claude_mcp());

        fs::remove_file(home.mcp_path()).expect("remove the mcp file");
        assert!(!should_sync_claude_mcp());

        fs::create_dir_all(home.claude_dir()).expect("create the claude dir");
        assert!(should_sync_claude_mcp(), "只有目录也算装过");
    }

    #[test]
    #[serial]
    fn import_reads_nothing_without_a_file() {
        let home = ClaudeMcpHome::new();
        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_claude(&mut config).unwrap(), 0);
        let servers = config.mcp.servers.expect("空配置本来就带着统一存储");
        assert!(servers.is_empty(), "没有文件就不该写入任何服务器");
        assert!(!home.mcp_path().exists());
    }

    #[test]
    #[serial]
    fn import_ignores_a_file_without_mcp_servers() {
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({"otherField": true}));

        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_claude(&mut config).unwrap(), 0);
        let servers = config.mcp.servers.expect("空配置本来就带着统一存储");
        assert!(servers.is_empty());
    }

    #[test]
    #[serial]
    fn import_reports_broken_json() {
        let home = ClaudeMcpHome::new();
        fs::write(home.mcp_path(), "{ not json").expect("write broken json");

        let mut config = MultiAppConfig::default();
        let error = import_from_claude(&mut config).unwrap_err().to_string();
        assert!(error.contains("解析 ~/.claude.json 失败"), "{error}");
    }

    #[test]
    #[serial]
    fn import_adds_new_servers_with_only_claude_enabled() {
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({
            "mcpServers": {
                "local": {"command": "python", "args": ["server.py"]},
                "remote": {"type": "http", "url": "https://example.com/mcp"}
            }
        }));

        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_claude(&mut config).unwrap(), 2);

        let servers = config.mcp.servers.expect("the unified store appears");
        assert_eq!(servers.len(), 2);
        let local = &servers["local"];
        assert_eq!(local.id, "local");
        assert_eq!(local.name, "local");
        assert_eq!(
            local.server,
            json!({"command": "python", "args": ["server.py"]})
        );
        assert!(local.apps.claude);
        assert!(!local.apps.codex);
        assert!(!local.apps.gemini);
        assert_eq!(local.description, None);
        assert!(local.tags.is_empty());

        let remote = &servers["remote"];
        assert!(remote.apps.claude);
        assert_eq!(
            remote.server,
            json!({"type": "http", "url": "https://example.com/mcp"})
        );
    }

    #[test]
    #[serial]
    fn import_enables_an_existing_server_without_touching_its_fields() {
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({"mcpServers": {"shared": {"command": "python"}}}));

        let existing = server(
            "shared",
            json!({"command": "python", "args": ["old.py"]}),
            false,
        );
        let mut existing = existing;
        existing.apps.codex = true;
        existing.name = "Kept Name".to_string();
        existing.description = Some("kept".to_string());
        let mut config = MultiAppConfig::default();
        config.mcp.servers = Some(HashMap::from([("shared".to_string(), existing)]));

        assert_eq!(import_from_claude(&mut config).unwrap(), 1);

        // 再导入一次：已经开着，不再计数。
        assert_eq!(import_from_claude(&mut config).unwrap(), 0);

        let servers = config.mcp.servers.expect("the store stays");
        let shared = &servers["shared"];
        assert!(shared.apps.claude, "给这个现有服务器打开 Claude");
        assert!(shared.apps.codex, "别的应用状态不受影响");
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
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({
            "mcpServers": {
                "ok": {"command": "python"},
                "no-command": {"type": "stdio"},
                "no-url": {"type": "http"},
                "not-an-object": 42
            }
        }));

        let mut config = MultiAppConfig::default();
        assert_eq!(import_from_claude(&mut config).unwrap(), 1);

        let servers = config.mcp.servers.expect("the unified store appears");
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("ok"));
    }

    #[test]
    #[serial]
    fn sync_enabled_writes_only_the_servers_enabled_for_claude() {
        let home = ClaudeMcpHome::new();
        fs::write(home.mcp_path(), "{}").expect("write an empty mcp file");

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
        config.mcp.claude = McpConfig { servers: legacy };

        sync_enabled_to_claude(&config).expect("sync the enabled ones");

        let map = home.read_mcp_file()["mcpServers"]
            .as_object()
            .expect("mcpServers written")
            .clone();
        assert_eq!(map.len(), 1);
        assert_eq!(map["on"]["command"], "python");
        assert_eq!(map["on"]["args"], json!(["on.py"]));
        assert!(!map.contains_key("off"));
    }

    #[test]
    #[serial]
    fn sync_single_server_merges_without_dropping_the_others() {
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({"mcpServers": {"keep": {"command": "python"}}}));

        sync_single_server_to_claude(
            &MultiAppConfig::default(),
            "added",
            &json!({"command": "python", "args": ["added.py"]}),
        )
        .expect("merge the new server in");

        let written = home.read_mcp_file();
        assert_eq!(written["mcpServers"]["keep"]["command"], "python");
        assert_eq!(written["mcpServers"]["added"]["args"], json!(["added.py"]));
    }

    #[test]
    #[serial]
    fn remove_server_drops_only_the_named_one() {
        let home = ClaudeMcpHome::new();
        home.write_mcp_file(json!({
            "mcpServers": {
                "gone": {"command": "python"},
                "stay": {"command": "python"}
            }
        }));

        remove_server_from_claude("gone").expect("remove the named server");
        let written = home.read_mcp_file();
        assert!(!written["mcpServers"]
            .as_object()
            .expect("mcpServers stays an object")
            .contains_key("gone"));
        assert!(written["mcpServers"]["stay"].is_object());

        // 删一个不存在的：其余的原地不动。
        remove_server_from_claude("ghost").expect("removing a stranger is fine");
        let again = home.read_mcp_file();
        assert_eq!(again, written);
    }
}
