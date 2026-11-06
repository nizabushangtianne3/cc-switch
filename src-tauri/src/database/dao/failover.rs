//! 故障转移队列 DAO
//!
//! 管理代理模式下的故障转移队列（基于 providers 表的 in_failover_queue 字段）

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// 故障转移队列条目（简化版，用于前端展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverQueueItem {
    pub provider_id: String,
    pub provider_name: String,
    pub sort_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_notes: Option<String>,
}

impl Database {
    pub(crate) fn invalidate_provider_caches(&self) {
        self.provider_table_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut cache) = self.failover_queue_cache.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.provider_row_cache.lock() {
            cache.clear();
        }
    }

    /// 获取故障转移队列（按 sort_index 排序）
    pub fn get_failover_queue(&self, app_type: &str) -> Result<Vec<FailoverQueueItem>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare_cached(
                "SELECT id, name, sort_index, notes
                 FROM providers
                 WHERE app_type = ?1 AND in_failover_queue = 1
                 ORDER BY COALESCE(sort_index, 999999), id ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let items = stmt
            .query_map([app_type], |row| {
                Ok(FailoverQueueItem {
                    provider_id: row.get(0)?,
                    provider_name: row.get(1)?,
                    sort_index: row.get(2)?,
                    provider_notes: row.get(3)?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(items)
    }

    /// 添加供应商到故障转移队列
    pub fn add_to_failover_queue(&self, app_type: &str, provider_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "UPDATE providers SET in_failover_queue = 1 WHERE id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        self.invalidate_provider_caches();
        Ok(())
    }

    /// 从故障转移队列中移除供应商
    pub fn remove_from_failover_queue(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        // 1. 从队列中移除
        conn.execute(
            "UPDATE providers SET in_failover_queue = 0 WHERE id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        // 2. 清除该供应商的健康状态（退出队列后不再需要健康监控）
        conn.execute(
            "DELETE FROM provider_health WHERE provider_id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        log::info!("已从故障转移队列移除供应商 {provider_id} ({app_type}), 并清除其健康状态");

        self.invalidate_provider_caches();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;

    fn provider(id: &str, name: &str, sort_index: Option<usize>) -> Provider {
        let mut provider = Provider::with_id(
            id.to_string(),
            name.to_string(),
            serde_json::json!({"env": {"ANTHROPIC_BASE_URL": "https://api.example.com"}}),
            None,
        );
        provider.sort_index = sort_index;
        provider
    }

    fn health_rows(db: &Database) -> i64 {
        let conn = db.conn.lock().expect("conn lock");
        conn.query_row("SELECT COUNT(*) FROM provider_health", [], |row| row.get(0))
            .expect("health count")
    }

    #[test]
    fn queue_lists_by_sort_index_then_id_with_missing_index_last() {
        let db = Database::memory().expect("memory db");
        db.save_provider("claude", &provider("late", "Late", None))
            .unwrap();
        db.save_provider("claude", &provider("two", "Two", Some(2)))
            .unwrap();
        db.save_provider("claude", &provider("one", "One", Some(1)))
            .unwrap();
        db.save_provider("claude", &provider("aaa", "Aaa", Some(2)))
            .unwrap();
        for id in ["late", "two", "one", "aaa"] {
            db.add_to_failover_queue("claude", id).unwrap();
        }

        let queue = db.get_failover_queue("claude").unwrap();
        let ids: Vec<&str> = queue.iter().map(|item| item.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["one", "aaa", "two", "late"]);
        assert_eq!(queue[0].provider_name, "One");
        assert_eq!(queue[1].sort_index, Some(2));
        assert_eq!(queue[3].sort_index, None);
        // 别的 app 的队列互不干扰。
        assert_eq!(db.get_failover_queue("codex").unwrap().len(), 0);
    }

    #[test]
    fn removing_a_provider_also_clears_its_monitored_health() {
        let db = Database::memory().expect("memory db");
        db.save_provider("claude", &provider("p1", "P1", Some(1)))
            .unwrap();
        db.add_to_failover_queue("claude", "p1").unwrap();
        {
            let conn = db.conn.lock().expect("conn lock");
            conn.execute(
                "INSERT INTO provider_health (
                    provider_id, app_type, is_healthy, consecutive_failures, updated_at
                ) VALUES ('p1', 'claude', 0, 3, '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("seed health row");
        }
        assert_eq!(health_rows(&db), 1);

        db.remove_from_failover_queue("claude", "p1").unwrap();

        assert_eq!(health_rows(&db), 0);
        assert_eq!(db.get_failover_queue("claude").unwrap().len(), 0);
        // 供应商行本身还在，只是退出队列。
        assert!(db.get_provider_by_id("p1", "claude").unwrap().is_some());
    }

    #[test]
    fn a_queue_write_invalidates_the_warm_snapshot_cache() {
        let db = Database::memory().expect("memory db");
        db.save_provider("claude", &provider("warm", "Warm", Some(1)))
            .unwrap();
        db.save_provider("claude", &provider("cold", "Cold", Some(2)))
            .unwrap();
        db.add_to_failover_queue("claude", "warm").unwrap();

        // 先经冷路径填一次温缓存。
        let warm = db.get_shared_failover_queue_providers("claude").unwrap();
        assert_eq!(warm.len(), 1);
        assert_eq!(warm[0].id, "warm");
        let cached = db
            .cached_failover_queue_providers("claude")
            .expect("warm snapshot");
        assert_eq!(cached.len(), 1);

        // 入队第二个供应商后 providers 表代次前进，旧快照必须失效。
        db.add_to_failover_queue("claude", "cold").unwrap();
        assert!(db.cached_failover_queue_providers("claude").is_none());

        let requeued = db.get_shared_failover_queue_providers("claude").unwrap();
        assert_eq!(requeued.len(), 2);
        assert_eq!(requeued[0].id, "warm");
        assert_eq!(requeued[1].id, "cold");
    }

    use serde_json::json;

    #[test]
    fn provider_loading_preserves_list_and_queue_order_with_app_local_endpoints() {
        let db = Database::memory().unwrap();
        for (id, created_at, queued) in [("b", 1, true), ("a", 2, true), ("outside", 0, false)] {
            let mut provider =
                Provider::with_id(id.to_string(), id.to_string(), json!({"model": id}), None);
            provider.sort_index = Some(1);
            provider.created_at = Some(created_at);
            provider.in_failover_queue = queued;
            db.save_provider("claude", &provider).unwrap();
            db.add_custom_endpoint("claude", id, &format!("https://{id}.example.com"))
                .unwrap();
        }
        let other_app = Provider::with_id("a".into(), "other app".into(), json!({}), None);
        db.save_provider("codex", &other_app).unwrap();
        db.add_custom_endpoint("codex", "a", "https://codex.example.com")
            .unwrap();

        let all = db.get_all_providers("claude").unwrap();
        assert_eq!(
            all.keys().map(String::as_str).collect::<Vec<_>>(),
            ["outside", "b", "a"]
        );
        let queued = db.get_shared_failover_queue_providers("claude").unwrap();
        let queue = db.get_failover_queue("claude").unwrap();
        assert_eq!(
            queued.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            queue
                .iter()
                .map(|p| p.provider_id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(queued[0].id, "a");
        assert_eq!(queued[1].id, "b");
        for provider in &queued {
            let meta = provider.meta.as_ref().unwrap();
            let url = format!("https://{}.example.com", provider.id);
            assert_eq!(meta.custom_endpoints.len(), 1);
            assert_eq!(meta.custom_endpoints[&url].url, url);
            assert!(meta.custom_endpoints[&url].added_at > 0);
            assert_eq!(
                provider.settings_config["model"].as_str(),
                Some(provider.id.as_str())
            );
            assert!(provider.in_failover_queue);
            let listed = &all[&provider.id].meta.as_ref().unwrap().custom_endpoints;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[&url].url, url);
            assert_eq!(listed[&url].added_at, meta.custom_endpoints[&url].added_at);
        }
    }
}
