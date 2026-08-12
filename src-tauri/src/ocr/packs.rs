//! OCR 语言包:内置中英(含日文/繁体/拼音)之外的识别模型按需下载。
//!
//! 一个语言包 = 一个 rec ONNX 模型 + 对应字典(det 模型各语言共用内置的 ch mobile det)。
//! 来源为 oar-ocr GitHub Releases(Apache-2.0,转换自 PaddleOCR 官方 PP-OCRv5 多语言模型),
//! ModelScope 同名镜像作为备源(国内直连友好)。文件字节数精确已知,下载后按大小校验。
//! 版本与字节数需与 scripts/fetchOcrModels.ts 的内置模型清单同步升级。
//!
//! 存放位置见 [`crate::core::paths::ocr_packs_dir`]。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::core::in_flight::InFlight;
use crate::core::{AppError, Result};

/// 与前端 src/constants/events.ts 的 TAURI_EVENT.OCR_PACK_PROGRESS 一一对应。
pub const PACK_PROGRESS_EVENT: &str = "ocr://pack-progress";

/// 内置语言(随包分发,不可删除):简中/繁中/英文/日文/拼音,PP-OCRv5 ch mobile 单模型覆盖。
/// 与前端 src/constants/ocr.ts 的 OCR_BUILTIN_LANGUAGE_ID 一一对应。
pub const BUILTIN_LANGUAGE_ID: &str = "zhEn";

const GITHUB_BASE: &str = "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0";
const MODELSCOPE_BASE: &str = "https://www.modelscope.cn/models/greatv/oar-ocr/resolve/master";

/// 正在下载中的包:同包并发下载会互踩同一 `.part` 文件,必须在 Rust 侧互斥
/// (前端的防重入随组件销毁失效,挡不住关闭设置页再重开的场景)。
static IN_FLIGHT: InFlight = InFlight::new();

/// 共享 HTTP 客户端:带连接/读取超时,网络僵死时下载会失败而非永久挂起。
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("build http client")
    })
}

pub struct LanguagePack {
    /// 稳定 id,进设置与目录名;前端据此取显示名文案。
    pub id: &'static str,
    pub rec_file: &'static str,
    pub rec_bytes: u64,
    pub dict_file: &'static str,
    pub dict_bytes: u64,
}

impl LanguagePack {
    fn total_bytes(&self) -> u64 {
        self.rec_bytes + self.dict_bytes
    }
}

/// 首发语言包(字节数来自 oar-ocr 下载注册表,亦经 HTTP HEAD 实测核对)。
pub const PACKS: &[LanguagePack] = &[
    LanguagePack {
        id: "korean",
        rec_file: "korean_pp-ocrv5_mobile_rec.onnx",
        rec_bytes: 13_446_374,
        dict_file: "ppocrv5_korean_dict.txt",
        dict_bytes: 47_451,
    },
    LanguagePack {
        id: "latin",
        rec_file: "latin_pp-ocrv5_mobile_rec.onnx",
        rec_bytes: 8_069_614,
        dict_file: "ppocrv5_latin_dict.txt",
        dict_bytes: 2_616,
    },
    LanguagePack {
        id: "eslav",
        rec_file: "eslav_pp-ocrv5_mobile_rec.onnx",
        rec_bytes: 7_915_218,
        dict_file: "ppocrv5_eslav_dict.txt",
        dict_bytes: 1_663,
    },
    LanguagePack {
        id: "th",
        rec_file: "th_pp-ocrv5_mobile_rec.onnx",
        rec_bytes: 7_918_606,
        dict_file: "ppocrv5_th_dict.txt",
        dict_bytes: 1_767,
    },
    LanguagePack {
        id: "arabic",
        rec_file: "arabic_pp-ocrv5_mobile_rec.onnx",
        rec_bytes: 8_026_538,
        dict_file: "ppocrv5_arabic_dict.txt",
        dict_bytes: 2_369,
    },
];

/// 语言包状态,供设置界面渲染。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackStatus {
    pub id: String,
    pub downloaded: bool,
    pub total_bytes: u64,
}

/// 下载进度事件负载。`received == total` 即完成;`error` 非空表示失败终态。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackProgress {
    id: String,
    received: u64,
    total: u64,
    error: Option<String>,
}

fn emit_progress(app: &AppHandle, id: &str, received: u64, total: u64, error: Option<String>) {
    let _ = app.emit(
        PACK_PROGRESS_EVENT,
        PackProgress {
            id: id.to_owned(),
            received,
            total,
            error,
        },
    );
}

fn find(id: &str) -> Result<&'static LanguagePack> {
    PACKS
        .iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| AppError::Ocr(format!("未知的 OCR 语言包: {id}")))
}

fn packs_root(app: &AppHandle) -> Result<PathBuf> {
    crate::core::paths::ocr_packs_dir(app)
}

fn file_matches(path: &PathBuf, expected: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == expected)
        .unwrap_or(false)
}

/// 已完整下载时返回 (rec, dict) 绝对路径。
pub fn resolve(app: &AppHandle, id: &str) -> Option<(PathBuf, PathBuf)> {
    let pack = PACKS.iter().find(|pack| pack.id == id)?;
    let dir = packs_root(app).ok()?.join(pack.id);
    let rec = dir.join(pack.rec_file);
    let dict = dir.join(pack.dict_file);
    (file_matches(&rec, pack.rec_bytes) && file_matches(&dict, pack.dict_bytes))
        .then_some((rec, dict))
}

pub fn list(app: &AppHandle) -> Vec<PackStatus> {
    PACKS
        .iter()
        .map(|pack| PackStatus {
            id: pack.id.to_owned(),
            downloaded: resolve(app, pack.id).is_some(),
            total_bytes: pack.total_bytes(),
        })
        .collect()
}

pub fn delete(app: &AppHandle, id: &str) -> Result<()> {
    let pack = find(id)?;
    let dir = packs_root(app)?.join(pack.id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("delete ocr pack {id}"))?;
    }
    super::invalidate_engine();
    Ok(())
}

/// 下载语言包(rec + dict),沿途 emit 进度事件。
/// **任何**失败路径都会 emit 带 error 的终态事件(前端据此收起进度条)。
/// 同包已在下载中时直接返回,由进行中的任务继续广播进度。
pub async fn download(app: &AppHandle, id: &str) -> Result<()> {
    let pack = match find(id) {
        Ok(pack) => pack,
        Err(err) => {
            emit_progress(app, id, 0, 0, Some(err.to_string()));
            return Err(err);
        }
    };
    let total = pack.total_bytes();
    let Some(_guard) = IN_FLIGHT.try_begin(pack.id) else {
        return Ok(());
    };

    let result = download_inner(app, pack, total).await;
    if let Err(err) = &result {
        emit_progress(app, pack.id, 0, total, Some(err.to_string()));
    }
    result
}

async fn download_inner(app: &AppHandle, pack: &LanguagePack, total: u64) -> Result<()> {
    let dir = packs_root(app)?.join(pack.id);
    std::fs::create_dir_all(&dir).context("create ocr pack dir")?;

    let mut base: u64 = 0;
    for (file, expected) in [
        (pack.rec_file, pack.rec_bytes),
        (pack.dict_file, pack.dict_bytes),
    ] {
        let target = dir.join(file);
        if !file_matches(&target, expected) {
            fetch_file(app, pack.id, file, &target, expected, base, total).await?;
        }
        base += expected;
    }
    super::invalidate_engine();
    emit_progress(app, pack.id, total, total, None);
    Ok(())
}

/// 单文件下载:主源失败自动切备源;先写 `.part` 再原子改名;完成后按精确字节数校验。
async fn fetch_file(
    app: &AppHandle,
    pack_id: &str,
    file: &str,
    target: &std::path::Path,
    expected: u64,
    base: u64,
    total: u64,
) -> Result<()> {
    let urls = [
        format!("{GITHUB_BASE}/{file}"),
        format!("{MODELSCOPE_BASE}/{file}"),
    ];
    let mut last_err = None;
    for url in &urls {
        match fetch_one(app, pack_id, url, target, expected, base, total).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                log::warn!("download {url} failed: {err}");
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::Ocr(format!("下载 {file} 失败"))))
}

async fn fetch_one(
    app: &AppHandle,
    pack_id: &str,
    url: &str,
    target: &std::path::Path,
    expected: u64,
    base: u64,
    total: u64,
) -> Result<()> {
    let response = http_client()
        .get(url)
        .send()
        .await
        .map_err(|err| AppError::Ocr(format!("请求失败: {err}")))?
        .error_for_status()
        .map_err(|err| AppError::Ocr(format!("HTTP 错误: {err}")))?;

    let part = target.with_extension("part");
    let mut writer = std::io::BufWriter::new(
        std::fs::File::create(&part).with_context(|| format!("create {part:?}"))?,
    );

    let mut written: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| AppError::Ocr(format!("下载中断: {err}")))?;
        std::io::Write::write_all(&mut writer, &chunk)
            .with_context(|| format!("write {part:?}"))?;
        written += chunk.len() as u64;

        // 进度事件限频(150ms),避免刷爆事件通道。
        if last_emit.elapsed().as_millis() >= 150 {
            last_emit = std::time::Instant::now();
            emit_progress(app, pack_id, base + written, total, None);
        }
    }
    std::io::Write::flush(&mut writer).with_context(|| format!("flush {part:?}"))?;
    drop(writer);

    if written != expected {
        let _ = std::fs::remove_file(&part);
        return Err(AppError::Ocr(format!(
            "文件大小不符(得到 {written} 字节,预期 {expected}),已丢弃"
        )));
    }
    std::fs::rename(&part, target).with_context(|| format!("rename {part:?}"))?;
    Ok(())
}
