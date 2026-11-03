//! Provider Adapter Trait
//!
//! 定义供应商适配器的统一接口，抽象不同上游供应商的处理逻辑。

use super::auth::AuthInfo;
use crate::provider::Provider;
use crate::proxy::error::ProxyError;
use serde_json::Value;

/// 供应商适配器 Trait
///
/// 所有供应商适配器都需要实现此 trait，提供统一的接口来处理：
/// - URL 构建
/// - 认证信息提取和头部注入
/// - 请求/响应格式转换（可选）
pub trait ProviderAdapter: Send + Sync {
    /// 适配器名称（用于日志和调试）
    fn name(&self) -> &'static str;

    /// 从 Provider 配置中提取 base_url
    fn extract_base_url(&self, provider: &Provider) -> Result<String, ProxyError>;

    /// 从 Provider 配置中提取认证信息
    fn extract_auth(&self, provider: &Provider) -> Option<AuthInfo>;

    /// 构建请求 URL
    fn build_url(&self, base_url: &str, endpoint: &str) -> String;

    /// Return auth headers as `(name, value)` pairs.
    ///
    /// The forwarder inserts these at the position of the original auth header
    /// so that header order is preserved.
    ///
    /// Returns `ProxyError::AuthError` when the credential contains characters
    /// that cannot be encoded as an HTTP header value (e.g. control chars,
    /// CR/LF), which would otherwise panic inside `HeaderValue::from_str`.
    fn get_auth_headers(
        &self,
        auth: &AuthInfo,
    ) -> Result<Vec<(http::HeaderName, http::HeaderValue)>, ProxyError>;

    /// 是否需要格式转换
    fn needs_transform(&self, _provider: &Provider) -> bool {
        false
    }

    /// 转换请求体
    fn transform_request(&self, body: Value, _provider: &Provider) -> Result<Value, ProxyError> {
        Ok(body)
    }
}

/// Build an HTTP `HeaderValue` from a credential / token string.
///
/// Returns `ProxyError::AuthError` when the string contains characters that
/// cannot live in an HTTP header value: control bytes below 32, DEL (127) and
/// CR/LF (a header would end there). Bytes 128..=255 pass through as opaque
/// values, which is what `http::HeaderValue` permits.
/// Adapters call this for every header value derived from user-pasted
/// material so a malformed key surfaces as a 401 instead of panicking
/// the worker via `HeaderValue::from_str(...).unwrap()`.
pub fn auth_header_value(s: &str) -> Result<http::HeaderValue, ProxyError> {
    http::HeaderValue::from_str(s)
        .map_err(|e| ProxyError::AuthError(format!("invalid auth header value: {e}")))
}

/// Gemini CLI OAuth 的请求头：Bearer token 加上客户端标识。
///
/// Claude 适配器的 `gemini_native` 格式与 Gemini 适配器共用这一套，两份逐字相同。
/// `token` 取 `access_token`，缺省时退回 `api_key`（上游把 OAuth token 放在
/// `api_key` 字段的配置）。每个请求都会构造一次。
pub fn google_oauth_headers(
    auth: &AuthInfo,
) -> Result<Vec<(http::HeaderName, http::HeaderValue)>, ProxyError> {
    let token = auth.access_token.as_ref().unwrap_or(&auth.api_key);
    Ok(vec![
        (
            http::HeaderName::from_static("authorization"),
            auth_header_value(&format!("Bearer {token}"))?,
        ),
        (
            http::HeaderName::from_static("x-goog-api-client"),
            http::HeaderValue::from_static("GeminiCLI/1.0"),
        ),
    ])
}

#[cfg(test)]
mod tests {
    // 测试模块的 `super` 是 adapter，不是 providers：auth 要往上两级找。
    use super::*;
    use crate::proxy::providers::auth::AuthStrategy;

    #[test]
    fn auth_header_value_accepts_a_plain_token() {
        let value = auth_header_value("sk-abc123").expect("a plain token is a valid header");
        assert_eq!(value.to_str().unwrap(), "sk-abc123");
    }

    #[test]
    fn auth_header_value_rejects_values_that_would_split_the_header() {
        // CR/LF 是头部结束符：放过它就是响应注入。
        for raw in [
            "sk-abc\r\nX-Evil: 1",
            "sk-abc\nX-Evil: 1",
            "sk-abc\rX-Evil: 1",
        ] {
            assert!(auth_header_value(raw).is_err(), "{raw:?} must not pass");
        }
        // 控制字符同理。
        assert!(auth_header_value("sk-abc\u{0}").is_err());
        assert!(auth_header_value("sk-abc\u{7f}").is_err());
        // `http::HeaderValue` accepts bytes 128..=255 as opaque values, so a
        // non-ASCII key passes: the upstream decides whether it works.
        let opaque = auth_header_value("sk-秘钥").expect("opaque bytes are valid");
        assert_eq!(opaque.as_bytes(), "sk-秘钥".as_bytes());
    }

    #[test]
    fn google_oauth_headers_prefers_the_access_token_and_falls_back_to_the_key() {
        let with_token =
            AuthInfo::with_access_token("sk-fallback".to_string(), "ya29-access".to_string());
        let headers = google_oauth_headers(&with_token).expect("headers");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0.as_str(), "authorization");
        assert_eq!(headers[0].1.to_str().unwrap(), "Bearer ya29-access");
        assert_eq!(headers[1].0.as_str(), "x-goog-api-client");
        assert_eq!(headers[1].1.to_str().unwrap(), "GeminiCLI/1.0");

        // 没有 access_token 时退回 api_key（上游把 OAuth token 放在 api_key 的配置）。
        let fallback = AuthInfo::new("sk-fallback".to_string(), AuthStrategy::GoogleOAuth);
        let headers = google_oauth_headers(&fallback).expect("headers");
        assert_eq!(headers[0].1.to_str().unwrap(), "Bearer sk-fallback");
    }

    #[test]
    fn google_oauth_headers_rejects_a_token_that_cannot_be_a_header() {
        let broken =
            AuthInfo::with_access_token("sk-fallback".to_string(), "ya29\r\nX-Evil: 1".to_string());
        assert!(google_oauth_headers(&broken).is_err());
    }
}
