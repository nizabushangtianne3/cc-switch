pub mod agy_account;
pub mod auto_sync;
pub mod backup_storage;
pub mod balance;
pub mod codex_oauth_models;
pub mod codex_session_compression;
pub mod coding_plan;
pub mod config;
pub mod env_checker;
pub mod env_manager;
pub mod kimi_oauth;
pub mod mcp;
pub mod model_fetch;
pub mod model_pricing;
pub mod omo;
pub mod pi_prompt_files;
pub(crate) mod pi_state;
pub mod profile;
pub mod prompt;
pub mod provider;
pub mod proxy;
pub mod s3;
pub mod s3_auto_sync;
pub mod s3_sync;
pub mod session_usage;
pub mod session_usage_codex;
pub mod session_usage_gemini;
pub mod session_usage_grokbuild;
pub mod session_usage_opencode;
pub mod session_usage_pi;
pub mod skill;
pub mod speedtest;
pub mod sql_helpers;
pub mod stream_check;
pub mod subscription;
pub mod subscription_agy;
pub mod subscription_grok;
pub mod sync_protocol;
pub mod usage_cache;
pub mod usage_stats;
pub mod webdav;
pub mod webdav_auto_sync;
pub mod webdav_sync;

pub use config::ConfigService;
pub use mcp::McpService;
pub use omo::OmoService;
pub use prompt::PromptService;
pub use provider::{ProviderService, ProviderSortUpdate, SwitchResult};
pub use proxy::ProxyService;
#[allow(unused_imports)]
pub use skill::{DiscoverableSkill, Skill, SkillRepo, SkillService};
pub use speedtest::{EndpointLatency, SpeedtestService};
pub use usage_cache::UsageCache;
#[allow(unused_imports)]
pub use usage_stats::{
    ApiKeyStats, DailyStats, LogFilters, ModelStats, PaginatedLogs, ProviderLimitStatus,
    ProviderStats, RequestLogDetail, UsageSummary, UsageSummaryByApp,
};

pub mod session_usage_kimi;
pub mod session_usage_mcode;

pub mod session_usage_agy;

/// 当前 Unix 毫秒时间戳。
pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_millis_returns_current_unix_milliseconds() {
        let now = now_millis();
        assert!(now > 1_600_000_000_000, "expected a timestamp after 2020");
        let system_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before the unix epoch")
            .as_millis() as i64;
        assert!(
            now <= system_now + 60_000,
            "now_millis must not run more than 60s ahead of the system clock"
        );
    }
}
