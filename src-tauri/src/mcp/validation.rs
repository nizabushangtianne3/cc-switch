//! MCP 服务器配置验证模块

use serde_json::Value;

use crate::error::AppError;

/// 基础校验：允许 stdio/http/sse；或省略 type（视为 stdio）。对应必填字段存在
pub fn validate_server_spec(spec: &Value) -> Result<(), AppError> {
    if !spec.is_object() {
        return Err(AppError::McpValidation(
            "MCP 服务器连接定义必须为 JSON 对象".into(),
        ));
    }
    let t_opt = spec.get("type").and_then(|x| x.as_str());
    // 支持三种：stdio/http/sse；若缺省 type 则按 stdio 处理（与社区常见 .mcp.json 一致）
    let is_stdio = t_opt.map(|t| t == "stdio").unwrap_or(true);
    let is_http = t_opt.map(|t| t == "http").unwrap_or(false);
    let is_sse = t_opt.map(|t| t == "sse").unwrap_or(false);

    if !(is_stdio || is_http || is_sse) {
        return Err(AppError::McpValidation(
            "MCP 服务器 type 必须是 'stdio'、'http' 或 'sse'（或省略表示 stdio）".into(),
        ));
    }

    if is_stdio {
        let cmd = spec.get("command").and_then(|x| x.as_str()).unwrap_or("");
        if cmd.trim().is_empty() {
            return Err(AppError::McpValidation(
                "stdio 类型的 MCP 服务器缺少 command 字段".into(),
            ));
        }
    }
    if is_http {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "http 类型的 MCP 服务器缺少 url 字段".into(),
            ));
        }
    }
    if is_sse {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "sse 类型的 MCP 服务器缺少 url 字段".into(),
            ));
        }
    }
    Ok(())
}

/// 写出侧的传输类型（各应用把统一规范投影成原生格式时共用）。
///
/// 规范里可能没有 `type`（面板 JSON 模式粘贴 `{"url": …}`、deeplink 都不补），
/// 此时不能一律当 stdio：那样 url 型服务器会被投影成没有 `url` 的 stdio 条目
/// （Codex 报 `url is not supported for stdio` 整份 config.toml 加载失败，
/// Hermes 只剩 `enabled: true` 的空壳）。显式写了 stdio 却只有 `url` 的规范同理。
pub(crate) fn spec_transport_type(spec: &Value) -> &str {
    let has = |key: &str| {
        spec.get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    let explicit = spec.get("type").and_then(|v| v.as_str());
    if matches!(explicit, None | Some("stdio")) && !has("command") && has("url") {
        return "http";
    }
    explicit.unwrap_or("stdio")
}

/// 从 MCP 条目中提取服务器规范
pub fn extract_server_spec(entry: &Value) -> Result<Value, AppError> {
    let obj = entry
        .as_object()
        .ok_or_else(|| AppError::McpValidation("MCP 服务器条目必须为 JSON 对象".into()))?;
    let server = obj
        .get("server")
        .ok_or_else(|| AppError::McpValidation("MCP 服务器条目缺少 server 字段".into()))?;

    if !server.is_object() {
        return Err(AppError::McpValidation(
            "MCP 服务器 server 字段必须为 JSON 对象".into(),
        ));
    }

    Ok(server.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_rejects_specs_that_are_not_objects() {
        for value in [json!(42), json!("stdio"), json!([]), json!(null)] {
            assert!(validate_server_spec(&value).is_err(), "{value} 不是对象");
        }
    }

    #[test]
    fn validate_accepts_the_three_transports_and_a_missing_type() {
        // 省略 type 按 stdio 处理（与社区常见 .mcp.json 一致）。
        assert!(validate_server_spec(&json!({"command": "python"})).is_ok());
        assert!(validate_server_spec(&json!({"type": "stdio", "command": "python"})).is_ok());
        assert!(validate_server_spec(&json!({"type": "http", "url": "https://x/mcp"})).is_ok());
        assert!(validate_server_spec(&json!({"type": "sse", "url": "https://x/sse"})).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_transports() {
        let error = validate_server_spec(&json!({"type": "websocket", "url": "wss://x"}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("stdio"), "{error}");
    }

    #[test]
    fn validate_requires_the_transports_own_field() {
        // stdio 要 command。
        assert!(validate_server_spec(&json!({"type": "stdio"})).is_err());
        assert!(
            validate_server_spec(&json!({"command": "  "})).is_err(),
            "空白 command 不算有"
        );
        // http / sse 要 url。
        assert!(validate_server_spec(&json!({"type": "http"})).is_err());
        assert!(validate_server_spec(&json!({"type": "http", "url": " "})).is_err());
        assert!(validate_server_spec(&json!({"type": "sse"})).is_err());
        assert!(validate_server_spec(&json!({"type": "sse", "url": "  "})).is_err());
    }

    #[test]
    fn a_non_string_type_falls_back_to_the_stdio_rules() {
        // type 不是字符串时 as_str 拿不到，按 stdio 规则走。
        assert!(validate_server_spec(&json!({"type": 42, "command": "python"})).is_ok());
        assert!(
            validate_server_spec(&json!({"type": 42})).is_err(),
            "没有 command 就过不去"
        );
    }

    #[test]
    fn transport_type_reads_the_spec_the_way_the_projections_write_it() {
        // 显式类型原样保留。
        assert_eq!(
            spec_transport_type(&json!({"type": "http", "url": "https://x"})),
            "http"
        );
        assert_eq!(
            spec_transport_type(&json!({"type": "sse", "url": "https://x"})),
            "sse"
        );
        assert_eq!(
            spec_transport_type(&json!({"type": "stdio", "command": "py"})),
            "stdio"
        );

        // 没写 type：按字段名判断，url 型不能落进 stdio。
        assert_eq!(spec_transport_type(&json!({"url": "https://x"})), "http");
        assert_eq!(spec_transport_type(&json!({"command": "py"})), "stdio");
        assert_eq!(
            spec_transport_type(&json!({"command": "py", "url": "https://x"})),
            "stdio"
        );

        // 写了 stdio 却只有 url：仍按 http 投影，否则客户端整份配置加载失败。
        assert_eq!(
            spec_transport_type(&json!({"type": "stdio", "url": "https://x"})),
            "http"
        );

        // 空串字段不算有。
        assert_eq!(
            spec_transport_type(&json!({"command": "  ", "url": "https://x"})),
            "http"
        );
        assert_eq!(spec_transport_type(&json!({})), "stdio");
    }

    #[test]
    fn extract_server_spec_unwraps_the_entry_around_the_spec() {
        let spec = json!({"command": "python", "args": ["server.py"]});
        let entry = json!({"enabled": true, "server": spec, "source": "ui"});
        assert_eq!(extract_server_spec(&entry).unwrap(), spec);
    }

    #[test]
    fn extract_server_spec_rejects_malformed_entries() {
        assert!(extract_server_spec(&json!(["nope"])).is_err());
        assert!(extract_server_spec(&json!({"enabled": true})).is_err());
        assert!(extract_server_spec(&json!({"server": "python"})).is_err());
    }
}
