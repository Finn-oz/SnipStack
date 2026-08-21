use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

use crate::core::Result;

const STATE_FILENAME: &str = "window-state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 交给持久化线程的一次落盘任务。generation 单调递增,
/// 写线程据此丢弃过期快照,保证磁盘内容只会前进不会回退。
struct PersistJob {
    generation: u64,
    path: PathBuf,
    json: String,
}

/// path 与 states 合在一把锁下:曾经分作 `RwLock<PathBuf>` + `Mutex<HashMap>`,
/// `save`(states → path)与 `rebase`(path → states)取锁顺序相反,存在 ABBA 死锁。
/// 单锁后不存在锁顺序问题,快照的 path/states 也天然一致。
struct StoreInner {
    path: PathBuf,
    states: HashMap<String, WindowState>,
}

pub struct WindowStateStore {
    inner: Mutex<StoreInner>,
    /// `save` 的落盘走专用线程:`hide_window` 等高频路径在主线程上执行,
    /// 同步 `fs::write` 在杀软扫描 / 云盘重定向 / 磁盘异常时没有超时,
    /// 会把主线程(乃至整个事件循环)堵死——见「托盘图标消失」排查。
    persist_tx: mpsc::Sender<PersistJob>,
    generation: AtomicU64,
    last_written: Arc<AtomicU64>,
    /// 序列化写盘:持久化线程、`flush_blocking`(退出路径)与 `rebase` 的
    /// 作废操作互斥。锁顺序约定:先 `inner` 后 `write_lock`,反向禁止
    /// (`write_snapshot` 只拿 `write_lock`,不碰 `inner`)。
    write_lock: Arc<Mutex<()>>,
}

impl WindowStateStore {
    pub fn new(app: &AppHandle) -> Result<Self> {
        let dir = crate::core::paths::state_dir(app)?;

        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;

        let path = dir.join(STATE_FILENAME);

        let states = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                    log::warn!("failed to parse window state at {path:?}, using defaults: {e}");
                    HashMap::new()
                }),
                Err(e) => {
                    log::warn!("failed to read window state at {path:?}, using defaults: {e}");
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        log::info!("window state store ready at {path:?}");

        let last_written = Arc::new(AtomicU64::new(0));
        let write_lock = Arc::new(Mutex::new(()));
        let (persist_tx, persist_rx) = mpsc::channel::<PersistJob>();
        spawn_persist_worker(
            persist_rx,
            Arc::clone(&last_written),
            Arc::clone(&write_lock),
        );

        Ok(Self {
            inner: Mutex::new(StoreInner { path, states }),
            persist_tx,
            generation: AtomicU64::new(0),
            last_written,
            write_lock,
        })
    }

    fn lock_inner(&self, op: &str) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            log::error!("window state mutex poisoned on {op}, recovering");
            poisoned.into_inner()
        })
    }

    /// 在 `inner` 锁内做一次快照,发放单调递增的 generation。
    fn snapshot_job(&self, inner: &StoreInner) -> Result<PersistJob> {
        let json = serde_json::to_string_pretty(&inner.states)
            .context("failed to serialize window states")?;
        Ok(PersistJob {
            generation: self.generation.fetch_add(1, Ordering::AcqRel) + 1,
            path: inner.path.clone(),
            json,
        })
    }

    /// 更新内存态并异步落盘。调用方(常在主线程)只承担序列化开销,不碰磁盘。
    pub fn save(&self, label: &str, state: WindowState) -> Result<()> {
        let job = {
            let mut inner = self.lock_inner("save");
            inner.states.insert(label.to_owned(), state);
            self.snapshot_job(&inner)?
        };

        // 持久化线程若已死(不应发生),退回同步写,宁慢勿丢。
        if let Err(mpsc::SendError(job)) = self.persist_tx.send(job) {
            log::warn!("window state persist worker gone, falling back to sync write");
            write_snapshot(&job, &self.last_written, &self.write_lock);
        }
        Ok(())
    }

    /// 同步落盘当前快照。仅退出路径使用:进程随后就没了,必须等写完。
    pub fn flush_blocking(&self) {
        let job = {
            let inner = self.lock_inner("flush");
            match self.snapshot_job(&inner) {
                Ok(job) => job,
                Err(err) => {
                    log::warn!("serialize window states on flush failed: {err}");
                    return;
                }
            }
        };
        write_snapshot(&job, &self.last_written, &self.write_lock);
    }

    pub fn get(&self, label: &str) -> Option<WindowState> {
        self.lock_inner("get").states.get(label).cloned()
    }

    /// 数据目录热切换后重新绑定窗口状态文件，并重新读取新目录里的状态。
    pub fn rebase(&self, app: &AppHandle) -> Result<()> {
        let dir = crate::core::paths::state_dir(app)?;
        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;
        let path = dir.join(STATE_FILENAME);
        let next_states = load_states(&path);

        let cutoff = {
            let mut inner = self.lock_inner("rebase");
            inner.path = path;
            inner.states = next_states;
            // 切换点之前发放的 generation 全部作废;之后的 save 已带新路径,不受影响。
            self.generation.load(Ordering::Acquire)
        };

        // 与写盘互斥地作废旧目录任务:等正在写的旧任务收尾,之后队列里
        // generation ≤ cutoff 的过期快照会被 write_snapshot 统一跳过。
        {
            let _guard = self.write_lock.lock().unwrap_or_else(|poisoned| {
                log::error!("window state write lock poisoned on rebase, recovering");
                poisoned.into_inner()
            });
            self.last_written.fetch_max(cutoff, Ordering::AcqRel);
        }
        Ok(())
    }
}

fn spawn_persist_worker(
    rx: mpsc::Receiver<PersistJob>,
    last_written: Arc<AtomicU64>,
    write_lock: Arc<Mutex<()>>,
) {
    if let Err(err) = std::thread::Builder::new()
        .name("window-state-persist".into())
        .spawn(move || {
            while let Ok(mut job) = rx.recv() {
                // 合并积压:磁盘上只需要最新快照。按 generation 取最大,不能按
                // 到达顺序取最后——generation 在 inner 锁内发放、send 在锁外,
                // 并发 save 时通道内可能乱序,按到达序会把旧快照当成最新写盘。
                while let Ok(other) = rx.try_recv() {
                    if other.generation > job.generation {
                        job = other;
                    }
                }
                write_snapshot(&job, &last_written, &write_lock);
            }
            // sender 全部释放(store 析构)→ 线程自然退出。
        })
    {
        log::warn!("spawn window state persist worker failed: {err}");
    }
}

/// 写盘走 tmp + rename,进程在写入途中被杀也不会留下截断的 JSON。
/// generation 已落后于 `last_written` 的过期快照直接跳过。
fn write_snapshot(job: &PersistJob, last_written: &AtomicU64, write_lock: &Mutex<()>) {
    let _guard = write_lock.lock().unwrap_or_else(|poisoned| {
        log::error!("window state write lock poisoned, recovering");
        poisoned.into_inner()
    });
    if job.generation <= last_written.load(Ordering::Acquire) {
        return;
    }

    let tmp = job.path.with_extension("json.tmp");
    let result = fs::write(&tmp, &job.json).and_then(|_| fs::rename(&tmp, &job.path));
    match result {
        Ok(()) => last_written.store(job.generation, Ordering::Release),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            log::warn!("write window state to {:?} failed: {err}", job.path);
        }
    }
}

fn load_states(path: &Path) -> HashMap<String, WindowState> {
    if !path.exists() {
        return HashMap::new();
    }

    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            log::warn!("failed to parse window state at {path:?}, using defaults: {e}");
            HashMap::new()
        }),
        Err(e) => {
            log::warn!("failed to read window state at {path:?}, using defaults: {e}");
            HashMap::new()
        }
    }
}

/// 读取窗口当前的实时几何（`outer_position` + `inner_size`）并落盘。
/// 在隐藏 / 关闭 / 退出等可靠生命周期点调用即可捕获用户的移动与缩放。
pub fn save_window_state(app: &AppHandle, label: &str) -> Result<()> {
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| anyhow::anyhow!("window not found: {label}"))?;

    let pos = window.outer_position().map_err(|e| anyhow::anyhow!(e))?;
    let size = window.inner_size().map_err(|e| anyhow::anyhow!(e))?;

    let store = app.state::<WindowStateStore>();
    store.save(
        label,
        WindowState {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
        },
    )
}

/// 恢复窗口的尺寸 + 位置。无存档返回 `Ok(false)`。
///
/// 始终恢复存档尺寸；位置在恢复前校验是否仍位于可用显示器范围内：
/// 若上次所在显示器已被拔出，则 fallback 到当前光标所在屏幕的中心，
/// 避免窗口出现在不可见的虚拟坐标区域。
pub fn restore_window_state(app: &AppHandle, label: &str) -> Result<bool> {
    let store = app.state::<WindowStateStore>();
    let Some(state) = store.get(label) else {
        return Ok(false);
    };

    let window = app
        .get_webview_window(label)
        .ok_or_else(|| anyhow::anyhow!("window not found: {label}"))?;

    window
        .set_size(PhysicalSize::new(state.width, state.height))
        .map_err(|e| anyhow::anyhow!(e))?;

    let monitors = window
        .available_monitors()
        .map_err(|e| anyhow::anyhow!(e))?;
    let on_screen = monitors.iter().any(|m| {
        let mx = m.position().x;
        let my = m.position().y;
        let mw = m.size().width as i32;
        let mh = m.size().height as i32;
        state.x >= mx && state.x < mx + mw && state.y >= my && state.y < my + mh
    });

    if on_screen {
        window
            .set_position(PhysicalPosition::new(state.x, state.y))
            .map_err(|e| anyhow::anyhow!(e))?;
    } else {
        super::position::center_on_cursor_monitor(&window)?;
    }

    Ok(true)
}
