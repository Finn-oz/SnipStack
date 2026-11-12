//! 设置持久化。
//!
//! - 落盘位置：`<app_data_dir>/config/settings.json`（dev/prod 由 `core::paths` 的环境子目录隔离）。
//! - 写入流程：先写到 `settings.json.tmp`，再原子替换主文件，避免中途断电留下半截 JSON。
//! - 缺字段兼容：`Settings` 各结构体都 `#[serde(default)]`，新版本新增字段不影响旧文件。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use anyhow::Context;
use tauri::AppHandle;

use crate::core::{AppError, Result};

use super::model::{
    Language, Settings, WINDOW_OPEN_GROUP_PREFIX, WINDOW_OPEN_SELECTION_ALL,
    WINDOW_OPEN_SELECTION_PRESERVE,
};

const FILENAME: &str = "settings.json";

/// 启动时读设置文件遇到 IO 错误的重试间隔（总计约 0.2 秒，启动路径上可接受）。
const READ_RETRY_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(30),
    std::time::Duration::from_millis(60),
    std::time::Duration::from_millis(120),
];

pub struct SettingsStore {
    path: RwLock<PathBuf>,
    current: RwLock<Settings>,
    /// 串行化所有写路径（合并、重置、整体替换、换目录）。
    ///
    /// 落盘在这把锁下进行，**不**在 `current` 的写锁下：`snapshot()` 的读者里有主线程
    /// （托盘菜单、热键打开窗口、窗口空闲计时器），而写入方里有不由用户触发的后台任务
    /// （自动检查更新记录时间）。磁盘被杀软扫描、云盘同步或硬件异常拖住时 `fsync`
    /// 没有时限，若此时持着 `current` 写锁，主线程读设置就跟着卡死——托盘、热键、
    /// 单实例转发全部失效。`current` 的写锁只在落盘成功后换入内存的一瞬间持有。
    writer: Mutex<()>,
}

impl SettingsStore {
    pub fn new(app: &AppHandle) -> Result<Self> {
        let dir = crate::core::paths::config_dir(app)?;
        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;

        let path = dir.join(FILENAME);

        let current = match load_from_disk(&path) {
            Some(settings) => settings,
            None => {
                // 真·首次启动：用系统 locale 推导默认语言并落盘，之后所有读取都走常规分支。
                let settings = default_settings_with_system_locale();
                if let Err(err) = write_atomic(&path, &settings) {
                    log::warn!("persist first-run settings failed: {err}");
                }
                settings
            }
        };
        log::info!("settings store ready at {path:?}");

        Ok(Self {
            path: RwLock::new(path),
            current: RwLock::new(current),
            writer: Mutex::new(()),
        })
    }

    pub fn snapshot(&self) -> Settings {
        self.current.read().expect("settings poisoned").clone()
    }

    /// 恢复默认设置并落盘，返回新的完整快照。
    pub fn reset(&self) -> Result<Settings> {
        let next = default_settings_with_system_locale();

        let _writer = crate::core::sync::lock_unpoisoned(&self.writer);
        self.persist_and_install(next)
    }

    /// 用 JSON patch 深度合并到当前设置，落盘后返回新快照。
    /// patch 必须是 object；非 object 视为「整个替换」语义不友好，直接报错。
    pub fn update(&self, patch: serde_json::Value) -> Result<Settings> {
        if !patch.is_object() {
            return Err(AppError::Other(anyhow::anyhow!(
                "settings patch must be a JSON object"
            )));
        }

        // 写者之间由 `writer` 串行，这里读到的快照在本次写完之前不会被别的写者改动。
        let _writer = crate::core::sync::lock_unpoisoned(&self.writer);

        let mut merged = serde_json::to_value(self.snapshot())
            .context("failed to serialize current settings for merge")?;
        deep_merge(&mut merged, patch);

        let next: Settings = serde_json::from_value(merged)
            .map_err(|err| AppError::Other(anyhow::anyhow!("invalid settings patch: {err}")))?;

        validate_settings(&next)?;

        self.persist_and_install(next)
    }

    /// 用完整设置文件替换当前设置；覆盖导入专用。
    pub fn replace_from_file(&self, path: &Path) -> Result<Settings> {
        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
        let next: Settings = serde_json::from_str(&content)
            .map_err(|err| AppError::Other(anyhow::anyhow!("invalid settings file: {err}")))?;

        validate_settings(&next)?;

        let _writer = crate::core::sync::lock_unpoisoned(&self.writer);
        self.persist_and_install(next)
    }

    /// 先落盘、成功后才换入内存，返回新快照。调用方必须持有 `writer` 锁。
    /// 落盘期间不持 `current` 的锁，读者不受磁盘快慢影响；落盘失败时内存保持原样。
    fn persist_and_install(&self, next: Settings) -> Result<Settings> {
        let path = self.path();
        write_atomic(&path, &next)?;
        *self.current.write().expect("settings poisoned") = next.clone();
        Ok(next)
    }

    /// 数据目录热切换后重新绑定设置文件，并把新路径里的设置加载进内存。
    pub fn rebase(&self, app: &AppHandle) -> Result<Settings> {
        let _writer = crate::core::sync::lock_unpoisoned(&self.writer);
        let dir = crate::core::paths::config_dir(app)?;
        fs::create_dir_all(&dir).with_context(|| format!("failed to create dir at {dir:?}"))?;
        let path = dir.join(FILENAME);
        let current = match load_from_disk(&path) {
            Some(settings) => settings,
            None => {
                let settings = default_settings_with_system_locale();
                write_atomic(&path, &settings)?;
                settings
            }
        };

        *self.path.write().expect("settings path poisoned") = path;
        *self.current.write().expect("settings poisoned") = current.clone();
        Ok(current)
    }

    fn path(&self) -> PathBuf {
        self.path.read().expect("settings path poisoned").clone()
    }
}

/// 生成默认设置，并沿用首次启动的系统语言推导规则。
fn default_settings_with_system_locale() -> Settings {
    let mut settings = Settings::default();
    if let Some(tag) = tauri_plugin_os::locale() {
        settings.appearance.language = Language::from_system_locale(&tag);
        log::info!(
            "default settings language from locale {tag}: {:?}",
            settings.appearance.language
        );
    }
    settings
}

/// 返回 `None` 表示主文件不存在（首次启动），调用方据此走「初始化默认」分支；
/// 读取过程中遇到 IO/解析错误会打 warn，并返回 `Settings::default()` 包装在 `Some` 里——
/// 这条路径表示「文件存在但坏了」，不要当成首次启动覆盖系统 locale。
///
/// 回落默认值之前先把读不出来的原文件另存一份：默认值很快会被下一次设置写入
/// （例如启动后几秒的自动检查更新记录时间）原子覆盖回主文件，不留副本的话用户的
/// 全部配置就此无法找回。
fn load_from_disk(path: &Path) -> Option<Settings> {
    if !path.exists() {
        return None;
    }

    match read_settings_file(path) {
        Ok(settings) => Some(settings),
        Err(err) => {
            log::warn!("settings file {path:?} unreadable, using defaults: {err}");
            preserve_unreadable_file(path);
            Some(Settings::default())
        }
    }
}

/// 读取并解析设置文件。读文件遇到 IO 错误时短暂重试——杀软扫描或同步盘占用造成的
/// 共享冲突通常几十毫秒内就消失；内容解析失败不重试。
fn read_settings_file(path: &Path) -> std::io::Result<Settings> {
    let mut attempt = 0;
    let content = loop {
        match fs::read_to_string(path) {
            Ok(content) => break content,
            Err(err) if attempt < READ_RETRY_DELAYS.len() => {
                log::warn!("read settings file {path:?} failed, retrying: {err}");
                std::thread::sleep(READ_RETRY_DELAYS[attempt]);
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    };

    serde_json::from_str::<Settings>(&content)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

/// 把读不出来的设置文件复制成同目录下的 `settings.json.unreadable-<unix 秒>`。
/// 尽力而为：复制失败只记日志，不影响启动。
fn preserve_unreadable_file(path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let backup = path.with_extension(format!("json.unreadable-{stamp}"));

    match fs::copy(path, &backup) {
        Ok(_) => log::warn!("kept unreadable settings file as {backup:?}"),
        Err(err) => log::warn!("keep unreadable settings file as {backup:?} failed: {err}"),
    }
}

/// 写入策略：把新内容写到 tmp 后 rename 成主文件；rename 在同一文件系统下是原子的。
fn write_atomic(path: &Path, settings: &Settings) -> Result<()> {
    let json = serde_json::to_string_pretty(settings).context("failed to serialize settings")?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("failed to create tmp settings at {tmp:?}"))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("failed to write tmp settings at {tmp:?}"))?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to promote tmp settings to {path:?}"))?;
    Ok(())
}

/// 校验设置之间的跨字段约束，避免非法配置写入磁盘。
fn validate_settings(settings: &Settings) -> Result<()> {
    validate_window_open_group(&settings.clipboard.window.select_group_on_open)?;

    // 三条全局快捷键两两互斥;空绑定(禁用)不参与比较。
    let bindings = [
        normalize_shortcut_value(&settings.shortcuts.open_clipboard),
        normalize_shortcut_value(&settings.shortcuts.open_preference),
        normalize_shortcut_value(&settings.shortcuts.snip),
    ];
    let filled: Vec<&String> = bindings.iter().filter(|b| !b.is_empty()).collect();
    for (index, binding) in filled.iter().enumerate() {
        if filled[index + 1..].contains(binding) {
            return Err(AppError::Other(anyhow::anyhow!(
                "global shortcuts must be unique"
            )));
        }
    }

    Ok(())
}

/// 校验打开剪贴板窗口时选中分组的字符串编码，避免非法设置值落盘。
fn validate_window_open_group(value: &str) -> Result<()> {
    if value == WINDOW_OPEN_SELECTION_PRESERVE || value == WINDOW_OPEN_SELECTION_ALL {
        return Ok(());
    }

    let Some(group_id) = value.strip_prefix(WINDOW_OPEN_GROUP_PREFIX) else {
        return Err(AppError::Other(anyhow::anyhow!(
            "open group selection is invalid"
        )));
    };

    if group_id.trim().is_empty() {
        return Err(AppError::Other(anyhow::anyhow!(
            "open group selection is invalid"
        )));
    }

    Ok(())
}

/// 归一化快捷键字面量，供跨字段校验忽略大小写和多余空白。
fn normalize_shortcut_value(value: &str) -> String {
    value
        .split('+')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("+")
}

fn deep_merge(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                match base_map.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        deep_merge(existing, v);
                    }
                    _ => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (slot, patch) => {
            *slot = patch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_merge_overrides_leaves_and_recurses_objects() {
        let mut base = serde_json::json!({
            "general": {"autoStart": false, "trayIcon": true},
            "clipboard": {"history": {"maxCount": 0}},
        });
        let patch = serde_json::json!({
            "general": {"autoStart": true},
            "clipboard": {"history": {"maxCount": 500}},
        });
        deep_merge(&mut base, patch);
        assert_eq!(
            base,
            serde_json::json!({
                "general": {"autoStart": true, "trayIcon": true},
                "clipboard": {"history": {"maxCount": 500}},
            })
        );
    }

    #[test]
    fn deep_merge_replaces_arrays_wholesale() {
        let mut base = serde_json::json!({"itemActions": ["copy", "star", "delete"]});
        let patch = serde_json::json!({"itemActions": ["copy", "pastePlain"]});
        deep_merge(&mut base, patch);
        assert_eq!(
            base,
            serde_json::json!({"itemActions": ["copy", "pastePlain"]})
        );
    }

    #[test]
    fn validate_settings_rejects_duplicate_global_shortcuts() {
        let mut settings = Settings::default();
        settings.shortcuts.open_preference = settings.shortcuts.open_clipboard.clone();

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn validate_settings_rejects_snip_duplicating_other_shortcuts() {
        let mut settings = Settings::default();
        settings.shortcuts.snip = settings.shortcuts.open_clipboard.clone();

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn validate_settings_normalizes_before_comparing() {
        let mut settings = Settings::default();
        settings.shortcuts.open_clipboard = "Alt+S".into();
        settings.shortcuts.snip = "alt + s".into();

        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn validate_settings_allows_empty_global_shortcuts() {
        let mut settings = Settings::default();
        settings.shortcuts.open_preference = String::new();

        assert!(validate_settings(&settings).is_ok());
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let partial = r#"{"general": {"autoStart": true}}"#;
        let parsed: Settings = serde_json::from_str(partial).unwrap();
        assert!(parsed.general.auto_start);
        assert!(!parsed.general.run_as_admin);
        assert!(parsed.general.tray_icon, "default kept");
        assert_eq!(parsed.shortcuts.open_clipboard, "Alt+C");
        assert_eq!(
            parsed.update.frequency,
            crate::settings::UpdateFrequency::Daily
        );
        assert_eq!(
            parsed.clipboard.content.sort,
            crate::db::models::ClipboardItemSort::UpdatedAt
        );
        assert!(!parsed.clipboard.content.copy_then_hide_window);
        assert!(
            parsed
                .clipboard
                .content
                .delete_favorite_items_only_in_favorite_group
        );
        assert!(!parsed.clipboard.content.delete_favorite_items);
        assert!(parsed.clipboard.content.delete_favorite_confirm);
        assert!(!parsed.clipboard.content.delete_pinned_items);
        assert!(parsed.clipboard.content.delete_pinned_confirm);
        assert!(!parsed.clipboard.content.update_on_reuse);
        assert_eq!(parsed.clipboard.history.cleanup_interval_hours, 0);
        assert!(parsed.clipboard.window.scroll_to_top_on_open);
        assert_eq!(
            parsed.clipboard.window.select_range_on_open,
            crate::settings::WindowOpenRangeSelection::Preserve
        );
        assert_eq!(
            parsed.clipboard.window.select_category_on_open,
            crate::settings::WindowOpenCategorySelection::Preserve
        );
        assert_eq!(
            parsed.clipboard.window.select_group_on_open,
            crate::settings::WINDOW_OPEN_SELECTION_PRESERVE
        );
    }

    #[test]
    fn validate_settings_rejects_invalid_open_group_selection() {
        let mut settings = Settings::default();
        settings.clipboard.window.select_group_on_open = "invalid".to_owned();

        assert!(validate_settings(&settings).is_err());
    }
}
