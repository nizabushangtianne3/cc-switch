//! 供应商路由器模块
//!
//! 负责选择和管理代理目标供应商，实现智能故障转移

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use crate::proxy::circuit_breaker::{AllowResult, CircuitBreaker, CircuitBreakerConfig};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

type CircuitBreakers = HashMap<String, HashMap<String, Arc<CircuitBreaker>>>;
type KeyCursors = HashMap<String, HashMap<String, usize>>;

/// Codex Official requests carry the selected account's native Authorization
/// header. Reusing that request against another account card would cross the
/// account boundary, so these cards must never participate in provider retry.
pub(crate) fn provider_supports_failover(app_type: &str, provider: &Provider) -> bool {
    app_type != AppType::Codex.as_str()
        || !crate::proxy::providers::is_codex_official_provider(provider)
}

fn stable_key_hash(value: &str) -> u64 {
    value.bytes().fold(1469598103934665603, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1099511628211)
    })
}

/// 供应商路由器
pub struct ProviderRouter {
    /// 数据库连接
    db: Arc<Database>,
    /// 熔断器管理器：按应用分组，避免请求热路径拼接复合字符串 key。
    circuit_breakers: Arc<RwLock<CircuitBreakers>>,
    /// Key 池轮询游标：同样按应用分组。
    key_cursors: Arc<RwLock<KeyCursors>>,
}

impl ProviderRouter {
    /// 创建新的供应商路由器
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            key_cursors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 选择可用的供应商（支持故障转移）
    ///
    /// 返回按优先级排序的可用供应商列表：
    /// - 故障转移关闭时：仅返回当前供应商
    /// - 故障转移开启时：仅使用故障转移队列，按队列顺序依次尝试（P1 → P2 → ...）
    pub async fn select_providers(&self, app_type: &str) -> Result<Vec<Arc<Provider>>, AppError> {
        // 代理模式下路由到代理路由那家，和直连指针无关。
        let current_provider = AppType::from_str(app_type).ok().and_then(|app_enum| {
            crate::mode::current::provider_in_use(&self.db, &app_enum)
                .ok()
                .flatten()
                .map(std::sync::Arc::new)
        });
        let auto_failover_enabled = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(config) => config.auto_failover_enabled,
            Err(e) => {
                log::error!("[{app_type}] 读取 proxy_config 失败: {e}，默认禁用故障转移");
                false
            }
        };
        self.select_providers_with_current(app_type, current_provider, auto_failover_enabled)
            .await
    }

    /// 同 [`Self::select_providers`]，正在用的那家由调用方给出：处理请求时上下文已经读过
    /// 一次（要读 `live-state.json` 和这一行），不用每个请求再读一遍，两处用的也一定是
    /// 同一家。
    pub async fn select_providers_with_current(
        &self,
        app_type: &str,
        current_provider: Option<std::sync::Arc<Provider>>,
        auto_failover_enabled: bool,
    ) -> Result<Vec<Arc<Provider>>, AppError> {
        let mut result = Vec::new();
        let mut total_providers = 0usize;
        let mut circuit_open_count = 0usize;

        if auto_failover_enabled
            && current_provider
                .as_ref()
                .is_some_and(|provider| !provider_supports_failover(app_type, provider))
        {
            // A selected Codex Official account is an explicit account choice.
            // Keep it as a single route even if an old failover setting remains
            // enabled; retrying would reuse its inbound token for another card.
            total_providers = 1;
            result.push(current_provider.expect("checked above"));
        } else if auto_failover_enabled {
            // 故障转移开启：仅按队列顺序依次尝试（P1 → P2 → ...）
            let db = Arc::clone(&self.db);
            let queue_app_type = app_type.to_string();
            let providers = tokio::task::spawn_blocking(move || {
                db.get_shared_failover_queue_providers(&queue_app_type)
            })
            .await
            .map_err(|error| AppError::Database(format!("故障转移队列读取任务失败: {error}")))??;
            for provider in providers {
                if !provider_supports_failover(app_type, &provider) {
                    continue;
                }
                total_providers += 1;

                let breaker = self
                    .get_or_create_circuit_breaker(app_type, &provider.id)
                    .await;

                if breaker.is_available().await {
                    result.push(provider);
                } else {
                    circuit_open_count += 1;
                }
            }
        } else {
            // 故障转移关闭：仅使用当前供应商，跳过熔断器检查
            if let Some(current) = current_provider {
                total_providers = 1;
                result.push(current);
            }
        }

        if result.is_empty() {
            if total_providers > 0 && circuit_open_count == total_providers {
                log::warn!("[{app_type}] [FO-004] 所有供应商均已熔断");
                return Err(AppError::AllProvidersCircuitOpen);
            } else {
                log::warn!("[{app_type}] [FO-005] 未配置供应商");
                return Err(AppError::NoProvidersConfigured);
            }
        }

        Ok(result)
    }

    /// 请求执行前获取熔断器“放行许可”
    ///
    /// - Closed：直接放行
    /// - Open：超时到达后切到 HalfOpen 并放行一次探测
    /// - HalfOpen：按限流规则放行探测
    ///
    /// 注意：调用方必须在请求结束后通过 `record_result()` 释放 HalfOpen 名额，
    /// 否则会导致该 Provider 长时间无法进入探测状态。
    pub async fn allow_provider_request(&self, provider_id: &str, app_type: &str) -> AllowResult {
        let breaker = self
            .get_or_create_circuit_breaker(app_type, provider_id)
            .await;
        breaker.allow_request().await
    }

    /// 记录供应商请求结果
    #[cfg(test)]
    pub async fn record_result(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
    ) -> Result<(), AppError> {
        // 1. 按应用独立获取熔断器配置
        let failure_threshold = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => app_config.circuit_failure_threshold,
            Err(_) => 5, // 默认值
        };
        self.record_result_with_threshold(
            provider_id,
            app_type,
            used_half_open_permit,
            success,
            error_msg,
            failure_threshold,
        )
        .await
    }

    /// 记录供应商请求结果；调用方已持有阈值时避免重复读取配置。
    ///
    /// 这个函数会等 provider_health 写库完成（spawn_blocking + SQLite 连接锁），
    /// 只在真的需要「写完再继续」的路径上用（HalfOpen 探测释放名额、测试断点）。
    /// 每请求热路径上的记账走 [`Self::record_result_detached`]。
    pub async fn record_result_with_threshold(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
        failure_threshold: u32,
    ) -> Result<(), AppError> {
        // 2. 更新熔断器状态
        let breaker = self
            .get_or_create_circuit_breaker(app_type, provider_id)
            .await;

        if success {
            breaker.record_success(used_half_open_permit).await;
        } else {
            breaker.record_failure(used_half_open_permit).await;
        }

        // 3. 更新数据库健康状态（使用配置的阈值）。
        // 写库持 Database 的阻塞锁并做磁盘 IO，移到阻塞线程池执行
        // （同 usage 写入的 #7818 约定），避免每请求的结果记账卡住 tokio worker。
        let db = Arc::clone(&self.db);
        let provider_id = provider_id.to_string();
        let app_type = app_type.to_string();
        let write = tokio::task::spawn_blocking(move || {
            db.update_provider_health_with_threshold(
                &provider_id,
                &app_type,
                success,
                error_msg,
                failure_threshold,
            )
        });
        let write_result = match write.await {
            Ok(result) => result,
            Err(e) => return Err(AppError::Database(format!("健康状态写入任务失败: {e}"))),
        };
        write_result?;

        Ok(())
    }

    /// 记账：熔断内存态同步更新（后续请求的可用性判断立即可见），
    /// provider_health 写库 detach，不挡调用方。
    ///
    /// 失败记账发生在每请求的关键路径上（试下一家、把错误交回客户端之前），
    /// 一次写库的磁盘 IO 不该叠加在这段延迟里；usage 记录早就是同款做法。
    pub async fn record_result_detached(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
        failure_threshold: u32,
    ) {
        let breaker = self
            .get_or_create_circuit_breaker(app_type, provider_id)
            .await;

        if success {
            breaker.record_success(used_half_open_permit).await;
        } else {
            breaker.record_failure(used_half_open_permit).await;
        }

        let db = Arc::clone(&self.db);
        let provider_id = provider_id.to_string();
        let app_type = app_type.to_string();
        let log_provider_id = provider_id.clone();
        let log_app_type = app_type.clone();
        tokio::spawn(async move {
            let write = tokio::task::spawn_blocking(move || {
                db.update_provider_health_with_threshold(
                    &provider_id,
                    &app_type,
                    success,
                    error_msg,
                    failure_threshold,
                )
            });
            if let Err(e) = write.await {
                log::warn!(
                    "[{log_app_type}] 健康状态写入任务失败: provider_id={log_provider_id}, error={e}"
                );
            }
        });
    }

    /// 重置熔断器（手动恢复）
    /// 重置指定供应商的熔断器
    pub async fn reset_provider_breaker(&self, provider_id: &str, app_type: &str) {
        let breakers = self.circuit_breakers.read().await;
        if let Some(breaker) = breakers
            .get(app_type)
            .and_then(|providers| providers.get(provider_id))
        {
            breaker.reset().await;
        }
    }

    /// 仅释放 HalfOpen permit，不影响健康统计（neutral 接口）
    ///
    /// 用于整流器等场景：请求结果不应计入 Provider 健康度，
    /// 但仍需释放占用的探测名额，避免 HalfOpen 状态卡死
    pub async fn release_permit_neutral(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        if !used_half_open_permit {
            return;
        }
        let breaker = self
            .get_or_create_circuit_breaker(app_type, provider_id)
            .await;
        breaker.release_half_open_permit();
    }

    /// 更新所有熔断器的配置（热更新）
    pub async fn update_all_configs(&self, config: CircuitBreakerConfig) {
        let breakers = self.circuit_breakers.read().await;
        for breaker in breakers.values().flat_map(|providers| providers.values()) {
            breaker.update_config(config.clone()).await;
        }
    }

    /// 更新指定应用已创建熔断器的配置（热更新）
    pub async fn update_app_configs(&self, app_type: &str, config: CircuitBreakerConfig) {
        let breakers = self.circuit_breakers.read().await;
        for breaker in breakers
            .get(app_type)
            .into_iter()
            .flat_map(|providers| providers.values())
        {
            breaker.update_config(config.clone()).await;
        }
    }

    /// 获取熔断器状态
    ///
    /// 测试与 Tauri 命令层都从这里读统计；`--lib` 视角下只有测试用得到，
    /// 所以保留 #[allow(dead_code)]，删掉它会让三个 forwarder 用例编不过。
    #[allow(dead_code)]
    pub async fn get_circuit_breaker_stats(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Option<crate::proxy::circuit_breaker::CircuitBreakerStats> {
        let breakers = self.circuit_breakers.read().await;

        if let Some(breaker) = breakers
            .get(app_type)
            .and_then(|providers| providers.get(provider_id))
        {
            Some(breaker.get_stats().await)
        } else {
            None
        }
    }

    /// 获取或创建熔断器
    async fn get_or_create_circuit_breaker(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Arc<CircuitBreaker> {
        {
            let breakers = self.circuit_breakers.read().await;
            if let Some(breaker) = breakers
                .get(app_type)
                .and_then(|providers| providers.get(provider_id))
            {
                return breaker.clone();
            }
        }

        // 按应用独立读熔断器配置：放在 map 写锁之外，创建首家时 await 配置不会
        // 把同应用的其余熔断查询全部堵在写锁上。
        let config = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => crate::proxy::circuit_breaker::CircuitBreakerConfig {
                failure_threshold: app_config.circuit_failure_threshold,
                success_threshold: app_config.circuit_success_threshold,
                timeout_seconds: app_config.circuit_timeout_seconds as u64,
                error_rate_threshold: app_config.circuit_error_rate_threshold,
                min_requests: app_config.circuit_min_requests,
            },
            Err(_) => crate::proxy::circuit_breaker::CircuitBreakerConfig::default(),
        };

        // 如果不存在，获取写锁创建
        let mut breakers = self.circuit_breakers.write().await;

        // 双重检查，防止竞争条件
        if let Some(breaker) = breakers
            .get(app_type)
            .and_then(|providers| providers.get(provider_id))
        {
            return breaker.clone();
        }

        let breaker = Arc::new(CircuitBreaker::new(config));
        breakers
            .entry(app_type.to_string())
            .or_default()
            .insert(provider_id.to_string(), breaker.clone());

        breaker
    }

    /// 在注入上游认证头前给出一个供应商的 Key 尝试顺序（池内下标）。
    ///
    /// 首元素是本次请求应当使用的 Key（轮询按游标、随机按种子、权重按权重），
    /// 其余元素是同一 Key 池内的候选 Key——某个 Key 遇到可重试错误时按这个顺序
    /// 依次顶上，池内 Key 全部失败之后才回退到供应商级故障转移。
    ///
    /// 轮询游标按供应商独立保存，且每个请求只推进一步（与是否重试无关，避免
    /// Key 级重试额外消耗轮询位置）；Key 池为空时返回 `None`，调用方保留
    /// fallback 认证材料。
    pub async fn select_api_key_indices(
        &self,
        app_type: &str,
        provider: &Provider,
    ) -> Option<SmallVec<[usize; 4]>> {
        let meta = provider.meta.as_ref()?;
        if meta
            .api_keys
            .iter()
            .all(|entry| entry.key.trim().is_empty())
        {
            return None;
        }

        let mut cursors = self.key_cursors.write().await;
        let provider_cursors = if let Some(provider_cursors) = cursors.get_mut(app_type) {
            provider_cursors
        } else {
            cursors.insert(app_type.to_string(), HashMap::new());
            cursors.get_mut(app_type).expect("just inserted")
        };
        let cursor = if let Some(cursor) = provider_cursors.get_mut(&provider.id) {
            cursor
        } else {
            provider_cursors.insert(provider.id.clone(), 0);
            provider_cursors
                .get_mut(&provider.id)
                .expect("just inserted")
        };
        let request_seq = *cursor as u64;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        let seed =
            now.rotate_left(13) ^ request_seq.rotate_left(37) ^ stable_key_hash(&provider.id);

        let keys = crate::proxy::key_pool::ordered_api_key_indices(
            &meta.api_keys,
            meta.api_key_strategy,
            *cursor,
            seed,
        )?;
        // 每个请求推进一步：随机/权重模式同样按请求序列推进，保持既有行为。
        *cursor = cursor.wrapping_add(1);
        Some(keys)
    }

    /// 只需要本次请求那一个 Key 时的便捷入口（等价于 [`Self::select_api_key_indices`] 首元素）。
    pub async fn select_api_key_index(&self, app_type: &str, provider: &Provider) -> Option<usize> {
        self.select_api_key_indices(app_type, provider)
            .await
            .and_then(|indices| indices.into_iter().next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::provider::{AuthBinding, AuthBindingSource, ProviderMeta};
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    fn managed_codex_official(id: &str, account_id: &str) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            "OpenAI Official".to_string(),
            json!({ "auth": {}, "config": "" }),
            None,
        );
        provider.category = Some("official".to_string());
        provider.meta = Some(ProviderMeta {
            provider_type: Some("codex_oauth".to_string()),
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some(account_id.to_string()),
            }),
            ..Default::default()
        });
        provider
    }

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().expect("reload settings");

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }

            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }

            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_router_creation() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let router = ProviderRouter::new(db);

        let breaker = router.get_or_create_circuit_breaker("claude", "test").await;
        assert!(breaker.allow_request().await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_disabled_uses_current_provider() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_order_ignoring_current() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // 设置 sort_index 来控制顺序：b=1, a=2
        let mut provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        provider_a.sort_index = Some(2);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        db.add_to_failover_queue("claude", "b").unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // 启用自动故障转移（使用新的 proxy_config API）
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 2);
        // 故障转移开启时：仅按队列顺序选择（忽略当前供应商）
        assert_eq!(providers[0].id, "b");
        assert_eq!(providers[1].id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_only_even_if_current_not_in_queue() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        // 只把 b 加入故障转移队列（模拟“当前供应商不在队列里”的常见配置）
        db.add_to_failover_queue("claude", "b").unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "b");
    }

    #[tokio::test]
    #[serial]
    async fn codex_official_current_stays_single_route_when_failover_is_stale() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let official = managed_codex_official("official-a", "account-a");
        let fallback = Provider::with_id(
            "fallback".to_string(),
            "Fallback".to_string(),
            json!({}),
            None,
        );
        db.save_provider("codex", &official).unwrap();
        db.save_provider("codex", &fallback).unwrap();
        db.set_current_provider("codex", &official.id).unwrap();
        db.add_to_failover_queue("codex", &fallback.id).unwrap();

        let mut config = db.get_proxy_config_for_app("codex").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let providers = ProviderRouter::new(db)
            .select_providers("codex")
            .await
            .unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, official.id);
    }

    #[tokio::test]
    #[serial]
    async fn stale_codex_official_queue_entries_are_not_retry_targets() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let current = Provider::with_id(
            "third-party".to_string(),
            "Third Party".to_string(),
            json!({}),
            None,
        );
        let official = managed_codex_official("official-a", "account-a");
        let fallback = Provider::with_id(
            "fallback".to_string(),
            "Fallback".to_string(),
            json!({}),
            None,
        );
        db.save_provider("codex", &current).unwrap();
        db.save_provider("codex", &official).unwrap();
        db.save_provider("codex", &fallback).unwrap();
        db.set_current_provider("codex", &current.id).unwrap();
        db.add_to_failover_queue("codex", &official.id).unwrap();
        db.add_to_failover_queue("codex", &fallback.id).unwrap();

        let mut config = db.get_proxy_config_for_app("codex").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let providers = ProviderRouter::new(db)
            .select_providers("codex")
            .await
            .unwrap();
        assert_eq!(
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fallback"]
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_select_providers_does_not_consume_half_open_permit() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();

        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        // 启用自动故障转移（使用新的 proxy_config API）
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        router
            .record_result("b", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        let providers = router.select_providers("claude").await.unwrap();
        assert_eq!(providers.len(), 2);

        assert!(router.allow_provider_request("b", "claude").await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_release_permit_neutral_frees_half_open_slot() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // 配置熔断器：1 次失败即熔断，0 秒超时立即进入 HalfOpen
        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        db.save_provider("claude", &provider_a).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // 启用自动故障转移
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        // 触发熔断：1 次失败
        router
            .record_result("a", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        // 第一次请求：获取 HalfOpen 探测名额
        let first = router.allow_provider_request("a", "claude").await;
        assert!(first.allowed);
        assert!(first.used_half_open_permit);

        // 第二次请求应被拒绝（名额已被占用）
        let second = router.allow_provider_request("a", "claude").await;
        assert!(!second.allowed);

        // 使用 release_permit_neutral 释放名额（不影响健康统计）
        router
            .release_permit_neutral("a", "claude", first.used_half_open_permit)
            .await;

        // 第三次请求应被允许（名额已释放）
        let third = router.allow_provider_request("a", "claude").await;
        assert!(third.allowed);
        assert!(third.used_half_open_permit);
    }
}
