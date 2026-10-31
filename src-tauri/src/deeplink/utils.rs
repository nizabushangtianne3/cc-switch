//! Deep link utility functions
//!
//! Common helpers for URL validation, Base64 decoding, etc.

use crate::error::AppError;
use base64::prelude::*;
use toml_edit::{Item, Value};
use url::Url;

/// 取 TOML 条目的非空字符串：去掉首尾空白，空串按无值处理。
///
/// `codex_link` 与 `grok_link` 都用它读链接附带的 `config.toml` 字段，
/// 两份逐字相同的私有拷贝已删。
pub fn non_empty_str(item: Option<&Item>) -> Option<String> {
    item.and_then(Item::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 去掉值的注释和空白装饰。
pub fn undecorated_value(value: &Value) -> Value {
    let mut value = value.clone();
    value.decor_mut().clear();
    value
}

/// 去掉原文的注释和空白；子表保留成子表（`[model_providers.custom.http_headers]`）。
///
/// `codex_link` 与 `grok_link` 都用它规范化链接附带的 `config.toml`，两份逐字
/// 相同的私有拷贝已删。
pub fn undecorated(item: &Item) -> Item {
    match item {
        Item::Value(value) => Item::Value(undecorated_value(value)),
        Item::Table(table) => {
            let mut table = table.clone();
            table.decor_mut().clear();
            table.set_implicit(false);
            for (_, value) in table.iter_mut() {
                if let Some(value) = value.as_value_mut() {
                    value.decor_mut().clear();
                }
            }
            Item::Table(table)
        }
        other => other.clone(),
    }
}

/// Validate that a string is a valid HTTP(S) URL
pub fn validate_url(url_str: &str, field_name: &str) -> Result<(), AppError> {
    let url = Url::parse(url_str)
        .map_err(|e| AppError::InvalidInput(format!("Invalid URL for '{field_name}': {e}")))?;

    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(AppError::InvalidInput(format!(
            "Invalid URL scheme for '{field_name}': must be http or https, got '{scheme}'"
        )));
    }

    Ok(())
}

/// Decode a Base64 parameter from deep link URL
///
/// This function handles common issues with Base64 in URLs:
/// - `+` being decoded as space
/// - Missing padding `=`
/// - Both standard and URL-safe Base64 variants
pub fn decode_base64_param(field: &str, raw: &str) -> Result<Vec<u8>, AppError> {
    let mut candidates: Vec<String> = Vec::new();
    // Keep spaces (to restore `+`), but remove newlines
    let trimmed = raw.trim_matches(|c| c == '\r' || c == '\n');

    // First try restoring spaces to "+"
    if trimmed.contains(' ') {
        let replaced = trimmed.replace(' ', "+");
        if !replaced.is_empty() && !candidates.contains(&replaced) {
            candidates.push(replaced);
        }
    }

    // Original value
    if !trimmed.is_empty() && !candidates.contains(&trimmed.to_string()) {
        candidates.push(trimmed.to_string());
    }

    // Add padding variants
    let existing = candidates.clone();
    for candidate in existing {
        let mut padded = candidate.clone();
        let remainder = padded.len() % 4;
        if remainder != 0 {
            padded.extend(std::iter::repeat_n('=', 4 - remainder));
        }
        if !candidates.contains(&padded) {
            candidates.push(padded);
        }
    }

    let mut last_error: Option<String> = None;
    for candidate in candidates {
        for engine in [
            &BASE64_STANDARD,
            &BASE64_STANDARD_NO_PAD,
            &BASE64_URL_SAFE,
            &BASE64_URL_SAFE_NO_PAD,
        ] {
            match engine.decode(&candidate) {
                Ok(bytes) => return Ok(bytes),
                Err(err) => last_error = Some(err.to_string()),
            }
        }
    }

    Err(AppError::InvalidInput(format!(
        "{field} 参数 Base64 解码失败：{}。请确认链接参数已用 Base64 编码并经过 URL 转义（尤其是将 '+' 编码为 %2B，或使用 URL-safe Base64）。",
        last_error.unwrap_or_else(|| "未知错误".to_string())
    )))
}

/// Infer homepage URL from API endpoint
///
/// Examples:
/// - https://api.anthropic.com/v1 → https://anthropic.com
/// - https://api.openai.com/v1 → https://openai.com
/// - https://api-test.company.com/v1 → https://test.company.com
pub fn infer_homepage_from_endpoint(endpoint: &str) -> Option<String> {
    let url = Url::parse(endpoint).ok()?;
    let host = url.host_str()?;

    // Remove common API prefixes
    let clean_host = host
        .strip_prefix("api.")
        .or_else(|| host.strip_prefix("api-"))
        .unwrap_or(host);

    Some(format!("https://{clean_host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> toml_edit::DocumentMut {
        text.parse::<toml_edit::DocumentMut>()
            .expect("test TOML must parse")
    }

    #[test]
    fn non_empty_str_reads_trimmed_strings_only() {
        let table = doc(
            "keep = \"  relay  \"\nblank = \"   \"\nnumeric = 7\narr = [1]\n[t]\nnested = \"n\"\n",
        );
        assert_eq!(non_empty_str(table.get("keep")).as_deref(), Some("relay"));
        assert_eq!(non_empty_str(table.get("blank")), None);
        assert_eq!(non_empty_str(table.get("numeric")), None);
        assert_eq!(non_empty_str(table.get("arr")), None);
        assert_eq!(non_empty_str(table.get("t")), None);
        assert_eq!(non_empty_str(None), None);
    }

    #[test]
    fn undecorated_value_strips_the_comment_suffix() {
        let doc = doc("key = 1  # trailing comment\n");
        let item = doc.get("key").unwrap();
        assert!(
            item.to_string().contains('#'),
            "fixture must carry a comment"
        );
        assert_eq!(undecorated(item).to_string(), "1");
    }

    #[test]
    fn undecorated_table_normalizes_decor_and_keeps_nested_tables() {
        let doc = doc("[outer]  # header note\nfirst = 1  # first\nsecond = \"x\"  # second\n\n[outer.inner]\ndeep = 2  # deep\n");
        let item = doc.get("outer").unwrap();
        // Table 的 Display 只渲染直接值行：值上的注释装饰一并归零。
        assert_eq!(undecorated(item).to_string(), "first = 1\nsecond = \"x\"\n");
        // 子表作为子树保留，由调用方按层级自行清理（codex/grok 链接解析即此用法）。
        let inner = item.get("inner").unwrap().to_string();
        assert!(inner.contains("deep = 2  # deep"), "{inner}");
    }

    #[test]
    fn undecorated_marks_an_implicit_intermediate_table_explicit() {
        let doc = doc("[a.b]\nk = 1\n");
        let cleaned = undecorated(doc.get("a").unwrap());
        let mut rendered = toml_edit::DocumentMut::new();
        rendered.insert("a", cleaned);
        let text = rendered.to_string();
        assert!(text.contains("[a]"), "{text}");
        assert!(text.contains("[a.b]"), "{text}");
        assert!(text.contains("k = 1"), "{text}");
    }

    #[test]
    fn undecorated_leaves_non_table_items_alone() {
        assert!(undecorated(&Item::None).to_string().is_empty());
    }

    #[test]
    fn validate_url_accepts_http_and_rejects_other_schemes() {
        validate_url("http://127.0.0.1:8080", "base_url").unwrap();
        validate_url("https://api.example.com/v1", "base_url").unwrap();
        for bad in [
            "ftp://api.example.com",
            "api.example.com",
            "",
            "ws://api.example.com",
        ] {
            let error = validate_url(bad, "base_url").unwrap_err().to_string();
            assert!(error.contains("base_url"), "{bad}: {error}");
        }
        let scheme_error = validate_url("ftp://api.example.com", "base_url")
            .unwrap_err()
            .to_string();
        assert!(
            scheme_error.contains("must be http or https"),
            "{scheme_error}"
        );
    }

    #[test]
    fn decode_base64_param_restores_plus_pads_and_accepts_url_safe() {
        let raw = BASE64_STANDARD.encode([251u8, 255]);
        let bytes = vec![251u8, 255];
        // '+' 被 URL 解码成空格：先补回再解。
        assert_eq!(
            decode_base64_param("cfg", &raw.replace('+', " ")).unwrap(),
            bytes
        );
        assert_eq!(decode_base64_param("cfg", &raw).unwrap(), bytes);
        let url_safe = raw.replace('+', "-").replace('/', "_");
        assert_eq!(decode_base64_param("cfg", &url_safe).unwrap(), bytes);
        // 缺 '=' 的输入按 4 的倍数补尾。
        assert_eq!(
            decode_base64_param("cfg", "aGVsbG8").unwrap(),
            b"hello".to_vec()
        );
        // URL 传输留下的换行只切首尾。
        assert_eq!(
            decode_base64_param("cfg", "\r\naGVsbG8=\r\n").unwrap(),
            b"hello".to_vec()
        );
    }

    #[test]
    fn decode_base64_param_reports_the_field_and_the_cause() {
        let error = decode_base64_param("cfg", "!!!!").unwrap_err().to_string();
        assert!(
            error.starts_with("无效输入: cfg 参数 Base64 解码失败"),
            "{error}"
        );
        assert!(
            error.contains("Invalid symbol"),
            "the base64 cause must survive: {error}"
        );

        // 全是换行的输入连候选都拼不出来，走“未知错误”兜底。
        let error = decode_base64_param("cfg", "\r\n\r\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("未知错误"), "{error}");
    }

    #[test]
    fn infer_homepage_from_endpoint_strips_api_prefixes() {
        assert_eq!(
            infer_homepage_from_endpoint("https://api.anthropic.com/v1").as_deref(),
            Some("https://anthropic.com")
        );
        // "api-" 前缀剥掉后剩下的整体就是主机名。
        assert_eq!(
            infer_homepage_from_endpoint("https://api-test.company.com/v1").as_deref(),
            Some("https://test.company.com")
        );
        assert_eq!(
            infer_homepage_from_endpoint("https://relay.example.com").as_deref(),
            Some("https://relay.example.com")
        );
        assert_eq!(infer_homepage_from_endpoint("not a url"), None);
        assert_eq!(infer_homepage_from_endpoint(""), None);
    }
}
