//! 运行期「黑匣子」诊断:panic 落盘、主线程心跳看门狗、系统资源采样。
//!
//! 背景:Windows 上观察到「运行一段时间后托盘图标消失、二次启动无响应,重启才恢复」,
//! 且复现机器不总是可达。证据采集因此做进进程内,随日志一起留在用户机器上:
//! - panic hook:任何线程 panic 先写日志(含 backtrace)再走原 hook;
//! - 看门狗:后台线程周期性向主线程投递探针闭包,超时未回执 → 判定主线程卡死,
//!   记录持续时长与资源计数;连续两次未回执时(Windows)写一份 minidump 到日志目录
//!   (每次卡死事件至多一份,恢复后重置;目录总量另有上限,见 windows.rs)。
//!   探针采用「投递后等回执」而非比对上次心跳时间,系统睡眠至多产生一次 miss 记录
//!   (唤醒后探针会被主线程很快消化),不足以触发 dump;
//!   预期内的主线程模态阻塞(如 Windows 拖拽的 `DoDragDrop`)由调用方通过
//!   [`expect_main_thread_block`] 标记,期间 miss 只记日志、不写 dump;
//! - 资源采样:GDI / USER / 句柄 / 工作集周期性落日志(仅 Windows 有计数),
//!   供句柄泄漏排查与 CI soak 测试断言使用(`scripts/soak-monitor.ps1` 会解析)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tauri::AppHandle;

#[cfg(target_os = "windows")]
mod windows;

/// 探针间隔。主线程正常时,每隔这么久往主线程发一次探针。
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
/// 单次探针的回执等待上限。超过即视为一次 miss。
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// 回执轮询步长。
const PROBE_POLL: Duration = Duration::from_millis(500);
/// 每多少次探针落一条资源采样日志(30s × 10 = 5 分钟)。
const RESOURCE_LOG_TICKS: u64 = 10;
/// 连续 miss 达到该值时写 minidump(即卡死约 1.5 分钟后:
/// 每轮 miss 耗时 = 30s 探针间隔 + 15s 回执等待)。
const DUMP_AFTER_MISSES: u32 = 2;
/// 处于「预期内阻塞」(如拖拽)时的升级阈值:正常拖拽不会持续这么久
/// (10 次 miss ≈ 7 分钟),仍然卡着就不再豁免——拖拽内部死锁恰是要抓的现场。
const DUMP_AFTER_MISSES_EXPECTED: u32 = 10;

/// 主线程回执的探针序号。
static PROBE_ECHO: AtomicU64 = AtomicU64::new(0);

/// 当前处于「预期内主线程阻塞」的嵌套计数(如 `DoDragDrop` 模态拖拽)。
static EXPECTED_BLOCKS: AtomicU64 = AtomicU64::new(0);

/// 标记一段预期内的主线程长阻塞。guard 存活期间看门狗对 miss 只记日志、
/// 不写 minidump,避免把合法的 OS 模态循环误判成卡死现场。
#[must_use = "guard 释放即标记结束,须持有到阻塞段落结束"]
pub struct MainThreadBlockGuard(());

// 目前只有 Windows 拖拽路径使用;macOS 拖拽是 fire-and-forget,不经此标记。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn expect_main_thread_block() -> MainThreadBlockGuard {
    EXPECTED_BLOCKS.fetch_add(1, Ordering::AcqRel);
    MainThreadBlockGuard(())
}

impl Drop for MainThreadBlockGuard {
    fn drop(&mut self) {
        EXPECTED_BLOCKS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 安装全局 panic hook:先写日志再链回原 hook(原 hook 负责 stderr 输出)。
/// 尽早调用;logger 尚未就绪时 `log::error!` 为 no-op,但原 hook 仍然生效。
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info
            .location()
            .map(|loc| loc.to_string())
            .unwrap_or_else(|| "unknown location".into());
        // release 构建默认不带 RUST_BACKTRACE,用 force_capture 保证有栈。
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(
            "panic on thread {:?} at {location}: {}\nbacktrace:\n{backtrace}",
            thread.name().unwrap_or("unnamed"),
            payload_text(info.payload()),
        );
        previous(info);
    }));
}

/// 启动看门狗线程。事件循环退出(`run_on_main_thread` 报错)后线程自然结束。
pub fn start_watchdog(app: &AppHandle) {
    let app = app.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("diagnostics-watchdog".into())
        .spawn(move || watchdog_loop(app))
    {
        log::warn!("spawn diagnostics watchdog failed: {err}");
    }
}

fn payload_text(payload: &dyn std::any::Any) -> &str {
    if let Some(text) = payload.downcast_ref::<&str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else {
        "non-string panic payload"
    }
}

fn watchdog_loop(app: AppHandle) {
    let mut seq: u64 = 0;
    let mut consecutive_misses: u32 = 0;
    let mut hung_since: Option<Instant> = None;
    let mut dump_attempted = false;

    loop {
        std::thread::sleep(PROBE_INTERVAL);

        seq += 1;
        let expected = seq;
        if app
            .run_on_main_thread(move || PROBE_ECHO.store(expected, Ordering::Release))
            .is_err()
        {
            // 事件循环已退出:正常关停路径,看门狗随之收工。
            return;
        }

        if wait_for_echo(expected) {
            if let Some(since) = hung_since.take() {
                log::warn!(
                    "diagnostics: main thread recovered after ~{:?}",
                    since.elapsed()
                );
                // 恢复后重置:下一次独立的卡死事件仍可取证(每次卡死至多一份,
                // 日志目录 MAX_HANG_DUMPS 兜底磁盘占用)。
                dump_attempted = false;
            }
            consecutive_misses = 0;
        } else {
            consecutive_misses += 1;
            let since = *hung_since.get_or_insert_with(Instant::now);
            let expected_block = EXPECTED_BLOCKS.load(Ordering::Acquire) > 0;
            if expected_block {
                log::warn!(
                    "diagnostics: probe missed during expected main-thread block (e.g. drag-out), miss #{consecutive_misses}, ~{:?} so far",
                    since.elapsed(),
                );
            } else {
                log::error!(
                    "diagnostics: main thread unresponsive (miss #{consecutive_misses}, ~{:?} so far); {}",
                    since.elapsed(),
                    resource_summary(),
                );
            }
            // 预期内阻塞只是提高阈值而非永久豁免:guard 若因阻塞段死锁而一直
            // 不释放,超过升级阈值照样取证。
            let dump_threshold = if expected_block {
                DUMP_AFTER_MISSES_EXPECTED
            } else {
                DUMP_AFTER_MISSES
            };
            if consecutive_misses >= dump_threshold && !dump_attempted {
                dump_attempted = true;
                #[cfg(target_os = "windows")]
                windows::write_hang_dump(&app);
            }
        }

        if seq.is_multiple_of(RESOURCE_LOG_TICKS) {
            log::info!("diagnostics: {}", resource_summary());
        }
    }
}

fn wait_for_echo(expected: u64) -> bool {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    while Instant::now() < deadline {
        if PROBE_ECHO.load(Ordering::Acquire) >= expected {
            return true;
        }
        std::thread::sleep(PROBE_POLL);
    }
    PROBE_ECHO.load(Ordering::Acquire) >= expected
}

fn resource_summary() -> String {
    #[cfg(target_os = "windows")]
    {
        windows::resource_summary()
    }
    #[cfg(not(target_os = "windows"))]
    {
        "resource counters unavailable on this platform".into()
    }
}
