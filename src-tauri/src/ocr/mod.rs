//! OCR 推理管线：PP-OCRv5 mobile(det + rec)经 oar-ocr / ONNX Runtime 离线识别。
//!
//! 引擎懒加载：首次识别时从 bundle resources 读模型构建,之后常驻复用。
//! 构建与推理都是 CPU 密集操作,调用方必须放在 `spawn_blocking` 里,不要阻塞异步运行时。

pub mod backfill;
pub mod packs;

use anyhow::Context;
use std::path::PathBuf;
use std::sync::Mutex;

use image::RgbaImage;
use oar_ocr::prelude::*;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

use crate::core::{AppError, Result};
use crate::settings::SnipLineBreak;

/// 低于该置信度的识别行按噪声丢弃。
const MIN_LINE_SCORE: f32 = 0.3;

/// 识别出的一行文本(管线已按阅读顺序排序,低置信度行已过滤)。
pub struct OcrLine {
    pub text: String,
}

/// 当前引擎实例,键为语言 id;语言切换或语言包增删时重建。
/// `Mutex` 同时串行化推理:桌面截屏场景不存在并发识别,简单优先。
static ENGINE: Mutex<Option<(String, OAROCR)>> = Mutex::new(None);

/// 空闲淘汰:ONNX 会话 + ort arena 常驻 RSS 以百 MB 计,而这是常驻托盘应用。
/// 每次识别递增世代并调度一个延时检查,若期间无新识别则释放引擎
/// (懒加载重建成本 ~0.2s,对下一次热键触发无感)。
static ENGINE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const ENGINE_IDLE_EVICT: std::time::Duration = std::time::Duration::from_secs(300);

fn schedule_engine_eviction() {
    use std::sync::atomic::Ordering;
    let epoch = ENGINE_EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(ENGINE_IDLE_EVICT).await;
        if ENGINE_EPOCH.load(Ordering::Relaxed) == epoch {
            invalidate_engine();
            log::info!("ocr engine evicted after {:?} idle", ENGINE_IDLE_EVICT);
        }
    });
}

/// 丢弃缓存的引擎(语言包下载/删除、空闲淘汰时调用),下次识别按最新状态重建。
pub(crate) fn invalidate_engine() {
    *crate::core::sync::lock_unpoisoned(&ENGINE) = None;
}

/// 解析内置模型资源文件;dev 模式下即 `src-tauri/resources/ocr/`。
fn resource(app: &AppHandle, name: &str) -> Result<PathBuf> {
    let path = app
        .path()
        .resolve(format!("resources/ocr/{name}"), BaseDirectory::Resource)
        .with_context(|| format!("resolve ocr resource {name}"))?;
    if !path.is_file() {
        return Err(AppError::Ocr(format!(
            "OCR 模型缺失: {name}(先执行 pnpm fetch:ocr-models)"
        )));
    }
    Ok(path)
}

/// 按设置选定的语言解析 (引擎键, det, rec, dict)。
/// 选中的语言包未下载(或校验失败)时回落内置中英,并记警告——识别永远可用。
fn engine_spec(app: &AppHandle, selected: &str) -> Result<(String, PathBuf, PathBuf, PathBuf)> {
    let det = resource(app, "pp-ocrv5_mobile_det.onnx")?;
    if selected != packs::BUILTIN_LANGUAGE_ID {
        if let Some((rec, dict)) = packs::resolve(app, selected) {
            return Ok((selected.to_owned(), det, rec, dict));
        }
        log::warn!("ocr language pack {selected} unavailable, falling back to builtin zh/en");
    }
    let rec = resource(app, "pp-ocrv5_mobile_rec.onnx")?;
    let dict = resource(app, "ppocrv5_dict.txt")?;
    Ok((packs::BUILTIN_LANGUAGE_ID.to_owned(), det, rec, dict))
}

fn build_engine_from_paths(
    det: &std::path::Path,
    rec: &std::path::Path,
    dict: &std::path::Path,
) -> Result<OAROCR> {
    OAROCRBuilder::new(
        det.to_string_lossy().into_owned(),
        rec.to_string_lossy().into_owned(),
        dict.to_string_lossy().into_owned(),
    )
    .build()
    .map_err(|err| AppError::Ocr(format!("构建 OCR 引擎失败: {err}")))
}

/// 对一张 RGBA 图做整图 OCR,返回按阅读顺序排列的行。
/// 识别语言取自设置 `snip.language`;所选语言包缺失时自动回落内置中英。
///
/// CPU 密集:调用方需置于 `spawn_blocking`。
pub fn recognize(app: &AppHandle, image: &RgbaImage) -> Result<Vec<OcrLine>> {
    let selected = app
        .try_state::<crate::settings::SettingsStore>()
        .map(|store| store.snapshot().snip.language)
        .unwrap_or_else(|| packs::BUILTIN_LANGUAGE_ID.to_owned());
    let (key, det, rec, dict) = engine_spec(app, &selected)?;

    let mut guard = crate::core::sync::lock_unpoisoned(&ENGINE);
    if guard.as_ref().map(|(k, _)| k != &key).unwrap_or(true) {
        let started = std::time::Instant::now();
        let engine = build_engine_from_paths(&det, &rec, &dict)?;
        log::info!("ocr engine ({key}) ready in {:?}", started.elapsed());
        *guard = Some((key, engine));
    }
    let (_, engine) = guard.as_mut().expect("ocr engine just built");

    let started = std::time::Instant::now();
    let lines = predict_lines(engine, image)?;
    log::info!(
        "ocr recognized {} lines in {:?}",
        lines.len(),
        started.elapsed()
    );
    drop(guard);
    schedule_engine_eviction();
    Ok(lines)
}

fn predict_lines(engine: &mut OAROCR, image: &RgbaImage) -> Result<Vec<OcrLine>> {
    // oar-ocr 管线接收 RGB 图;截屏帧是 RGBA,从借用直接转出 RGB,不复制原图。
    let rgb: image::RgbImage = image::buffer::ConvertBuffer::convert(image);
    let results = engine
        .predict(vec![rgb])
        .map_err(|err| AppError::Ocr(format!("OCR 识别失败: {err}")))?;

    let mut lines = Vec::new();
    for result in &results {
        for region in &result.text_regions {
            let Some(text) = region.text.as_ref() else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            // 引擎未给出置信度时按可信处理,只过滤明确的低分噪声(误检的花纹/图标)。
            let score = region.confidence.unwrap_or(1.0);
            if score < MIN_LINE_SCORE {
                continue;
            }
            lines.push(OcrLine {
                text: text.to_owned(),
            });
        }
    }
    Ok(lines)
}

/// 按换行策略把识别行拼成最终文本。
///
/// `Merge` 模式的拼接规则:相邻两段的接缝处若任一侧是 CJK 字符则直接相连,
/// 否则(拉丁词与拉丁词)补一个空格——对应中英混排里「中文续行不加空格、英文断词加空格」。
pub fn join_lines(lines: &[OcrLine], mode: SnipLineBreak) -> String {
    match mode {
        SnipLineBreak::Keep => lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        SnipLineBreak::Merge => {
            let mut merged = String::new();
            for line in lines {
                if merged.is_empty() {
                    merged.push_str(&line.text);
                    continue;
                }
                let prev = merged.chars().next_back();
                let next = line.text.chars().next();
                if !joins_without_space(prev) && !joins_without_space(next) {
                    merged.push(' ');
                }
                merged.push_str(&line.text);
            }
            merged
        }
    }
}

/// 判断接缝一侧的字符是否可无空格直连(CJK 及全角标点)。
fn joins_without_space(ch: Option<char>) -> bool {
    let Some(ch) = ch else {
        return false;
    };
    matches!(u32::from(ch),
        0x2E80..=0x303F      // CJK 部首、康熙部首、CJK 标点
        | 0x3040..=0x30FF    // 日文假名
        | 0x3400..=0x4DBF    // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 统一表意
        | 0xF900..=0xFAFF    // CJK 兼容表意
        | 0xFF00..=0xFFEF    // 全角形式
        | 0x20000..=0x2FA1F  // CJK 扩展 B 及以后
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> OcrLine {
        OcrLine {
            text: text.to_owned(),
        }
    }

    /// 真实引擎端到端:构建 PP-OCRv5 引擎识别中英混排夹具图。
    /// 模型未下载(CI 默认不带)时跳过;本地先执行 `pnpm fetch:ocr-models`。
    #[test]
    fn recognizes_fixture_image_with_real_engine() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let det = root.join("resources/ocr/pp-ocrv5_mobile_det.onnx");
        let rec = root.join("resources/ocr/pp-ocrv5_mobile_rec.onnx");
        let dict = root.join("resources/ocr/ppocrv5_dict.txt");
        if !det.is_file() || !rec.is_file() || !dict.is_file() {
            eprintln!("skip: ocr models not present, run `pnpm fetch:ocr-models`");
            return;
        }

        let mut engine = build_engine_from_paths(&det, &rec, &dict).expect("build ocr engine");
        let fixture = image::open(root.join("tests/fixtures/snip-zh-en.png"))
            .expect("open fixture image")
            .to_rgba8();
        let lines = predict_lines(&mut engine, &fixture).expect("recognize fixture");
        let joined = join_lines(&lines, SnipLineBreak::Keep);

        assert!(joined.contains("quick brown fox"), "joined = {joined}");
        assert!(joined.contains("截屏取字"), "joined = {joined}");
        assert!(joined.contains("剪贴板历史"), "joined = {joined}");
        assert!(joined.contains("2026"), "joined = {joined}");
    }

    #[test]
    fn keep_mode_preserves_line_breaks() {
        let lines = [line("第一行"), line("second line")];
        assert_eq!(
            join_lines(&lines, SnipLineBreak::Keep),
            "第一行\nsecond line"
        );
    }

    #[test]
    fn merge_mode_joins_cjk_without_space() {
        let lines = [line("剪贴板历史管理,支持"), line("全文搜索与置顶。")];
        assert_eq!(
            join_lines(&lines, SnipLineBreak::Merge),
            "剪贴板历史管理,支持全文搜索与置顶。"
        );
    }

    #[test]
    fn merge_mode_joins_latin_with_space() {
        let lines = [line("clipboard history with"), line("full-text search")];
        assert_eq!(
            join_lines(&lines, SnipLineBreak::Merge),
            "clipboard history with full-text search"
        );
    }

    #[test]
    fn merge_mode_mixed_boundary_prefers_no_space() {
        let lines = [line("支持 Windows 11"), line("与中英混排 OCR")];
        assert_eq!(
            join_lines(&lines, SnipLineBreak::Merge),
            "支持 Windows 11与中英混排 OCR"
        );
    }
}
