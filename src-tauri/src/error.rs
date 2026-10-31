use std::path::Path;
use std::sync::PoisonError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("配置错误: {0}")]
    Config(String),
    #[error("无效输入: {0}")]
    InvalidInput(String),
    /// Native files changed after CC Switch last read them.
    #[error("并发冲突: {0}")]
    Conflict(String),
    #[error("IO 错误: {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    IoContext {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON 解析错误: {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON 序列化失败: {source}")]
    JsonSerialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("TOML 解析错误: {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("锁获取失败: {0}")]
    Lock(String),
    #[error("MCP 校验失败: {0}")]
    McpValidation(String),
    #[error("{0}")]
    Message(String),
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("{zh} ({en})")]
    Localized {
        key: &'static str,
        zh: String,
        en: String,
    },
    #[error("数据库错误: {0}")]
    Database(String),
    #[error("OMO 配置文件不存在")]
    OmoConfigNotFound,
    #[error("所有供应商已熔断，无可用渠道")]
    AllProvidersCircuitOpen,
    #[error("未配置供应商")]
    NoProvidersConfigured,
}

impl AppError {
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn json(path: impl AsRef<Path>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn toml(path: impl AsRef<Path>, source: toml::de::Error) -> Self {
        Self::Toml {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn localized(key: &'static str, zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self::Localized {
            key,
            zh: zh.into(),
            en: en.into(),
        }
    }
}

impl<T> From<PoisonError<T>> for AppError {
    fn from(err: PoisonError<T>) -> Self {
        Self::Lock(err.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(err.to_string())
    }
}

impl From<AppError> for String {
    fn from(err: AppError) -> Self {
        err.to_string()
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// 格式化为 JSON 错误字符串，前端可解析为结构化错误
pub fn format_skill_error(
    code: &str,
    context: &[(&str, &str)],
    suggestion: Option<&str>,
) -> String {
    use serde_json::json;

    let mut ctx_map = serde_json::Map::new();
    for (key, value) in context {
        ctx_map.insert(key.to_string(), json!(value));
    }

    let error_obj = json!({
        "code": code,
        "context": ctx_map,
        "suggestion": suggestion,
    });

    serde_json::to_string(&error_obj).unwrap_or_else(|_| {
        // 如果 JSON 序列化失败，返回简单格式
        format!("ERROR:{code}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_skill_error_emits_structured_json() {
        let json = format_skill_error(
            "backup_failed",
            &[
                ("skill", "commit-msg"),
                ("path", "/data/skills"),
                ("reason", "access denied"),
            ],
            Some("Retry with administrator rights"),
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("skill error must be valid JSON");
        assert_eq!(parsed["code"], "backup_failed");
        assert_eq!(parsed["context"]["skill"], "commit-msg");
        assert_eq!(parsed["context"]["path"], "/data/skills");
        assert_eq!(parsed["context"]["reason"], "access denied");
        assert_eq!(parsed["suggestion"], "Retry with administrator rights");
    }

    #[test]
    fn format_skill_error_without_suggestion_keeps_an_empty_context() {
        let json = format_skill_error("noop", &[], None);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("skill error must be valid JSON");
        assert_eq!(parsed["code"], "noop");
        assert_eq!(parsed["context"], serde_json::json!({}));
        assert!(parsed["suggestion"].is_null());
    }

    #[test]
    fn display_prefixes_identify_the_error_family_for_the_frontend() {
        assert_eq!(AppError::Config("bad".into()).to_string(), "配置错误: bad");
        assert_eq!(
            AppError::InvalidInput("bad".into()).to_string(),
            "无效输入: bad"
        );
        assert_eq!(
            AppError::Conflict("edit".into()).to_string(),
            "并发冲突: edit"
        );
        assert_eq!(
            AppError::Lock("mutex".into()).to_string(),
            "锁获取失败: mutex"
        );
        assert_eq!(
            AppError::McpValidation("shape".into()).to_string(),
            "MCP 校验失败: shape"
        );
        assert_eq!(AppError::Message("plain".into()).to_string(), "plain");
        assert_eq!(
            AppError::Database("db".into()).to_string(),
            "数据库错误: db"
        );
        assert_eq!(
            AppError::HttpStatus {
                status: 429,
                body: "slow down".into()
            }
            .to_string(),
            "HTTP 429: slow down"
        );
        assert_eq!(
            AppError::OmoConfigNotFound.to_string(),
            "OMO 配置文件不存在"
        );
        assert_eq!(
            AppError::AllProvidersCircuitOpen.to_string(),
            "所有供应商已熔断，无可用渠道"
        );
        assert_eq!(AppError::NoProvidersConfigured.to_string(), "未配置供应商");
    }

    #[test]
    fn localized_errors_carry_the_locale_key_and_both_strings() {
        let error = AppError::localized("switch.locked", "切换锁定中", "Switch in progress");
        assert_eq!(error.to_string(), "切换锁定中 (Switch in progress)");
        match error {
            AppError::Localized { key, zh, en } => {
                assert_eq!(key, "switch.locked");
                assert_eq!(zh, "切换锁定中");
                assert_eq!(en, "Switch in progress");
            }
            other => panic!("expected Localized, got {other:?}"),
        }
    }

    #[test]
    fn io_context_errors_keep_their_own_prefix_without_a_path() {
        let error = AppError::IoContext {
            context: "读取代理配置".into(),
            source: std::io::Error::other("denied"),
        };
        assert_eq!(error.to_string(), "读取代理配置: denied");
    }

    #[test]
    fn source_errors_wrap_their_path_and_cause() {
        let io = AppError::io("/data/config.json", std::io::Error::other("denied"));
        let message = io.to_string();
        assert!(
            message.starts_with("IO 错误: /data/config.json: "),
            "{message}"
        );
        assert!(message.ends_with("denied"), "{message}");

        let json = AppError::json(
            "/data/settings.json",
            serde_json::from_str::<()>("not json").unwrap_err(),
        );
        assert!(
            json.to_string()
                .starts_with("JSON 解析错误: /data/settings.json: "),
            "{json}"
        );

        let toml = AppError::toml(
            "/data/config.toml",
            toml::from_str::<toml::Value>("x = =").unwrap_err(),
        );
        assert!(
            toml.to_string()
                .starts_with("TOML 解析错误: /data/config.toml: "),
            "{toml}"
        );
    }

    #[test]
    fn foreign_errors_map_into_the_two_shared_variants() {
        let from_sqlite: AppError = rusqlite::Error::InvalidQuery.into();
        assert_eq!(
            from_sqlite.to_string(),
            "数据库错误: Query is not read-only"
        );

        let poisoned: AppError = std::sync::PoisonError::<()>::new(()).into();
        assert!(
            poisoned.to_string().starts_with("锁获取失败: "),
            "{poisoned}"
        );

        // 前端拿到的就是 display 字符串，不需要二次结构。
        let message: String = AppError::Message("boom".into()).into();
        assert_eq!(message, "boom");
    }

    #[test]
    fn serialization_is_the_display_string_so_the_frontend_shows_one_line() {
        let json = serde_json::to_string(&AppError::InvalidInput("bad".into()))
            .expect("errors must serialize");
        assert_eq!(
            serde_json::from_str::<String>(&json).expect("serialized string"),
            "无效输入: bad"
        );
    }
}
