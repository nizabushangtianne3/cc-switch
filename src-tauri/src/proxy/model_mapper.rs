//! 模型映射模块
//!
//! 在请求转发前，根据 Provider 配置替换请求中的模型名称。
//!
//! 每个请求都会走到这里，所以接口按 Copy-on-write 设计：模型名真的
//! 被改写时才克隆整份请求体，其余情况（没配置映射、映射后不变、
//! 不带 `[1m]` 标记）保持借用，不产生任何拷贝。

use crate::claude_desktop_config::ONE_M_CONTEXT_MARKER;
use crate::provider::Provider;
use serde_json::Value;
use std::borrow::Cow;

/// 模型映射配置
pub struct ModelMapping {
    pub haiku_model: Option<String>,
    pub sonnet_model: Option<String>,
    pub opus_model: Option<String>,
    pub fable_model: Option<String>,
    pub subagent_model: Option<String>,
    pub default_model: Option<String>,
}

impl ModelMapping {
    /// 从 Provider 配置中提取模型映射
    pub fn from_provider(provider: &Provider) -> Self {
        let env = provider.settings_config.get("env");

        Self {
            haiku_model: env
                .and_then(|e| e.get("ANTHROPIC_DEFAULT_HAIKU_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            sonnet_model: env
                .and_then(|e| e.get("ANTHROPIC_DEFAULT_SONNET_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            opus_model: env
                .and_then(|e| e.get("ANTHROPIC_DEFAULT_OPUS_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            fable_model: env
                .and_then(|e| e.get("ANTHROPIC_DEFAULT_FABLE_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            subagent_model: env
                .and_then(|e| e.get("CLAUDE_CODE_SUBAGENT_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            default_model: env
                .and_then(|e| e.get("ANTHROPIC_MODEL"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        }
    }

    /// 检查是否配置了任何模型映射
    pub fn has_mapping(&self) -> bool {
        self.haiku_model.is_some()
            || self.sonnet_model.is_some()
            || self.opus_model.is_some()
            || self.fable_model.is_some()
            || self.subagent_model.is_some()
            || self.default_model.is_some()
    }

    /// 根据原始模型名称获取映射后的模型
    pub fn map_model(&self, original_model: &str) -> String {
        let model_lower = original_model.to_lowercase();

        // 1. 按模型类型匹配
        if model_lower.contains("fable") {
            if let Some(ref m) = self.fable_model {
                return m.clone();
            }
            // 未单独配置 fable 档时归入 opus 档，与 Claude Code 官方
            // 分类器降级方向一致（fable→opus），避免落到 default 失去层级。
            if let Some(ref m) = self.opus_model {
                return m.clone();
            }
        }
        if model_lower.contains("haiku") {
            if let Some(ref m) = self.haiku_model {
                return m.clone();
            }
        }
        if model_lower.contains("opus") {
            if let Some(ref m) = self.opus_model {
                return m.clone();
            }
        }
        if model_lower.contains("sonnet") {
            if let Some(ref m) = self.sonnet_model {
                return m.clone();
            }
        }

        if let Some(ref m) = self.subagent_model {
            if strip_one_m_suffix_for_upstream(original_model) == strip_one_m_suffix_for_upstream(m)
            {
                return original_model.to_string();
            }
        }

        // 2. 默认模型
        if let Some(ref m) = self.default_model {
            return m.clone();
        }

        // 3. 无映射，保持原样
        original_model.to_string()
    }
}

/// 应用模型映射。
///
/// 返回 `Some(映射后的请求体)` 表示模型名真的被改写（此时才克隆整份请求体）；
/// 没配置映射、或映射后模型不变时返回 `None`，调用方继续借用原请求体。
pub fn apply_model_mapping(body: &Value, provider: &Provider) -> Option<Value> {
    let mapping = ModelMapping::from_provider(provider);
    if !mapping.has_mapping() {
        return None;
    }

    let original_model = body.get("model").and_then(|m| m.as_str())?;
    let mapped_model = mapping.map_model(original_model);
    if mapped_model == original_model {
        return None;
    }

    log::debug!("[ModelMapper] 模型映射: {original_model} → {mapped_model}");
    let mut mapped_body = body.clone();
    mapped_body["model"] = Value::String(mapped_model);
    Some(mapped_body)
}

/// Claude Code 通过 `[1M]` 后缀声明 100 万上下文能力；上游 API
/// 通常不接受这个本地能力标记，转发前需要剥离。
pub fn strip_one_m_suffix_for_upstream(model: &str) -> &str {
    let trimmed = model.trim_end();
    let marker = ONE_M_CONTEXT_MARKER.as_bytes();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= marker.len()
        && bytes[bytes.len() - marker.len()..].eq_ignore_ascii_case(marker)
    {
        return trimmed[..trimmed.len() - marker.len()].trim_end();
    }
    model
}

/// 就地剥离请求体模型名上的 `[1m]` 标记，返回是否真的剥离过。
///
/// 只有带标记时才让借用中的请求体落成 Owned；没有标记时零拷贝。
pub fn strip_one_m_suffix_for_upstream_from_body(body: &mut Cow<'_, Value>) -> bool {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return false;
    };
    let stripped = strip_one_m_suffix_for_upstream(model);
    if stripped == model {
        return false;
    }

    log::debug!("[ModelMapper] 去除本地 1M 标记: {model} → {stripped}");
    body.to_mut()["model"] = Value::String(stripped.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_provider_with_mapping() -> Provider {
        Provider {
            id: "test".to_string(),
            name: "Test".to_string(),
            settings_config: json!({
                "env": {
                    "ANTHROPIC_MODEL": "default-model",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "haiku-mapped",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "sonnet-mapped",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "opus-mapped",
                    "ANTHROPIC_DEFAULT_FABLE_MODEL": "fable-mapped"
                }
            }),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    fn create_provider_without_mapping() -> Provider {
        Provider {
            id: "test".to_string(),
            name: "Test".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    fn test_sonnet_mapping() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "claude-sonnet-4-5-20250929"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "sonnet-mapped");
    }

    #[test]
    fn test_haiku_mapping() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "claude-haiku-4-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "haiku-mapped");
    }

    #[test]
    fn test_opus_mapping() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "claude-opus-4-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "opus-mapped");
    }

    #[test]
    fn test_fable_mapping() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "claude-fable-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "fable-mapped");
    }

    #[test]
    fn test_fable_with_one_m_suffix_mapping() {
        // Claude Code 实际会发 claude-fable-5[1m] 形态（issue #3980）
        let provider = create_provider_with_mapping();
        let body = json!({"model": "claude-fable-5[1m]"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "fable-mapped");
    }

    #[test]
    fn test_fable_falls_back_to_opus_when_unset() {
        let mut provider = create_provider_with_mapping();
        provider.settings_config = json!({
            "env": {
                "ANTHROPIC_MODEL": "default-model",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "opus-mapped"
            }
        });
        let body = json!({"model": "claude-fable-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "opus-mapped");
    }

    #[test]
    fn test_fable_falls_back_to_default_without_opus() {
        let mut provider = create_provider_with_mapping();
        provider.settings_config = json!({
            "env": {
                "ANTHROPIC_MODEL": "default-model"
            }
        });
        let body = json!({"model": "claude-fable-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "default-model");
    }

    #[test]
    fn test_thinking_does_not_affect_model_mapping() {
        // Issue #2081: thinking 参数不应影响模型映射
        let provider = create_provider_with_mapping();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "thinking": {"type": "enabled"}
        });
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "sonnet-mapped");
    }

    #[test]
    fn test_thinking_adaptive_does_not_affect_model_mapping() {
        // Issue #2081: adaptive thinking 也不应影响模型映射
        let provider = create_provider_with_mapping();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "thinking": {"type": "adaptive"}
        });
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "sonnet-mapped");
    }

    #[test]
    fn test_thinking_disabled() {
        let provider = create_provider_with_mapping();
        let body = json!({
            "model": "claude-sonnet-4-5",
            "thinking": {"type": "disabled"}
        });
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "sonnet-mapped");
    }

    #[test]
    fn test_unknown_model_uses_default() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "some-unknown-model"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "default-model");
    }

    #[test]
    fn test_subagent_model_preserved_before_default_fallback() {
        let mut provider = create_provider_with_mapping();
        provider.settings_config = json!({
            "env": {
                "ANTHROPIC_MODEL": "default-model",
                "CLAUDE_CODE_SUBAGENT_MODEL": "gpt-5.4-mini"
            }
        });

        let body = json!({"model": "gpt-5.4-mini"});

        // 子代理模型与映射表同档时保持原样：不产生拷贝、不改写。
        assert!(apply_model_mapping(&body, &provider).is_none());
        assert_eq!(body["model"], "gpt-5.4-mini");
    }

    #[test]
    fn test_subagent_model_preserved_with_one_m_suffix_before_default_fallback() {
        let mut provider = create_provider_with_mapping();
        provider.settings_config = json!({
            "env": {
                "ANTHROPIC_MODEL": "default-model",
                "CLAUDE_CODE_SUBAGENT_MODEL": "gpt-5.4-mini"
            }
        });

        let body = json!({"model": "gpt-5.4-mini[1M]"});

        assert!(apply_model_mapping(&body, &provider).is_none());
        assert_eq!(body["model"], "gpt-5.4-mini[1M]");
    }

    #[test]
    fn test_no_mapping_configured() {
        let provider = create_provider_without_mapping();
        let body = json!({"model": "claude-sonnet-4-5"});

        assert!(apply_model_mapping(&body, &provider).is_none());
        assert_eq!(body["model"], "claude-sonnet-4-5");
    }

    #[test]
    fn test_case_insensitive() {
        let provider = create_provider_with_mapping();
        let body = json!({"model": "Claude-SONNET-4-5"});
        let result = apply_model_mapping(&body, &provider).expect("mapped");
        assert_eq!(result["model"], "sonnet-mapped");
    }

    /// 测试辅助：按值进来、就地剥离后交回去，保持断言的写法。
    fn stripped(body: Value) -> Value {
        let mut cow = Cow::Owned(body);
        strip_one_m_suffix_for_upstream_from_body(&mut cow);
        cow.into_owned()
    }

    #[test]
    fn strips_one_m_suffix_before_upstream() {
        let body = json!({"model": "deepseek-v4-pro[1M]"});
        let result = stripped(body);
        assert_eq!(result["model"], "deepseek-v4-pro");
    }

    #[test]
    fn strips_one_m_suffix_after_mapping() {
        let mut provider = create_provider_with_mapping();
        provider.settings_config = json!({
            "env": {
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "deepseek-v4-pro [1M]"
            }
        });

        let body = json!({"model": "claude-sonnet-4-6"});
        let mapped = apply_model_mapping(&body, &provider).expect("mapped");
        let result = stripped(mapped);

        assert_eq!(result["model"], "deepseek-v4-pro");
    }

    #[test]
    fn keeps_model_without_one_m_suffix() {
        let body = json!({"model": "deepseek-v4-pro"});
        let result = stripped(body);
        assert_eq!(result["model"], "deepseek-v4-pro");
    }

    #[test]
    fn borrowed_body_without_marker_is_never_cloned() {
        // `[1m]` 剥离是绝大多数请求都会过的分支：没有标记时必须保持借用，
        // 否则整份请求体会被无谓拷贝一份。
        let owned = json!({"model": "deepseek-v4-pro"});
        let mut cow = Cow::Borrowed(&owned);
        assert!(!strip_one_m_suffix_for_upstream_from_body(&mut cow));
        assert!(matches!(cow, Cow::Borrowed(_)));
    }

    #[test]
    fn borrowed_body_with_marker_is_materialized_once() {
        let owned = json!({"model": "deepseek-v4-pro[1M]"});
        let mut cow = Cow::Borrowed(&owned);
        assert!(strip_one_m_suffix_for_upstream_from_body(&mut cow));
        assert_eq!(cow["model"], "deepseek-v4-pro");
    }
}
