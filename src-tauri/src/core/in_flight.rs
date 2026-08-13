//! 进程内「进行中」去重注册表:同一 key 的任务同时只允许一份。
//!
//! 守卫式 API:`try_begin` 成功返回 [`InFlightGuard`],Drop 时自动注销——
//! 任务 panic 或 future 被丢弃也不会把 key 永久滞留在集合里。

use std::sync::Mutex;

use super::sync::lock_unpoisoned;

pub struct InFlight(Mutex<Vec<String>>);

impl InFlight {
    pub const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    /// key 空闲时登记并返回守卫;已被占用(同任务进行中)返回 `None`。
    pub fn try_begin(&'static self, key: &str) -> Option<InFlightGuard> {
        let mut keys = lock_unpoisoned(&self.0);
        if keys.iter().any(|existing| existing == key) {
            return None;
        }
        keys.push(key.to_owned());
        Some(InFlightGuard {
            registry: self,
            key: key.to_owned(),
        })
    }
}

pub struct InFlightGuard {
    registry: &'static InFlight,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.registry.0).retain(|existing| existing != &self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static REGISTRY: InFlight = InFlight::new();

    #[test]
    fn rejects_duplicate_and_releases_on_drop() {
        let guard = REGISTRY.try_begin("a").expect("first begin");
        assert!(
            REGISTRY.try_begin("a").is_none(),
            "duplicate must be rejected"
        );
        assert!(REGISTRY.try_begin("b").is_some(), "other keys unaffected");
        drop(guard);
        assert!(REGISTRY.try_begin("a").is_some(), "released after drop");
    }
}
