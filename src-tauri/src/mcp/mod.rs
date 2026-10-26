//! MCP (Model Context Protocol) 服务器管理模块
//!
//! 本模块负责 MCP 服务器配置的验证、同步和导入导出。
//!
//! ## 模块结构
//!
//! - `validation` - 服务器配置验证
//! - `claude` - Claude MCP 同步和导入
//! - `codex` - Codex MCP 同步和导入（含 TOML 转换）
//! - `gemini` - Gemini MCP 同步和导入
//! - `opencode` - OpenCode MCP 同步和导入（含 local/remote 格式转换）
//! - `hermes` - Hermes MCP 同步和导入
//! - `omp` - Omp MCP 同步和导入

use crate::app_config::McpConfig;
use serde_json::Value;
use std::collections::HashMap;

pub mod agy;
mod claude;
mod codex;
mod gemini;
mod grokbuild;
mod hermes;
pub(crate) mod omp;
mod opencode;
mod validation;

// 重新导出公共 API
pub use claude::{
    import_from_claude, remove_server_from_claude, sync_enabled_to_claude,
    sync_single_server_to_claude,
};
pub use codex::{
    import_from_codex, remove_server_from_codex, sync_enabled_to_codex, sync_single_server_to_codex,
};
pub use gemini::{
    import_from_gemini, remove_server_from_gemini, sync_enabled_to_gemini,
    sync_single_server_to_gemini,
};
pub use grokbuild::{
    import_from_grokbuild, remove_server_from_grokbuild, sync_single_server_to_grokbuild,
};
pub use hermes::{import_from_hermes, remove_server_from_hermes, sync_single_server_to_hermes};
pub use omp::{remove_server_from_omp, sync_single_server_to_omp};
pub use opencode::{
    import_from_opencode, remove_server_from_opencode, sync_single_server_to_opencode,
};

pub(crate) mod kimi;
pub(crate) mod mcode;
pub(crate) mod pi;

/// 从数据库配置里挑出启用了的 MCP 服务条目。
pub(crate) fn collect_enabled_servers(cfg: &McpConfig) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    for (id, entry) in cfg.servers.iter() {
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        match validation::extract_server_spec(entry) {
            Ok(spec) => {
                out.insert(id.clone(), spec);
            }
            Err(err) => {
                log::warn!("跳过无效的 MCP 条目 '{id}': {err}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(enabled: Option<bool>, server: Value) -> Value {
        let mut map = serde_json::Map::new();
        if let Some(flag) = enabled {
            map.insert("enabled".into(), Value::Bool(flag));
        }
        map.insert("server".into(), server);
        Value::Object(map)
    }

    #[test]
    fn collect_enabled_servers_skips_disabled_and_invalid_entries() {
        let stdio = json!({"type": "stdio", "command": "node"});
        let mut servers = HashMap::new();
        servers.insert("ok".to_string(), entry(Some(true), stdio.clone()));
        servers.insert("off".to_string(), entry(Some(false), stdio.clone()));
        servers.insert("plain".to_string(), entry(None, stdio));
        servers.insert(
            "invalid".to_string(),
            entry(Some(true), Value::from("not-an-object")),
        );

        let cfg = McpConfig { servers };
        let out = collect_enabled_servers(&cfg);

        assert_eq!(out.len(), 1);
        assert_eq!(out["ok"]["command"], "node");
        assert!(!out.contains_key("off"));
        assert!(!out.contains_key("plain"));
        assert!(!out.contains_key("invalid"));
    }
}
