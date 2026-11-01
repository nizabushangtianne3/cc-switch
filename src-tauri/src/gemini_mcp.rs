use serde_json::{Map, Value};
use std::path::PathBuf;

use crate::config::{read_json_value, write_json_value};
use crate::error::AppError;
use crate::gemini_config::get_gemini_settings_path;

/// 获取 Gemini MCP 配置文件路径（~/.gemini/settings.json）
fn user_config_path() -> PathBuf {
    get_gemini_settings_path()
}

/// 读取 Gemini settings.json 中的 mcpServers 映射
///
/// 执行反向格式转换以保持与统一 MCP 结构的兼容性：
/// - httpUrl → url + type: "http"
/// - 仅有 url 字段 → 补齐 type: "sse"（Gemini 以字段名推断传输类型）
/// - 仅有 command 字段 → 补齐 type: "stdio"
pub fn read_mcp_servers_map() -> Result<std::collections::HashMap<String, Value>, AppError> {
    let path = user_config_path();
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }

    let root = read_json_value(&path)?;
    let mut servers: std::collections::HashMap<String, Value> = root
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    // 反向格式转换：Gemini 特有格式 → 统一 MCP 格式
    for (_, spec) in servers.iter_mut() {
        if let Some(obj) = spec.as_object_mut() {
            // httpUrl → url + type: "http"
            if let Some(http_url) = obj.remove("httpUrl") {
                obj.insert("url".to_string(), http_url);
                obj.insert("type".to_string(), Value::String("http".to_string()));
            }

            // Gemini CLI 不使用 type 字段：这里补齐成统一结构，便于校验与导入
            if obj.get("type").is_none() {
                if obj.contains_key("command") {
                    obj.insert("type".to_string(), Value::String("stdio".to_string()));
                } else if obj.contains_key("url") {
                    obj.insert("type".to_string(), Value::String("sse".to_string()));
                }
            }
        }
    }

    Ok(servers)
}

/// 将给定的启用 MCP 服务器映射写入到 Gemini settings.json 的 mcpServers 字段
/// 仅覆盖 mcpServers，其他字段保持不变
pub fn set_mcp_servers_map(
    servers: &std::collections::HashMap<String, Value>,
) -> Result<(), AppError> {
    let path = user_config_path();
    let mut root = if path.exists() {
        read_json_value(&path)?
    } else {
        serde_json::json!({})
    };

    // 构建 mcpServers 对象：移除 UI 辅助字段（enabled/source），仅保留实际 MCP 规范
    let mut out: Map<String, Value> = Map::new();
    for (id, spec) in servers.iter() {
        let mut obj = if let Some(map) = spec.as_object() {
            map.clone()
        } else {
            return Err(AppError::McpValidation(format!(
                "MCP 服务器 '{id}' 不是对象"
            )));
        };

        // 提取 server 字段（如果存在）
        if let Some(server_val) = obj.remove("server") {
            let server_obj = server_val.as_object().cloned().ok_or_else(|| {
                AppError::McpValidation(format!("MCP 服务器 '{id}' server 字段不是对象"))
            })?;
            obj = server_obj;
        }

        // Gemini CLI 格式转换：
        // - Gemini 不使用 "type" 字段（从字段名推断传输类型）
        // - HTTP 使用 "httpUrl" 字段，SSE 使用 "url" 字段
        let transport_type = obj.get("type").and_then(|v| v.as_str());
        if transport_type == Some("http") {
            // HTTP streaming: 将 "url" 重命名为 "httpUrl"
            if let Some(url_value) = obj.remove("url") {
                obj.insert("httpUrl".to_string(), url_value);
            }
        }
        // SSE 保持 "url" 字段不变

        // 移除 UI 辅助字段和 type 字段（Gemini 不需要）
        obj.remove("type");
        obj.remove("enabled");
        obj.remove("source");
        obj.remove("id");
        obj.remove("name");
        obj.remove("description");
        obj.remove("tags");
        obj.remove("homepage");
        obj.remove("docs");

        // Timeout 转换：Claude/Codex 使用 startup_timeout_sec/tool_timeout_sec
        // Gemini CLI 只支持 timeout（单位 ms）
        // 默认值：startup=10s, tool=60s
        const DEFAULT_STARTUP_MS: u64 = 10_000;
        const DEFAULT_TOOL_MS: u64 = 60_000;

        let extract_timeout =
            |obj: &mut Map<String, Value>, key: &str, multiplier: u64| -> Option<u64> {
                obj.remove(key).and_then(|val| {
                    val.as_u64()
                        .map(|n| n * multiplier)
                        .or_else(|| val.as_f64().map(|f| (f * multiplier as f64) as u64))
                })
            };

        // 分别收集 startup 和 tool timeout，未设置时使用默认值
        let startup_ms = extract_timeout(&mut obj, "startup_timeout_sec", 1000)
            .or_else(|| extract_timeout(&mut obj, "startup_timeout_ms", 1))
            .unwrap_or(DEFAULT_STARTUP_MS);
        let tool_ms = extract_timeout(&mut obj, "tool_timeout_sec", 1000)
            .or_else(|| extract_timeout(&mut obj, "tool_timeout_ms", 1))
            .unwrap_or(DEFAULT_TOOL_MS);

        // 取最大值作为 Gemini timeout
        let final_timeout = startup_ms.max(tool_ms);
        obj.insert("timeout".to_string(), Value::Number(final_timeout.into()));

        out.insert(id.clone(), Value::Object(obj));
    }

    {
        let obj = root
            .as_object_mut()
            .ok_or_else(|| AppError::Config("~/.gemini/settings.json 根必须是对象".into()))?;
        obj.insert("mcpServers".into(), Value::Object(out));
    }

    write_json_value(&path, &root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::settings::{get_settings, update_settings, AppSettings};
    use serde_json::json;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;

    /// HOME 和全局设置一起换成测试现场：`~/.gemini/settings.json` 落进临时目录，
    /// 设置里的目录覆盖被清空，退出时连设置一起还原。
    struct GeminiTestHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        home: PathBuf,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
        previous_settings: AppSettings,
    }

    impl GeminiTestHome {
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

        fn settings_path(&self) -> PathBuf {
            self.home.join(".gemini").join("settings.json")
        }

        fn write_settings(&self, value: serde_json::Value) {
            let path = self.settings_path();
            fs::create_dir_all(path.parent().expect("settings has a parent"))
                .expect("create gemini dir");
            fs::write(
                path,
                serde_json::to_string_pretty(&value).expect("serialize settings"),
            )
            .expect("write settings");
        }

        fn read_settings(&self) -> serde_json::Value {
            let text = fs::read_to_string(self.settings_path()).expect("settings written");
            serde_json::from_str(&text).expect("valid json")
        }
    }

    impl Drop for GeminiTestHome {
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
    fn read_mcp_servers_map_is_empty_without_a_file() {
        let home = GeminiTestHome::new();
        assert!(read_mcp_servers_map().unwrap().is_empty());
        assert!(!home.settings_path().exists(), "读不该顺手创建文件");
    }

    #[test]
    #[serial]
    fn read_mcp_servers_map_lifts_the_gemini_shape_into_the_unified_one() {
        let home = GeminiTestHome::new();
        home.write_settings(json!({
            "mcpServers": {
                "remote": {"httpUrl": "https://example.com/mcp", "headers": {"X-Token": "t"}},
                "sse": {"url": "https://example.com/sse"},
                "local": {"command": "node", "args": ["server.js"]},
                "typed": {"type": "stdio", "command": "py"}
            },
            "theme": "dark"
        }));

        let servers = read_mcp_servers_map().unwrap();
        assert_eq!(servers.len(), 4);

        let remote = &servers["remote"];
        assert_eq!(remote["url"], "https://example.com/mcp");
        assert_eq!(remote["type"], "http");
        assert!(remote.get("httpUrl").is_none());
        assert_eq!(remote["headers"]["X-Token"], "t");

        // 没有 type 时按字段名推断：url 是 sse，command 是 stdio。
        assert_eq!(servers["sse"]["type"], "sse");
        assert_eq!(servers["local"]["type"], "stdio");
        // 已经写了 type 的不改写。
        assert_eq!(servers["typed"]["type"], "stdio");
    }

    #[test]
    #[serial]
    fn read_mcp_servers_map_ignores_a_broken_or_missing_field() {
        let home = GeminiTestHome::new();
        home.write_settings(json!({"mcpServers": "nope"}));
        assert!(read_mcp_servers_map().unwrap().is_empty());

        home.write_settings(json!({"theme": "dark"}));
        assert!(read_mcp_servers_map().unwrap().is_empty());
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_writes_only_the_gemini_shape() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([(
            "local".to_string(),
            json!({"command": "node", "args": ["server.js"], "type": "stdio"}),
        )]);

        set_mcp_servers_map(&servers).unwrap();

        assert!(home.settings_path().exists(), "写入要创建 settings.json");
        let written = home.read_settings();
        assert_eq!(
            written.as_object().expect("root object").len(),
            1,
            "只有 mcpServers 一个键"
        );

        let local = &written["mcpServers"]["local"];
        assert_eq!(local["command"], "node");
        assert_eq!(local["args"], json!(["server.js"]));
        assert!(local.get("type").is_none(), "Gemini 不认 type 字段");
        // 没给 timeout：startup 默认 10s、tool 默认 60s，取最大。
        assert_eq!(local["timeout"], json!(60_000));
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_keeps_the_other_gemini_settings() {
        let home = GeminiTestHome::new();
        home.write_settings(json!({
            "theme": "dark",
            "mcpServers": {"old": {"command": "go"}}
        }));

        let servers = HashMap::from([(
            "fresh".to_string(),
            json!({"command": "node", "type": "stdio"}),
        )]);
        set_mcp_servers_map(&servers).unwrap();

        let written = home.read_settings();
        assert_eq!(written["theme"], "dark");
        assert_eq!(written["mcpServers"]["fresh"]["command"], "node");
        assert!(
            written["mcpServers"].get("old").is_none(),
            "整个 mcpServers 被替换，不是合并"
        );
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_moves_http_url_to_httpurl_and_strips_ui_fields() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([(
            "remote".to_string(),
            json!({
                "url": "https://example.com/mcp",
                "type": "http",
                "headers": {"X-Token": "t"},
                "enabled": true,
                "source": "ui",
                "id": "remote",
                "name": "Remote",
                "description": "d",
                "tags": ["a"],
                "homepage": "https://example.com",
                "docs": "https://example.com/docs"
            }),
        )]);

        set_mcp_servers_map(&servers).unwrap();

        let written = home.read_settings();
        let remote = &written["mcpServers"]["remote"];
        assert_eq!(remote["httpUrl"], "https://example.com/mcp");
        assert!(remote.get("url").is_none(), "HTTP 用 httpUrl 承载地址");
        assert_eq!(remote["headers"]["X-Token"], "t", "真实配置字段要保留");
        for field in [
            "type",
            "enabled",
            "source",
            "id",
            "name",
            "description",
            "tags",
            "homepage",
            "docs",
        ] {
            assert!(
                remote.get(field).is_none(),
                "{field} 是 UI 辅助字段，不该落盘"
            );
        }
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_takes_the_largest_timeout_of_both_sources() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([
            (
                "slow-start".to_string(),
                json!({"command": "node", "type": "stdio", "startup_timeout_sec": 90, "tool_timeout_sec": 5}),
            ),
            (
                "in-ms".to_string(),
                json!({"command": "node", "type": "stdio", "startup_timeout_ms": 5_000, "tool_timeout_ms": 2_000}),
            ),
            (
                "float".to_string(),
                json!({"command": "node", "type": "stdio", "startup_timeout_sec": 1.5, "tool_timeout_ms": 1_000}),
            ),
            (
                "unset".to_string(),
                json!({"command": "node", "type": "stdio", "garbage_timeout": "x"}),
            ),
        ]);

        set_mcp_servers_map(&servers).unwrap();

        let written = home.read_settings();
        let out = &written["mcpServers"];
        assert_eq!(out["slow-start"]["timeout"], json!(90_000));
        assert_eq!(out["in-ms"]["timeout"], json!(5_000));
        assert_eq!(out["float"]["timeout"], json!(1_500));
        assert_eq!(out["unset"]["timeout"], json!(60_000));
        assert_eq!(
            out["unset"]["garbage_timeout"], "x",
            "只认四个 timeout 字段，其余原样透传"
        );
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_unwraps_the_nested_server_object() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([(
            "full".to_string(),
            json!({
                "server": {"command": "npx", "args": ["-y", "@example/mcp"], "env": {"TOKEN": "t"}},
                "enabled": true
            }),
        )]);

        set_mcp_servers_map(&servers).unwrap();

        let written = home.read_settings();
        let full = &written["mcpServers"]["full"];
        assert_eq!(full["command"], "npx");
        assert_eq!(full["args"], json!(["-y", "@example/mcp"]));
        assert_eq!(full["env"]["TOKEN"], "t");
        assert!(full.get("enabled").is_none());
        assert!(full.get("server").is_none(), "server 包裹层要拆掉");
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_rejects_specs_that_are_not_objects() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([("broken".to_string(), json!(["not", "an", "object"]))]);
        let error = set_mcp_servers_map(&servers).unwrap_err().to_string();
        assert!(error.contains("不是对象"), "{error}");
        assert!(!home.settings_path().exists(), "校验不过就不落盘");
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_rejects_a_server_field_that_is_not_an_object() {
        let home = GeminiTestHome::new();
        let servers = HashMap::from([("broken".to_string(), json!({"server": "npx"}))]);
        let error = set_mcp_servers_map(&servers).unwrap_err().to_string();
        assert!(error.contains("server 字段不是对象"), "{error}");
        assert!(!home.settings_path().exists(), "校验不过就不落盘");
    }

    #[test]
    #[serial]
    fn set_mcp_servers_map_rejects_a_root_that_is_not_an_object() {
        let home = GeminiTestHome::new();
        home.write_settings(json!(["settings.json 被写坏了"]));
        let servers = HashMap::from([("ok".to_string(), json!({"command": "node"}))]);
        let error = set_mcp_servers_map(&servers).unwrap_err().to_string();
        assert!(error.contains("根必须是对象"), "{error}");
    }

    #[test]
    #[serial]
    fn servers_survive_a_write_then_read_cycle() {
        let home = GeminiTestHome::new();
        home.write_settings(json!({
            "mcpServers": {
                "remote": {"httpUrl": "https://example.com/mcp", "headers": {"X-Token": "t"}},
                "local": {"command": "node", "args": ["server.js"]}
            }
        }));

        let read = read_mcp_servers_map().unwrap();
        set_mcp_servers_map(&read).unwrap();
        let cycled = read_mcp_servers_map().unwrap();

        let mut names: Vec<&str> = cycled.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["local", "remote"]);
        assert_eq!(cycled["remote"]["url"], "https://example.com/mcp");
        assert_eq!(cycled["remote"]["type"], "http");
        assert_eq!(cycled["remote"]["headers"]["X-Token"], "t");
        assert_eq!(cycled["local"]["command"], "node");
        assert_eq!(cycled["local"]["args"], json!(["server.js"]));
        assert_eq!(cycled["local"]["type"], "stdio");
    }
}
