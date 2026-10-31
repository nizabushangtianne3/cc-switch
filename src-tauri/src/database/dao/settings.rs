//! 通用设置数据访问对象
//!
//! 提供键值对形式的通用设置存储。

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use rusqlite::Connection;

pub(crate) fn proxy_runtime_configs_from_conn(
    conn: &Connection,
) -> Result<
    (
        crate::proxy::types::RectifierConfig,
        crate::proxy::types::OptimizerConfig,
        crate::proxy::types::CopilotOptimizerConfig,
    ),
    AppError,
> {
    const RECTIFIER_KEY: &str = "rectifier_config";
    const OPTIMIZER_KEY: &str = "optimizer_config";
    const COPILOT_KEY: &str = "copilot_optimizer_config";

    let mut rectifier_json: Option<String> = None;
    let mut optimizer_json: Option<String> = None;
    let mut copilot_json: Option<String> = None;
    let mut stmt = conn
        .prepare_cached("SELECT key, value FROM settings WHERE key IN (?1, ?2, ?3)")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut rows = stmt
        .query(params![RECTIFIER_KEY, OPTIMIZER_KEY, COPILOT_KEY])
        .map_err(|e| AppError::Database(e.to_string()))?;

    while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
        let key: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
        let value: String = row.get(1).map_err(|e| AppError::Database(e.to_string()))?;
        match key.as_str() {
            RECTIFIER_KEY => rectifier_json = Some(value),
            OPTIMIZER_KEY => optimizer_json = Some(value),
            COPILOT_KEY => copilot_json = Some(value),
            _ => {}
        }
    }

    let rectifier_config = rectifier_json
        .map(|json| {
            serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("解析整流器配置失败，使用默认配置: {e}");
                crate::proxy::types::RectifierConfig::default()
            })
        })
        .unwrap_or_default();
    let optimizer_config = optimizer_json
        .map(|json| {
            serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("解析优化器配置失败，使用默认配置: {e}");
                crate::proxy::types::OptimizerConfig::default()
            })
        })
        .unwrap_or_default();
    let copilot_optimizer_config = copilot_json
        .map(|json| {
            serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("解析 Copilot 优化器配置失败，使用默认配置: {e}");
                crate::proxy::types::CopilotOptimizerConfig::default()
            })
        })
        .unwrap_or_default();

    Ok((rectifier_config, optimizer_config, copilot_optimizer_config))
}

impl Database {
    const LEGACY_COMMON_CONFIG_MIGRATED_KEY: &'static str = "common_config_legacy_migrated_v1";

    fn config_snippet_cleared_key(app_type: &str) -> String {
        format!("common_config_{app_type}_cleared")
    }

    /// 获取设置值
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query(params![key])
            .map_err(|e| AppError::Database(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    /// 以布尔语义读取 flag：`"true"` 或 `"1"` → true，其它全部 false。
    ///
    /// 用于一次性启动 flag（`official_providers_seeded` / `first_run_notice_shown` 等）。
    /// 与 `is_legacy_common_config_migrated` 等只认 `"true"` 的历史辅助函数**不同**——
    /// 这里同时接受 `"1"` 是为了兼容 `init_default_official_providers` 既有写法。
    pub fn get_bool_flag(&self, key: &str) -> Result<bool, AppError> {
        Ok(matches!(
            self.get_setting(key)?.as_deref(),
            Some("true") | Some("1")
        ))
    }

    /// 设置值
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        self.invalidate_proxy_request_config_cache();
        Ok(())
    }

    // --- 通用配置片段 (Common Config Snippet) ---

    /// 获取通用配置片段
    pub fn get_config_snippet(&self, app_type: &str) -> Result<Option<String>, AppError> {
        self.get_setting(&format!("common_config_{app_type}"))
    }

    /// 检查通用配置片段是否被用户显式清空
    pub fn is_config_snippet_cleared(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self
            .get_setting(&Self::config_snippet_cleared_key(app_type))?
            .as_deref()
            == Some("true"))
    }

    /// 设置通用配置片段是否被显式清空
    pub fn set_config_snippet_cleared(
        &self,
        app_type: &str,
        cleared: bool,
    ) -> Result<(), AppError> {
        let key = Self::config_snippet_cleared_key(app_type);
        if cleared {
            self.set_setting(&key, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// 当前是否允许从 live 配置自动抽取通用配置片段
    pub fn should_auto_extract_config_snippet(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self.get_config_snippet(app_type)?.is_none()
            && !self.is_config_snippet_cleared(app_type)?)
    }

    /// 检查历史通用配置迁移是否已经执行过
    pub fn is_legacy_common_config_migrated(&self) -> Result<bool, AppError> {
        Ok(self
            .get_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY)?
            .as_deref()
            == Some("true"))
    }

    /// 标记历史通用配置迁移已经执行完成
    pub fn set_legacy_common_config_migrated(&self, migrated: bool) -> Result<(), AppError> {
        if migrated {
            self.set_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                params![Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// 设置通用配置片段
    pub fn set_config_snippet(
        &self,
        app_type: &str,
        snippet: Option<String>,
    ) -> Result<(), AppError> {
        let key = format!("common_config_{app_type}");
        if let Some(value) = snippet {
            self.set_setting(&key, &value)
        } else {
            // 如果为 None 则删除
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    // --- 全局出站代理 ---

    /// 全局代理 URL 的存储键名
    const GLOBAL_PROXY_URL_KEY: &'static str = "global_proxy_url";

    /// 获取全局出站代理 URL
    ///
    /// 返回 None 表示未配置或已清除代理（直连）
    /// 返回 Some(url) 表示已配置代理
    pub fn get_global_proxy_url(&self) -> Result<Option<String>, AppError> {
        self.get_setting(Self::GLOBAL_PROXY_URL_KEY)
    }

    /// 设置全局出站代理 URL
    ///
    /// - 传入非空字符串：启用代理
    /// - 传入空字符串或 None：清除代理设置（直连）
    pub fn set_global_proxy_url(&self, url: Option<&str>) -> Result<(), AppError> {
        match url {
            Some(u) if !u.trim().is_empty() => {
                self.set_setting(Self::GLOBAL_PROXY_URL_KEY, u.trim())
            }
            _ => {
                // 清除代理设置
                let conn = lock_conn!(self.conn);
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    params![Self::GLOBAL_PROXY_URL_KEY],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                Ok(())
            }
        }
    }

    // --- 整流器配置 ---

    /// 获取整流器配置
    ///
    /// 返回整流器配置，如果不存在则返回默认值（全部开启）
    pub fn get_rectifier_config(&self) -> Result<crate::proxy::types::RectifierConfig, AppError> {
        match self.get_setting("rectifier_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析整流器配置失败: {e}"))),
            None => Ok(crate::proxy::types::RectifierConfig::default()),
        }
    }

    /// 更新整流器配置
    pub fn set_rectifier_config(
        &self,
        config: &crate::proxy::types::RectifierConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Database(format!("序列化整流器配置失败: {e}")))?;
        self.set_setting("rectifier_config", &json)
    }

    // --- 优化器配置 ---

    /// 获取优化器配置
    ///
    /// 返回优化器配置，如果不存在则返回默认值（默认关闭）
    pub fn get_optimizer_config(&self) -> Result<crate::proxy::types::OptimizerConfig, AppError> {
        match self.get_setting("optimizer_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析优化器配置失败: {e}"))),
            None => Ok(crate::proxy::types::OptimizerConfig::default()),
        }
    }

    /// 更新优化器配置
    pub fn set_optimizer_config(
        &self,
        config: &crate::proxy::types::OptimizerConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Database(format!("序列化优化器配置失败: {e}")))?;
        self.set_setting("optimizer_config", &json)
    }

    // --- Copilot 优化器配置 ---

    /// 获取 Copilot 优化器配置
    ///
    /// 返回配置，如果不存在则返回默认值（默认开启）
    pub fn get_copilot_optimizer_config(
        &self,
    ) -> Result<crate::proxy::types::CopilotOptimizerConfig, AppError> {
        match self.get_setting("copilot_optimizer_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析 Copilot 优化器配置失败: {e}"))),
            None => Ok(crate::proxy::types::CopilotOptimizerConfig::default()),
        }
    }

    /// 更新 Copilot 优化器配置
    pub fn set_copilot_optimizer_config(
        &self,
        config: &crate::proxy::types::CopilotOptimizerConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Database(format!("序列化 Copilot 优化器配置失败: {e}")))?;
        self.set_setting("copilot_optimizer_config", &json)
    }

    // --- 日志配置 ---

    /// 获取日志配置
    pub fn get_log_config(&self) -> Result<crate::proxy::types::LogConfig, AppError> {
        match self.get_setting("log_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("解析日志配置失败: {e}"))),
            None => Ok(crate::proxy::types::LogConfig::default()),
        }
    }

    /// 更新日志配置
    pub fn set_log_config(&self, config: &crate::proxy::types::LogConfig) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Database(format!("序列化日志配置失败: {e}")))?;
        self.set_setting("log_config", &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Database {
        Database::memory().expect("memory db")
    }

    #[test]
    fn settings_round_trip_and_missing_keys_read_as_none() {
        let db = memory_db();
        assert_eq!(db.get_setting("absent").unwrap(), None);

        db.set_setting("probe", "first").unwrap();
        assert_eq!(db.get_setting("probe").unwrap().as_deref(), Some("first"));

        db.set_setting("probe", "second").unwrap();
        assert_eq!(db.get_setting("probe").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn bool_flags_only_accept_true_or_one() {
        let db = memory_db();
        assert!(!db.get_bool_flag("absent").unwrap());

        db.set_setting("flag", "1").unwrap();
        assert!(db.get_bool_flag("flag").unwrap());

        db.set_setting("flag", "true").unwrap();
        assert!(db.get_bool_flag("flag").unwrap());

        db.set_setting("flag", "yes").unwrap();
        assert!(!db.get_bool_flag("flag").unwrap());
    }

    #[test]
    fn config_snippet_and_its_cleared_marker_gate_auto_extraction() {
        let db = memory_db();
        assert!(db.get_config_snippet("claude").unwrap().is_none());
        assert!(db.should_auto_extract_config_snippet("claude").unwrap());

        db.set_config_snippet("claude", Some("snippet".to_string()))
            .unwrap();
        assert_eq!(
            db.get_config_snippet("claude").unwrap().as_deref(),
            Some("snippet")
        );
        assert!(!db.should_auto_extract_config_snippet("claude").unwrap());

        // 清空片段后恢复可抽取，但用户显式清过的标记要一直压着它。
        db.set_config_snippet("claude", None).unwrap();
        assert!(db.get_config_snippet("claude").unwrap().is_none());
        assert!(db.should_auto_extract_config_snippet("claude").unwrap());

        db.set_config_snippet_cleared("claude", true).unwrap();
        assert!(db.is_config_snippet_cleared("claude").unwrap());
        assert!(!db.should_auto_extract_config_snippet("claude").unwrap());

        // 取消清除是删键，不是写 "false"。
        db.set_config_snippet_cleared("claude", false).unwrap();
        assert!(!db.is_config_snippet_cleared("claude").unwrap());
        assert!(db.should_auto_extract_config_snippet("claude").unwrap());
    }

    #[test]
    fn legacy_common_config_migrated_only_counts_true() {
        let db = memory_db();
        assert!(!db.is_legacy_common_config_migrated().unwrap());

        db.set_legacy_common_config_migrated(true).unwrap();
        assert!(db.is_legacy_common_config_migrated().unwrap());

        db.set_legacy_common_config_migrated(false).unwrap();
        assert!(!db.is_legacy_common_config_migrated().unwrap());

        // 和 get_bool_flag 不同：这里只认 "true"，"1" 不算。
        db.set_setting(Database::LEGACY_COMMON_CONFIG_MIGRATED_KEY, "1")
            .unwrap();
        assert!(!db.is_legacy_common_config_migrated().unwrap());
    }

    #[test]
    fn global_proxy_url_trims_and_clears() {
        let db = memory_db();
        assert_eq!(db.get_global_proxy_url().unwrap(), None);

        db.set_global_proxy_url(Some("  http://127.0.0.1:7890  "))
            .unwrap();
        assert_eq!(
            db.get_global_proxy_url().unwrap().as_deref(),
            Some("http://127.0.0.1:7890")
        );

        db.set_global_proxy_url(Some("   ")).unwrap();
        assert_eq!(db.get_global_proxy_url().unwrap(), None);

        db.set_global_proxy_url(Some("http://127.0.0.1:7890"))
            .unwrap();
        assert_eq!(
            db.get_global_proxy_url().unwrap().as_deref(),
            Some("http://127.0.0.1:7890")
        );
        db.set_global_proxy_url(None).unwrap();
        assert_eq!(db.get_global_proxy_url().unwrap(), None);
    }

    #[test]
    fn stored_configs_fall_back_to_defaults_then_round_trip() {
        let db = memory_db();

        let rectifier = db.get_rectifier_config().unwrap();
        assert!(rectifier.enabled);
        assert!(rectifier.request_thinking_signature);
        let optimizer = db.get_optimizer_config().unwrap();
        assert!(!optimizer.enabled);
        assert!(optimizer.cache_injection);
        let copilot = db.get_copilot_optimizer_config().unwrap();
        assert!(copilot.enabled);
        assert!(copilot.warmup_downgrade);
        let log = db.get_log_config().unwrap();
        assert!(log.enabled);
        assert_eq!(log.level, "info");

        let rectifier = crate::proxy::types::RectifierConfig {
            enabled: false,
            request_media_heuristic: false,
            ..Default::default()
        };
        db.set_rectifier_config(&rectifier).unwrap();
        let stored = db.get_rectifier_config().unwrap();
        assert!(!stored.enabled);
        assert!(!stored.request_media_heuristic);
        assert!(stored.request_thinking_budget);

        let optimizer = crate::proxy::types::OptimizerConfig {
            enabled: true,
            cache_injection: false,
            ..Default::default()
        };
        db.set_optimizer_config(&optimizer).unwrap();
        let stored = db.get_optimizer_config().unwrap();
        assert!(stored.enabled);
        assert!(!stored.cache_injection);
        assert!(stored.thinking_optimizer);

        let copilot = crate::proxy::types::CopilotOptimizerConfig {
            enabled: false,
            warmup_model: "gpt-5.1-codex-mini".to_string(),
            ..Default::default()
        };
        db.set_copilot_optimizer_config(&copilot).unwrap();
        let stored = db.get_copilot_optimizer_config().unwrap();
        assert!(!stored.enabled);
        assert_eq!(stored.warmup_model, "gpt-5.1-codex-mini");
        assert!(stored.subagent_detection);

        let log = crate::proxy::types::LogConfig {
            enabled: false,
            level: "debug".to_string(),
        };
        db.set_log_config(&log).unwrap();
        let stored = db.get_log_config().unwrap();
        assert!(!stored.enabled);
        assert_eq!(stored.level, "debug");
    }

    #[test]
    fn a_corrupt_stored_config_is_a_database_error_not_a_panic() {
        let db = memory_db();
        db.set_setting("rectifier_config", "not json").unwrap();
        let error = db.get_rectifier_config().unwrap_err().to_string();
        assert!(error.contains("解析整流器配置失败"), "{error}");

        db.set_setting("log_config", "not json").unwrap();
        let error = db.get_log_config().unwrap_err().to_string();
        assert!(error.contains("解析日志配置失败"), "{error}");
    }
}
