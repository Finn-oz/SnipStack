//! Windows 侧诊断实现:GDI/USER/句柄/内存计数 + 主线程卡死时的自采 minidump。
//!
//! - 资源计数走 `GetGuiResources` / `GetProcessHandleCount` / `GetProcessMemoryInfo`,
//!   都是进程内直读,毫秒级,可在看门狗线程放心调用。
//!   GDI / USER 对象单进程上限各 10000,逼近上限时窗口/菜单/托盘图标会静默创建失败,
//!   是「托盘图标消失 + 窗口弹不出」的候选病因之一,采样曲线用于确认或排除。
//! - minidump 用 `MiniDumpWriteDump` 对本进程自拍。文档推荐 out-of-process,但这里
//!   只在「主线程已卡死」的兜底场景由看门狗线程执行,失败也只是少一份证据;
//!   dump 类型选 Normal + ThreadInfo:含全部线程栈,足够定位卡死点,体积也小。

use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use winapi::um::processthreadsapi::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessHandleCount,
};
use winapi::um::psapi::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use winapi::um::winuser::{GetGuiResources, GR_GDIOBJECTS, GR_USEROBJECTS};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump, MINIDUMP_TYPE,
};

/// 日志目录里最多保留的卡死 dump 数,防止反复卡死刷满磁盘。
const MAX_HANG_DUMPS: usize = 3;

pub(super) fn resource_summary() -> String {
    unsafe {
        let process = GetCurrentProcess();
        let gdi = GetGuiResources(process, GR_GDIOBJECTS);
        let user = GetGuiResources(process, GR_USEROBJECTS);

        let mut handles: u32 = 0;
        GetProcessHandleCount(process, &mut handles);

        let mut mem: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        mem.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let working_set_mb = if GetProcessMemoryInfo(process, &mut mem, mem.cb) != 0 {
            mem.WorkingSetSize / (1024 * 1024)
        } else {
            0
        };

        format!("gdi={gdi} user={user} handles={handles} working_set_mb={working_set_mb}")
    }
}

/// 往日志目录写一份本进程 minidump。只写一次证据,失败仅记日志,不重试。
pub(super) fn write_hang_dump(app: &AppHandle) {
    match try_write_dump(app) {
        Ok(Some(path)) => log::error!("diagnostics: hang minidump written to {path:?}"),
        Ok(None) => {
            log::warn!("diagnostics: skip hang minidump, already {MAX_HANG_DUMPS} dumps in log dir")
        }
        Err(err) => log::error!("diagnostics: write hang minidump failed: {err:?}"),
    }
}

fn try_write_dump(app: &AppHandle) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;

    let dir = app.path().app_log_dir().context("resolve app log dir")?;
    std::fs::create_dir_all(&dir).context("create log dir")?;

    let existing = std::fs::read_dir(&dir)
        .context("read log dir")?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("hang-") && name.ends_with(".dmp")
        })
        .count();
    if existing >= MAX_HANG_DUMPS {
        return Ok(None);
    }

    let path = dir.join(format!(
        "hang-{}.dmp",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let file = File::create(&path).context("create dump file")?;

    let dump_type = MINIDUMP_TYPE(MiniDumpNormal.0 | MiniDumpWithThreadInfo.0);
    let result = unsafe {
        MiniDumpWriteDump(
            windows::Win32::System::Threading::GetCurrentProcess(),
            GetCurrentProcessId(),
            HANDLE(file.as_raw_handle() as isize),
            dump_type,
            None,
            None,
            None,
        )
    };
    if let Err(err) = result {
        let _ = std::fs::remove_file(&path);
        return Err(anyhow::anyhow!("MiniDumpWriteDump failed: {err}"));
    }

    Ok(Some(path))
}
