//! app data 目录解析的单一入口。
//!
//! 所有持久化位置都从这里取根，集中三件本来散落各模块的事：
//! - `app_local_data_dir().context(...)` 这段解析样板（原先在 settings/window/db/storage/app_store 各写一份）；
//! - `db/`、`resources/`、`config/`、`state/` 这些语义目录名；
//! - 开发 / 生产环境的数据隔离：dev 数据落 `dev/`、release 数据落 `prod/`，互不污染，
//!   便于后续导入导出 / 备份 / 迁移按环境整目录操作。
//! - 自定义数据目录：`<app_local_data>/<env>/storage.json` 始终作为启动锚点，
//!   真实数据根由该 bootstrap manifest 指向。
//!
//! 只解析路径、不建目录：创建行为是各调用方特化的（settings/window/db 建自己的目录、
//! 图片/图标写时懒建），塞进这里会改掉懒建语义。叶子文件名（`clipboard.db` /
//! `settings.json` 等）仍由各模块自己拥有——要统一的是「根在哪」。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::core::Result;

/// SQLite 主库与 WAL / SHM sidecar 所在目录名，挂在 [`app_data_dir`] 下。
const DB_DIR: &str = "db";
/// 资源文件（图片、应用图标）的公共父目录名，挂在 [`app_data_dir`] 下。
const RESOURCES_DIR: &str = "resources";
/// 用户配置目录名，挂在 [`app_data_dir`] 下。
const CONFIG_DIR: &str = "config";
/// 本机运行状态目录名，挂在 [`app_data_dir`] 下。
const STATE_DIR: &str = "state";
/// 固定留在 `<app_local_data>/<env>` 的 bootstrap manifest 文件名。
const STORAGE_MANIFEST_FILENAME: &str = "storage.json";
/// 写入真实数据根的 identity manifest 文件名，用于识别 SnipStack 数据目录。
const STORAGE_IDENTITY_FILENAME: &str = ".snipstack-storage.json";
/// 用户选择父目录后创建的数据子目录名。
const CUSTOM_DATA_DIR_NAME: &str = "SnipStackData";
/// 存储 manifest 格式版本。
const STORAGE_MANIFEST_VERSION: u16 = 1;

/// 开发环境数据子目录名（`tauri dev`）。
const DEV_ENV_DIR: &str = "dev";
/// 生产环境数据子目录名（`tauri build`）。
const PROD_ENV_DIR: &str = "prod";

/// 当前环境的数据子目录名：dev 构建走 `dev/`，release 构建走 `prod/`。
/// `cfg!(dev)` 是 Tauri 在 `tauri dev` 时注入的 cfg，与各模块原先选文件名后缀用的判定同源。
const fn env_dir() -> &'static str {
    if cfg!(dev) {
        DEV_ENV_DIR
    } else {
        PROD_ENV_DIR
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocation {
    pub current_path: String,
    pub default_path: String,
    pub is_custom: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageManifest {
    version: u16,
    environment: String,
    data_dir: PathBuf,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageIdentity {
    version: u16,
    environment: String,
    created_at: DateTime<Utc>,
}

/// `<app_local_data>/<env>`：固定启动锚点。自定义数据目录启用后，这里仍保留
/// `storage.json` 用于解析真实数据根。
pub fn bootstrap_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_local_data_dir()
        .context("failed to resolve app local data dir")?;
    Ok(dir.join(env_dir()))
}

/// 默认数据根。未启用自定义数据目录时，真实数据仍落在 `<app_local_data>/<env>`。
pub fn default_data_dir(app: &AppHandle) -> Result<PathBuf> {
    bootstrap_dir(app)
}

/// OCR 语言包缓存根:`<app_local_data>/ocr-packs`。属应用级缓存,刻意不挂 env、
/// 不随自定义数据目录迁移、不进备份——删除后可随时重新下载。
pub fn ocr_packs_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_local_data_dir()
        .context("failed to resolve app local data dir")?;
    Ok(dir.join("ocr-packs"))
}

/// 用户选择父目录后创建 SnipStack 自有数据子目录。
pub fn custom_data_dir(parent: &Path) -> PathBuf {
    parent.join(CUSTOM_DATA_DIR_NAME).join(env_dir())
}

/// 返回当前真实数据根、默认数据根，以及是否处于自定义目录。
pub fn storage_location(app: &AppHandle) -> Result<StorageLocation> {
    let current = app_data_dir(app)?;
    let default = default_data_dir(app)?;
    Ok(StorageLocation {
        is_custom: current != default,
        current_path: current.to_string_lossy().into_owned(),
        default_path: default.to_string_lossy().into_owned(),
    })
}

/// 本进程已解析出的数据根，解析一次后在整个进程生命周期内保持不变（除非显式切换）。
///
/// 不缓存的话每次取路径都重新读 manifest、重新探测目录：自定义目录所在的磁盘若在
/// 运行中途才挂载上来，前一刻数据库开在默认目录、后一刻图片却写进自定义目录，
/// 同一次运行的数据被劈成两半。
static RESOLVED_DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// `<data_root>`：当前环境所有持久化位置的根；其下按语义拆分为 db、resources、
/// config 与 state。真实根由 bootstrap manifest 决定，首次调用时解析并缓存。
pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf> {
    let mut resolved = crate::core::sync::lock_unpoisoned(&RESOLVED_DATA_DIR);
    if let Some(dir) = resolved.as_ref() {
        return Ok(dir.clone());
    }

    let dir = resolve_data_dir(&bootstrap_dir(app)?, &default_data_dir(app)?)?;
    *resolved = Some(dir.clone());
    Ok(dir)
}

/// 按 bootstrap manifest 解析真实数据根；指向的自定义目录用不了时回落默认目录。
///
/// 回落**只对本次运行生效，不改写 manifest**。自定义目录暂时不可达是常态而非损坏：
/// U 盘没插、网络盘或晚挂载的分区在开机自启时还没就绪、identity 文件被杀软短暂锁住。
/// 若在这里把指向永久改回默认目录，用户下次只会看到一个空历史；他再去偏好里重选
/// 原目录时，迁移流程会用默认目录里几乎为空的数据去替换原目录的全部历史。
/// 保留指向则下次启动目录可达时自动恢复。manifest 只由首次启动和显式切换写入。
fn resolve_data_dir(bootstrap: &Path, default: &Path) -> Result<PathBuf> {
    let manifest_path = storage_manifest_path(bootstrap);

    fs::create_dir_all(bootstrap)
        .with_context(|| format!("failed to create bootstrap dir at {bootstrap:?}"))?;

    let manifest = match read_storage_manifest(&manifest_path) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            write_storage_manifest(&manifest_path, &storage_manifest(default.to_path_buf()))?;
            return Ok(default.to_path_buf());
        }
        Err(err) => {
            log::warn!("storage manifest unreadable, using default data dir this run: {err}");
            return Ok(default.to_path_buf());
        }
    };

    if manifest.environment != env_dir() || manifest.version != STORAGE_MANIFEST_VERSION {
        log::warn!("storage manifest metadata mismatch, using default data dir this run");
        return Ok(default.to_path_buf());
    }

    if manifest.data_dir == default {
        return Ok(manifest.data_dir);
    }

    if !manifest.data_dir.exists() {
        log::warn!(
            "storage data dir {:?} is unavailable, using default data dir this run",
            manifest.data_dir
        );
        return Ok(default.to_path_buf());
    }

    match has_valid_storage_identity(&manifest.data_dir) {
        Ok(true) => Ok(manifest.data_dir),
        Ok(false) => {
            log::warn!(
                "storage data dir {:?} identity mismatch, using default data dir this run",
                manifest.data_dir
            );
            Ok(default.to_path_buf())
        }
        Err(err) => {
            log::warn!(
                "storage data dir {:?} identity unreadable, using default data dir this run: {err}",
                manifest.data_dir
            );
            Ok(default.to_path_buf())
        }
    }
}

/// manifest 当前指向的数据目录；manifest 不存在、读不出或不属于当前环境时为 `None`。
/// 与 [`app_data_dir`] 不同：本次运行回落到默认目录时，这里仍是用户配置的那个目录。
pub fn configured_data_dir(app: &AppHandle) -> Option<PathBuf> {
    let bootstrap = bootstrap_dir(app).ok()?;
    let manifest = read_storage_manifest(&storage_manifest_path(&bootstrap)).ok()??;
    if manifest.environment != env_dir() || manifest.version != STORAGE_MANIFEST_VERSION {
        return None;
    }

    Some(manifest.data_dir)
}

/// 目录是否已经是一份可直接使用的 SnipStack 数据：identity 有效且已有数据库目录。
pub fn is_populated_data_dir(data_dir: &Path) -> bool {
    matches!(has_valid_storage_identity(data_dir), Ok(true)) && data_dir.join(DB_DIR).is_dir()
}

/// 将当前真实数据根切换到指定目录。调用方负责在写入前完成数据迁移。
pub fn set_app_data_dir(app: &AppHandle, data_dir: PathBuf) -> Result<()> {
    let bootstrap = bootstrap_dir(app)?;
    fs::create_dir_all(&bootstrap)
        .with_context(|| format!("failed to create bootstrap dir at {bootstrap:?}"))?;

    write_storage_identity(&data_dir)?;
    write_storage_manifest(
        &storage_manifest_path(&bootstrap),
        &storage_manifest(data_dir.clone()),
    )?;
    *crate::core::sync::lock_unpoisoned(&RESOLVED_DATA_DIR) = Some(data_dir);
    Ok(())
}

/// 切换失败后的回退：本次运行继续使用 `session_dir`，manifest 还原为切换前的指向
/// `configured`（切换前读不到指向时就指向 `session_dir`）。
///
/// 两者在回落运行里不是同一个目录——manifest 指向暂时不可达的自定义目录，而本次运行
/// 用的是默认目录——所以不能简单地把 manifest 写成当前目录，那会丢掉原来的指向。
/// 只写 manifest、不写 identity：被还原的指向可能此刻就不可达。
pub fn restore_data_dir(
    app: &AppHandle,
    configured: Option<PathBuf>,
    session_dir: PathBuf,
) -> Result<()> {
    let bootstrap = bootstrap_dir(app)?;
    let pointer = configured.unwrap_or_else(|| session_dir.clone());
    write_storage_manifest(
        &storage_manifest_path(&bootstrap),
        &storage_manifest(pointer),
    )?;
    *crate::core::sync::lock_unpoisoned(&RESOLVED_DATA_DIR) = Some(session_dir);
    Ok(())
}

/// 写入真实数据根 identity manifest，供迁移目标目录校验。
pub fn write_storage_identity(data_dir: &Path) -> Result<()> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create storage dir at {data_dir:?}"))?;
    let identity = StorageIdentity {
        version: STORAGE_MANIFEST_VERSION,
        environment: env_dir().to_owned(),
        created_at: Utc::now(),
    };
    let path = data_dir.join(STORAGE_IDENTITY_FILENAME);
    let json =
        serde_json::to_string_pretty(&identity).context("failed to serialize storage identity")?;

    fs::write(&path, json).with_context(|| format!("failed to write storage identity {path:?}"))?;
    Ok(())
}

/// 校验目标目录是否可作为 SnipStack 数据根：空目录允许使用，已有 identity 时必须匹配。
pub fn validate_storage_target(data_dir: &Path) -> Result<()> {
    let identity_path = data_dir.join(STORAGE_IDENTITY_FILENAME);
    if identity_path.exists() {
        let content = fs::read_to_string(&identity_path)
            .with_context(|| format!("failed to read storage identity {identity_path:?}"))?;
        let identity: StorageIdentity =
            serde_json::from_str(&content).context("failed to parse storage identity")?;
        if identity.version == STORAGE_MANIFEST_VERSION && identity.environment == env_dir() {
            return Ok(());
        }

        return Err(anyhow::anyhow!("目标目录不是当前环境的 SnipStack 数据目录").into());
    }

    if data_dir.exists()
        && fs::read_dir(data_dir)
            .with_context(|| format!("failed to read storage target {data_dir:?}"))?
            .next()
            .is_some()
    {
        return Err(anyhow::anyhow!("目标 SnipStack 数据目录已存在且不是有效数据目录").into());
    }

    Ok(())
}

/// `<app_data_dir>/db`：SQLite 主库与 sidecar 的目录。
pub fn db_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app_data_dir(app)?.join(DB_DIR))
}

/// `<app_data_dir>/resources`：图片、应用图标等资源文件的公共父目录。
pub fn resources_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app_data_dir(app)?.join(RESOURCES_DIR))
}

/// `<app_data_dir>/config`：用户偏好配置目录。
pub fn config_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app_data_dir(app)?.join(CONFIG_DIR))
}

/// `<app_data_dir>/state`：窗口位置等本机运行状态目录。
pub fn state_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app_data_dir(app)?.join(STATE_DIR))
}

fn storage_manifest_path(bootstrap: &Path) -> PathBuf {
    bootstrap.join(STORAGE_MANIFEST_FILENAME)
}

fn storage_manifest(data_dir: PathBuf) -> StorageManifest {
    StorageManifest {
        version: STORAGE_MANIFEST_VERSION,
        environment: env_dir().to_owned(),
        data_dir,
        updated_at: Utc::now(),
    }
}

fn read_storage_manifest(path: &Path) -> Result<Option<StorageManifest>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    Ok(Some(
        serde_json::from_str(&content).with_context(|| format!("failed to parse {path:?}"))?,
    ))
}

fn write_storage_manifest(path: &Path, manifest: &StorageManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create manifest dir at {parent:?}"))?;
    }

    let json =
        serde_json::to_string_pretty(manifest).context("failed to serialize storage manifest")?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).with_context(|| format!("failed to write {tmp:?}"))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to promote {tmp:?} to {path:?}"))?;
    Ok(())
}

fn has_valid_storage_identity(data_dir: &Path) -> Result<bool> {
    let identity_path = data_dir.join(STORAGE_IDENTITY_FILENAME);
    if !identity_path.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(&identity_path)
        .with_context(|| format!("failed to read storage identity {identity_path:?}"))?;
    let identity: StorageIdentity =
        serde_json::from_str(&content).context("failed to parse storage identity")?;
    Ok(identity.version == STORAGE_MANIFEST_VERSION && identity.environment == env_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("snipstack-storage-paths-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();

            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn write_manifest_pointing_to(bootstrap: &Path, data_dir: &Path) -> String {
        let path = storage_manifest_path(bootstrap);
        write_storage_manifest(&path, &storage_manifest(data_dir.to_path_buf())).unwrap();
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn first_run_writes_manifest_pointing_to_default() {
        let temp = TempDir::new();
        let bootstrap = temp.path().join("bootstrap");

        let resolved = resolve_data_dir(&bootstrap, &bootstrap).unwrap();

        assert_eq!(resolved, bootstrap);
        let manifest = read_storage_manifest(&storage_manifest_path(&bootstrap))
            .unwrap()
            .unwrap();
        assert_eq!(manifest.data_dir, bootstrap);
    }

    #[test]
    fn custom_data_dir_with_identity_is_used() {
        let temp = TempDir::new();
        let bootstrap = temp.path().join("bootstrap");
        let custom = temp.path().join("custom");
        write_storage_identity(&custom).unwrap();
        write_manifest_pointing_to(&bootstrap, &custom);

        assert_eq!(resolve_data_dir(&bootstrap, &bootstrap).unwrap(), custom);
    }

    /// 自定义目录暂时不可达（盘没挂载）时本次运行回落默认目录，但 manifest 的指向
    /// 必须原样保留：目录回来后下次启动要能自动恢复。
    #[test]
    fn unavailable_custom_data_dir_falls_back_without_rewriting_manifest() {
        let temp = TempDir::new();
        let bootstrap = temp.path().join("bootstrap");
        let custom = temp.path().join("not-mounted").join("custom");
        let before = write_manifest_pointing_to(&bootstrap, &custom);

        assert_eq!(resolve_data_dir(&bootstrap, &bootstrap).unwrap(), bootstrap);
        assert_eq!(
            fs::read_to_string(storage_manifest_path(&bootstrap)).unwrap(),
            before
        );

        // 目录恢复可达后，同一份 manifest 重新解析出自定义目录。
        write_storage_identity(&custom).unwrap();
        assert_eq!(resolve_data_dir(&bootstrap, &bootstrap).unwrap(), custom);
    }

    #[test]
    fn custom_data_dir_without_identity_falls_back_without_rewriting_manifest() {
        let temp = TempDir::new();
        let bootstrap = temp.path().join("bootstrap");
        let custom = temp.path().join("custom");
        fs::create_dir_all(&custom).unwrap();
        let before = write_manifest_pointing_to(&bootstrap, &custom);

        assert_eq!(resolve_data_dir(&bootstrap, &bootstrap).unwrap(), bootstrap);
        assert_eq!(
            fs::read_to_string(storage_manifest_path(&bootstrap)).unwrap(),
            before
        );
    }

    #[test]
    fn unreadable_manifest_falls_back_without_rewriting_it() {
        let temp = TempDir::new();
        let bootstrap = temp.path().join("bootstrap");
        fs::create_dir_all(&bootstrap).unwrap();
        fs::write(storage_manifest_path(&bootstrap), "{ not json").unwrap();

        assert_eq!(resolve_data_dir(&bootstrap, &bootstrap).unwrap(), bootstrap);
        assert_eq!(
            fs::read_to_string(storage_manifest_path(&bootstrap)).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn populated_data_dir_needs_identity_and_database_dir() {
        let temp = TempDir::new();
        let empty = temp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let identity_only = temp.path().join("identity-only");
        write_storage_identity(&identity_only).unwrap();
        let populated = temp.path().join("populated");
        write_storage_identity(&populated).unwrap();
        fs::create_dir_all(populated.join(DB_DIR)).unwrap();

        assert!(!is_populated_data_dir(&temp.path().join("missing")));
        assert!(!is_populated_data_dir(&empty));
        assert!(!is_populated_data_dir(&identity_only));
        assert!(is_populated_data_dir(&populated));
    }

    #[test]
    fn empty_storage_target_is_allowed() {
        let temp = TempDir::new();

        validate_storage_target(temp.path()).unwrap();
    }

    #[test]
    fn custom_data_dir_uses_named_data_container() {
        let temp = TempDir::new();

        assert_eq!(
            custom_data_dir(temp.path()),
            temp.path().join("SnipStackData").join(env_dir())
        );
    }

    #[test]
    fn storage_target_with_matching_identity_is_allowed() {
        let temp = TempDir::new();

        write_storage_identity(temp.path()).unwrap();

        validate_storage_target(temp.path()).unwrap();
    }

    #[test]
    fn storage_target_with_mismatched_identity_is_rejected() {
        let temp = TempDir::new();
        let identity = StorageIdentity {
            version: STORAGE_MANIFEST_VERSION,
            environment: "other".to_owned(),
            created_at: Utc::now(),
        };
        fs::write(
            temp.path().join(STORAGE_IDENTITY_FILENAME),
            serde_json::to_string_pretty(&identity).unwrap(),
        )
        .unwrap();

        assert!(validate_storage_target(temp.path()).is_err());
    }

    #[test]
    fn non_empty_storage_target_without_identity_is_rejected() {
        let temp = TempDir::new();
        fs::write(temp.path().join("random.txt"), "not SnipStack").unwrap();

        assert!(validate_storage_target(temp.path()).is_err());
    }
}
