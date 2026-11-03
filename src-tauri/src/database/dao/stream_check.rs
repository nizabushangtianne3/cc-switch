//! 连通检测日志 DAO
//!
//! 连通检测已不再写日志；`stream_check_logs` 表只为兼容旧库保留，这里只负责清理残留行。

use crate::database::{lock_conn, Database};
use crate::error::AppError;

impl Database {
    /// Delete stream check logs older than `retain_days` days.
    /// Returns the number of deleted rows.
    pub fn cleanup_old_stream_check_logs(&self, retain_days: i64) -> Result<u64, AppError> {
        let cutoff = chrono::Utc::now().timestamp() - retain_days * 86400;
        let conn = lock_conn!(self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM stream_check_logs WHERE tested_at < ?1",
                [cutoff],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if deleted > 0 {
            log::info!("Cleaned up {deleted} stream_check_logs older than {retain_days} days");
        }
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// 旧库里的残留行：连通检测早已停写，只剩清理还在跑。
    fn seed_row(db: &Database, id: &str, tested_at: i64) {
        let conn = db.conn.lock().expect("db lock");
        conn.execute(
            "INSERT INTO stream_check_logs (
                 provider_id, provider_name, app_type, status, success, message,
                 response_time_ms, http_status, model_used, retry_count, tested_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                format!("Provider {id}"),
                "claude",
                "failed",
                0,
                "stale",
                1,
                500,
                "model",
                0,
                tested_at
            ],
        )
        .expect("seed the residual row");
    }

    fn row_count(db: &Database) -> i64 {
        let conn = db.conn.lock().expect("db lock");
        conn.query_row("SELECT COUNT(*) FROM stream_check_logs", [], |row| {
            row.get(0)
        })
        .expect("count the rows")
    }

    #[test]
    fn cleanup_removes_only_rows_older_than_the_retention_window() {
        let db = Database::memory().expect("memory db");
        let now = chrono::Utc::now().timestamp();
        seed_row(&db, "old", now - 30 * 86400);
        seed_row(&db, "fresh", now - 3600);

        let removed = db.cleanup_old_stream_check_logs(7).expect("clean up");
        assert_eq!(removed, 1, "只有超过 7 天的那行被删");
        assert_eq!(row_count(&db), 1, "窗口内的行留下");
    }

    #[test]
    fn cleanup_on_an_empty_table_removes_nothing() {
        let db = Database::memory().expect("memory db");
        assert_eq!(db.cleanup_old_stream_check_logs(7).unwrap(), 0);
    }
}
