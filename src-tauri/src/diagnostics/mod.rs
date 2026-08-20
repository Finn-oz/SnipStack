//! 运行期「黑匣子」诊断:panic 落盘、主线程心跳看门狗、系统资源采样。
//!
//! 背景:Windows 上观察到「运行一段时间后托盘图标消失、二次启动无响应,重启才恢复」,
//! 且复现机器不总是可达。证据采集因此做进进程内,随日志一起留在用户机器上:
//! - panic hook:任何线程 panic 先写日志(含 backtrace)再走原 hook;
//! - 看门狗:后台线程周期性向主线程投递探针闭包,超时未回执 → 判定主线程卡死,
//!   记录持续时长与资源计数;连续两次未回执时(Windows)写一份 minidump 到日志目录。
//!   探针采用「投递后等回执」而非比对上次心跳时间,睡眠唤醒后不会误报;
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
/// 连续 miss 达到该值时写 minidump(即卡死约 1 分钟后)。
const DUMP_AFTER_MISSES: u32 = 2;

/// 主线程回执的探针序号。
static PROBE_ECHO: AtomicU64 = AtomicU64::new(0);

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
            }
            consecutive_misses = 0;
        } else {
            consecutive_misses += 1;
            let since = *hung_since.get_or_insert_with(Instant::now);
            log::error!(
                "diagnostics: main thread unresponsive (miss #{consecutive_misses}, ~{:?} so far); {}",
                since.elapsed(),
                resource_summary(),
            );
            if consecutive_misses >= DUMP_AFTER_MISSES && !dump_attempted {
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
