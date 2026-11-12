//! SnipStack 历史备份包导出与接收壳识别。
//!
//! `.snipstackbak` 有两种格式：明文模式是标准 ZIP；加密模式是 SnipStack 自有容器。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use anyhow::{anyhow, Context};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Sqlite, SqlitePool};
use tauri::{AppHandle, Emitter, Manager};
use tempfile::{NamedTempFile, TempDir};
use walkdir::WalkDir;
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::{AppError, Result};

pub const BACKUP_EXTENSION: &str = "snipstackbak";
pub const BACKUP_RECEIVED_EVENT: &str = "backup://received";

const MAGIC: &[u8; 12] = b"SNIPSTACKBK1";
const ZIP_MAGIC: &[u8; 2] = b"PK";
const HEADER_LEN_BYTES: usize = 4;
const FORMAT_VERSION: u16 = 1;
const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const SETTINGS_FILENAME: &str = "settings.json";
const DB_FILENAME: &str = "clipboard.db";
const DB_ARCHIVE_DIR: &str = "db";
const RESOURCES_ARCHIVE_DIR: &str = "resources";
const CONFIG_ARCHIVE_DIR: &str = "config";
const MANIFEST_FILENAME: &str = "manifest.json";
/// 启动时检查数据根下有没有残留的覆盖导入回滚副本，有就在日志里醒目记一条。
///
/// 覆盖导入中途进程被杀或断电时，导入前的原数据留在回滚目录里，而下次启动会照常
/// 建出一个空库——用户只看到历史没了。这条日志是排查时找回数据的线索；
/// 不自动挪回：此刻现场可能是「新库已就位」的半成品，替用户选哪一份都可能选错。
pub fn log_rollback_leftovers(app: &AppHandle) {
    let Ok(root) = crate::core::paths::app_data_dir(app) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(ROLLBACK_DIR_PREFIX))
        {
            log::error!(
                "interrupted backup import left pre-import data at {:?}; it is not in use",
                entry.path()
            );
        }
    }
}

/// 覆盖导入时存放导入前原数据的回滚目录名前缀（建在数据根下）。
/// 数据目录迁移据此识别残留的回滚副本，避免把它连同旧目录一起删掉。
pub(crate) const ROLLBACK_DIR_PREFIX: &str = ".import-rollback-";
const INVALID_BACKUP_DB_MESSAGE: &str = "备份中的历史数据库无效或已损坏";
const RESTORE_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(100),
    std::time::Duration::from_millis(250),
    std::time::Duration::from_millis(500),
];

/// 本版本内置的 migration 集合，用于导入前判断备份库能否被本版本打开。
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// 偏好窗口被销毁时暂存的待处理备份接收事件。
///
/// `emit_received_backup` 在 preference 不存在（已空闲销毁）时无法 push 事件——重建是异步的，
/// 前端 listener 尚未挂载。改为存入此 slot，由前端重建后通过 `take_pending_backup` 主动拉取，
/// 两条路径（存活 push / 销毁 pull）互斥，避免事件丢失。
static PENDING_BACKUP: LazyLock<Mutex<Option<BackupReceivedPayload>>> =
    LazyLock::new(|| Mutex::new(None));

/// 应用是否已进入可安全建窗的就绪态（由 `RunEvent::Ready` 置位）。
///
/// macOS 冷启动用文件关联打开 app 时，系统会在事件循环刚起步、`setup` 尚未跑完时，
/// 经 ObjC `application:openURLs:` **同步**投递 `RunEvent::Opened`。此刻去建偏好窗口会
/// panic，而该回调跨 `extern "C"` 边界不可 unwind，直接 abort（应用闪退）。
/// 故未就绪时只把文件路径压入 [`PENDING_OPEN_FILES`]，待 `Ready` 后由
/// [`mark_app_ready`] 统一补投。
#[cfg(target_os = "macos")]
static APP_READY: AtomicBool = AtomicBool::new(false);

/// 就绪前到达的待处理打开文件路径队列（仅冷启动文件关联场景会用到）。
#[cfg(target_os = "macos")]
static PENDING_OPEN_FILES: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHistoryBackupOptions {
    pub mode: BackupExportMode,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryBackupInput {
    pub path: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryBackupOptions {
    pub strategy: BackupImportStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupImportStrategy {
    Merge,
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupExportMode {
    Encrypted,
    Plain,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHistoryBackupResult {
    pub path: String,
    pub total_bytes: u64,
    pub item_count: i64,
    pub text_count: i64,
    pub image_count: i64,
    pub files_count: i64,
    pub resource_bytes: u64,
    pub exported_at: DateTime<Utc>,
    pub mode: BackupExportMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHistoryBackupResult {
    pub strategy: BackupImportStrategy,
    pub imported_items: u64,
    pub skipped_items: u64,
    pub imported_resources: u64,
    pub imported_settings: bool,
    pub requires_restart: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReceivedPayload {
    pub path: String,
    pub source: BackupReceiveSource,
    pub mode: BackupContainerMode,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupReceiveSource {
    OpenFile,
    DragDrop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupContainerMode {
    Encrypted,
    Plain,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerHeader {
    format_version: u16,
    mode: BackupContainerMode,
    kdf: Option<KdfHeader>,
    cipher: Option<CipherHeader>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KdfHeader {
    algorithm: String,
    memory_kib: u32,
    time_cost: u32,
    parallelism: u32,
    salt: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CipherHeader {
    algorithm: String,
    nonce: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    format_version: u16,
    app_name: String,
    app_version: String,
    exported_at: DateTime<Utc>,
    platform: String,
    encryption: ManifestEncryption,
    item_count: i64,
    text_count: i64,
    image_count: i64,
    files_count: i64,
    resource_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ManifestEncryption {
    None,
    Password,
}

#[derive(Debug, Clone, Copy)]
struct BackupCounts {
    item_count: i64,
    text_count: i64,
    image_count: i64,
    files_count: i64,
}

#[derive(Debug, Clone)]
struct BackupSourcePaths {
    db_path: PathBuf,
    resources_dir: PathBuf,
    settings_path: PathBuf,
}

/// 导出当前环境历史数据库、资源文件和设置为 `.snipstackbak` 备份包。
pub async fn export_history_backup(
    app: &AppHandle,
    pool: &SqlitePool,
    target_path: String,
    options: ExportHistoryBackupOptions,
) -> Result<ExportHistoryBackupResult> {
    let target = normalize_backup_path(PathBuf::from(target_path))?;
    let password = validate_password_options(&options)?;
    let exported_at = Utc::now();
    let counts = load_counts(pool).await?;
    let source_paths = backup_source_paths(app)?;
    let resource_bytes = dir_size(&source_paths.resources_dir)?;
    let manifest = build_manifest(app, exported_at, options.mode, counts, resource_bytes)?;

    checkpoint_database(pool).await?;

    let payload_file = NamedTempFile::new().context("failed to create temporary backup payload")?;
    write_payload_zip(&source_paths, payload_file.path(), &manifest, &target)?;
    let total_bytes = write_container(&target, payload_file.path(), options.mode, password)?;

    Ok(ExportHistoryBackupResult {
        path: target.to_string_lossy().into_owned(),
        total_bytes,
        item_count: counts.item_count,
        text_count: counts.text_count,
        image_count: counts.image_count,
        files_count: counts.files_count,
        resource_bytes,
        exported_at,
        mode: options.mode,
    })
}

/// 从 `.snipstackbak` 导入历史和设置；合并写入当前库，覆盖热替换当前数据。
pub async fn import_history_backup(
    app: &AppHandle,
    db: &crate::db::DatabaseState,
    input: ImportHistoryBackupInput,
    options: ImportHistoryBackupOptions,
) -> Result<ImportHistoryBackupResult> {
    validate_import_options(&input, &options)?;

    let path = PathBuf::from(input.path);
    ensure_backup_extension(&path)?;
    let payload = read_backup_payload(&path, input.password.as_deref())?;
    let temp = extract_payload_zip(&payload)?;
    validate_extracted_payload(temp.path())?;
    validate_backup_database(&temp.path().join(DB_ARCHIVE_DIR).join(DB_FILENAME)).await?;
    let backup_settings =
        read_json_file(&temp.path().join(CONFIG_ARCHIVE_DIR).join(SETTINGS_FILENAME))?;

    match options.strategy {
        BackupImportStrategy::Merge => {
            let pool = db.pool().await;
            merge_import(app, &pool, temp.path(), backup_settings, &options).await
        }
        BackupImportStrategy::Overwrite => {
            let backup_settings = parse_backup_settings(backup_settings)?;
            overwrite_import(app, db, temp.path(), backup_settings, &options).await
        }
    }
}

/// 识别 `.snipstackbak` 文件头并返回容器模式；不解密、不导入。
pub fn inspect_backup_file(path: &Path) -> Result<BackupContainerMode> {
    ensure_backup_extension(path)?;

    let mut file = File::open(path).with_context(|| format!("failed to open backup {path:?}"))?;
    inspect_backup_reader(&mut file)
}

/// 将系统打开文件或拖入文件统一转成偏好页接收事件。
///
/// preference 已改为空闲可销毁窗口：若窗口仍存活，照常 show + push 事件；
/// 若已销毁，先把 payload 存入 [`PENDING_BACKUP`]，再 show 触发重建——
/// 前端重建后经 `take_pending_backup` 主动拉取，规避「重建异步、push 丢失」竞态。
pub fn emit_received_backup(
    app: &AppHandle,
    path: PathBuf,
    source: BackupReceiveSource,
) -> Result<()> {
    let mode = inspect_backup_file(&path)?;

    let payload = BackupReceivedPayload {
        path: path.to_string_lossy().into_owned(),
        source,
        mode,
    };

    let exists = app
        .get_webview_window(crate::window::PREFERENCE_WINDOW_LABEL)
        .is_some();

    if !exists {
        set_pending_backup(payload.clone());
    }

    crate::window::show_window(app, crate::window::PREFERENCE_WINDOW_LABEL)?;

    if exists {
        app.emit(BACKUP_RECEIVED_EVENT, payload)
            .context("failed to emit backup received event")?;
    }

    Ok(())
}

/// 存入待处理备份接收事件，覆盖旧值（仅保留最近一次）。
fn set_pending_backup(payload: BackupReceivedPayload) {
    let mut guard = PENDING_BACKUP.lock().unwrap_or_else(|poisoned| {
        log::error!("pending backup mutex poisoned on set, recovering");
        poisoned.into_inner()
    });
    *guard = Some(payload);
}

/// 取走并清空待处理备份接收事件，供偏好窗口重建后首屏拉取。
pub fn take_pending_backup() -> Option<BackupReceivedPayload> {
    let mut guard = PENDING_BACKUP.lock().unwrap_or_else(|poisoned| {
        log::error!("pending backup mutex poisoned on take, recovering");
        poisoned.into_inner()
    });
    guard.take()
}

/// 处理一次「系统打开文件」请求，对应用就绪状态鲁棒。
///
/// macOS 冷启动双击文件关联时，系统会在 `setup` 完成前从 ObjC `application:openURLs:`
/// 同步投递 `RunEvent::Opened`。此时建窗会读取尚未 `manage` 的 state 而 panic，且 panic
/// 无法穿过 tao 的 `extern "C"` 回调边界（`panic_cannot_unwind`）→ abort。
///
/// 故未就绪时只把路径压入 [`PENDING_OPEN_FILES`]，待 [`mark_app_ready`] 在 `RunEvent::Ready`
/// 时排空处理；已就绪则立即处理。调用方仍应额外用 `catch_unwind` 兜底，杜绝任何残余 panic 越界。
#[cfg(target_os = "macos")]
pub fn handle_open_file(app: &AppHandle, path: PathBuf, source: BackupReceiveSource) {
    if !is_backup_path(&path) {
        return;
    }

    if !APP_READY.load(Ordering::Acquire) {
        let mut guard = PENDING_OPEN_FILES.lock().unwrap_or_else(|poisoned| {
            log::error!("pending open files mutex poisoned on push, recovering");
            poisoned.into_inner()
        });
        guard.push(path);
        return;
    }

    if let Err(err) = emit_received_backup(app, path, source) {
        log::error!("handle open file failed: {err:?}");
    }
}

/// 标记应用已就绪并排空启动期暂存的待打开文件。
///
/// 在 `RunEvent::Ready` 调用——此时事件循环已运行、所有 state 已 `manage`，建窗安全。
/// 排空前先置位 [`APP_READY`]，使其后到达的 `Opened` 直接走立即处理路径。
#[cfg(target_os = "macos")]
pub fn mark_app_ready(app: &AppHandle) {
    APP_READY.store(true, Ordering::Release);

    let pending: Vec<PathBuf> = {
        let mut guard = PENDING_OPEN_FILES.lock().unwrap_or_else(|poisoned| {
            log::error!("pending open files mutex poisoned on drain, recovering");
            poisoned.into_inner()
        });
        std::mem::take(&mut *guard)
    };

    for path in pending {
        if let Err(err) = emit_received_backup(app, path, BackupReceiveSource::OpenFile) {
            log::error!("drain pending open file failed: {err:?}");
        }
    }
}

/// 从进程参数中查找 `.snipstackbak` 路径，供 Windows 文件关联和第二实例回调使用。
pub fn backup_path_from_args(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .map(PathBuf::from)
        .find(|path| is_backup_path(path))
}

/// 判断路径是否看起来是 SnipStack 备份包。
pub fn is_backup_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(BACKUP_EXTENSION))
}

fn validate_password_options(
    options: &ExportHistoryBackupOptions,
) -> Result<Option<Zeroizing<String>>> {
    match options.mode {
        BackupExportMode::Encrypted => {
            let Some(password) = options.password.as_ref() else {
                return app_error("请输入备份密码");
            };
            if password.chars().count() < 8 {
                return app_error("备份密码至少需要 8 个字符");
            }

            Ok(Some(Zeroizing::new(password.clone())))
        }
        BackupExportMode::Plain => {
            if options
                .password
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                return app_error("明文备份不应包含密码");
            }

            Ok(None)
        }
    }
}

fn validate_import_options(
    input: &ImportHistoryBackupInput,
    _options: &ImportHistoryBackupOptions,
) -> Result<()> {
    let path = PathBuf::from(input.path.as_str());
    match inspect_backup_file(&path)? {
        BackupContainerMode::Encrypted => {
            if input.password.as_deref().is_none_or(str::is_empty) {
                return app_error("请输入备份密码");
            }
        }
        BackupContainerMode::Plain => {
            if input
                .password
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                return app_error("明文备份不应包含密码");
            }
        }
    }

    Ok(())
}

fn normalize_backup_path(path: PathBuf) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return app_error("请选择备份保存位置");
    }

    match path.extension().and_then(|value| value.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case(BACKUP_EXTENSION) => Ok(path),
        Some(_) => app_error(format!("备份文件后缀必须是 .{BACKUP_EXTENSION}")),
        None => Ok(path.with_extension(BACKUP_EXTENSION)),
    }
}

fn ensure_backup_extension(path: &Path) -> Result<()> {
    if is_backup_path(path) {
        return Ok(());
    }

    app_error(format!("请选择 .{BACKUP_EXTENSION} 备份文件"))
}

async fn checkpoint_database(pool: &SqlitePool) -> Result<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .context("failed to checkpoint backup database")?;

    Ok(())
}

async fn load_counts(pool: &SqlitePool) -> Result<BackupCounts> {
    let item_count = count_items(pool, None).await?;
    let text_count = count_items(pool, Some("text")).await?;
    let image_count = count_items(pool, Some("image")).await?;
    let files_count = count_items(pool, Some("files")).await?;

    Ok(BackupCounts {
        item_count,
        text_count,
        image_count,
        files_count,
    })
}

async fn count_items(pool: &SqlitePool, kind: Option<&str>) -> Result<i64> {
    let count = if let Some(kind) = kind {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clipboard_items WHERE kind = ?")
            .bind(kind)
            .fetch_one(pool)
            .await
    } else {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clipboard_items")
            .fetch_one(pool)
            .await
    }
    .context("failed to count backup items")?;

    Ok(count)
}

fn build_manifest(
    app: &AppHandle,
    exported_at: DateTime<Utc>,
    mode: BackupExportMode,
    counts: BackupCounts,
    resource_bytes: u64,
) -> Result<BackupManifest> {
    Ok(BackupManifest {
        format_version: FORMAT_VERSION,
        app_name: app.package_info().name.to_string(),
        app_version: app.package_info().version.to_string(),
        exported_at,
        platform: current_platform().to_owned(),
        encryption: match mode {
            BackupExportMode::Encrypted => ManifestEncryption::Password,
            BackupExportMode::Plain => ManifestEncryption::None,
        },
        item_count: counts.item_count,
        text_count: counts.text_count,
        image_count: counts.image_count,
        files_count: counts.files_count,
        resource_bytes,
    })
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    }
}

/// 汇总备份允许写入包内的源路径，避免把日志、缓存、临时文件等环境杂项带进迁移包。
fn backup_source_paths(app: &AppHandle) -> Result<BackupSourcePaths> {
    Ok(BackupSourcePaths {
        db_path: crate::db::db_path(app)?,
        resources_dir: crate::core::paths::resources_dir(app)?,
        settings_path: crate::core::paths::config_dir(app)?.join(SETTINGS_FILENAME),
    })
}

fn write_payload_zip(
    source_paths: &BackupSourcePaths,
    path: &Path,
    manifest: &BackupManifest,
    target_path: &Path,
) -> Result<()> {
    let file = File::create(path).with_context(|| format!("failed to create payload {path:?}"))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("manifest.json", options)
        .context("failed to write backup manifest entry")?;
    let manifest_bytes =
        serde_json::to_vec_pretty(manifest).context("failed to serialize backup manifest")?;
    zip.write_all(&manifest_bytes)
        .context("failed to write backup manifest")?;

    add_optional_file(
        &mut zip,
        &source_paths.db_path,
        &archive_path(DB_ARCHIVE_DIR, Path::new("clipboard.db"))?,
        options,
        target_path,
    )?;
    add_dir_contents(
        &mut zip,
        &source_paths.resources_dir,
        RESOURCES_ARCHIVE_DIR,
        options,
        target_path,
    )?;
    add_optional_file(
        &mut zip,
        &source_paths.settings_path,
        &archive_path(CONFIG_ARCHIVE_DIR, Path::new(SETTINGS_FILENAME))?,
        options,
        target_path,
    )?;

    zip.finish()
        .context("failed to finish backup payload archive")?;

    Ok(())
}

fn add_file<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    archive_name: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    zip.start_file(archive_name, options)
        .with_context(|| format!("failed to start archive file {archive_name}"))?;
    let mut file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    std::io::copy(&mut file, zip).with_context(|| format!("failed to archive {path:?}"))?;

    Ok(())
}

fn add_optional_file<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    archive_name: &str,
    options: SimpleFileOptions,
    target_path: &Path,
) -> Result<()> {
    if !path.exists() || same_path(path, target_path) {
        return Ok(());
    }

    add_file(zip, path, archive_name, options)
}

fn add_dir_contents<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    root: &Path,
    archive_root: &str,
    options: SimpleFileOptions,
    target_path: &Path,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {path:?}"))?;
        if !metadata.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if should_skip_backup_path(path, file_name, target_path) {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to strip root {root:?} from {path:?}"))?;
        let archive_name = archive_path(archive_root, relative)?;
        add_file(zip, path, &archive_name, options)?;
    }

    Ok(())
}

fn archive_path(prefix: &str, relative: &Path) -> Result<String> {
    let mut parts = Vec::new();
    if !prefix.is_empty() {
        parts.push(prefix.to_owned());
    }
    for part in relative.components() {
        let std::path::Component::Normal(value) = part else {
            return app_error("backup path contains unsupported component");
        };
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("backup path is not valid utf-8"))?;
        parts.push(value.to_owned());
    }

    Ok(parts.join("/"))
}

fn should_skip_backup_path(path: &Path, file_name: &str, target_path: &Path) -> bool {
    if same_path(path, target_path) {
        return true;
    }

    file_name.ends_with(".tmp")
        || file_name.ends_with(".temp")
        || file_name.starts_with(".tmp")
        || file_name == ".DS_Store"
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());

    left == right
}

fn write_container(
    target: &Path,
    payload_path: &Path,
    mode: BackupExportMode,
    password: Option<Zeroizing<String>>,
) -> Result<u64> {
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;

    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file under {parent:?}"))?;
    match mode {
        BackupExportMode::Plain => {
            copy_file_into_writer(payload_path, &mut temp)?;
        }
        BackupExportMode::Encrypted => {
            let password = password.ok_or_else(|| anyhow!("missing backup password"))?;
            let mut payload = Vec::new();
            File::open(payload_path)
                .with_context(|| format!("failed to open payload {payload_path:?}"))?
                .read_to_end(&mut payload)
                .context("failed to read backup payload")?;

            let encrypted = encrypt_payload(&payload, &password)?;
            write_header(&mut temp, &encrypted.header)?;
            temp.write_all(&encrypted.ciphertext)
                .context("failed to write encrypted backup payload")?;
        }
    }

    temp.flush().context("failed to flush backup file")?;
    temp.as_file()
        .sync_all()
        .context("failed to sync backup file")?;
    temp.persist(target)
        .map_err(|err| anyhow!(err))
        .with_context(|| format!("failed to persist backup to {target:?}"))?;

    let total_bytes = fs::metadata(target)
        .with_context(|| format!("failed to read backup metadata {target:?}"))?
        .len();

    Ok(total_bytes)
}

struct EncryptedPayload {
    header: ContainerHeader,
    ciphertext: Vec<u8>,
}

fn encrypt_payload(payload: &[u8], password: &str) -> Result<EncryptedPayload> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(KEY_LEN),
    )
    .context("failed to build argon2 params")?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), &salt, key.as_mut())
        .context("failed to derive backup key")?;

    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .context("failed to initialize backup cipher")?;
    let nonce = XNonce::from(nonce);
    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .map_err(|_| anyhow!("failed to encrypt backup payload"))?;

    Ok(EncryptedPayload {
        header: ContainerHeader {
            format_version: FORMAT_VERSION,
            mode: BackupContainerMode::Encrypted,
            kdf: Some(KdfHeader {
                algorithm: "argon2id".to_owned(),
                memory_kib: ARGON2_MEMORY_KIB,
                time_cost: ARGON2_TIME_COST,
                parallelism: ARGON2_PARALLELISM,
                salt: salt.to_vec(),
            }),
            cipher: Some(CipherHeader {
                algorithm: "xchacha20poly1305".to_owned(),
                nonce: nonce.to_vec(),
            }),
        },
        ciphertext,
    })
}

fn write_header<W: Write>(writer: &mut W, header: &ContainerHeader) -> Result<()> {
    let header_bytes = serde_json::to_vec(header).context("failed to serialize backup header")?;
    let header_len: u32 = header_bytes
        .len()
        .try_into()
        .context("backup header is too large")?;

    writer
        .write_all(MAGIC)
        .context("failed to write backup magic")?;
    writer
        .write_all(&header_len.to_le_bytes())
        .context("failed to write backup header length")?;
    writer
        .write_all(&header_bytes)
        .context("failed to write backup header")?;

    Ok(())
}

fn read_backup_payload(path: &Path, password: Option<&str>) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("failed to open backup {path:?}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read backup {path:?}"))?;

    if bytes.starts_with(ZIP_MAGIC) {
        return Ok(bytes);
    }
    if !bytes.starts_with(MAGIC) {
        return app_error("不是有效的 SnipStack 备份文件");
    }

    let mut cursor = Cursor::new(bytes.as_slice());
    cursor.set_position(MAGIC.len() as u64);
    let header = read_container_header_after_magic(&mut cursor)?;
    let ciphertext_start = cursor.position() as usize;
    let Some(kdf) = header.kdf else {
        return app_error("加密备份文件头无效");
    };
    let Some(cipher) = header.cipher else {
        return app_error("加密备份文件头无效");
    };
    let Some(password) = password else {
        return app_error("请输入备份密码");
    };

    decrypt_payload(&bytes[ciphertext_start..], password, &kdf, &cipher)
}

fn decrypt_payload(
    ciphertext: &[u8],
    password: &str,
    kdf: &KdfHeader,
    cipher_header: &CipherHeader,
) -> Result<Vec<u8>> {
    if kdf.algorithm != "argon2id" || cipher_header.algorithm != "xchacha20poly1305" {
        return app_error("暂不支持该备份加密格式");
    }
    if kdf.salt.len() != SALT_LEN || cipher_header.nonce.len() != NONCE_LEN {
        return app_error("加密备份文件头无效");
    }

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let params = Params::new(
        kdf.memory_kib,
        kdf.time_cost,
        kdf.parallelism,
        Some(KEY_LEN),
    )
    .context("failed to build argon2 params")?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), &kdf.salt, key.as_mut())
        .context("failed to derive backup key")?;

    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).context("failed to initialize cipher")?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&cipher_header.nonce);
    let nonce = XNonce::from(nonce);
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| AppError::Other(anyhow!("备份密码不正确或文件已损坏")))
}

fn extract_payload_zip(payload: &[u8]) -> Result<TempDir> {
    let temp = tempfile::tempdir().context("failed to create temporary import directory")?;
    let cursor = Cursor::new(payload);
    let mut archive = ZipArchive::new(cursor).context("failed to read backup zip payload")?;
    archive
        .extract(temp.path())
        .context("failed to extract backup payload")?;

    Ok(temp)
}

fn validate_extracted_payload(root: &Path) -> Result<()> {
    if !root.join(MANIFEST_FILENAME).exists() {
        return app_error("备份文件缺少 manifest.json");
    }
    if !root.join(DB_ARCHIVE_DIR).join(DB_FILENAME).exists() {
        return app_error("备份文件缺少历史数据库");
    }
    if !root
        .join(CONFIG_ARCHIVE_DIR)
        .join(SETTINGS_FILENAME)
        .exists()
    {
        return app_error("备份文件缺少设置文件");
    }

    Ok(())
}

fn inspect_backup_reader<R: Read>(reader: &mut R) -> Result<BackupContainerMode> {
    let mut magic = [0u8; MAGIC.len()];
    let read = reader
        .read(&mut magic)
        .context("failed to read backup magic")?;
    if read >= MAGIC.len() && &magic == MAGIC {
        let header = read_container_header_after_magic(reader)?;
        return Ok(header.mode);
    }
    if read >= ZIP_MAGIC.len() && &magic[..ZIP_MAGIC.len()] == ZIP_MAGIC {
        return Ok(BackupContainerMode::Plain);
    }

    app_error("不是有效的 SnipStack 备份文件")
}

fn read_container_header_after_magic<R: Read>(reader: &mut R) -> Result<ContainerHeader> {
    let mut len = [0u8; HEADER_LEN_BYTES];
    reader
        .read_exact(&mut len)
        .context("failed to read backup header length")?;
    let header_len = u32::from_le_bytes(len) as usize;
    if header_len == 0 || header_len > 64 * 1024 {
        return app_error("备份文件头无效");
    }

    let mut header = vec![0u8; header_len];
    reader
        .read_exact(&mut header)
        .context("failed to read backup header")?;
    let header: ContainerHeader =
        serde_json::from_slice(&header).context("failed to parse backup header")?;
    if header.format_version != FORMAT_VERSION {
        return app_error("暂不支持该备份格式版本");
    }

    Ok(header)
}

async fn merge_import(
    app: &AppHandle,
    pool: &SqlitePool,
    root: &Path,
    mut settings_patch: serde_json::Value,
    _options: &ImportHistoryBackupOptions,
) -> Result<ImportHistoryBackupResult> {
    let db_path = root.join(DB_ARCHIVE_DIR).join(DB_FILENAME);
    let backup_pool = open_backup_db(&db_path).await?;
    let merged = merge_history(pool, &backup_pool).await;
    backup_pool.close().await;
    let outcome = merged?;
    let resources = root.join(RESOURCES_ARCHIVE_DIR);
    let imported_resources =
        copy_dir_missing(&resources, &crate::core::paths::resources_dir(app)?)?;
    emit_clipboard_imported(app);

    strip_machine_local_settings(&mut settings_patch);
    let next = app
        .state::<crate::settings::SettingsStore>()
        .update(settings_patch)?;
    crate::commands::apply_settings_side_effects(app, &next);
    emit_settings_updated(app, &next);

    Ok(ImportHistoryBackupResult {
        strategy: BackupImportStrategy::Merge,
        imported_items: outcome.imported_items,
        skipped_items: outcome.skipped_items,
        imported_resources,
        imported_settings: true,
        requires_restart: false,
    })
}

/// 覆盖导入：主库、resources、设置三者要么全部换成备份内容，要么全部回到导入前。
///
/// 现场数据先 rename 进同卷回滚目录，新内容就位、`db::init` 成功、设置落盘后才删回滚目录；
/// 任一步失败都把回滚目录里的原件挪回并重开旧库，保证 `DatabaseState` 里始终是可用连接池。
async fn overwrite_import(
    app: &AppHandle,
    db: &crate::db::DatabaseState,
    root: &Path,
    backup_settings: crate::settings::Settings,
    _options: &ImportHistoryBackupOptions,
) -> Result<ImportHistoryBackupResult> {
    let _pause_guard = pause_watcher(app);

    let live = LiveDataPaths {
        db_path: crate::db::db_path(app)?,
        resources_dir: crate::core::paths::resources_dir(app)?,
    };
    let db_src = root.join(DB_ARCHIVE_DIR).join(DB_FILENAME);
    let resources_src = root.join(RESOURCES_ARCHIVE_DIR);
    let resources_src = resources_src.exists().then_some(resources_src);
    let store = app.state::<crate::settings::SettingsStore>();
    let store = store.inner();

    let mut imported = None;
    let imported_slot = &mut imported;
    let live_ref = &live;
    let db_src_ref = db_src.as_path();
    let resources_src_ref = resources_src.as_deref();
    let replaced = db
        .close_and_replace(move || async move {
            let swap = swap_live_data(
                live_ref,
                db_src_ref,
                resources_src_ref,
                || crate::db::init(app),
                || {
                    store.replace_with(|current| {
                        overlay_machine_local_settings(backup_settings, current)
                    })
                },
            )
            .await;
            *imported_slot = Some(swap.result);
            swap.pool
        })
        .await;
    // 导入自身的错误优先上报：它带着「原数据保存在哪里」，连旧库都重开不了时
    // 也不能被连接池的错误盖掉。
    let next = match imported {
        Some(Err(err)) => return Err(err),
        Some(Ok(next)) => {
            replaced?;
            next
        }
        None => {
            replaced?;
            return app_error("备份导入未执行");
        }
    };

    refresh_apps_registry(app).await;
    emit_clipboard_imported(app);
    crate::commands::apply_settings_side_effects(app, &next);
    emit_settings_updated(app, &next);

    Ok(ImportHistoryBackupResult {
        strategy: BackupImportStrategy::Overwrite,
        imported_items: 0,
        skipped_items: 0,
        imported_resources: 0,
        imported_settings: true,
        requires_restart: false,
    })
}

/// 覆盖导入涉及的现场路径。db 与 resources 都在数据根目录下，回滚目录建在 resources 的父目录，
/// 保证挪动走同卷 rename 而不是再拷一份。
#[derive(Debug, Clone)]
struct LiveDataPaths {
    db_path: PathBuf,
    resources_dir: PathBuf,
}

impl LiveDataPaths {
    /// 主库连同 WAL sidecar；三者必须一起挪动才能保持一致。
    fn db_files(&self) -> [PathBuf; 3] {
        let base = self.db_path.as_os_str();
        let with_suffix = |suffix: &str| {
            let mut name = base.to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        };

        [
            self.db_path.clone(),
            with_suffix("-wal"),
            with_suffix("-shm"),
        ]
    }

    fn rollback_parent(&self) -> Result<&Path> {
        self.resources_dir
            .parent()
            .ok_or_else(|| AppError::Other(anyhow!("resources dir has no parent")))
    }
}

/// 覆盖导入的回滚现场：记录被挪进回滚目录的原件与已放入新内容的现场路径。
///
/// `restore` 幂等，可在 Windows 句柄迟迟不释放时重试；回滚目录里还有原件时绝不自动删除，
/// drop 时会保留目录并记日志，防止用户唯一的数据随临时目录一起消失。
struct OverwriteRollback {
    dir: Option<TempDir>,
    /// (现场路径, 回滚目录内路径)，按挪出顺序记录，按倒序挪回。
    moved: Vec<(PathBuf, PathBuf)>,
    /// 可能已放入新内容、恢复前要先清掉的现场路径。
    placed: Vec<PathBuf>,
}

impl OverwriteRollback {
    /// 在数据根目录下新建唯一命名的回滚目录；历史残留目录名字不同，不影响本次导入。
    fn create(live: &LiveDataPaths) -> Result<Self> {
        let parent = live.rollback_parent()?;
        fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
        let dir = tempfile::Builder::new()
            .prefix(ROLLBACK_DIR_PREFIX)
            .tempdir_in(parent)
            .with_context(|| format!("failed to create rollback dir under {parent:?}"))?;

        Ok(Self {
            dir: Some(dir),
            moved: Vec::new(),
            placed: Vec::new(),
        })
    }

    fn dir_path(&self) -> Result<PathBuf> {
        self.dir
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .ok_or_else(|| AppError::Other(anyhow!("rollback dir already released")))
    }

    /// 把现场主库（含 sidecar）与需要替换的 resources 挪进回滚目录，再放入备份内容。
    /// 备份里没有 resources 目录时保留现有 resources，与此前行为一致。
    fn swap_in(
        &mut self,
        live: &LiveDataPaths,
        db_src: &Path,
        resources_src: Option<&Path>,
    ) -> Result<()> {
        let rollback_dir = self.dir_path()?;
        let mut targets = live.db_files().to_vec();
        if resources_src.is_some() {
            targets.push(live.resources_dir.clone());
        }

        for live_path in targets {
            if !path_exists(&live_path)? {
                continue;
            }
            let name = live_path
                .file_name()
                .ok_or_else(|| anyhow!("live path {live_path:?} has no file name"))?;
            let held = rollback_dir.join(name);
            fs::rename(&live_path, &held)
                .with_context(|| format!("failed to move {live_path:?} to {held:?}"))?;
            self.moved.push((live_path, held));
        }

        self.placed.extend(live.db_files());
        copy_file_to(db_src, &live.db_path)?;

        if let Some(src) = resources_src {
            self.placed.push(live.resources_dir.clone());
            copy_dir_all(src, &live.resources_dir)?;
        }

        Ok(())
    }

    /// 清掉放入的新内容后把原件挪回现场。每完成一项就出栈，重试时不会误删已恢复的原件。
    fn restore(&mut self) -> Result<()> {
        while let Some(path) = self.placed.last() {
            remove_path_if_exists(path)?;
            self.placed.pop();
        }

        while let Some((live_path, held)) = self.moved.last() {
            fs::rename(held, live_path)
                .with_context(|| format!("failed to move {held:?} back to {live_path:?}"))?;
            self.moved.pop();
        }

        Ok(())
    }

    /// 导入已完整生效，删除回滚目录；删不掉只是残留占盘，记日志即可。
    fn commit(mut self) {
        self.moved.clear();
        if let Some(dir) = self.dir.take() {
            if let Err(err) = dir.close() {
                log::warn!("remove backup import rollback dir failed: {err}");
            }
        }
    }

    /// 放弃自动删除回滚目录并返回其路径，供错误文案告知用户原数据所在位置。
    fn keep(&mut self) -> Option<PathBuf> {
        self.dir.take().map(TempDir::keep)
    }
}

impl Drop for OverwriteRollback {
    fn drop(&mut self) {
        if self.moved.is_empty() {
            return;
        }
        if let Some(path) = self.keep() {
            log::error!("backup import rollback dir kept with original data at {path:?}");
        }
    }
}

/// 覆盖导入一次换装的结果：`pool` 是应装进 `DatabaseState` 的连接池（成功时为新库，
/// 失败时为恢复后的旧库），`result` 是导入本身的结果。
struct SwapOutcome<T> {
    pool: Result<SqlitePool>,
    result: Result<T>,
}

/// 覆盖导入的核心事务，只依赖路径与注入的打开/收尾动作，便于脱离 AppHandle 测试。
///
/// 调用前现有连接池必须已关闭。`open` 打开（并 migrate）`live.db_path` 处的库，成功导入和
/// 失败后重开旧库都用它；`finish` 是新库就位后最后一个可能失败的步骤（设置落盘），
/// 它失败同样整体回滚。
async fn swap_live_data<T, Open, OpenFut, Finish>(
    live: &LiveDataPaths,
    db_src: &Path,
    resources_src: Option<&Path>,
    open: Open,
    finish: Finish,
) -> SwapOutcome<T>
where
    Open: Fn() -> OpenFut,
    OpenFut: std::future::Future<Output = Result<SqlitePool>>,
    Finish: FnOnce() -> Result<T>,
{
    let mut rollback = match OverwriteRollback::create(live) {
        Ok(rollback) => rollback,
        Err(err) => {
            return SwapOutcome {
                pool: open().await,
                result: Err(err),
            };
        }
    };

    let import_err = match rollback.swap_in(live, db_src, resources_src) {
        Err(err) => err,
        Ok(()) => match open().await {
            Err(err) => err,
            Ok(pool) => match finish() {
                Ok(value) => {
                    rollback.commit();
                    return SwapOutcome {
                        pool: Ok(pool),
                        result: Ok(value),
                    };
                }
                Err(err) => {
                    pool.close().await;
                    err
                }
            },
        },
    };
    log::error!("backup overwrite import failed, rolling back: {import_err:?}");

    let result = match restore_with_retry(&mut rollback).await {
        Ok(()) => Err(rolled_back_error(&import_err)),
        Err(restore_err) => {
            let kept = rollback.keep();
            log::error!(
                "backup overwrite rollback failed, original data kept at {kept:?}: {restore_err:?}"
            );
            app_error(format!(
                "导入失败且未能自动恢复原数据，导入前的数据保存在 {}",
                kept.map(|path| path.display().to_string())
                    .unwrap_or_default()
            ))
        }
    };

    SwapOutcome {
        pool: open().await,
        result,
    }
}

/// Windows 上刚关闭的连接可能还占着文件句柄，恢复失败时短暂等待后重试。
async fn restore_with_retry(rollback: &mut OverwriteRollback) -> Result<()> {
    let mut attempt = 0;
    loop {
        match rollback.restore() {
            Ok(()) => return Ok(()),
            Err(err) if attempt < RESTORE_RETRY_DELAYS.len() => {
                log::warn!("restore backup import rollback failed, retrying: {err:?}");
                tokio::time::sleep(RESTORE_RETRY_DELAYS[attempt]).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// 已回滚的导入错误：告诉用户数据没动，并带上底层根因（如磁盘已满、文件被占用）。
fn rolled_back_error(err: &AppError) -> AppError {
    let cause = match err {
        AppError::Other(err) => err.root_cause().to_string(),
        other => other.to_string(),
    };

    AppError::Other(anyhow!("导入未完成，已恢复为导入前的数据（{cause}）"))
}

/// 只有 `NotFound` 视为不存在；权限等其它错误照常上抛，避免把读不到的现场数据当成空位覆盖。
fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(AppError::Other(
            anyhow!(err).context(format!("failed to inspect {path:?}")),
        )),
    }
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path_exists(path)? {
        return Ok(());
    }
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to inspect {path:?}"))?;
    let removed = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    removed.with_context(|| format!("failed to remove {path:?}"))?;

    Ok(())
}

/// 校验备份库能被本版本打开：只读打开后检查 `_sqlx_migrations`，在动任何现有数据之前拒绝
/// 来自更新版本或已损坏的备份，避免覆盖导入后每次启动 migration 都失败。
async fn validate_backup_database(path: &Path) -> Result<()> {
    let pool = match open_backup_db(path).await {
        Ok(pool) => pool,
        Err(err) => {
            log::warn!("open backup database for validation failed: {err:?}");
            return app_error(INVALID_BACKUP_DB_MESSAGE);
        }
    };
    let checked = ensure_backup_migrations_known(&pool).await;
    pool.close().await;

    checked
}

async fn ensure_backup_migrations_known(pool: &SqlitePool) -> Result<()> {
    let applied = sqlx::query_as::<_, (i64, bool, Vec<u8>)>(
        "SELECT version, success, checksum FROM _sqlx_migrations",
    )
    .fetch_all(pool)
    .await;

    match applied {
        Ok(applied) => check_applied_migrations(&applied),
        Err(err) => {
            log::warn!("read backup migrations failed: {err}");
            app_error(INVALID_BACKUP_DB_MESSAGE)
        }
    }
}

/// 按 sqlx `Migrator::run` 的同一套检查判断：未知版本（VersionMissing）、未完成（Dirty）、
/// checksum 不符（VersionMismatch）都会让本版本启动失败，一律在导入前拒绝。
/// 空表同样拒绝——合法备份至少应用过初始 migration。
fn check_applied_migrations(applied: &[(i64, bool, Vec<u8>)]) -> Result<()> {
    if applied.is_empty() {
        return app_error(INVALID_BACKUP_DB_MESSAGE);
    }

    for (version, success, checksum) in applied {
        let known = MIGRATOR
            .iter()
            .filter(|migration| !migration.migration_type.is_down_migration())
            .find(|migration| migration.version == *version);
        let Some(known) = known else {
            return app_error("备份来自更新版本的 SnipStack，请先升级应用再导入");
        };
        if !success || known.checksum.as_ref() != checksum.as_slice() {
            return app_error(INVALID_BACKUP_DB_MESSAGE);
        }
    }

    Ok(())
}

fn parse_backup_settings(value: serde_json::Value) -> Result<crate::settings::Settings> {
    serde_json::from_value(value).map_err(|err| {
        log::warn!("parse backup settings failed: {err}");
        AppError::Other(anyhow!("备份中的设置文件无效"))
    })
}

/// 从备份设置 patch 里剔除本机状态字段（引导进度、更新检查记录、隐藏的管理员运行选项），
/// 合并导入后这些字段保持本机当前值。键名与 `settings::model` 的 serde camelCase 命名一致。
fn strip_machine_local_settings(patch: &mut serde_json::Value) {
    let Some(root) = patch.as_object_mut() else {
        return;
    };

    root.remove("onboarding");
    if let Some(update) = root
        .get_mut("update")
        .and_then(serde_json::Value::as_object_mut)
    {
        update.remove("lastCheckedAt");
        update.remove("skippedVersion");
    }
    if let Some(general) = root
        .get_mut("general")
        .and_then(serde_json::Value::as_object_mut)
    {
        general.remove("runAsAdmin");
    }
}

/// 覆盖导入的设置：以备份设置为底，本机状态字段取本机当前值，与 [`strip_machine_local_settings`]
/// 剔除的字段集合保持一致。
fn overlay_machine_local_settings(
    mut imported: crate::settings::Settings,
    current: &crate::settings::Settings,
) -> crate::settings::Settings {
    imported.onboarding = current.onboarding.clone();
    imported.update.last_checked_at = current.update.last_checked_at.clone();
    imported.update.skipped_version = current.update.skipped_version.clone();
    imported.general.run_as_admin = current.general.run_as_admin;

    imported
}

struct MergeOutcome {
    imported_items: u64,
    skipped_items: u64,
}

async fn open_backup_db(path: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);

    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open backup database at {path:?}"))?)
}

async fn merge_history(current: &SqlitePool, backup: &SqlitePool) -> Result<MergeOutcome> {
    let mut tx = current.begin().await.context("failed to begin import")?;
    merge_groups(&mut tx, backup).await?;
    merge_apps(&mut tx, backup).await?;
    merge_file_type_icons(&mut tx, backup).await?;
    let outcome = merge_items(&mut tx, backup).await?;
    tx.commit().await.context("failed to commit import")?;

    Ok(outcome)
}

async fn merge_groups(tx: &mut sqlx::Transaction<'_, Sqlite>, backup: &SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            bool,
            i64,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, name, icon, is_hidden, sort_order, created_at, updated_at FROM clipboard_groups",
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup groups")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO clipboard_groups \
             (id, name, icon, is_hidden, sort_order, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .bind(row.5)
        .bind(row.6)
        .execute(&mut **tx)
        .await
        .context("failed to import group")?;
    }

    Ok(())
}

async fn merge_apps(tx: &mut sqlx::Transaction<'_, Sqlite>, backup: &SqlitePool) -> Result<()> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, name, icon_file, platform, created_at, updated_at FROM clipboard_apps"
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup apps")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO clipboard_apps \
             (id, name, icon_file, platform, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .bind(row.5)
        .execute(&mut **tx)
        .await
        .context("failed to import app")?;
    }

    Ok(())
}

async fn merge_file_type_icons(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    backup: &SqlitePool,
) -> Result<()> {
    let rows = sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT cache_key, platform, icon_file, created_at, updated_at FROM file_type_icons",
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup file type icons")?;

    for row in rows {
        sqlx::query(
            "INSERT OR IGNORE INTO file_type_icons \
             (cache_key, platform, icon_file, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(row.0)
        .bind(row.1)
        .bind(row.2)
        .bind(row.3)
        .bind(row.4)
        .execute(&mut **tx)
        .await
        .context("failed to import file type icon")?;
    }

    Ok(())
}

async fn merge_items(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    backup: &SqlitePool,
) -> Result<MergeOutcome> {
    let rows = sqlx::query_as::<_, BackupItemRow>(
        "SELECT id, kind, sub_kind, group_id, source_app_id, content, content_hash, search_text, \
         summary, file_types, size, width, height, use_count, is_favorite, is_pinned, is_sensitive, platform, note, \
         created_at, updated_at FROM clipboard_items ORDER BY created_at ASC",
    )
    .fetch_all(backup)
    .await
    .context("failed to read backup items")?;

    let mut imported_items = 0;
    let mut skipped_items = 0;
    for row in rows {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM clipboard_items WHERE kind = ? AND content_hash = ? LIMIT 1",
        )
        .bind(row.kind.as_str())
        .bind(row.content_hash.as_str())
        .fetch_optional(&mut **tx)
        .await
        .context("failed to check duplicate item")?;
        if exists.is_some() {
            skipped_items += 1;
            continue;
        }

        sqlx::query(
            "INSERT OR IGNORE INTO clipboard_items \
             (id, kind, sub_kind, group_id, source_app_id, content, content_hash, search_text, \
              summary, file_types, size, width, height, use_count, is_favorite, is_pinned, is_sensitive, platform, note, \
              created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row.id)
        .bind(row.kind)
        .bind(row.sub_kind)
        .bind(row.group_id)
        .bind(row.source_app_id)
        .bind(row.content)
        .bind(row.content_hash)
        .bind(row.search_text)
        .bind(row.summary)
        .bind(row.file_types)
        .bind(row.size)
        .bind(row.width)
        .bind(row.height)
        .bind(row.use_count)
        .bind(row.is_favorite)
        .bind(row.is_pinned)
        .bind(row.is_sensitive)
        .bind(row.platform)
        .bind(row.note)
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(&mut **tx)
        .await
        .context("failed to import item")?;
        imported_items += 1;
    }

    Ok(MergeOutcome {
        imported_items,
        skipped_items,
    })
}

#[derive(sqlx::FromRow)]
struct BackupItemRow {
    id: String,
    kind: String,
    sub_kind: Option<String>,
    group_id: Option<String>,
    source_app_id: Option<String>,
    content: String,
    content_hash: String,
    search_text: Option<String>,
    summary: Option<String>,
    file_types: Option<String>,
    size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    use_count: i64,
    is_favorite: bool,
    is_pinned: bool,
    is_sensitive: bool,
    platform: String,
    note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct WatcherPauseRestore {
    pause: Option<crate::clipboard::WatcherPause>,
    previous: bool,
}

impl Drop for WatcherPauseRestore {
    fn drop(&mut self) {
        if let Some(pause) = &self.pause {
            pause.set_paused(self.previous);
        }
    }
}

/// 暂停剪贴板监听，并在 guard drop 时恢复导入前的暂停状态。
fn pause_watcher(app: &AppHandle) -> WatcherPauseRestore {
    let pause = app.try_state::<crate::clipboard::WatcherPause>();
    let previous = pause.as_ref().is_some_and(|state| state.is_paused());
    if let Some(state) = pause.as_ref() {
        state.set_paused(true);
    }

    WatcherPauseRestore {
        pause: pause.map(|state| state.inner().clone()),
        previous,
    }
}

/// 覆盖导入后从新数据库重建来源应用内存缓存。
async fn refresh_apps_registry(app: &AppHandle) {
    let Some(registry) = app.try_state::<crate::clipboard::AppsRegistry>() else {
        return;
    };

    if let Err(err) = registry.load_from_db().await {
        log::warn!("refresh apps registry after backup overwrite failed: {err}");
    }
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    Ok(serde_json::from_str(&content).with_context(|| format!("failed to parse {path:?}"))?)
}

fn copy_file_to(src: &Path, dst: &Path) -> Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {parent:?}"))?;
    fs::copy(src, dst).with_context(|| format!("failed to copy {src:?} to {dst:?}"))?;

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<u64> {
    if !src.exists() {
        return Ok(0);
    }

    let mut copied = 0;
    for entry in WalkDir::new(src)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        let relative = path
            .strip_prefix(src)
            .with_context(|| format!("failed to strip {src:?} from {path:?}"))?;
        if relative.as_os_str().is_empty() {
            continue;
        }

        let target = dst.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create directory {target:?}"))?;
            continue;
        }

        copy_file_to(path, &target)?;
        copied += 1;
    }

    Ok(copied)
}

fn copy_dir_missing(src: &Path, dst: &Path) -> Result<u64> {
    if !src.exists() {
        return Ok(0);
    }

    let mut copied = 0;
    for entry in WalkDir::new(src)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(src)
            .with_context(|| format!("failed to strip {src:?} from {path:?}"))?;
        let target = dst.join(relative);
        if target.exists() {
            continue;
        }

        copy_file_to(path, &target)?;
        copied += 1;
    }

    Ok(copied)
}

fn emit_clipboard_imported(app: &AppHandle) {
    if let Err(err) = app.emit(
        "clipboard://updated",
        serde_json::json!({
            "id": null,
            "deduplicated": false,
            "imported": true,
        }),
    ) {
        log::warn!("emit clipboard import update failed: {err}");
    }
}

fn emit_settings_updated(app: &AppHandle, settings: &crate::settings::Settings) {
    if let Err(err) = app.emit("settings://updated", settings) {
        log::warn!("emit settings import update failed: {err}");
    }
}

fn copy_file_into_writer<W: Write>(path: &Path, writer: &mut W) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut reader = BufReader::new(file);
    std::io::copy(&mut reader, writer).with_context(|| format!("failed to copy {path:?}"))?;

    Ok(())
}

fn dir_size(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut total = 0;
    for entry in WalkDir::new(path)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata at {path:?}"))?;

        if metadata.is_file() {
            total += metadata.len();
        }
    }

    Ok(total)
}

fn app_error<T>(message: impl Into<String>) -> Result<T> {
    Err(AppError::Other(anyhow!(message.into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::tempdir;
    use zip::ZipArchive;

    #[test]
    fn backup_extension_validation_accepts_expected_suffix() {
        let path = normalize_backup_path(PathBuf::from("demo.snipstackbak")).unwrap();
        assert_eq!(path, PathBuf::from("demo.snipstackbak"));
    }

    #[test]
    fn backup_extension_validation_appends_missing_suffix() {
        let path = normalize_backup_path(PathBuf::from("demo")).unwrap();
        assert_eq!(path, PathBuf::from("demo.snipstackbak"));
    }

    #[test]
    fn backup_extension_validation_rejects_other_suffix() {
        assert!(normalize_backup_path(PathBuf::from("demo.zip")).is_err());
    }

    #[test]
    fn encrypted_payload_header_round_trips() {
        let encrypted = encrypt_payload(b"hello", "password-123").unwrap();
        let mut bytes = Vec::new();
        write_header(&mut bytes, &encrypted.header).unwrap();
        bytes.extend_from_slice(&encrypted.ciphertext);

        let mut cursor = Cursor::new(bytes);

        assert_eq!(
            inspect_backup_reader(&mut cursor).unwrap(),
            BackupContainerMode::Encrypted
        );
    }

    #[test]
    fn inspect_backup_reader_recognizes_plain_zip() {
        let mut cursor = Cursor::new(b"PK\x03\x04demo".to_vec());

        assert_eq!(
            inspect_backup_reader(&mut cursor).unwrap(),
            BackupContainerMode::Plain
        );
    }

    #[test]
    fn inspect_backup_reader_recognizes_encrypted_container() {
        let encrypted = encrypt_payload(b"hello", "password-123").unwrap();
        let mut bytes = Vec::new();
        write_header(&mut bytes, &encrypted.header).unwrap();
        bytes.extend_from_slice(&encrypted.ciphertext);
        let mut cursor = Cursor::new(bytes);

        assert_eq!(
            inspect_backup_reader(&mut cursor).unwrap(),
            BackupContainerMode::Encrypted
        );
    }

    #[test]
    fn payload_zip_contains_only_backup_whitelist() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let resources = root.join("resources");
        fs::create_dir_all(resources.join("clipboard-images/origin")).unwrap();
        fs::write(root.join("clipboard.db"), b"db").unwrap();
        fs::write(root.join("settings.json"), b"{}").unwrap();
        fs::write(root.join("window-state.json"), b"skip").unwrap();
        fs::write(root.join("settings.json.bak"), b"skip").unwrap();
        fs::write(resources.join("clipboard-images/origin/demo.png"), b"image").unwrap();

        let payload = root.join("payload.zip");
        let target = root.join("backup.snipstackbak");
        let manifest = BackupManifest {
            format_version: FORMAT_VERSION,
            app_name: "SnipStack".to_owned(),
            app_version: "0.0.0".to_owned(),
            exported_at: Utc::now(),
            platform: "macos".to_owned(),
            encryption: ManifestEncryption::None,
            item_count: 1,
            text_count: 1,
            image_count: 0,
            files_count: 0,
            resource_bytes: 5,
        };
        let source_paths = BackupSourcePaths {
            db_path: root.join("clipboard.db"),
            resources_dir: resources,
            settings_path: root.join("settings.json"),
        };

        write_payload_zip(&source_paths, &payload, &manifest, &target).unwrap();

        let file = File::open(payload).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut names = Vec::new();
        for index in 0..archive.len() {
            let file = archive.by_index(index).unwrap();
            names.push(file.name().to_owned());
        }
        names.sort();

        assert_eq!(
            names,
            vec![
                "config/settings.json",
                "db/clipboard.db",
                "manifest.json",
                "resources/clipboard-images/origin/demo.png",
            ]
        );
    }

    /// 按生产 `db::init` 的方式打开文件库：WAL、不存在则创建、跑完整 migrations。
    async fn open_file_db(path: &Path) -> Result<SqlitePool> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .context("failed to open test db")?;
        MIGRATOR
            .run(&pool)
            .await
            .context("failed to migrate test db")?;

        Ok(pool)
    }

    async fn insert_marker(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO clipboard_groups (id, name, icon, created_at, updated_at) \
             VALUES (?, ?, 'icon', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn marker_ids(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM clipboard_groups ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// 建一个带标记行的文件库并关闭，模拟导出后的备份库或关闭连接池后的现场库。
    async fn create_db_with_marker(path: &Path, id: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let pool = open_file_db(path).await.unwrap();
        insert_marker(&pool, id).await;
        pool.close().await;
    }

    struct OverwriteFixture {
        _temp: TempDir,
        root: PathBuf,
        live: LiveDataPaths,
        backup_db: PathBuf,
        backup_resources: PathBuf,
    }

    async fn overwrite_fixture() -> OverwriteFixture {
        let temp = tempdir().unwrap();
        let root = temp.path().join("data");
        let live = LiveDataPaths {
            db_path: root.join("db").join(DB_FILENAME),
            resources_dir: root.join("resources"),
        };
        create_db_with_marker(&live.db_path, "old-group").await;
        fs::create_dir_all(live.resources_dir.join("images")).unwrap();
        fs::write(live.resources_dir.join("images/old.png"), b"old").unwrap();

        let backup_db = temp.path().join("payload/db").join(DB_FILENAME);
        create_db_with_marker(&backup_db, "new-group").await;
        let backup_resources = temp.path().join("payload/resources");
        fs::create_dir_all(backup_resources.join("images")).unwrap();
        fs::write(backup_resources.join("images/new.png"), b"new").unwrap();

        OverwriteFixture {
            _temp: temp,
            root,
            live,
            backup_db,
            backup_resources,
        }
    }

    fn rollback_dirs(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(ROLLBACK_DIR_PREFIX)
            })
            .map(|entry| entry.path())
            .collect()
    }

    async fn assert_live_is_original(fixture: &OverwriteFixture, pool: SqlitePool) {
        assert_eq!(marker_ids(&pool).await, vec!["old-group".to_owned()]);
        pool.close().await;
        assert_eq!(
            fs::read(fixture.live.resources_dir.join("images/old.png")).unwrap(),
            b"old"
        );
        assert!(!fixture.live.resources_dir.join("images/new.png").exists());
        assert!(rollback_dirs(&fixture.root).is_empty());
    }

    #[tokio::test]
    async fn overwrite_swap_installs_backup_and_removes_rollback_dir() {
        let fixture = overwrite_fixture().await;
        let db_path = fixture.live.db_path.clone();

        let outcome = swap_live_data(
            &fixture.live,
            &fixture.backup_db,
            Some(&fixture.backup_resources),
            || open_file_db(&db_path),
            || Ok("settings"),
        )
        .await;

        assert_eq!(outcome.result.unwrap(), "settings");
        let pool = outcome.pool.unwrap();
        assert_eq!(marker_ids(&pool).await, vec!["new-group".to_owned()]);
        pool.close().await;
        assert!(fixture.live.resources_dir.join("images/new.png").exists());
        assert!(!fixture.live.resources_dir.join("images/old.png").exists());
        assert!(rollback_dirs(&fixture.root).is_empty());
    }

    #[tokio::test]
    async fn overwrite_swap_restores_original_when_new_db_cannot_open() {
        let fixture = overwrite_fixture().await;
        fs::write(&fixture.backup_db, b"not a sqlite database at all").unwrap();
        let db_path = fixture.live.db_path.clone();

        let outcome = swap_live_data(
            &fixture.live,
            &fixture.backup_db,
            Some(&fixture.backup_resources),
            || open_file_db(&db_path),
            || Ok(()),
        )
        .await;

        let message = outcome.result.unwrap_err().to_string();
        assert!(message.contains("已恢复为导入前的数据"), "{message}");
        assert_live_is_original(&fixture, outcome.pool.unwrap()).await;
    }

    #[tokio::test]
    async fn overwrite_swap_restores_original_when_finish_fails() {
        let fixture = overwrite_fixture().await;
        let db_path = fixture.live.db_path.clone();

        let outcome = swap_live_data(
            &fixture.live,
            &fixture.backup_db,
            Some(&fixture.backup_resources),
            || open_file_db(&db_path),
            || app_error::<()>("settings write failed"),
        )
        .await;

        assert!(outcome.result.is_err());
        assert_live_is_original(&fixture, outcome.pool.unwrap()).await;
    }

    #[tokio::test]
    async fn overwrite_swap_restores_original_when_backup_db_copy_fails() {
        let fixture = overwrite_fixture().await;
        let missing = fixture.backup_db.with_file_name("missing.db");
        let db_path = fixture.live.db_path.clone();

        let outcome = swap_live_data(
            &fixture.live,
            &missing,
            Some(&fixture.backup_resources),
            || open_file_db(&db_path),
            || Ok(()),
        )
        .await;

        assert!(outcome.result.is_err());
        assert_live_is_original(&fixture, outcome.pool.unwrap()).await;
    }

    #[tokio::test]
    async fn overwrite_swap_ignores_leftover_rollback_dir() {
        let fixture = overwrite_fixture().await;
        let leftover = fixture.root.join(format!("{ROLLBACK_DIR_PREFIX}stale"));
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join(DB_FILENAME), b"stale").unwrap();
        let db_path = fixture.live.db_path.clone();

        let outcome = swap_live_data(
            &fixture.live,
            &fixture.backup_db,
            None,
            || open_file_db(&db_path),
            || Ok(()),
        )
        .await;

        assert!(outcome.result.is_ok());
        let pool = outcome.pool.unwrap();
        assert_eq!(marker_ids(&pool).await, vec!["new-group".to_owned()]);
        pool.close().await;
        assert!(
            fixture.live.resources_dir.join("images/old.png").exists(),
            "backup without resources keeps current resources"
        );
        assert_eq!(rollback_dirs(&fixture.root), vec![leftover]);
    }

    #[tokio::test]
    async fn database_state_keeps_usable_pool_after_failed_overwrite() {
        let fixture = overwrite_fixture().await;
        let db_path = fixture.live.db_path.clone();
        let state = crate::db::DatabaseState::new(open_file_db(&db_path).await.unwrap());

        let mut imported = None;
        let imported_slot = &mut imported;
        let live = &fixture.live;
        let backup_db = fixture.backup_db.as_path();
        let backup_resources = fixture.backup_resources.as_path();
        let db_path_ref = db_path.as_path();
        state
            .close_and_replace(move || async move {
                let swap = swap_live_data(
                    live,
                    backup_db,
                    Some(backup_resources),
                    || open_file_db(db_path_ref),
                    || app_error::<()>("settings write failed"),
                )
                .await;
                *imported_slot = Some(swap.result);
                swap.pool
            })
            .await
            .unwrap();

        assert!(imported.unwrap().is_err());
        assert_live_is_original(&fixture, state.pool().await).await;
    }

    #[tokio::test]
    async fn backup_db_with_current_migrations_is_accepted() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(DB_FILENAME);
        create_db_with_marker(&path, "group").await;

        validate_backup_database(&path).await.unwrap();
    }

    #[tokio::test]
    async fn backup_db_with_unknown_migration_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(DB_FILENAME);
        let pool = open_file_db(&path).await.unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES (99990101000000, 'future', 1, x'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let message = validate_backup_database(&path)
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("请先升级应用"), "{message}");
    }

    #[tokio::test]
    async fn backup_db_without_migrations_table_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(DB_FILENAME);
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE something (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let message = validate_backup_database(&path)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(message, INVALID_BACKUP_DB_MESSAGE);
    }

    #[tokio::test]
    async fn backup_db_that_is_not_sqlite_is_rejected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join(DB_FILENAME);
        fs::write(&path, b"definitely not sqlite").unwrap();

        let message = validate_backup_database(&path)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(message, INVALID_BACKUP_DB_MESSAGE);
    }

    #[test]
    fn applied_migrations_check_rejects_empty_dirty_and_mismatched() {
        let known = MIGRATOR.iter().next().unwrap();
        let checksum = known.checksum.to_vec();

        assert!(check_applied_migrations(&[(known.version, true, checksum.clone())]).is_ok());
        assert!(check_applied_migrations(&[]).is_err());
        assert!(check_applied_migrations(&[(known.version, false, checksum)]).is_err());
        assert!(check_applied_migrations(&[(known.version, true, vec![0])]).is_err());
    }

    #[test]
    fn strip_machine_local_settings_removes_only_local_keys() {
        let mut patch = serde_json::json!({
            "general": {"autoStart": true, "runAsAdmin": true},
            "onboarding": {"completed": true},
            "update": {
                "autoCheck": true,
                "lastCheckedAt": "2026-01-01",
                "skippedVersion": "9.9.9",
            },
            "appearance": {"language": "en-US"},
        });

        strip_machine_local_settings(&mut patch);

        assert_eq!(
            patch,
            serde_json::json!({
                "general": {"autoStart": true},
                "update": {"autoCheck": true},
                "appearance": {"language": "en-US"},
            })
        );
    }

    #[test]
    fn strip_machine_local_settings_keys_match_settings_model() {
        let mut settings = crate::settings::Settings::default();
        settings.general.run_as_admin = true;
        settings.onboarding.completed = true;
        settings.update.last_checked_at = Some("2026-01-01".into());
        settings.update.skipped_version = Some("9.9.9".into());
        let mut patch = serde_json::to_value(&settings).unwrap();

        strip_machine_local_settings(&mut patch);
        let parsed: crate::settings::Settings = serde_json::from_value(patch).unwrap();

        assert_eq!(parsed, crate::settings::Settings::default());
    }

    #[test]
    fn overlay_machine_local_settings_keeps_current_local_fields() {
        let mut imported = crate::settings::Settings::default();
        imported.general.auto_start = true;
        imported.general.run_as_admin = true;
        imported.onboarding.completed = false;
        imported.update.last_checked_at = Some("backup".into());
        imported.update.skipped_version = Some("backup".into());
        imported.update.auto_check = true;

        let mut current = crate::settings::Settings::default();
        current.onboarding.completed = true;
        current.onboarding.last_step = 3;
        current.update.last_checked_at = Some("local".into());

        let next = overlay_machine_local_settings(imported, &current);

        assert!(next.general.auto_start);
        assert!(next.update.auto_check);
        assert!(!next.general.run_as_admin);
        assert_eq!(next.onboarding, current.onboarding);
        assert_eq!(next.update.last_checked_at.as_deref(), Some("local"));
        assert_eq!(next.update.skipped_version, None);
    }

    #[tokio::test]
    async fn merge_preserves_is_sensitive_flag() {
        use crate::db::items::{content_hash, insert_item};
        use crate::db::models::{ClipboardItem, ClipboardKind, Platform};
        use crate::db::test_support::memory_pool;
        use chrono::DateTime;

        // The backup pool stands in for a real backup database, which is a
        // snapshot of the live clipboard.db and therefore already carries the
        // `is_sensitive` flag written by the production insert path.
        let backup = memory_pool().await;
        let current = memory_pool().await;

        let ts = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let content = "AKIAIOSFODNN7EXAMPLE".to_owned();
        let item = ClipboardItem {
            id: "secret-1".to_owned(),
            kind: ClipboardKind::Text,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content_hash: content_hash(ClipboardKind::Text, &content),
            content,
            search_text: None,
            summary: None,
            file_types: None,
            size: None,
            width: None,
            height: None,
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: true,
            platform: Platform::Macos,
            note: None,
            created_at: ts,
            updated_at: ts,
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
        insert_item(&backup, &item).await.unwrap();

        let outcome = merge_history(&current, &backup).await.unwrap();
        assert_eq!(
            outcome.imported_items, 1,
            "sensitive item should be imported"
        );

        // Regression: the merge SELECT/INSERT used to omit `is_sensitive`, so
        // every imported row silently fell back to the column DEFAULT (0) and
        // secrets were rendered in plaintext. The flag must survive the merge.
        let imported: bool =
            sqlx::query_scalar("SELECT is_sensitive FROM clipboard_items WHERE id = ?")
                .bind("secret-1")
                .fetch_one(&current)
                .await
                .unwrap();
        assert!(
            imported,
            "merged sensitive item must keep is_sensitive = true"
        );
    }
}
