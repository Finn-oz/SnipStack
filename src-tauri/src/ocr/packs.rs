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
use crate::core::Result;

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

/// 一个下载资产:文件名 + 精确字节数(快速失败)+ SHA-256(完整性边界)。
pub struct Asset {
    pub file: &'static str,
    pub bytes: u64,
    /// 小写十六进制 SHA-256。来自 oar-ocr 下载注册表(独立于资产本身的出处),
    /// 内置模型的哈希亦与之逐字节核对一致。字节数只挡意外截断;哈希才防蓄意替换。
    pub sha256: &'static str,
}

pub struct LanguagePack {
    /// 稳定 id,进设置与目录名;前端据此取显示名文案。
    pub id: &'static str,
    pub rec: Asset,
    pub dict: Asset,
}

impl LanguagePack {
    fn total_bytes(&self) -> u64 {
        self.rec.bytes + self.dict.bytes
    }
}

/// 首发语言包(字节数与 SHA-256 均来自 oar-ocr 下载注册表)。
pub const PACKS: &[LanguagePack] = &[
    LanguagePack {
        id: "korean",
        rec: Asset {
            file: "korean_pp-ocrv5_mobile_rec.onnx",
            bytes: 13_446_374,
            sha256: "2d7ed96308065a86103325d22af07a88c4d06afc009f21602a4882342c0cc054",
        },
        dict: Asset {
            file: "ppocrv5_korean_dict.txt",
            bytes: 47_451,
            sha256: "a88071c68c01707489baa79ebe0405b7beb5cca229f4fc94cc3ef992328802d7",
        },
    },
    LanguagePack {
        id: "latin",
        rec: Asset {
            file: "latin_pp-ocrv5_mobile_rec.onnx",
            bytes: 8_069_614,
            sha256: "e3a6bfeea1c8a01d6fccfd480a0bd363fd907f8c65931e228bb2736f5c3e142f",
        },
        dict: Asset {
            file: "ppocrv5_latin_dict.txt",
            bytes: 2_616,
            sha256: "ccbcc45730b3fbbd9050c5bc74db6a99067141ef1035e3d14889a84a6b9b1aff",
        },
    },
    LanguagePack {
        id: "eslav",
        rec: Asset {
            file: "eslav_pp-ocrv5_mobile_rec.onnx",
            bytes: 7_915_218,
            sha256: "36a66a68097e88b103e0f60f489e88c7239d3ea79d96fbac2d80ac9d134944cd",
        },
        dict: Asset {
            file: "ppocrv5_eslav_dict.txt",
            bytes: 1_663,
            sha256: "3e95f1581557162870cacdba5af91a4c6be2890710d395b0c3c7578e7ee5e6eb",
        },
    },
    LanguagePack {
        id: "th",
        rec: Asset {
            file: "th_pp-ocrv5_mobile_rec.onnx",
            bytes: 7_918_606,
            sha256: "5f6ee21242691681261fee01bc39867da9cc8ff9b889f2f048b3cb7f74380217",
        },
        dict: Asset {
            file: "ppocrv5_th_dict.txt",
            bytes: 1_767,
            sha256: "57f5406f94bb6688fb7077f7be65f08bbd71cecf48c01ea26c522cb5c4836b7a",
        },
    },
    LanguagePack {
        id: "arabic",
        rec: Asset {
            file: "arabic_pp-ocrv5_mobile_rec.onnx",
            bytes: 8_026_538,
            sha256: "2768206d9a0ce48eba45b59619184e18161dde8f44115f029920ca17a9dc0384",
        },
        dict: Asset {
            file: "ppocrv5_arabic_dict.txt",
            bytes: 2_369,
            sha256: "7f92f7dbb9b75a4787a83bfb4f6d14a8ab515525130c9d40a9036f61cf6999e9",
        },
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

fn find(app: &AppHandle, id: &str) -> Result<&'static LanguagePack> {
    PACKS
        .iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| super::ocr_error(app, crate::i18n::commands::Key::OcrPackUnknown, id))
}

fn packs_root(app: &AppHandle) -> Result<PathBuf> {
    crate::core::paths::ocr_packs_dir(app)
}

/// 落盘文件字节数是否匹配(快速失败,不是完整性边界——那是 SHA-256 的职责)。
fn size_matches(path: &PathBuf, expected: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() == expected)
        .unwrap_or(false)
}

/// 已完整下载时返回 (rec, dict) 绝对路径。
/// 加载期只按字节数判「是否已下载」;真正的完整性校验在下载落盘时用 SHA-256 完成,
/// 能写该目录的本地攻击者本就越过了信任边界,加载期再哈希收益有限、代价(每次识别哈希 20MB)不值。
pub fn resolve(app: &AppHandle, id: &str) -> Option<(PathBuf, PathBuf)> {
    let pack = PACKS.iter().find(|pack| pack.id == id)?;
    let dir = packs_root(app).ok()?.join(pack.id);
    let rec = dir.join(pack.rec.file);
    let dict = dir.join(pack.dict.file);
    (size_matches(&rec, pack.rec.bytes) && size_matches(&dict, pack.dict.bytes))
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
    let pack = find(app, id)?;
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
    let pack = match find(app, id) {
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
    for asset in [&pack.rec, &pack.dict] {
        let target = dir.join(asset.file);
        if !size_matches(&target, asset.bytes) {
            fetch_file(app, pack.id, asset, &target, base, total).await?;
        }
        base += asset.bytes;
    }
    super::invalidate_engine();
    emit_progress(app, pack.id, total, total, None);
    Ok(())
}

/// 单文件下载:主源失败自动切备源;先写 `.part` 再原子改名;完成后按精确字节数校验。
async fn fetch_file(
    app: &AppHandle,
    pack_id: &str,
    asset: &Asset,
    target: &std::path::Path,
    base: u64,
    total: u64,
) -> Result<()> {
    let urls = [
        format!("{GITHUB_BASE}/{}", asset.file),
        format!("{MODELSCOPE_BASE}/{}", asset.file),
    ];
    let mut last_err = None;
    for url in &urls {
        match fetch_one(app, pack_id, url, asset, target, base, total).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                log::warn!("download {url} failed: {err}");
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        super::ocr_error(
            app,
            crate::i18n::commands::Key::OcrPackDownloadFailed,
            asset.file,
        )
    }))
}

async fn fetch_one(
    app: &AppHandle,
    pack_id: &str,
    url: &str,
    asset: &Asset,
    target: &std::path::Path,
    base: u64,
    total: u64,
) -> Result<()> {
    use crate::i18n::commands::Key;
    use sha2::{Digest, Sha256};

    let response = http_client()
        .get(url)
        .send()
        .await
        .map_err(|err| super::ocr_error(app, Key::OcrPackRequestFailed, err))?
        .error_for_status()
        .map_err(|err| super::ocr_error(app, Key::OcrPackHttpError, err))?;

    let part = target.with_extension("part");
    let mut writer = std::io::BufWriter::new(
        std::fs::File::create(&part).with_context(|| format!("create {part:?}"))?,
    );

    // 边写边算 SHA-256,免二次读盘。
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|err| super::ocr_error(app, Key::OcrPackDownloadInterrupted, err))?;
        std::io::Write::write_all(&mut writer, &chunk)
            .with_context(|| format!("write {part:?}"))?;
        hasher.update(&chunk);
        written += chunk.len() as u64;

        // 进度事件限频(150ms),避免刷爆事件通道。
        if last_emit.elapsed().as_millis() >= 150 {
            last_emit = std::time::Instant::now();
            emit_progress(app, pack_id, base + written, total, None);
        }
    }
    std::io::Write::flush(&mut writer).with_context(|| format!("flush {part:?}"))?;
    drop(writer);

    // 字节数是快速失败;SHA-256 才是完整性边界——同字节数的恶意 ONNX 会被哈希拦下。
    if written != asset.bytes {
        let _ = std::fs::remove_file(&part);
        return Err(super::ocr_error(
            app,
            Key::OcrPackSizeMismatch,
            format!("{written} B / {} B", asset.bytes),
        ));
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != asset.sha256 {
        let _ = std::fs::remove_file(&part);
        log::warn!(
            "ocr asset {} sha256 mismatch from {url}: got {digest}, want {}",
            asset.file,
            asset.sha256
        );
        return Err(super::ocr_error(app, Key::OcrPackHashMismatch, asset.file));
    }
    std::fs::rename(&part, target).with_context(|| format!("rename {part:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// 每个钉死的 SHA-256 都是 64 位小写十六进制;防止手误录成大写/截断,
    /// 否则下载永远校验失败。
    #[test]
    fn pinned_hashes_are_well_formed() {
        for pack in PACKS {
            for asset in [&pack.rec, &pack.dict] {
                assert_eq!(asset.sha256.len(), 64, "{} hash length", asset.file);
                assert!(
                    asset
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                    "{} hash must be lowercase hex",
                    asset.file
                );
            }
        }
    }

    /// 随包分发的内置模型必须与钉死的哈希一致(CI 在 fetch:ocr-models 后跑测试,
    /// 上游资产被同字节数替换会在此被逐字节抓出,阻止投毒模型进签名安装包)。
    /// 模型未拉取时跳过(本地先 `pnpm fetch:ocr-models`)。
    #[test]
    fn bundled_builtin_models_match_pinned_hashes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/ocr");
        let pinned = [
            (
                "pp-ocrv5_mobile_det.onnx",
                "1eb7b4f7ab657ebd1c66d5f79bca7497f29768a2e3c15e52daecbba1a8e4a039",
            ),
            (
                "pp-ocrv5_mobile_rec.onnx",
                "243a0f06d826761323e9045e9b113ab2c191c3aa50565585e628300b8eda0224",
            ),
        ];
        for (file, want) in pinned {
            let path = root.join(file);
            if !path.is_file() {
                eprintln!("skip: {file} not present, run `pnpm fetch:ocr-models`");
                continue;
            }
            let bytes = std::fs::read(&path).expect("read bundled model");
            let digest = format!("{:x}", Sha256::digest(&bytes));
            assert_eq!(digest, want, "{file} hash drifted from pinned value");
        }
    }
}
