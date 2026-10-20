//! 请求体过滤模块
//!
//! 过滤不应透传到上游的私有参数，防止内部信息泄露。
//!
//! ## 过滤规则
//! - 以 `_` 开头的字段被视为私有参数，会被就地递归移除
//! - JSON Schema 的 properties / patternProperties / definitions / $defs 名称
//!   是用户定义的字段名，不按私有参数过滤
//!
//! ## 使用场景
//! - `_internal_id`: 内部追踪 ID
//! - `_debug_mode`: 调试标记
//! - `_session_token`: 会话令牌
//! - `_client_version`: 客户端版本
//!
//! 出站管道每个请求都要过一次这里，所以不重建文档：先用只读扫描确认
//! 没有私有字段（绝大多数请求），连拷贝都不产生；有就在原值上摘键。

use serde_json::Value;

/// 只读扫描：请求体里是否存在需要过滤的私有字段。
///
/// 与 [`strip_private_params`] 同一套规则（含 JSON Schema 名称表例外）。
pub fn contains_private_params(value: &Value) -> bool {
    contains_private_params_under(value, None)
}

fn contains_private_params_under(value: &Value, parent_key: Option<&str>) -> bool {
    match value {
        Value::Object(map) => {
            let is_schema_name_map = parent_key.is_some_and(matches_schema_name_map);
            map.iter().any(|(key, child)| {
                (!is_schema_name_map && key.starts_with('_'))
                    || contains_private_params_under(child, Some(key.as_str()))
            })
        }
        Value::Array(items) => items
            .iter()
            .any(|item| contains_private_params_under(item, parent_key)),
        _ => false,
    }
}

/// 就地移除私有字段（以 `_` 开头的字段），返回是否真的移除过。
///
/// 没有私有字段时是零分配的只读遍历；有也只摘掉对应的键，
/// 不会像「重建整棵文档」那样给每个对象都重新分配 Map。
pub fn strip_private_params(value: &mut Value) -> bool {
    let mut removed: Vec<String> = Vec::new();
    let changed = strip_in_place(value, None, &mut removed);
    if !removed.is_empty() {
        // 仅在有过滤时记录日志（避免每次请求都打印）
        log::debug!("[BodyFilter] 过滤私有参数: {removed:?}");
    }
    changed
}

fn strip_in_place(value: &mut Value, parent_key: Option<&str>, removed: &mut Vec<String>) -> bool {
    match value {
        Value::Object(map) => {
            // Schema 名称表里的键是用户定义的字段名，原样保留。
            let is_schema_name_map = parent_key.is_some_and(matches_schema_name_map);
            let mut changed = false;
            if !is_schema_name_map {
                let mut removed_here = false;
                map.retain(|key, _| {
                    if key.starts_with('_') {
                        removed.push(key.clone());
                        removed_here = true;
                        false
                    } else {
                        true
                    }
                });
                changed = removed_here;
            }
            for (key, child) in map.iter_mut() {
                if strip_in_place(child, Some(key.as_str()), removed) {
                    changed = true;
                }
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items.iter_mut() {
                if strip_in_place(item, parent_key, removed) {
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    }
}

fn matches_schema_name_map(key: &str) -> bool {
    matches!(
        key,
        "properties" | "patternProperties" | "definitions" | "$defs"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 测试辅助：就地过滤后把值交回去，保持既有断言的写法。
    fn filtered(mut body: Value) -> Value {
        strip_private_params(&mut body);
        body
    }

    #[test]
    fn test_filter_top_level_private_params() {
        let input = json!({
            "model": "claude-3",
            "_internal_id": "abc123",
            "_debug": true,
            "max_tokens": 1024
        });

        let output = filtered(input);

        assert!(output.get("model").is_some());
        assert!(output.get("max_tokens").is_some());
        assert!(output.get("_internal_id").is_none());
        assert!(output.get("_debug").is_none());
    }

    #[test]
    fn test_filter_nested_private_params() {
        let input = json!({
            "model": "claude-3",
            "messages": [
                {
                    "role": "user",
                    "content": "hello",
                    "_session_token": "secret"
                }
            ],
            "metadata": {
                "user_id": "user-1",
                "_tracking_id": "track-1"
            }
        });

        let output = filtered(input);

        // 顶级字段保留
        assert!(output.get("model").is_some());
        assert!(output.get("messages").is_some());
        assert!(output.get("metadata").is_some());

        // messages 数组中的私有参数被过滤
        let messages = output.get("messages").unwrap().as_array().unwrap();
        assert!(messages[0].get("role").is_some());
        assert!(messages[0].get("content").is_some());
        assert!(messages[0].get("_session_token").is_none());

        // metadata 对象中的私有参数被过滤
        let metadata = output.get("metadata").unwrap();
        assert!(metadata.get("user_id").is_some());
        assert!(metadata.get("_tracking_id").is_none());
    }

    #[test]
    fn test_filter_deeply_nested() {
        let input = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "keep": "value",
                        "_remove": "secret"
                    }
                }
            }
        });

        let output = filtered(input);

        let level3 = output
            .get("level1")
            .unwrap()
            .get("level2")
            .unwrap()
            .get("level3")
            .unwrap();

        assert!(level3.get("keep").is_some());
        assert!(level3.get("_remove").is_none());
    }

    #[test]
    fn test_filter_array_of_objects() {
        let input = json!({
            "items": [
                {"id": 1, "_secret": "a"},
                {"id": 2, "_secret": "b"},
                {"id": 3, "_secret": "c"}
            ]
        });

        let output = filtered(input);
        let items = output.get("items").unwrap().as_array().unwrap();

        for item in items {
            assert!(item.get("id").is_some());
            assert!(item.get("_secret").is_none());
        }
    }

    #[test]
    fn test_no_private_params() {
        let input = json!({
            "model": "claude-3",
            "messages": [{"role": "user", "content": "hello"}]
        });

        let mut body = input.clone();
        assert!(!strip_private_params(&mut body));

        // 无私有参数时，输出应与输入相同
        assert_eq!(input, body);
    }

    #[test]
    fn test_empty_object() {
        let mut body = json!({});
        assert!(!strip_private_params(&mut body));
        assert_eq!(body, json!({}));
    }

    #[test]
    fn test_primitive_values() {
        // 原始值不应被修改
        for mut value in [json!(42), json!("string"), json!(true), json!(null)] {
            assert!(!strip_private_params(&mut value));
            assert!(!contains_private_params(&value));
        }
    }

    #[test]
    fn test_contains_private_params_matches_strip() {
        let with_private = json!({ "a": { "b": ["ok", { "_x": 1 }] } });
        let without_private = json!({ "a": { "b": ["ok", { "x": 1 }] } });

        assert!(contains_private_params(&with_private));
        assert!(!contains_private_params(&without_private));

        let mut body = with_private;
        assert!(strip_private_params(&mut body));
    }

    #[test]
    fn test_preserves_json_schema_property_names_with_underscore() {
        let input = json!({
            "tools": [
                {
                    "name": "lookup",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "_id": {"type": "string", "_internal_note": "remove"},
                            "_meta": {"type": "object"}
                        },
                        "_private_schema_note": "remove"
                    }
                }
            ]
        });

        let mut body = input;
        strip_private_params(&mut body);
        let schema = &body["tools"][0]["input_schema"];

        assert!(schema["properties"].get("_id").is_some());
        assert!(schema["properties"].get("_meta").is_some());
        assert!(schema["properties"]["_id"].get("_internal_note").is_none());
        assert!(schema.get("_private_schema_note").is_none());
    }

    #[test]
    fn test_schema_property_names_do_not_force_a_strip() {
        // `properties` 里的 `_x` 是合法字段名：只读扫描不能误报，
        // 否则借用中的请求体会被无谓拷贝一份。
        let body = json!({
            "tools": [{
                "input_schema": { "properties": { "_id": { "type": "string" } } }
            }]
        });

        assert!(!contains_private_params(&body));
        let mut body = body;
        assert!(!strip_private_params(&mut body));
    }
}
