//! Stable JSON helpers for cache-sensitive request bodies.

use crate::hash::short_sha256_hex;
use std::fmt::Write as _;

use serde_json::Value;

pub(crate) fn canonical_json_string(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

/// 把 value 的键序稳定形式直接写进 `out`，中间节点不建任何临时字符串。
///
/// 旧实现在每个对象/数组上都要 collect 出 Vec、逐项 `format!`、`join`、再
/// `format!` 包一层，等于把整棵子树逐层拷三遍；深层嵌套的请求体上这段拷贝
/// 才是主要开销，而不是分配次数本身。
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            let _ = write!(out, "{value}");
        }
        Value::String(value) => out.push_str(
            &serde_json::to_string(value)
                .expect("serializing a JSON string for canonical output should not fail"),
        ),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(value, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // preserve_order 之下迭代序是插入序，必须显式按键排序，
            // 同一份内容才能得到同一串字节。
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);

            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(key).expect(
                        "serializing a JSON object key for canonical output should not fail",
                    ),
                );
                out.push(':');
                write_canonical(value, out);
            }
            out.push('}');
        }
    }
}

pub(crate) fn canonicalize_json_string_if_parseable(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }

    serde_json::from_str::<Value>(trimmed)
        .map(|parsed| canonical_json_string(&parsed))
        .unwrap_or_else(|_| value.to_string())
}

/// Normalize a tool-call `arguments` string into a valid JSON payload.
///
/// Identical to [`canonicalize_json_string_if_parseable`] except that an empty
/// (or whitespace-only) value is coerced to `"{}"` instead of being passed
/// through verbatim. A no-argument tool call must serialize as `"{}"`; strict
/// upstreams such as Minimax reject `arguments: ""` with a 400
/// `invalid function arguments json string` error, whereas lenient ones
/// (OpenAI, Kimi) silently treat it as an empty object.
pub(crate) fn canonicalize_tool_arguments_str(value: &str) -> String {
    if value.trim().is_empty() {
        return "{}".to_string();
    }
    canonicalize_json_string_if_parseable(value)
}

/// Normalize a tool-call `arguments` field from a Responses/Chat item.
///
/// Mirrors the inline `match` that several transform paths used to duplicate:
/// a string is canonicalized (with empty coerced to `"{}"`), a structured
/// value is serialized canonically, and a missing field defaults to `"{}"`.
pub(crate) fn canonicalize_tool_arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => canonicalize_tool_arguments_str(s),
        Some(v) => canonical_json_string(v),
        None => "{}".to_string(),
    }
}

pub(crate) fn short_value_hash(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "absent".to_string();
    };
    short_sha256_hex(canonical_json_string(value).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_string_sorts_nested_object_keys() {
        let left = json!({
            "b": 2,
            "a": {
                "d": true,
                "c": [3, {"z": 1, "y": 2}]
            }
        });
        let right = json!({
            "a": {
                "c": [3, {"y": 2, "z": 1}],
                "d": true
            },
            "b": 2
        });

        assert_eq!(canonical_json_string(&left), canonical_json_string(&right));
        assert_eq!(
            short_value_hash(Some(&left)),
            short_value_hash(Some(&right))
        );
    }

    #[test]
    fn canonicalize_json_string_if_parseable_sorts_keys_and_removes_whitespace() {
        assert_eq!(
            canonicalize_json_string_if_parseable(r#"{ "b": 2, "a": 1 }"#),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn canonical_json_string_pins_the_exact_wire_format() {
        // 无空格、键序稳定、空容器不留缝隙；字符串按 JSON 规则转义。
        assert_eq!(canonical_json_string(&json!({})), "{}");
        assert_eq!(canonical_json_string(&json!([])), "[]");
        assert_eq!(canonical_json_string(&Value::Null), "null");
        assert_eq!(canonical_json_string(&json!(true)), "true");
        assert_eq!(canonical_json_string(&json!(12)), "12");
        assert_eq!(canonical_json_string(&json!(1.5)), "1.5");
        assert_eq!(canonical_json_string(&json!("plain")), "\"plain\"");
        assert_eq!(
            canonical_json_string(&json!({"a\"b": "x\ny", "c": null})),
            "{\"a\\\"b\":\"x\\ny\",\"c\":null}"
        );
        assert_eq!(
            canonical_json_string(&json!({
                "outer": {"z": 1, "a": [3, {"y": 2, "x": 1}], "m": []},
                "inner": []
            })),
            "{\"inner\":[],\"outer\":{\"a\":[3,{\"x\":1,\"y\":2}],\"m\":[],\"z\":1}}"
        );
    }

    #[test]
    fn canonicalize_json_string_if_parseable_preserves_plain_text() {
        assert_eq!(
            canonicalize_json_string_if_parseable("plain text"),
            "plain text"
        );
    }

    #[test]
    fn canonicalize_tool_arguments_str_coerces_empty_to_object() {
        assert_eq!(canonicalize_tool_arguments_str(""), "{}");
        assert_eq!(canonicalize_tool_arguments_str("   "), "{}");
        assert_eq!(canonicalize_tool_arguments_str("\n\t"), "{}");
    }

    #[test]
    fn canonicalize_tool_arguments_str_canonicalizes_valid_json() {
        assert_eq!(
            canonicalize_tool_arguments_str(r#"{ "b": 2, "a": 1 }"#),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn canonicalize_tool_arguments_handles_field_variants() {
        // Missing field -> empty object.
        assert_eq!(canonicalize_tool_arguments(None), "{}");
        // Empty string field -> empty object.
        assert_eq!(canonicalize_tool_arguments(Some(&json!(""))), "{}");
        // String field with JSON -> canonicalized.
        assert_eq!(
            canonicalize_tool_arguments(Some(&json!(r#"{"b":2,"a":1}"#))),
            r#"{"a":1,"b":2}"#
        );
        // Structured (non-string) field -> canonical serialization.
        assert_eq!(
            canonicalize_tool_arguments(Some(&json!({"b": 2, "a": 1}))),
            r#"{"a":1,"b":2}"#
        );
    }
}
