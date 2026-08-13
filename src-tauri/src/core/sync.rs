//! 并发小工具。

use std::sync::{Mutex, MutexGuard};

/// 取锁并从锁毒中恢复。适用场景:锁内是会话/缓存态,持锁线程 panic 不会留下
/// 需要回滚的半成品,恢复继续比连锁 panic 更合理。恢复时记告警便于排查。
pub fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        log::warn!("recovered from poisoned mutex");
        poisoned.into_inner()
    })
}
