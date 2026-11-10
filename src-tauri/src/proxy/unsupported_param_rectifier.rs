//! 上游以 400 拒绝「不支持的参数」时的去参重试与模型能力缓存。
//!
//! 第三方模型对请求参数的支持度参差（`reasoning_effort`、`parallel_tool_calls`、
//! `tool_choice`…），客户端按通用形态发过来的请求会被整条 400 拒掉，客户端看到
//! 400 就中止会话，后续同会话请求也全废。这里在代理层消化它：
//!
//! 1. 400 响应体里解析出被点名的参数（顶层或 `a.b` 嵌套路径）；
//! 2. 从请求体删掉这些键，对同一家重试一次——客户端拿到的是 200，会话不中断；
//! 3. 把「这家这个模型不支持这些参数」记进能力缓存，后续请求（同会话或新会话）
//!    发送前直接剥掉，零报错。
//!
//! 解析保守为先：只认明确点名参数的措辞，解析不到就放弃整流，不猜。

use std::collections::HashMap;
use std::collections::HashSet;

use serde_json::Value;

/// 请求语义核心键：被上游误报时也不能删，删了请求就废了。
const PROTECTED_KEYS: &[&str] = &["model", "messages", "stream", "input"];

/// provider_id → model → 该模型不支持的参数路径集合。和 settings 里的
/// `unsupported_params_by_model` 同形；读取用 `&str` 直接查，热路径零分配。
pub(crate) type UnsupportedParamCache =
    std::sync::RwLock<HashMap<String, HashMap<String, HashSet<String>>>>;

/// 从上游 400 响应体提取被点名「不支持」的参数路径。
///
/// 覆盖 OpenAI 结构化 `error.param` 与常见措辞（`Unsupported parameter: 'x'`、
/// `does not support the 'x' parameter`、`unknown field x`、中文「不支持的参数」）。
/// 返回值已是去重、过滤后的合法键路径列表；空列表表示解析不到，调用方应放弃。
pub(crate) fn parse_unsupported_params(body: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();

    if let Ok(json) = serde_json::from_str::<Value>(body) {
        for pointer in ["error.param", "error.metadata.param"] {
            let mut node = &json;
            let mut found = true;
            for segment in pointer.split('.') {
                match node.get(segment) {
                    Some(next) => node = next,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            if found {
                if let Some(name) = node.as_str() {
                    push_param(&mut paths, name);
                }
            }
        }
        let message = json
            .pointer("/error/message")
            .or_else(|| json.pointer("/message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        parse_message(&mut paths, message);
        // 兜底：结构化字段和措辞模式都没拿到时，扫描消息里被引号包住的「像参数
        // 名」的 token。网关点名参数还有一种形态——名字在措辞前面，如
        // `"thinking" is not supported for this model...`（Anthropic 系的流内
        // error 事件就这样，且不带结构化 param 字段），上面的模式只看得见
        // 措辞后面的词。合法性仍由 push_param 把关（点分 ident、非核心键）。
        //
        // 只对 JSON 错误信封、且上游明说「不支持」的措辞兜底：参数名可能在
        // 措辞前面（`"thinking" is not supported for this model`），上面的模式
        // 只看得见后面的词。没有「不支持」措辞的错误（Codex 的签名诊断
        // `Invalid \`signature\` in \`thinking\` block` 等）不兜底——那里被引号
        // 包住的词经常不是参数名，收了会抢走别的整流器。
        if paths.is_empty() && message_indicates_unsupported(message) {
            collect_quoted_params(message, &mut paths);
        }
    } else {
        parse_message(&mut paths, body);
    }

    paths
}

/// 上游明说「不支持」的措辞：引号兜底只在这类消息里跑。
fn message_indicates_unsupported(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("not support") || lower.contains("unsupported")
}

/// 扫描文本里引号（单引号 / 双引号 / 反引号）包住的 token，收下合法参数路径。
fn collect_quoted_params(text: &str, paths: &mut Vec<String>) {
    let mut index = 0;
    while index < text.len() {
        let quote = text.as_bytes()[index];
        if quote == b'\'' || quote == b'"' || quote == b'`' {
            if let Some(offset) = text[index + 1..].find(quote as char) {
                push_param(paths, &text[index + 1..index + 1 + offset]);
                index += offset + 2;
                continue;
            }
        }
        index += 1;
    }
}

/// 措辞模式：引号或反引号点名的参数名。宁少勿猜。
fn parse_message(paths: &mut Vec<String>, message: &str) {
    const PATTERNS: &[&str] = &[
        "Unsupported parameter",
        "Unsupported value for",
        "Invalid parameter",
        "Unknown parameter",
        "unknown field",
        "not support parameter",
        "does not support",
        "unsupported parameter",
        "unsupported value for",
        "invalid parameter",
        "不支持的参数",
        "不支持参数",
    ];

    for pattern in PATTERNS {
        // 大小写不敏感：各家措辞首字母大小写不一（Unknown / unknown field）
        let needle = pattern.to_lowercase();
        let lower = message.to_lowercase();
        let mut rest: &str = &lower;
        while let Some(index) = rest.find(&needle) {
            let after = &rest[index + needle.len()..];
            let after = after.trim_start_matches([':', '：']);
            let after = after.trim_start();
            let after = after
                .strip_prefix("the ")
                .or_else(|| after.strip_prefix("parameter "))
                .unwrap_or(after);
            if let Some(name) = leading_quoted(after) {
                push_param(paths, &name);
            } else if let Some(name) = leading_param_word(after) {
                // 无引号形式（`Unknown field tool_choice in body`）：只收含下划线
                // 或命中常见参数名的 token，别的词不猜。
                push_param(paths, &name);
            }
            rest = &rest[index + needle.len()..];
        }
    }
}

/// 无引号形式里句首那个「像参数名」的词：含下划线，或命中常见请求参数清单。
fn leading_param_word(text: &str) -> Option<String> {
    const KNOWN: &[&str] = &[
        "temperature",
        "top_p",
        "top_k",
        "max_tokens",
        "max_completion_tokens",
        "tool_choice",
        "parallel_tool_calls",
        "reasoning_effort",
        "reasoning",
        "response_format",
        "metadata",
        "user",
        "stream_options",
        "prediction",
        "service_tier",
        "truncation",
        "include",
        "instructions",
        "store",
        "n",
        "stop",
    ];
    let word: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    if word.is_empty() {
        return None;
    }
    if word.contains('_') || KNOWN.contains(&word.as_str()) {
        Some(word)
    } else {
        None
    }
}

/// 取句首引号（单引号、双引号、反引号）包住的键名。
fn leading_quoted(text: &str) -> Option<String> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' && quote != '`' {
        return None;
    }
    let end = text[1..].find(quote)? + 1;
    Some(text[1..end].to_string())
}

/// 合法的点分键路径（`reasoning.effort` 也接受），且不是受保护的核心键。
fn push_param(paths: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty() || !is_param_path(name) {
        return;
    }
    let root = name.split('.').next().unwrap_or_default();
    if PROTECTED_KEYS.contains(&root) {
        return;
    }
    if !paths.iter().any(|existing| existing == name) {
        paths.push(name.to_string());
    }
}

fn is_param_path(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_ident)
}

fn is_ident(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 从请求体删除这些点分路径指向的键，父节点被删空时一并清掉。
/// 返回是否真的删过东西。
pub(crate) fn strip_request_params(body: &mut Value, paths: &[String]) -> bool {
    let mut stripped = false;
    for path in paths {
        let segments: Vec<&str> = path.split('.').collect();
        if strip_path(body, &segments) {
            stripped = true;
        }
    }
    stripped
}

fn strip_path(node: &mut Value, segments: &[&str]) -> bool {
    let (first, rest) = match segments.split_first() {
        Some(split) => split,
        None => return false,
    };
    match node {
        Value::Object(map) => {
            if rest.is_empty() {
                return map.remove(*first).is_some();
            }
            match map.get_mut(*first) {
                Some(child) => {
                    if !strip_path(child, rest) {
                        return false;
                    }
                    // 子节点被删空了：父键也没存在的意义
                    if let Some(Value::Object(inner)) = map.get(*first) {
                        if inner.is_empty() {
                            map.remove(*first);
                        }
                    }
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

/// 从能力缓存取这家这个模型已确认不支持的参数。
pub(crate) fn cached_params(
    cache: &UnsupportedParamCache,
    provider_id: &str,
    model: &str,
) -> Vec<String> {
    let guard = cache.read().unwrap_or_else(|e| e.into_inner());
    // 两层都用 &str 直接查（String: Borrow<str>），常见的不命中路径一个字节都不分配。
    guard
        .get(provider_id)
        .and_then(|by_model| by_model.get(model))
        .map(|paths| {
            let mut list: Vec<String> = paths.iter().cloned().collect();
            list.sort();
            list
        })
        .unwrap_or_default()
}

/// 把这家这个模型不支持的参数并进能力缓存（重复命中幂等）。
pub(crate) fn remember_params(
    cache: &UnsupportedParamCache,
    provider_id: &str,
    model: &str,
    paths: &[String],
) {
    if paths.is_empty() {
        return;
    }
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    let entry = guard
        .entry(provider_id.to_string())
        .or_default()
        .entry(model.to_string())
        .or_default();
    for path in paths {
        entry.insert(path.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_structured_param() {
        let body = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;
        assert_eq!(parse_unsupported_params(body), vec!["max_tokens"]);
    }

    #[test]
    fn parses_unsupported_value_wording() {
        let body = r#"{"error":{"message":"Unsupported value for 'temperature' parameter: only the default is supported"}}"#;
        assert_eq!(parse_unsupported_params(body), vec!["temperature"]);
    }

    #[test]
    fn parses_does_not_support_wording() {
        let body = r#"{"error":{"message":"The model `kimi-k3` does not support the 'reasoning' parameter"}}"#;
        assert_eq!(parse_unsupported_params(body), vec!["reasoning"]);
    }

    #[test]
    fn parses_unknown_field_and_chinese_wordings() {
        let en = r#"{"error":{"message":"Unknown field tool_choice in request body"}}"#;
        assert_eq!(parse_unsupported_params(en), vec!["tool_choice"]);
        let zh = r#"{"error":{"message":"不支持的参数: temperature"}}"#;
        assert_eq!(parse_unsupported_params(zh), vec!["temperature"]);
    }

    #[test]
    fn parses_plain_text_body() {
        let body = "Unsupported parameter: 'top_p' is not supported with this model";
        assert_eq!(parse_unsupported_params(body), vec!["top_p"]);
    }

    #[test]
    fn parses_param_named_before_the_wording() {
        // Anthropic 系流内 error 事件的形态：参数名在措辞前面，且没有结构化
        // param 字段（AMD 实测）。
        let body = r#"{"error":{"type":"invalid_request_error","message":"\"thinking\" is not supported for this model. Remove the \"thinking\" parameter or use a model that supports extended thinking."}}"#;
        assert_eq!(parse_unsupported_params(body), vec!["thinking"]);
        // 带点分的嵌套路径也收（引号兜底对合法点分 ident 一视同仁）
        let nested = r#"{"error":{"message":"\"reasoning.effort\" is not supported"}}"#;
        assert_eq!(parse_unsupported_params(nested), vec!["reasoning.effort"]);
        // 没有「不支持」措辞的消息不兜底：Codex 的签名诊断就是这样，引号里的
        // 词不是参数名，收了会抢走签名整流器
        let signature_like = r#"{"error":{"message":"messages.1.content.0: Invalid `signature` in `thinking` block"}}"#;
        assert!(parse_unsupported_params(signature_like).is_empty());
        // 「不支持」措辞里引号包住的非参数 token（带连字符）不收
        let model_named = r#"{"error":{"message":"model \"kimi-k3\" does not support thinking"}}"#;
        assert!(parse_unsupported_params(model_named).is_empty());
    }

    #[test]
    fn ignores_core_keys_and_unparseable_bodies() {
        // 上游误报核心键也不删
        let body = r#"{"error":{"param":"messages"}}"#;
        assert!(parse_unsupported_params(body).is_empty());
        // 认不出的措辞不猜
        let vague = r#"{"error":{"message":"your request was invalid"}}"#;
        assert!(parse_unsupported_params(vague).is_empty());
    }

    #[test]
    fn strips_top_level_and_nested_paths() {
        let mut body = json!({
            "model": "m",
            "temperature": 0.5,
            "reasoning": { "effort": "high", "summary": "auto" },
        });
        assert!(strip_request_params(
            &mut body,
            &["temperature".to_string(), "reasoning.effort".to_string()],
        ));
        assert!(body.get("temperature").is_none());
        // 只剩 summary：reasoning 不删空
        assert_eq!(body["reasoning"], json!({ "summary": "auto" }));
        // 剥空后父键清掉
        assert!(strip_request_params(
            &mut body,
            &["reasoning.summary".to_string()],
        ));
        assert!(body.get("reasoning").is_none());
        // strip 是底层原语：给什么删什么；核心键保护在 parse 层
        // （ignores_core_keys_and_unparseable_bodies），不进这里。
        assert!(strip_request_params(&mut body, &["model".to_string()],));
        assert!(body.get("model").is_none());
    }

    #[test]
    fn cache_round_trip_and_dedup() {
        let cache: UnsupportedParamCache = std::sync::RwLock::new(HashMap::new());
        remember_params(&cache, "p1", "m1", &["temperature".to_string()]);
        remember_params(
            &cache,
            "p1",
            "m1",
            &["temperature".to_string(), "top_p".to_string()],
        );
        assert_eq!(
            cached_params(&cache, "p1", "m1"),
            vec!["temperature", "top_p"]
        );
        assert!(cached_params(&cache, "p1", "other").is_empty());
        // 剥空集合不写缓存
        remember_params(&cache, "p1", "m2", &[]);
        assert!(cached_params(&cache, "p1", "m2").is_empty());
    }
}
