//! 自动同步的公共骨架。
//!
//! S3 与 WebDAV 两端的去抖合并、抑制、变更监听、worker 循环完全一致，差异只在
//! 「用哪个 service 上传、配哪种 settings、发什么事件、日志打什么前缀」。这里按
//! [`AutoSyncKind`] 收成一份实现，两个后端各自实现这几个钩子即可。
//!
//! 有一处**故意不合并**：变更通道和抑制深度两端各自持有 static。手动跑某一端的
//! 同步命令时只抑制自己那一端的自动同步，另一端的自动同步仍要被本次库变更触发
//! ——见 `commands/s3_sync.rs` / `commands/webdav_sync.rs` 各自包住自己的命令。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{channel, Receiver, Sender};

use crate::database::Database;
use crate::error::AppError;
use crate::services::sync_protocol::should_trigger_auto_sync_for_table;

pub(crate) const MAX_AUTO_SYNC_WAIT_MS: u64 = 10_000;
const AUTO_SYNC_DEBOUNCE_MS: u64 = 1000;

/// 一个后端自动同步要执行的那一次上传。
///
/// 做成关联函数而不是字段，是为了让每个后端保留自己的 `OnceLock` / `AtomicUsize`
/// static：Rust 没有泛型 static，合成一份共享 static 会改变抑制语义。
pub(crate) trait AutoSyncKind: Send + Sync + 'static {
    /// 日志前缀，例如 "S3" / "WebDAV"。
    const TAG: &'static str;
    /// 发到前端的同步状态事件名。
    const EVENT: &'static str;
    /// 本后端自己的变更通道。端点之间刻意隔离，不共享。
    fn change_tx() -> &'static OnceLock<Sender<String>>;
    /// 本后端自己的抑制深度。手动同步只抑制自己这一端。
    fn suppress_depth() -> &'static AtomicUsize;
    /// 把失败写回这一端的状态。
    fn persist_error(error: &AppError);
    /// 执行一次上传；成功/失败都自行 emit 状态。
    fn upload(
        db: Arc<Database>,
        app: AppHandle,
    ) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>>;
}

/// 抑制本端的自动同步；Drop 时恢复。每个后端一份深度。
pub(crate) struct SuppressionGuard<K: AutoSyncKind> {
    _kind: std::marker::PhantomData<K>,
}

impl<K: AutoSyncKind> SuppressionGuard<K> {
    pub fn new() -> Self {
        K::suppress_depth().fetch_add(1, Ordering::SeqCst);
        Self {
            _kind: std::marker::PhantomData,
        }
    }
}

impl<K: AutoSyncKind> Drop for SuppressionGuard<K> {
    fn drop(&mut self) {
        let _ = K::suppress_depth().fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            Some(value.saturating_sub(1))
        });
    }
}

pub(crate) fn is_auto_sync_suppressed<K: AutoSyncKind>() -> bool {
    K::suppress_depth().load(Ordering::SeqCst) > 0
}

pub fn should_trigger_for_table(table: &str) -> bool {
    should_trigger_auto_sync_for_table(table)
}

pub(crate) fn enqueue_change_signal(tx: &Sender<String>, table: &str) -> bool {
    match tx.try_send(table.to_string()) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => false,
    }
}

pub(crate) fn auto_sync_wait_duration(started_at: Instant, now: Instant) -> Option<Duration> {
    let max_wait = Duration::from_millis(MAX_AUTO_SYNC_WAIT_MS);
    let debounce = Duration::from_millis(AUTO_SYNC_DEBOUNCE_MS);
    let elapsed = now.saturating_duration_since(started_at);
    if elapsed >= max_wait {
        return None;
    }
    Some(debounce.min(max_wait - elapsed))
}

pub(crate) fn emit_status<K: AutoSyncKind>(app: &AppHandle, status: &str, error: Option<&str>) {
    let payload = match error {
        Some(message) => json!({
            "source": "auto",
            "status": status,
            "error": message,
        }),
        None => json!({
            "source": "auto",
            "status": status,
        }),
    };

    if let Err(err) = app.emit(K::EVENT, payload) {
        log::debug!(
            "[{}] failed to emit sync status update event: {err}",
            K::TAG
        );
    }
}

pub fn notify_db_changed<K: AutoSyncKind>(table: &str) {
    if is_auto_sync_suppressed::<K>() {
        return;
    }
    if !should_trigger_for_table(table) {
        return;
    }
    let Some(tx) = K::change_tx().get() else {
        return;
    };
    let _ = enqueue_change_signal(tx, table);
}

pub fn start_worker<K: AutoSyncKind>(db: Arc<Database>, app: tauri::AppHandle) {
    if K::change_tx().get().is_some() {
        return;
    }

    // Buffer size 1 is enough: we only need "dirty" signals, not every event.
    let (tx, rx) = channel::<String>(1);
    if K::change_tx().set(tx).is_err() {
        return;
    }

    tauri::async_runtime::spawn(run_worker_loop::<K>(db, rx, app));
}

async fn run_worker_loop<K: AutoSyncKind>(
    db: Arc<Database>,
    mut rx: Receiver<String>,
    app: tauri::AppHandle,
) {
    while let Some(first_table) = rx.recv().await {
        let started_at = Instant::now();
        let mut merged_count = 1usize;

        while let Some(wait_for) = auto_sync_wait_duration(started_at, Instant::now()) {
            let timeout = tokio::time::timeout(wait_for, rx.recv()).await;

            match timeout {
                Ok(Some(_)) => merged_count += 1,
                Ok(None) => return,
                Err(_) => break,
            }
        }

        log::debug!(
            "[{}][AutoSync] Triggered by table={first_table}, merged_changes={merged_count}",
            K::TAG
        );

        if let Err(err) = K::upload(db.clone(), app.clone()).await {
            log::warn!("[{}][AutoSync] Upload failed: {err}", K::TAG);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // 直接拿两个真实后端来测：不造 mock kind，且隔离语义测的是线上这一对。
    use crate::services::s3_auto_sync::S3AutoSync;
    use crate::services::webdav_auto_sync::WebDavAutoSync;

    #[test]
    fn should_trigger_sync_for_config_tables_only() {
        assert!(should_trigger_for_table("providers"));
        assert!(should_trigger_for_table("profiles"));
        assert!(should_trigger_for_table("settings"));
        assert!(!should_trigger_for_table("proxy_request_logs"));
        assert!(!should_trigger_for_table("provider_health"));
    }

    #[test]
    fn max_wait_caps_flush_latency_for_continuous_events() {
        let started = Instant::now();
        let later = started + Duration::from_millis(MAX_AUTO_SYNC_WAIT_MS + 1);
        assert!(auto_sync_wait_duration(started, later).is_none());

        // 未到上限时按去抖窗口等待，且不会超过剩余额度。
        let soon = started + Duration::from_millis(AUTO_SYNC_DEBOUNCE_MS / 2);
        let wait = auto_sync_wait_duration(started, soon).expect("within max wait");
        // 等待窗口本身就是整个去抖间隔，只被「距上限还剩多少」压短。
        assert_eq!(wait, Duration::from_millis(AUTO_SYNC_DEBOUNCE_MS));

        // 逼近上限时被剩余额度截断：只剩 300ms 就只能等 300ms。
        let near_limit = started + Duration::from_millis(MAX_AUTO_SYNC_WAIT_MS - 300);
        let capped = auto_sync_wait_duration(started, near_limit).expect("still within max wait");
        assert_eq!(capped, Duration::from_millis(300));
    }

    #[tokio::test]
    async fn enqueue_change_signal_drops_when_channel_is_full() {
        let (tx, _rx) = channel::<String>(1);
        assert!(enqueue_change_signal(&tx, "providers"));
        assert!(!enqueue_change_signal(&tx, "providers"));
    }

    #[tokio::test]
    async fn notify_db_changed_ignores_unsupported_tables_and_missing_channel() {
        // 通道还没起：不该 panic，直接返回。
        notify_db_changed::<S3AutoSync>("providers");
        notify_db_changed::<S3AutoSync>("proxy_request_logs");
    }

    #[test]
    #[serial]
    fn each_kind_keeps_its_own_suppression_depth() {
        // 这是刻意保留的隔离语义：抑制一端不影响另一端。
        assert!(!is_auto_sync_suppressed::<S3AutoSync>());
        assert!(!is_auto_sync_suppressed::<WebDavAutoSync>());

        let first_guard = SuppressionGuard::<S3AutoSync>::new();
        assert!(is_auto_sync_suppressed::<S3AutoSync>());
        assert!(
            !is_auto_sync_suppressed::<WebDavAutoSync>(),
            "第一端的抑制不能波及第二端"
        );
        drop(first_guard);
        assert!(!is_auto_sync_suppressed::<S3AutoSync>());
    }

    #[test]
    #[serial]
    fn suppression_depth_saturates_instead_of_going_negative() {
        let _outer = SuppressionGuard::<WebDavAutoSync>::new();
        {
            let _nested = SuppressionGuard::<WebDavAutoSync>::new();
            assert_eq!(WebDavAutoSync::suppress_depth().load(Ordering::SeqCst), 2);
        }
        assert_eq!(WebDavAutoSync::suppress_depth().load(Ordering::SeqCst), 1);
        // 手动多减一次，深度停在 0 而不是回绕。
        WebDavAutoSync::suppress_depth().fetch_sub(1, Ordering::SeqCst);
        let _ = WebDavAutoSync::suppress_depth().fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |v| Some(v.saturating_sub(1)),
        );
        assert_eq!(WebDavAutoSync::suppress_depth().load(Ordering::SeqCst), 0);
    }
}
