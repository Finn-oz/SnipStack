//! 截屏取字(框选 OCR)会话:冻结帧捕获、每显示器覆盖层窗口、选区裁剪与结果处理。
//!
//! 流程:热键/托盘触发 `start_snip` → xcap 抓取各显示器物理像素帧 → 每显示器建一个
//! 无边框置顶覆盖层窗口(前端 `/snip` 路由显示冻结帧 + 拖拽框选)→ 前端提交 CSS 像素
//! 选区 → 按显示器 scale factor 折算回物理像素裁剪 → OCR → 写剪贴板 + 入历史。
//!
//! 坐标约定:`SnipFrame` 内全部为物理像素;与前端交互的选区为该覆盖层窗口内的
//! CSS 像素(每个覆盖层恰好铺满一个显示器,折算只需乘 scale factor)。

use std::path::PathBuf;
use std::sync::Mutex;

use image::{ImageEncoder, RgbaImage};
use serde::Deserialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use crate::core::{AppError, Result};
use crate::db::items::content_hash;
use crate::db::models::{ClipboardItem, ClipboardKind, Platform};
use crate::settings::SettingsStore;
use crate::{clipboard, ocr};

/// 覆盖层窗口 label 前缀,后跟显示器序号。
pub const OVERLAY_LABEL_PREFIX: &str = "snip-overlay-";
/// 与前端 src/constants/events.ts 的 TAURI_EVENT.SNIP_DONE 一一对应。
pub const SNIP_DONE_EVENT: &str = "snip://done";

/// 选区过小视为误触,按取消处理(物理像素)。
const MIN_SELECTION_PX: u32 = 4;

/// 单显示器冻结帧与几何信息(物理像素)。
struct SnipFrame {
    image: RgbaImage,
    width: u32,
    height: u32,
    scale_factor: f64,
    /// 提供给覆盖层 webview 展示的 JPEG 冻结帧路径。
    preview_path: PathBuf,
}

#[derive(Default)]
pub struct SnipState {
    session: Mutex<Option<Vec<SnipFrame>>>,
}

/// 前端提交的选区(覆盖层窗口内 CSS 像素)。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnipSelection {
    pub monitor: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 识别完成广播事件负载。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SnipDone {
    ok: bool,
    chars: usize,
    error: Option<String>,
}

pub fn init(app: &AppHandle) {
    app.manage(SnipState::default());
}

/// 冻结帧临时目录(每次会话清空重建)。
fn preview_dir() -> PathBuf {
    std::env::temp_dir().join("snipstack-snip")
}

/// 启动一次截屏取字会话。已有会话时忽略(热键连按)。
pub fn start_snip(app: &AppHandle) -> Result<()> {
    let state = app.state::<SnipState>();
    {
        let session = state.session.lock().expect("snip state poisoned");
        if session.is_some() {
            return Ok(());
        }
    }

    let monitors = xcap::Monitor::all()
        .map_err(|err| AppError::Other(anyhow::anyhow!("enumerate monitors: {err}")))?;
    if monitors.is_empty() {
        return Err(AppError::Other(anyhow::anyhow!("no monitor found")));
    }

    let dir = preview_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|err| AppError::Other(anyhow::anyhow!("create snip temp dir: {err}")))?;

    // 先完成全部截屏再建覆盖层,避免覆盖层出现在冻结帧里。
    let mut frames = Vec::with_capacity(monitors.len());
    let mut geometries = Vec::with_capacity(monitors.len());
    for (index, monitor) in monitors.iter().enumerate() {
        let image = monitor
            .capture_image()
            .map_err(|err| AppError::Other(anyhow::anyhow!("capture monitor {index}: {err}")))?;
        let (x, y) = (monitor_x(monitor)?, monitor_y(monitor)?);
        let scale_factor = monitor_scale(monitor)?;
        let (width, height) = (image.width(), image.height());

        // JPEG 只用于覆盖层展示(编码远快于 PNG);OCR 用内存里的原始 RGBA。
        let preview_path = dir.join(format!("frame-{index}.jpg"));
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::io::BufWriter::new(std::fs::File::create(&preview_path).map_err(|err| {
                AppError::Other(anyhow::anyhow!("create frame preview {index}: {err}"))
            })?),
            85,
        );
        encoder
            .encode(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|err| AppError::Other(anyhow::anyhow!("encode frame preview: {err}")))?;

        frames.push(SnipFrame {
            image,
            width,
            height,
            scale_factor,
            preview_path,
        });
        geometries.push((x, y, width, height));
    }

    *state.session.lock().expect("snip state poisoned") = Some(frames);

    for (index, (x, y, width, height)) in geometries.into_iter().enumerate() {
        if let Err(err) = build_overlay_window(app, index, x, y, width, height) {
            log::error!("build snip overlay {index} failed: {err}");
            let _ = cancel_snip(app);
            return Err(err);
        }
    }
    Ok(())
}

/// 建一个铺满指定显示器的覆盖层窗口;先建成不可见,前端帧图就绪后再显示(避免白屏闪烁)。
fn build_overlay_window(
    app: &AppHandle,
    index: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<()> {
    let label = format!("{OVERLAY_LABEL_PREFIX}{index}");
    let window = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html/#/snip?monitor={index}").into()),
    )
    .title("SnipStack Snip")
    .inner_size(1.0, 1.0)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .always_on_top(true)
    .decorations(false)
    .shadow(false)
    .skip_taskbar(true)
    .visible(false)
    .disable_drag_drop_handler()
    .build()
    .map_err(|err| AppError::Other(anyhow::anyhow!("build snip overlay window: {err}")))?;

    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|err| AppError::Other(anyhow::anyhow!("position snip overlay: {err}")))?;
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|err| AppError::Other(anyhow::anyhow!("size snip overlay: {err}")))?;
    Ok(())
}

/// 覆盖层前端就绪(帧图已加载):显示并聚焦,聚焦后才能收到 Esc。
pub fn overlay_ready(app: &AppHandle, monitor: usize) -> Result<()> {
    let label = format!("{OVERLAY_LABEL_PREFIX}{monitor}");
    let Some(window) = app.get_webview_window(&label) else {
        return Ok(());
    };
    window
        .show()
        .map_err(|err| AppError::Other(anyhow::anyhow!("show snip overlay: {err}")))?;
    if monitor == 0 {
        let _ = window.set_focus();
    }
    Ok(())
}

/// 覆盖层冻结帧路径,供前端 convertFileSrc 展示。
pub fn frame_preview_path(app: &AppHandle, monitor: usize) -> Result<String> {
    let state = app.state::<SnipState>();
    let session = state.session.lock().expect("snip state poisoned");
    let frames = session
        .as_ref()
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("no active snip session")))?;
    let frame = frames
        .get(monitor)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("snip monitor {monitor} out of range")))?;
    Ok(frame.preview_path.to_string_lossy().into_owned())
}

/// 取消会话:销毁覆盖层、清理状态与临时文件。
pub fn cancel_snip(app: &AppHandle) -> Result<()> {
    destroy_overlays(app);
    let state = app.state::<SnipState>();
    *state.session.lock().expect("snip state poisoned") = None;
    let _ = std::fs::remove_dir_all(preview_dir());
    Ok(())
}

fn destroy_overlays(app: &AppHandle) {
    for (label, window) in app.webview_windows() {
        if label.starts_with(OVERLAY_LABEL_PREFIX) {
            if let Err(err) = window.destroy() {
                log::warn!("destroy snip overlay {label} failed: {err}");
            }
        }
    }
}

/// 确认选区:立即关掉覆盖层,后台完成裁剪 → OCR → 写剪贴板 → 入历史,完成后广播结果。
pub async fn confirm_snip(app: AppHandle, selection: SnipSelection) -> Result<()> {
    // 取出会话帧并销毁覆盖层,先把屏幕还给用户。
    let frame = {
        let state = app.state::<SnipState>();
        let mut session = state.session.lock().expect("snip state poisoned");
        let mut frames = session
            .take()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("no active snip session")))?;
        if selection.monitor >= frames.len() {
            return Err(AppError::Other(anyhow::anyhow!(
                "snip monitor {} out of range",
                selection.monitor
            )));
        }
        frames.swap_remove(selection.monitor)
    };
    destroy_overlays(&app);
    let _ = std::fs::remove_dir_all(preview_dir());

    // CSS 像素 → 物理像素,并夹取到帧边界内。
    let scale = frame.scale_factor;
    let x = ((selection.x * scale).round().max(0.0) as u32).min(frame.width.saturating_sub(1));
    let y = ((selection.y * scale).round().max(0.0) as u32).min(frame.height.saturating_sub(1));
    let w = ((selection.width * scale).round() as u32).min(frame.width - x);
    let h = ((selection.height * scale).round() as u32).min(frame.height - y);
    if w < MIN_SELECTION_PX || h < MIN_SELECTION_PX {
        return Ok(());
    }

    let cropped = image::imageops::crop_imm(&frame.image, x, y, w, h).to_image();
    drop(frame);

    let settings = app.state::<SettingsStore>().snapshot().snip;
    let ocr_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let lines = ocr::recognize(&ocr_app, cropped_for_ocr(&cropped))?;
        let text = ocr::join_lines(&lines, settings.line_break);
        Ok::<_, AppError>((text, cropped))
    })
    .await
    .map_err(|err| AppError::Other(anyhow::anyhow!("snip ocr task: {err}")))?;

    let (text, cropped) = match outcome {
        Ok(value) => value,
        Err(err) => {
            log::error!("snip ocr failed: {err}");
            let _ = app.emit(
                SNIP_DONE_EVENT,
                SnipDone {
                    ok: false,
                    chars: 0,
                    error: Some(err.to_string()),
                },
            );
            return Err(err);
        }
    };

    if settings.auto_copy && !text.is_empty() {
        let guard = app.state::<std::sync::Arc<clipboard::WritebackGuard>>();
        clipboard::write_plain_text(guard.inner().as_ref(), &text)?;
    }

    if settings.save_to_history {
        if let Err(err) = save_history_item(&app, &cropped, &text).await {
            log::error!("save snip history failed: {err}");
        }
    }

    let _ = app.emit(
        SNIP_DONE_EVENT,
        SnipDone {
            ok: true,
            chars: text.chars().count(),
            error: None,
        },
    );
    Ok(())
}

/// OCR 前处理留口:目前直接用裁剪图。后续可做放大/二值化等增强。
fn cropped_for_ocr(cropped: &RgbaImage) -> RgbaImage {
    cropped.clone()
}

/// 把裁剪图作为图片条目入历史,`search_text` 填 OCR 文本使 FTS 可搜。
/// 去重命中(同像素再截)时更新已有条目的 search_text。
async fn save_history_item(app: &AppHandle, cropped: &RgbaImage, text: &str) -> Result<()> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            cropped.as_raw(),
            cropped.width(),
            cropped.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| AppError::Other(anyhow::anyhow!("encode snip png: {err}")))?;

    let payload = clipboard::ImagePayload {
        bytes: png,
        width: cropped.width(),
        height: cropped.height(),
    };
    let store = app.state::<clipboard::ImageStore>();
    let stored = store.store(&payload)?;

    let now = chrono::Utc::now();
    let search_text = (!text.is_empty()).then(|| text.to_owned());
    let item = ClipboardItem {
        id: uuid::Uuid::new_v4().to_string(),
        kind: ClipboardKind::Image,
        sub_kind: None,
        group_id: None,
        source_app_id: None,
        content_hash: content_hash(ClipboardKind::Image, &stored.file_name),
        content: stored.file_name,
        search_text: search_text.clone(),
        summary: None,
        file_types: None,
        size: Some(stored.size),
        width: Some(stored.width),
        height: Some(stored.height),
        use_count: 1,
        is_favorite: false,
        is_pinned: false,
        is_sensitive: false,
        platform: current_platform(),
        note: None,
        created_at: now,
        updated_at: now,
        source_app_name: None,
        source_app_icon_file: None,
        source_app_icon_path: None,
        image_thumbnail_path: None,
        file_entries: None,
        files_preview_kind: None,
        available_actions: Vec::new(),
        color_preview: None,
        display_created_at: String::new(),
    };

    let pool = app.state::<crate::db::DatabaseState>().pool().await;
    let result = clipboard::persist_and_notify(app, &pool, &item, None).await?;
    if result.deduplicated {
        // 同图重截:补写 OCR 文本(UPDATE 触发 FTS 触发器重建索引)。
        sqlx::query("UPDATE clipboard_items SET search_text = ? WHERE id = ?")
            .bind(&search_text)
            .bind(&result.id)
            .execute(&pool)
            .await
            .map_err(|err| AppError::Other(anyhow::anyhow!("update snip search_text: {err}")))?;
    }
    Ok(())
}

fn current_platform() -> Platform {
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(not(target_os = "windows"))]
    {
        Platform::Macos
    }
}

fn monitor_x(monitor: &xcap::Monitor) -> Result<i32> {
    monitor
        .x()
        .map_err(|err| AppError::Other(anyhow::anyhow!("monitor x: {err}")))
}

fn monitor_y(monitor: &xcap::Monitor) -> Result<i32> {
    monitor
        .y()
        .map_err(|err| AppError::Other(anyhow::anyhow!("monitor y: {err}")))
}

fn monitor_scale(monitor: &xcap::Monitor) -> Result<f64> {
    monitor
        .scale_factor()
        .map(f64::from)
        .map_err(|err| AppError::Other(anyhow::anyhow!("monitor scale factor: {err}")))
}
