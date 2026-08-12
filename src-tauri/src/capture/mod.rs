//! 截屏取字(框选 OCR)会话:冻结帧捕获、每显示器覆盖层窗口、选区裁剪与结果处理。
//!
//! 流程:热键/托盘触发 `start_snip`(原子占位会话)→ 后台线程 xcap 抓取各显示器物理
//! 像素帧并编码 JPEG 冻结帧 → 主线程建覆盖层窗口(前端 `/snip` 路由显示冻结帧 +
//! 拖拽框选)→ 前端提交 CSS 像素选区 → 按显示器 scale factor 折算回物理像素裁剪 →
//! OCR → 入历史 + 写剪贴板。除「选区过小视为误触」外,任何终态(成功/失败)都会
//! 广播一次 `snip://done`。
//!
//! 坐标约定:`SnipFrame` 内全部为物理像素;与前端交互的选区为该覆盖层窗口内的
//! CSS 像素(每个覆盖层恰好铺满一个显示器,折算只需乘 scale factor)。
//!
//! 冻结帧含全屏内容,按敏感数据对待:每会话独立临时目录,confirm/cancel 即删,
//! 应用启动时清扫整个根目录兜底(覆盖崩溃/强杀残留)。

use std::path::PathBuf;
use std::sync::Mutex;

use image::buffer::ConvertBuffer;
use image::{ImageEncoder, RgbImage, RgbaImage};
use serde::Deserialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use crate::core::{AppError, Result};
use crate::db::items::content_hash;
use crate::db::models::{ClipboardItem, ClipboardKind};
use crate::settings::SettingsStore;
use crate::{clipboard, ocr};

/// 覆盖层窗口 label 前缀,后跟显示器序号。
pub const OVERLAY_LABEL_PREFIX: &str = "snip-overlay-";
/// 与前端 src/constants/events.ts 的 TAURI_EVENT.SNIP_DONE 一一对应。
pub const SNIP_DONE_EVENT: &str = "snip://done";

/// 选区过小视为误触,按取消处理(物理像素)。
const MIN_SELECTION_PX: u32 = 4;

/// 单显示器冻结帧与几何信息(物理像素;宽高从 `image` 上取,不另存副本)。
struct SnipFrame {
    image: RgbaImage,
    scale_factor: f64,
    /// 提供给覆盖层 webview 展示的 JPEG 冻结帧路径。
    preview_path: PathBuf,
}

/// 一次截屏取字会话。`start_snip` 在锁内原子占位(此时 `frames` 为空),
/// 后台捕获完成后回填;并发触发以「session 已存在」为准直接忽略。
struct SnipSession {
    frames: Vec<SnipFrame>,
    /// 会话开始时光标所在的显示器;该覆盖层获得焦点,Esc 才能直达。
    focus_index: usize,
    /// 本会话专属的冻结帧临时目录(避免并发会话互删共享目录)。
    dir: PathBuf,
}

#[derive(Default)]
pub struct SnipState {
    session: Mutex<Option<SnipSession>>,
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
    // 隐私兜底:清掉上次崩溃/强杀残留的全屏冻结帧。
    let _ = std::fs::remove_dir_all(temp_root());
}

/// 冻结帧临时目录根;每个会话在其下建独立子目录。
fn temp_root() -> PathBuf {
    std::env::temp_dir().join("snipstack-snip")
}

fn emit_done(app: &AppHandle, ok: bool, chars: usize, error: Option<String>) {
    let _ = app.emit(SNIP_DONE_EVENT, SnipDone { ok, chars, error });
}

/// 启动一次截屏取字会话。已有会话时忽略(热键连按/多入口并发)。
/// 占位与检查在同一锁内完成;截屏与编码移入阻塞线程池,不冻结热键回调线程。
pub fn start_snip(app: &AppHandle) -> Result<()> {
    {
        let state = app.state::<SnipState>();
        let mut session = state.session.lock().expect("snip state poisoned");
        if session.is_some() {
            return Ok(());
        }
        *session = Some(SnipSession {
            frames: Vec::new(),
            focus_index: 0,
            dir: temp_root().join(uuid::Uuid::new_v4().to_string()),
        });
    }

    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(err) = capture_frames(&app) {
            log::error!("start snip failed: {err}");
            let _ = cancel_snip(&app);
            emit_done(&app, false, 0, Some(err.to_string()));
        }
    });
    Ok(())
}

/// 抓取全部显示器冻结帧并回填会话,然后回主线程建覆盖层窗口。
fn capture_frames(app: &AppHandle) -> Result<()> {
    let dir = {
        let state = app.state::<SnipState>();
        let session = state.session.lock().expect("snip state poisoned");
        session
            .as_ref()
            .map(|s| s.dir.clone())
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("snip session cancelled")))?
    };

    let monitors = xcap::Monitor::all()
        .map_err(|err| AppError::Other(anyhow::anyhow!("enumerate monitors: {err}")))?;
    if monitors.is_empty() {
        return Err(AppError::Other(anyhow::anyhow!("no monitor found")));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|err| AppError::Other(anyhow::anyhow!("create snip temp dir: {err}")))?;

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
        // image crate 的 JPEG 编码只接受 L8/Rgb8,RGBA 帧必须先转 RGB。
        let rgb: RgbImage = image.convert();
        let preview_path = dir.join(format!("frame-{index}.jpg"));
        let file = std::fs::File::create(&preview_path).map_err(|err| {
            AppError::Other(anyhow::anyhow!("create frame preview {index}: {err}"))
        })?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 85)
            .encode(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .map_err(|err| AppError::Other(anyhow::anyhow!("encode frame preview: {err}")))?;
        drop(rgb);

        frames.push(SnipFrame {
            image,
            scale_factor,
            preview_path,
        });
        geometries.push((x, y, width, height));
    }

    let focus_index = focused_monitor_index(app, &geometries);
    {
        let state = app.state::<SnipState>();
        let mut session = state.session.lock().expect("snip state poisoned");
        let Some(current) = session.as_mut() else {
            // 捕获期间被取消:目录清理由 cancel 完成,这里静默结束。
            return Ok(());
        };
        current.frames = frames;
        current.focus_index = focus_index;
    }

    // 建窗回主线程(macOS 硬约束,Windows 同样安全)。
    let main_app = app.clone();
    app.run_on_main_thread(move || {
        for (index, (x, y, width, height)) in geometries.into_iter().enumerate() {
            if let Err(err) = build_overlay_window(&main_app, index, x, y, width, height) {
                log::error!("build snip overlay {index} failed: {err}");
                let _ = cancel_snip(&main_app);
                emit_done(&main_app, false, 0, Some(err.to_string()));
                return;
            }
        }
    })
    .map_err(|err| AppError::Other(anyhow::anyhow!("dispatch overlay build: {err}")))?;
    Ok(())
}

/// 光标当前所在显示器的序号;取不到光标位置或不在任何屏内时回落 0。
fn focused_monitor_index(app: &AppHandle, geometries: &[(i32, i32, u32, u32)]) -> usize {
    let Ok(cursor) = app.cursor_position() else {
        return 0;
    };
    geometries
        .iter()
        .position(|(x, y, width, height)| {
            cursor.x >= f64::from(*x)
                && cursor.x < f64::from(*x) + f64::from(*width)
                && cursor.y >= f64::from(*y)
                && cursor.y < f64::from(*y) + f64::from(*height)
        })
        .unwrap_or(0)
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

    let focus_index = {
        let state = app.state::<SnipState>();
        let session = state.session.lock().expect("snip state poisoned");
        session.as_ref().map(|s| s.focus_index).unwrap_or(0)
    };
    if monitor == focus_index {
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
        .map(|s| &s.frames)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("no active snip session")))?;
    let frame = frames
        .get(monitor)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("snip monitor {monitor} out of range")))?;
    Ok(frame.preview_path.to_string_lossy().into_owned())
}

/// 取消会话:销毁覆盖层、清理状态与本会话临时目录。
pub fn cancel_snip(app: &AppHandle) -> Result<()> {
    destroy_overlays(app);
    let dir = {
        let state = app.state::<SnipState>();
        let mut session = state.session.lock().expect("snip state poisoned");
        session.take().map(|s| s.dir)
    };
    if let Some(dir) = dir {
        let _ = std::fs::remove_dir_all(dir);
    }
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

/// 确认选区:立即关掉覆盖层,后台完成裁剪 → OCR → 入历史 → 写剪贴板。
/// 除「选区过小」外的任何终态都广播 `snip://done`(覆盖层已销毁,事件是唯一反馈通道)。
pub async fn confirm_snip(app: AppHandle, selection: SnipSelection) -> Result<()> {
    // 取出会话并销毁覆盖层,先把屏幕还给用户;临时目录立即删除。
    let (frame, dir) = {
        let state = app.state::<SnipState>();
        let mut session = state.session.lock().expect("snip state poisoned");
        let taken = session
            .take()
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("no active snip session")))?;
        let mut frames = taken.frames;
        let frame =
            (selection.monitor < frames.len()).then(|| frames.swap_remove(selection.monitor));
        (frame, taken.dir)
    };
    destroy_overlays(&app);
    let _ = std::fs::remove_dir_all(dir);
    let Some(frame) = frame else {
        return Err(AppError::Other(anyhow::anyhow!(
            "snip monitor {} out of range",
            selection.monitor
        )));
    };

    // CSS 像素 → 物理像素,并夹取到帧边界内。
    let (frame_w, frame_h) = (frame.image.width(), frame.image.height());
    let scale = frame.scale_factor;
    let x = ((selection.x * scale).round().max(0.0) as u32).min(frame_w.saturating_sub(1));
    let y = ((selection.y * scale).round().max(0.0) as u32).min(frame_h.saturating_sub(1));
    let w = ((selection.width * scale).round() as u32).min(frame_w - x);
    let h = ((selection.height * scale).round() as u32).min(frame_h - y);
    if w < MIN_SELECTION_PX || h < MIN_SELECTION_PX {
        return Ok(());
    }

    let cropped = image::imageops::crop_imm(&frame.image, x, y, w, h).to_image();
    drop(frame);

    let settings = app.state::<SettingsStore>().snapshot().snip;
    let ocr_app = app.clone();
    let save_to_history = settings.save_to_history;
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        // QR/条码优先:命中直接取码值(TextSniper 同款行为),未命中回落 OCR。
        let text = if settings.detect_qr {
            decode_barcodes(&cropped)
        } else {
            None
        };
        let text = match text {
            Some(text) => text,
            None => {
                let lines = ocr::recognize(&ocr_app, &cropped)?;
                ocr::join_lines(&lines, settings.line_break)
            }
        };
        // PNG 编码也是重活,一并留在阻塞线程,不占 async runtime。
        let png = save_to_history
            .then(|| encode_png(&cropped))
            .transpose()?
            .map(|bytes| (bytes, cropped.width(), cropped.height()));
        Ok::<_, AppError>((text, png))
    })
    .await
    .unwrap_or_else(|err| Err(AppError::Other(anyhow::anyhow!("snip task crashed: {err}"))));

    let (text, png) = match outcome {
        Ok(value) => value,
        Err(err) => {
            log::error!("snip ocr failed: {err}");
            emit_done(&app, false, 0, Some(err.to_string()));
            return Err(err);
        }
    };

    // 先入历史再写剪贴板:即使剪贴板被占用失败,识别结果仍可从历史找回。
    if let Some((png, width, height)) = png {
        if let Err(err) = save_history_item(&app, png, width, height, &text).await {
            log::error!("save snip history failed: {err}");
        }
    }

    if settings.auto_copy && !text.is_empty() {
        let guard = app.state::<std::sync::Arc<clipboard::WritebackGuard>>();
        if let Err(err) = clipboard::write_plain_text(guard.inner().as_ref(), &text) {
            log::error!("snip write clipboard failed: {err}");
            emit_done(&app, false, text.chars().count(), Some(err.to_string()));
            return Err(err);
        }
    }

    emit_done(&app, true, text.chars().count(), None);
    Ok(())
}

/// 尝试解码框选区域内的 QR/条码;多个码按出现顺序换行拼接,未命中返回 `None`。
fn decode_barcodes(image: &RgbaImage) -> Option<String> {
    // grayscale 接受引用,避免整幅 RGBA 的额外拷贝。
    let luma = image::imageops::grayscale(image);
    let (width, height) = (luma.width(), luma.height());
    let results = rxing::helpers::detect_multiple_in_luma(luma.into_raw(), width, height).ok()?;

    let texts: Vec<String> = results
        .iter()
        .map(|result| result.getText().trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect();
    if texts.is_empty() {
        return None;
    }
    log::info!("snip decoded {} barcode(s)", texts.len());
    Some(texts.join("\n"))
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| AppError::Other(anyhow::anyhow!("encode snip png: {err}")))?;
    Ok(png)
}

/// 把裁剪图作为图片条目入历史,`search_text` 填 OCR 文本使 FTS 可搜。
/// 识别为空时写入空串作「已处理」标记(后台图片 OCR 据此跳过);
/// 去重命中(同像素再截)时只用**非空**结果更新,绝不覆盖掉已有索引。
async fn save_history_item(
    app: &AppHandle,
    png: Vec<u8>,
    width: u32,
    height: u32,
    text: &str,
) -> Result<()> {
    let payload = clipboard::ImagePayload {
        bytes: png,
        width,
        height,
    };
    let store = app.state::<clipboard::ImageStore>();
    let stored = store.store(&payload)?;

    let now = chrono::Utc::now();
    let item = ClipboardItem {
        id: uuid::Uuid::new_v4().to_string(),
        kind: ClipboardKind::Image,
        sub_kind: None,
        group_id: None,
        source_app_id: None,
        content_hash: content_hash(ClipboardKind::Image, &stored.file_name),
        content: stored.file_name,
        search_text: Some(text.to_owned()),
        summary: None,
        file_types: None,
        size: Some(stored.size),
        width: Some(stored.width),
        height: Some(stored.height),
        use_count: 1,
        is_favorite: false,
        is_pinned: false,
        is_sensitive: false,
        platform: clipboard::current_platform(),
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
    if result.deduplicated && !text.is_empty() {
        // 同图重截:用非空结果补写/刷新 OCR 文本(UPDATE 触发 FTS 触发器重建索引)。
        // 空结果不写,避免语言包不匹配等场景抹掉已有索引。
        sqlx::query("UPDATE clipboard_items SET search_text = ? WHERE id = ?")
            .bind(text)
            .bind(&result.id)
            .execute(&pool)
            .await
            .map_err(|err| AppError::Other(anyhow::anyhow!("update snip search_text: {err}")))?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use rxing::Writer;

    /// 用 rxing 自编码一个 QR 再走产品解码路径,验证「框选到码直接取值」链路。
    #[test]
    fn decodes_qr_code_in_selection() {
        let content = "https://github.com/snipstack/SnipStack";
        let matrix = rxing::qrcode::QRCodeWriter
            .encode(content, &rxing::BarcodeFormat::QR_CODE, 240, 240)
            .expect("encode qr fixture");

        let mut img = RgbaImage::from_pixel(
            matrix.width(),
            matrix.height(),
            image::Rgba([255, 255, 255, 255]),
        );
        for y in 0..matrix.height() {
            for x in 0..matrix.width() {
                if matrix.get(x, y) {
                    img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
                }
            }
        }

        let decoded = decode_barcodes(&img).expect("decode qr");
        assert_eq!(decoded, content);
    }

    #[test]
    fn plain_text_region_has_no_barcode() {
        let img = RgbaImage::from_pixel(160, 60, image::Rgba([250, 250, 250, 255]));
        assert!(decode_barcodes(&img).is_none());
    }

    /// 冻结帧编码走 RGB 通道:image crate 的 JPEG encode 不接受 Rgba8,
    /// 这里用真实编码调用防止回归(评审发现的功能级 bug)。
    #[test]
    fn frozen_frame_jpeg_encoding_accepts_converted_rgb() {
        let rgba = RgbaImage::from_pixel(64, 48, image::Rgba([120, 180, 240, 255]));
        let rgb: RgbImage = rgba.convert();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode(rgb.as_raw(), 64, 48, image::ExtendedColorType::Rgb8)
            .expect("jpeg encode rgb frame");
        assert!(!out.is_empty());
    }
}
