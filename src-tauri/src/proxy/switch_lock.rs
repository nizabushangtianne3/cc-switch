//! Per-app switch lock
//!
//! 确保同一应用同时只有一个切换、进入 / 退出代理的操作在执行，防止并发操作导致
//! 指针、代理路由和客户端文件不一致。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

/// 每个应用类型一把互斥锁，保证同一应用的切换操作串行执行。
///
/// 不同应用之间（如 Claude 和 Codex）可以并行切换。
#[derive(Clone, Default)]
pub struct SwitchLockManager {
    locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
}

impl SwitchLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取指定应用的切换锁。
    ///
    /// 返回 `OwnedMutexGuard`，持有期间同一 `app_type` 的其他切换会排队等待。
    pub async fn lock_for_app(&self, app_type: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let locks = self.locks.read().await;
            if let Some(lock) = locks.get(app_type) {
                lock.clone()
            } else {
                drop(locks);
                let mut locks = self.locks.write().await;
                locks
                    .entry(app_type.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            }
        };
        lock.lock_owned().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 同一 app 的第二个 lock 调用必须等第一个 guard 释放，切换才是串行的。
    #[tokio::test]
    async fn same_app_switches_are_serialized() {
        let manager = SwitchLockManager::new();
        let entered = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let first = manager.lock_for_app("claude").await;
        let second_guard = {
            let entered = entered.clone();
            let max_concurrent = max_concurrent.clone();
            tokio::spawn(async move {
                let guard = manager.lock_for_app("claude").await;
                let current = entered.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(current, Ordering::SeqCst);
                drop(guard);
            })
        };
        // 让 second 先排队：如果 first 不释放，它永远进不到计数段
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            entered.load(Ordering::SeqCst),
            0,
            "first guard 未释放时，同一 app 的第二个切换不能进入临界区"
        );
        drop(first);
        second_guard.await.expect("join second switch");
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
    }

    /// 不同 app 各自独立，互不阻塞。
    #[tokio::test]
    async fn different_apps_do_not_block_each_other() {
        let manager = SwitchLockManager::new();
        let _claude = manager.lock_for_app("claude").await;
        let codex = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            manager.lock_for_app("codex"),
        )
        .await
        .expect("不同 app 的锁必须立即可用");
        drop(codex);
    }

    /// 同一 app 重复取锁拿到的是同一把锁（排队有意义），且 guard 释放后可再取。
    #[tokio::test]
    async fn repeated_locks_refer_to_the_same_lock_and_reacquire() {
        let manager = SwitchLockManager::new();
        {
            let locks = manager.locks.read().await;
            assert!(locks.is_empty(), "未取锁前不该创建条目");
        }
        let first = manager.lock_for_app("codex").await;
        {
            let locks = manager.locks.read().await;
            assert_eq!(locks.len(), 1, "取锁后登记一个 app 条目");
        }
        drop(first);
        let second = manager.lock_for_app("codex").await;
        {
            let locks = manager.locks.read().await;
            assert_eq!(locks.len(), 1, "重复取锁复用同一个条目，不无限增长");
        }
        drop(second);
        manager.lock_for_app("codex").await;
    }
}
