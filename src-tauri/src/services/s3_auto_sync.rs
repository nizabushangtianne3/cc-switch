//! S3 自动同步的后端钩子与外部入口。
//!
//! 共用的去抖合并、抑制、worker 循环都在 [`crate::services::auto_sync`]，这里只
//! 提供 S3 特有的三个差异点：上传走哪个 service、配哪种 settings、发什么事件。

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, OnceLock};

use tauri::AppHandle;
use tokio::sync::mpsc::Sender;

use crate::error::AppError;
use crate::services::auto_sync::{self, AutoSyncKind};
use crate::services::s3_sync;
use crate::settings;

static DB_CHANGE_TX: OnceLock<Sender<String>> = OnceLock::new();
static AUTO_SYNC_SUPPRESS_DEPTH: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct S3AutoSync;

impl AutoSyncKind for S3AutoSync {
    const TAG: &'static str = "S3";
    const EVENT: &'static str = "s3-sync-status-updated";

    fn change_tx() -> &'static OnceLock<Sender<String>> {
        &DB_CHANGE_TX
    }

    fn suppress_depth() -> &'static AtomicUsize {
        &AUTO_SYNC_SUPPRESS_DEPTH
    }

    fn persist_error(error: &AppError) {
        let Some(mut sync) = settings::get_s3_sync_settings() else {
            return;
        };
        sync.status.last_error = Some(error.to_string());
        sync.status.last_error_source = Some("auto".to_string());
        let _ = settings::update_s3_sync_status(sync.status);
    }

    fn upload(
        db: Arc<crate::database::Database>,
        app: AppHandle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AppError>> + Send>> {
        Box::pin(async move {
            let Some(mut sync) = settings::get_s3_sync_settings() else {
                return Ok(());
            };
            // worker 是无条件起的，所以「这一端没开」必须在这里挡掉。
            if !(sync.enabled && sync.auto_sync) {
                return Ok(());
            }
            let result = s3_sync::run_with_sync_lock(s3_sync::upload(&db, &mut sync)).await;
            match result {
                Ok(_) => {
                    auto_sync::emit_status::<Self>(&app, "success", None);
                    Ok(())
                }
                Err(err) => {
                    Self::persist_error(&err);
                    auto_sync::emit_status::<Self>(&app, "error", Some(&err.to_string()));
                    Err(err)
                }
            }
        })
    }
}

/// 抑制 S3 的自动同步。外部只有 `commands::s3_sync` 用它。
pub(crate) type AutoSyncSuppressionGuard = auto_sync::SuppressionGuard<S3AutoSync>;

/// 只有 tests 用它判空；生产路径的抑制判断在泛型 notify_db_changed 里。
#[cfg(test)]
pub(crate) fn is_auto_sync_suppressed() -> bool {
    auto_sync::is_auto_sync_suppressed::<S3AutoSync>()
}

pub fn notify_db_changed(table: &str) {
    auto_sync::notify_db_changed::<S3AutoSync>(table)
}

pub fn start_worker(db: Arc<crate::database::Database>, app: tauri::AppHandle) {
    auto_sync::start_worker::<S3AutoSync>(db, app)
}

#[cfg(test)]
mod tests {
    use super::{AutoSyncSuppressionGuard, S3AutoSync};
    use crate::settings::S3SyncSettings;
    use serial_test::serial;

    #[test]
    #[serial]
    fn suppression_guard_enables_and_restores_state() {
        assert!(!crate::services::auto_sync::is_auto_sync_suppressed::<
            S3AutoSync,
        >());
        {
            let _guard = AutoSyncSuppressionGuard::new();
            assert!(crate::services::auto_sync::is_auto_sync_suppressed::<
                S3AutoSync,
            >());
        }
        assert!(!crate::services::auto_sync::is_auto_sync_suppressed::<
            S3AutoSync,
        >());
    }

    #[test]
    fn s3_auto_sync_gate_needs_both_flags() {
        let gate = |enabled: bool, auto_sync: bool| {
            let settings = S3SyncSettings {
                enabled,
                auto_sync,
                ..S3SyncSettings::default()
            };
            settings.enabled && settings.auto_sync
        };
        assert!(gate(true, true));
        assert!(!gate(false, true));
        assert!(!gate(true, false));
    }

    #[test]
    fn service_layer_does_not_depend_on_commands_layer() {
        let source = include_str!("s3_auto_sync.rs");
        let needle = ["crate", "commands", ""].join("::");
        assert!(
            !source.contains(&needle),
            "services layer should not depend on commands layer"
        );
    }
}
