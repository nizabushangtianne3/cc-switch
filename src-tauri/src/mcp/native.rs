//! MCP 原生配置的公共读取与导入。
//!
//! Pi / Kimi / MCode / AGy 四家的原生配置文件形状相同（顶层 `mcpServers` 映射），
//! 读取校验与导入逻辑因此完全同构。差异只由 [`NativeSource`] 表达。
//!
//! Omp 不走这里：它的文档从 omp 配置里直接取，不读盘上的 mcp.json。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::app_config::{McpApps, McpServer};
use crate::error::AppError;
use crate::store::AppState;

/// 读原生 MCP 配置：文件不存在按空配置处理，形状不对时点名 `label` 报错。
///
/// `tolerate_empty` 只对 AGy 为真：它首次启动会落一个 0 字节的 mcp_config.json，
/// 别家遇到 0 字节就是损坏的配置，必须报错。
pub(crate) fn read_native(
    path: &Path,
    label: &str,
    tolerate_empty: bool,
) -> Result<Value, AppError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({"mcpServers": {}})),
        Err(e) => return Err(AppError::io(path, e)),
    };
    if tolerate_empty && text.trim().is_empty() {
        return Ok(json!({"mcpServers": {}}));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| AppError::Config(format!("Invalid {label} MCP JSON")))?;
    if !value.is_object() || value.get("mcpServers").is_some_and(|v| !v.is_object()) {
        return Err(AppError::Config(format!(
            "Invalid {label} mcpServers object"
        )));
    }
    Ok(value)
}

/// 只保留传输字段，用于比对两条连接定义是不是同一个传输。
pub(crate) fn transport_fields_of(unified: &Value, fields: &[&str]) -> Value {
    let mut spec = unified.clone();
    if let Some(object) = spec.as_object_mut() {
        object.retain(|key, _| fields.contains(&key.as_str()));
    }
    spec
}

/// 一家原生配置文件怎么读、怎么写进统一账号库。
pub(crate) struct NativeSource {
    /// 配置文件路径。
    pub path: PathBuf,
    /// 报错时点名哪一家（"Pi" / "Kimi Code" / "MiniMax Code" / "Antigravity"）。
    pub label: &'static str,
    /// 是否把 0 字节文件当成空配置（只有 AGy 需要）。
    pub tolerate_empty: bool,
    /// 从原生条目里读启用位的字段名。
    pub enabled_field: &'static str,
    /// 该字段是否取反（AGy 存的是 `disabled`）。
    pub inverted: bool,
    /// 把启用位写进 `McpApps` 的哪一位。
    pub set_app: fn(&mut McpApps, bool),
    /// 各家把原生条目归一成统一格式的那一步（Kimi 把 `transport` 并进 `type`，
    /// AGy 把 `serverUrl` 并进 `url`，另两家没有前置改写）。
    pub unify: for<'a> fn(&'a Value) -> Value,
    /// 比对传输时只看哪些字段。
    pub transport_fields: &'static [&'static str],
}

/// 导入一家原生配置里的 MCP 服务器。
///
/// 重复导入同一条目时：传输定义一致就只更新启用位，冲突则跳过并累进错误信息。
pub(crate) fn import(state: &AppState, source: &NativeSource) -> Result<usize, AppError> {
    let document = read_native(&source.path, source.label, source.tolerate_empty)?;
    let mut existing = state.db.get_all_mcp_servers()?;
    let mut count = 0;
    let mut skipped = Vec::new();
    for (id, native) in document["mcpServers"].as_object().into_iter().flatten() {
        let mut spec = (source.unify)(native);
        if super::validation::validate_server_spec(&spec).is_err() {
            skipped.push(format!("'{id}': invalid transport configuration"));
            continue;
        }
        let enabled = native
            .get(source.enabled_field)
            .and_then(Value::as_bool)
            .map(|flag| flag != source.inverted)
            .unwrap_or(true);
        spec.as_object_mut().unwrap().remove(source.enabled_field);
        let server = if let Some(mut server) = existing.shift_remove(id) {
            let stored = (source.unify)(&server.server);
            if transport_fields_of(&stored, source.transport_fields)
                != transport_fields_of(&spec, source.transport_fields)
            {
                skipped.push(format!("'{id}': conflicts with an existing server"));
                continue;
            }
            (source.set_app)(&mut server.apps, enabled);
            server
        } else {
            count += 1;
            let mut apps = McpApps::default();
            (source.set_app)(&mut apps, enabled);
            McpServer {
                id: id.clone(),
                name: id.clone(),
                server: spec,
                apps,
                ..Default::default()
            }
        };
        state.db.save_mcp_server(&server)?;
    }
    if skipped.is_empty() {
        Ok(count)
    } else {
        Err(AppError::InvalidInput(format!(
            "Imported {count} {} MCP servers; skipped {}. Native configurations were preserved.",
            source.label,
            skipped.join("; ")
        )))
    }
}
