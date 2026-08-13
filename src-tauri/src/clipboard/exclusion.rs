//! 剪贴板监控排除约定(仅 Windows 生效)。
//!
//! 密码管理器等应用通过在剪贴板上注册特定具名格式,请求监控类程序忽略本次内容:
//! - `ExcludeClipboardContentFromMonitorProcessing`:微软官方约定,存在即排除。
//! - `Clipboard Viewer Ignore`:KeePass 生态事实标准(名字含空格),存在即排除。
//! - `CanIncludeInClipboardHistory`:DWORD 值为 0 时排除(0 = 请勿进入历史)。
//!
//! `CanUploadToCloudClipboard` 只约束云剪贴板同步;本应用数据纯本地,不据此排除。
//! 参考:微软 Clipboard Formats 文档与 CopyQ/KeePass 的兼容实现。

#[cfg(target_os = "windows")]
pub use windows_impl::should_exclude_current;

#[cfg(not(target_os = "windows"))]
pub fn should_exclude_current() -> bool {
    false
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::sync::OnceLock;

    use windows::core::w;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    struct ExclusionFormats {
        exclude_from_monitor: u32,
        viewer_ignore: u32,
        can_include_in_history: u32,
    }

    fn formats() -> &'static ExclusionFormats {
        static FORMATS: OnceLock<ExclusionFormats> = OnceLock::new();
        FORMATS.get_or_init(|| unsafe {
            ExclusionFormats {
                exclude_from_monitor: RegisterClipboardFormatW(w!(
                    "ExcludeClipboardContentFromMonitorProcessing"
                )),
                viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
                can_include_in_history: RegisterClipboardFormatW(w!(
                    "CanIncludeInClipboardHistory"
                )),
            }
        })
    }

    /// 判断当前剪贴板内容是否按约定应被监控方忽略。
    /// 检测失败(格式注册失败、剪贴板被占用等)时一律按「不排除」处理,宁可多记一条也不静默丢数据。
    pub fn should_exclude_current() -> bool {
        let formats = formats();

        if formats.exclude_from_monitor != 0
            && unsafe { IsClipboardFormatAvailable(formats.exclude_from_monitor) }.is_ok()
        {
            log::info!("clipboard exclusion: ExcludeClipboardContentFromMonitorProcessing present");
            return true;
        }
        if formats.viewer_ignore != 0
            && unsafe { IsClipboardFormatAvailable(formats.viewer_ignore) }.is_ok()
        {
            log::info!("clipboard exclusion: Clipboard Viewer Ignore present");
            return true;
        }
        if formats.can_include_in_history != 0
            && unsafe { IsClipboardFormatAvailable(formats.can_include_in_history) }.is_ok()
            && read_dword_format(formats.can_include_in_history) == Some(0)
        {
            log::info!("clipboard exclusion: CanIncludeInClipboardHistory = 0");
            return true;
        }
        false
    }

    /// 读取具名 DWORD 格式的值;失败(剪贴板被占用/负载异常)返回 `None`。
    /// 不做 sleep 重试:本函数跑在 watcher 回调线程上,语义又是 fail-open,
    /// 阻塞等待的代价高于偶尔漏判一次。
    fn read_dword_format(format: u32) -> Option<u32> {
        if unsafe { OpenClipboard(None) }.is_err() {
            return None;
        }
        let value = unsafe {
            GetClipboardData(format).ok().and_then(|handle| {
                let hglobal = HGLOBAL(handle.0 as *mut core::ffi::c_void);
                // 负载大小由源应用决定,读 DWORD 前必须确认至少 4 字节,否则越界读。
                if GlobalSize(hglobal) < 4 {
                    return None;
                }
                let ptr = GlobalLock(hglobal) as *const u32;
                if ptr.is_null() {
                    return None;
                }
                let value = ptr.read_unaligned();
                // GlobalUnlock 在解锁完成时按 API 约定返回 FALSE,windows crate 映射为 Err,忽略。
                let _ = GlobalUnlock(hglobal);
                Some(value)
            })
        };
        let _ = unsafe { CloseClipboard() };
        value
    }
}
