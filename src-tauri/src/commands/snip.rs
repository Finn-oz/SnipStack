//! 截屏取字(框选 OCR)命令入口:转发到 `capture` 模块。

use tauri::AppHandle;

use crate::capture::{self, SnipSelection};
use crate::core::Result;

/// 启动截屏取字会话(托盘/前端入口;热键路径不经此命令)。
#[tauri::command]
pub async fn start_snip(app: AppHandle) -> Result<()> {
    capture::start_snip(&app)
}

/// 覆盖层页面帧图加载完成,通知显示窗口。
#[tauri::command]
pub async fn snip_overlay_ready(app: AppHandle, monitor: usize) -> Result<()> {
    capture::overlay_ready(&app, monitor)
}

/// 取指定显示器冻结帧的本地路径(前端 convertFileSrc 展示)。
#[tauri::command]
pub async fn get_snip_frame(app: AppHandle, monitor: usize) -> Result<String> {
    capture::frame_preview_path(&app, monitor)
}

/// 取消当前截屏取字会话(Esc / 右键)。
#[tauri::command]
pub async fn snip_cancel(app: AppHandle) -> Result<()> {
    capture::cancel_snip(&app)
}

/// 提交选区,后台执行 OCR 与结果处理。
#[tauri::command]
pub async fn snip_confirm(app: AppHandle, selection: SnipSelection) -> Result<()> {
    capture::confirm_snip(app, selection).await
}
