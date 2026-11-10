//! Stable JSON helpers for cache-sensitive request bodies.

use crate::hash::short_sha256_hex;
use smallvec::SmallVec;
use std::fmt::Write as _;

use serde_json::Value;

/// 需要转义的字节表：`"`、`\` 与 `0x00..=0x1F` 的控制字符，其余（含 DEL 与非
/// ASCII）原样输出。与 `serde_json` 的转义规则逐字节一致，见
/// [`write_json_string_matches_serde_json_escaping`] 的逐字符对照测试。
const NEEDS_ESCAPE: [bool; 256] = {
    let mut table = [false; 256];
    let mut byte = 0usize;
    while byte < 256 {
        table[byte] = byte < 0x20 || byte == b'"' as usize || byte == b'\\' as usize;
        byte += 1;
    }
    table
};

/// 把 `value` 按 JSON 字符串规则追加进 `out`（含两端引号），全程不产生中间 String。
///
/// 旧实现是 `serde_json::to_string(value)` 再 `push_str`：字符串和对象键每个都要
/// 一次堆分配加一次整段拷贝。这段在请求体规范化里每个字符串、每个键都要过一次——
/// 一个 422 KB 的 Claude 请求体有上千个字符串，于是分配次数跟着文档里的字符串
/// 数量线性增长。转义直接写进目标缓冲后，分配次数与字符串数量脱钩。
///
/// 只扫一遍：不需要转义的字节什么都不做，转义点之间的大段按 `&str` 整段 push，
/// 遇不到转义时等于一次 `push_str` 加一次首尾引号。
#[inline]
fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    let bytes = value.as_bytes();
    let mut plain_start = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        if !NEEDS_ESCAPE[byte as usize] {
            continue;
        }
        out.push_str(&value[plain_start..index]);
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            _ => {
                // 其余控制字符：serde_json 的 `\\u00xx` 小写十六进制。
                let _ = write!(out, "\\u{byte:04x}");
            }
        }
        plain_start = index + 1;
    }
    out.push_str(&value[plain_start..]);
    out.push('"');
}

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
        Value::String(value) => write_json_string(out, value),
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
            // 键表按小对象落在内联存储里：JSON 对象普遍只有几个键，以前每个对象
            // 都要为这张临时表分配一次 Vec。
            let mut entries: SmallVec<[(&str, &Value); 8]> = map
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect();
            entries.sort_unstable_by_key(|(key, _)| *key);

            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_string(out, key);
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

    /// 转义必须与 `serde_json` 逐字节一致：规范化后的字符串就是上游收到的字节。
    ///
    /// 语料覆盖短形式转义（`\b` `\t` `\n` `\f` `\r`）、`\"` `\\`、其余控制字符的
    /// `\u00xx`、DEL（不转义）、非 ASCII（不转义）、以及长串和密集转义。
    #[test]
    fn write_json_string_matches_serde_json_escaping() {
        let mut corpus = vec![
            String::new(),
            "plain ascii".to_string(),
            "quote\" inside".to_string(),
            "back\\slash".to_string(),
            "both \" and \\".to_string(),
            "tab\t nl\n cr\r".to_string(),
            "form\u{000c}feed back\u{0008}space".to_string(),
            "del\u{007f} stays".to_string(),
            "中文与表情 🎀 原样".to_string(),
            "control \u{0001}\u{001f}\u{000b}\u{001b}".to_string(),
            "a".repeat(2048),
            "\"\\\t\n".repeat(512),
        ];
        // 每个控制字符码位单独过一遍，一个都不能漏。
        for byte in 0u8..0x20 {
            corpus.push(format!("before{byte}after"));
        }

        for text in &corpus {
            let scalar = Value::String(text.clone());
            assert_eq!(
                canonical_json_string(&scalar),
                serde_json::to_string(&scalar).expect("serialize a string"),
                "string escaping diverged for {text:?}"
            );

            // 键走同一条转义路径；单键对象不涉及排序，可直接对照。
            let mut map = serde_json::Map::new();
            map.insert(text.clone(), Value::from(1));
            let mut expected = String::from("{");
            expected.push_str(&serde_json::to_string(text).expect("serialize a key"));
            expected.push_str(":1}");
            assert_eq!(
                canonical_json_string(&Value::Object(map)),
                expected,
                "key escaping diverged for {text:?}"
            );
        }
    }

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

/// 分配计数探针。
///
/// 「规范化一次请求体要做多少次堆分配」是这段代码的核心成本，回归测试要能看见
/// 它，而不是只断言输出相等——输出相等恰恰是逐个字符串分配也能做到的事。
/// 线程本地计数：测试各自跑在自己的线程上，并行互不干扰。
#[cfg(test)]
mod alloc_probe {
    use super::*;
    use serde_json::{json, Value};
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    std::thread_local! {
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    #[global_allocator]
    static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

    /// 一段带 `strings` 个字符串、每四个字符串一个对象的请求体。
    fn body(strings: usize) -> Value {
        let mut messages = Vec::with_capacity(strings / 4);
        for index in 0..strings / 4 {
            messages.push(json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": format!("turn {index}: quoted text with a backslash")},
                    {"type": "text", "text": format!("second paragraph number {index}")},
                    {"type": "text", "text": format!("third paragraph number {index}")},
                    {"type": "text", "text": format!("fourth paragraph number {index}")},
                ],
            }));
        }
        json!({"model": "m", "messages": messages})
    }

    fn allocations_for(value: &Value) -> usize {
        ALLOCATIONS.with(|count| count.set(0));
        let canonical = canonical_json_string(value);
        let measured = ALLOCATIONS.with(|count| count.get());
        // 输出本身要用到，否则整段调用可能被优化掉。
        assert!(canonical.len() > 64);
        measured
    }

    #[test]
    fn canonicalizing_does_not_allocate_per_string_or_key() {
        let small = body(8);
        let large = body(160);
        let small_cost = allocations_for(&small);
        let large_cost = allocations_for(&large);

        // 字符串和键多了 20 倍，分配次数只该跟着输出缓冲的翻倍增长走（对数级）。
        // 旧的「每个字符串一次 to_string」实现下，这一项会跟着字符串数线性涨。
        // 输出缓冲从零开始翻倍增长：两组的输出长度差约 20 倍，翻倍次数只差
        // log2(20) ≈ 4.3 次。断言留了余量，但仍远低于旧实现的线性增长。
        assert!(
            large_cost <= small_cost + 8,
            "分配次数随字符串数量线性增长: {small_cost} -> {large_cost}"
        );
    }
}
