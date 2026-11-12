use std::{sync::Mutex, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Utc};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update as TauriUpdate, UpdaterExt};
use url::Url;

use crate::core::{AppError, Result};
use crate::settings::{SettingsStore, Update as UpdateSettings, UpdateFrequency};

const UPDATE_PROGRESS_EVENT: &str = "update://progress";
const STABLE_ENDPOINT_ENV: &str = "SNIPSTACK_UPDATE_ENDPOINT";
const BETA_ENDPOINT_ENV: &str = "SNIPSTACK_UPDATE_BETA_ENDPOINT";
const NIGHTLY_ENDPOINT_ENV: &str = "SNIPSTACK_UPDATE_NIGHTLY_ENDPOINT";
// 尚未搭建更新服务;先指向 GitHub Releases 的 latest.json 约定路径,发布前手动检查只会优雅失败。
const DEFAULT_STABLE_ENDPOINT: &str =
    "https://github.com/Finn-oz/SnipStack/releases/latest/download/latest.json";
const DEFAULT_BETA_ENDPOINT: &str =
    "https://github.com/Finn-oz/SnipStack/releases/latest/download/latest.json";
const DEFAULT_NIGHTLY_ENDPOINT: &str =
    "https://github.com/Finn-oz/SnipStack/releases/latest/download/latest.json";
/// 更新检查（拉取 latest.json）的总时限。
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// 更新请求建立连接的时限，检查与下载共用。
const UPDATE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 更新请求两次读到数据之间允许的最长间隔，检查与下载共用。
const UPDATE_READ_TIMEOUT: Duration = Duration::from_secs(60);

const AUTO_CHECK_INITIAL_DELAY_SECONDS: u64 = 8;
const AUTO_CHECK_SETTINGS_REFRESH_SECONDS: u64 = 60 * 60;
const AUTO_CHECK_FAILURE_RETRY_SECONDS: u64 = 60 * 60;

pub struct UpdateState {
    current: Mutex<Option<PendingUpdate>>,
}

struct PendingUpdate {
    update: TauriUpdate,
    metadata: UpdateMetadata,
    bytes: Option<Vec<u8>>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub update: Option<UpdateMetadata>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    pub current_version: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
    pub target: String,
    pub download_url: String,
    pub downloaded: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub progress: Option<f64>,
}

#[derive(Clone, Copy)]
pub enum CheckMode {
    Manual,
    Auto,
}

impl UpdateState {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    fn snapshot(&self, current_version: String) -> AppUpdateStatus {
        let update = self.with_current(|current| {
            current.as_ref().map(|pending| {
                let mut metadata = pending.metadata.clone();
                metadata.downloaded = pending.bytes.is_some();
                metadata
            })
        });

        AppUpdateStatus {
            current_version,
            update,
        }
    }

    fn set_update(&self, update: Option<PendingUpdate>) {
        self.with_current(|current| {
            *current = update;
        });
    }

    /// 记录一次检查发现的更新。与当前待安装的是同一版本时保留已下载的安装包：
    /// 更新窗口每次显示都会重新检查，不保留的话已经下好的包被丢掉，界面仍显示
    /// 「已下载」，点安装却报没有可安装的更新。
    fn set_checked_update(&self, update: TauriUpdate, metadata: UpdateMetadata) {
        self.with_current(|current| {
            let bytes = current
                .take()
                .filter(|pending| pending.metadata.version == metadata.version)
                .and_then(|pending| pending.bytes);
            *current = Some(PendingUpdate {
                update,
                metadata,
                bytes,
            });
        });
    }

    fn mark_downloaded(&self, version: &str, bytes: Vec<u8>) -> Result<UpdateMetadata> {
        self.with_current(|current| {
            let pending = current
                .as_mut()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            pending.bytes = Some(bytes);
            pending.metadata.downloaded = true;

            Ok(pending.metadata.clone())
        })
    }

    fn install_downloaded(&self, version: &str) -> Result<()> {
        self.with_current(|current| {
            let pending = current
                .as_mut()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            let bytes = pending
                .bytes
                .as_ref()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("update is not downloaded")))?;

            pending
                .update
                .install(bytes)
                .context("failed to install update")?;

            Ok(())
        })
    }

    fn with_current<R>(&self, f: impl FnOnce(&mut Option<PendingUpdate>) -> R) -> R {
        let mut guard = self.current.lock().unwrap_or_else(|poisoned| {
            log::error!("update state mutex poisoned, recovering");
            poisoned.into_inner()
        });
        f(&mut guard)
    }
}

pub fn init(app: &AppHandle) {
    app.manage(UpdateState::new());
}

pub async fn status(app: &AppHandle) -> AppUpdateStatus {
    app.state::<UpdateState>()
        .snapshot(app.package_info().version.to_string())
}

pub async fn check(app: &AppHandle, mode: CheckMode) -> Result<AppUpdateStatus> {
    if matches!(mode, CheckMode::Auto) && !should_auto_check(app) {
        return Ok(status(app).await);
    }

    let settings = app.state::<SettingsStore>().snapshot();
    let endpoints = update_endpoints(
        settings.update.include_beta,
        settings.update.include_nightly,
    )?;
    let exit_app = app.clone();
    let updater = app
        .updater_builder()
        .endpoints(endpoints)
        .context("failed to configure update endpoints")?
        // 插件与 reqwest 默认都不设超时：连接建立后对端不再回数据（代理、防火墙挂住）时
        // 请求永不返回，自动检查循环就此停摆，手动检查与下载也一直转圈。
        // 检查用总时限；下载体积大，只限制「连接」与「两次读到数据之间」的间隔。
        .timeout(UPDATE_CHECK_TIMEOUT)
        .configure_client(|client| {
            client
                .connect_timeout(UPDATE_CONNECT_TIMEOUT)
                .read_timeout(UPDATE_READ_TIMEOUT)
        })
        // Windows 上安装更新时插件直接结束进程，不经过 `RunEvent::Exit`：
        // 窗口几何要在这里刷盘，并留下正常退出标记，否则每次自动更新在日志里都像一次崩溃。
        .on_before_exit(move || {
            crate::window::save_all_window_states(&exit_app);
            log::info!("app exit (clean shutdown, installing update)");
        })
        .build()
        .context("failed to build updater")?;

    let found = updater
        .check()
        .await
        .context("failed to check for updates")?;
    let state = app.state::<UpdateState>();

    if let Some(update) = found {
        let metadata = metadata_from_update(&update);
        let skipped = settings
            .update
            .skipped_version
            .as_deref()
            .is_some_and(|version| version == metadata.version);

        if skipped {
            state.set_update(None);
        } else {
            state.set_checked_update(update, metadata);
        }
    } else {
        state.set_update(None);
    }

    persist_last_checked_at(app);

    Ok(status(app).await)
}

pub async fn download(app: &AppHandle, version: String) -> Result<UpdateMetadata> {
    let update = {
        let state = app.state::<UpdateState>();
        state.with_current(|current| {
            let pending = current
                .as_ref()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("no update is ready")))?;

            if pending.metadata.version != version {
                return Err(AppError::Other(anyhow::anyhow!(
                    "the selected update is no longer current"
                )));
            }

            Ok(pending.update.clone())
        })?
    };

    let mut downloaded = 0_u64;
    let app_handle = app.clone();
    let bytes = update
        .download(
            move |chunk_len, total| {
                downloaded = downloaded.saturating_add(chunk_len as u64);
                emit_progress(&app_handle, downloaded, total);
            },
            || {},
        )
        .await
        .context("failed to download update")?;

    app.state::<UpdateState>().mark_downloaded(&version, bytes)
}

pub fn install(app: &AppHandle, version: String) -> Result<()> {
    log::info!("installing downloaded update {version}");
    app.state::<UpdateState>().install_downloaded(&version)?;
    log::info!("update {version} installed, requesting app restart");
    app.request_restart();

    Ok(())
}

pub fn skip(app: &AppHandle, version: String) -> Result<AppUpdateStatus> {
    let patch = serde_json::json!({
        "update": {
            "skippedVersion": version,
        },
    });
    let next = app.state::<SettingsStore>().update(patch)?;
    crate::commands::emit_settings_updated(app, &next);
    app.state::<UpdateState>().set_update(None);

    Ok(app
        .state::<UpdateState>()
        .snapshot(app.package_info().version.to_string()))
}

pub fn schedule_auto_check(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(AUTO_CHECK_INITIAL_DELAY_SECONDS)).await;

        loop {
            let delay = next_auto_check_delay(&handle);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
                continue;
            }

            run_auto_check(&handle).await;
            tokio::time::sleep(Duration::from_secs(AUTO_CHECK_FAILURE_RETRY_SECONDS)).await;
        }
    });
}

async fn run_auto_check(app: &AppHandle) {
    match check(app, CheckMode::Auto).await {
        Ok(status) if status.update.is_some() => {
            if let Err(err) = crate::window::show_window(app, crate::window::UPDATE_WINDOW_LABEL) {
                log::warn!("show update window after automatic check failed: {err}");
            }
        }
        Ok(_) => {}
        Err(err) => {
            log::warn!("automatic update check failed: {err}");
        }
    }
}

fn metadata_from_update(update: &TauriUpdate) -> UpdateMetadata {
    UpdateMetadata {
        body: update.body.clone(),
        current_version: update.current_version.clone(),
        date: update.date.map(|date| {
            date.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| date.to_string())
        }),
        download_url: update.download_url.to_string(),
        downloaded: false,
        target: update.target.clone(),
        version: update.version.clone(),
    }
}

fn update_endpoints(include_beta: bool, include_nightly: bool) -> Result<Vec<Url>> {
    let stable_endpoint =
        std::env::var(STABLE_ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_STABLE_ENDPOINT.to_owned());
    let beta_endpoint =
        std::env::var(BETA_ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_BETA_ENDPOINT.to_owned());
    let nightly_endpoint =
        std::env::var(NIGHTLY_ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_NIGHTLY_ENDPOINT.to_owned());

    update_endpoints_from_values(
        include_beta,
        include_nightly,
        &stable_endpoint,
        &beta_endpoint,
        &nightly_endpoint,
    )
}

fn update_endpoints_from_values(
    include_beta: bool,
    include_nightly: bool,
    stable_endpoint: &str,
    beta_endpoint: &str,
    nightly_endpoint: &str,
) -> Result<Vec<Url>> {
    let mut endpoints = Vec::new();
    if include_nightly {
        endpoints.push(parse_endpoint(nightly_endpoint)?);
    }
    if include_beta {
        endpoints.push(parse_endpoint(beta_endpoint)?);
    }
    endpoints.push(parse_endpoint(stable_endpoint)?);

    Ok(endpoints)
}

fn parse_endpoint(endpoint: &str) -> Result<Url> {
    endpoint
        .parse::<Url>()
        .map_err(|err| AppError::Other(anyhow::anyhow!("update endpoint is invalid: {err}")))
}

fn should_auto_check(app: &AppHandle) -> bool {
    next_auto_check_delay(app).is_zero()
}

fn next_auto_check_delay(app: &AppHandle) -> Duration {
    let settings = app.state::<SettingsStore>().snapshot();

    next_auto_check_delay_for_settings(&settings.update, Utc::now())
}

fn next_auto_check_delay_for_settings(settings: &UpdateSettings, now: DateTime<Utc>) -> Duration {
    let settings_refresh = Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS);

    if !settings.auto_check {
        return settings_refresh;
    }

    let Some(last_checked_at) = settings.last_checked_at.as_deref() else {
        return Duration::ZERO;
    };
    let Ok(last_checked_at) = chrono::DateTime::parse_from_rfc3339(last_checked_at) else {
        return Duration::ZERO;
    };

    let elapsed = now
        .signed_duration_since(last_checked_at.with_timezone(&Utc))
        .num_seconds();
    let remaining = frequency_seconds(settings.frequency).saturating_sub(elapsed);
    if remaining <= 0 {
        return Duration::ZERO;
    }

    Duration::from_secs(remaining as u64).min(settings_refresh)
}

fn frequency_seconds(frequency: UpdateFrequency) -> i64 {
    match frequency {
        UpdateFrequency::Daily => 24 * 60 * 60,
        UpdateFrequency::Weekly => 7 * 24 * 60 * 60,
        UpdateFrequency::Monthly => 30 * 24 * 60 * 60,
    }
}

fn persist_last_checked_at(app: &AppHandle) {
    let patch = serde_json::json!({
        "update": {
            "lastCheckedAt": Utc::now().to_rfc3339(),
        },
    });

    match app.state::<SettingsStore>().update(patch) {
        Ok(next) => crate::commands::emit_settings_updated(app, &next),
        Err(err) => log::warn!("persist update check timestamp failed: {err}"),
    }
}

fn emit_progress(app: &AppHandle, downloaded: u64, total: Option<u64>) {
    let progress = total.filter(|value| *value > 0).map(|value| {
        let ratio = downloaded as f64 / value as f64;
        ratio.clamp(0.0, 1.0)
    });

    if let Err(err) = app.emit(
        UPDATE_PROGRESS_EVENT,
        UpdateDownloadProgress {
            downloaded,
            total,
            progress,
        },
    ) {
        log::warn!("emit update progress failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_settings(
        auto_check: bool,
        frequency: UpdateFrequency,
        last_checked_at: Option<String>,
    ) -> UpdateSettings {
        UpdateSettings {
            auto_check,
            frequency,
            include_beta: false,
            include_nightly: false,
            last_checked_at,
            skipped_version: None,
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn auto_check_delay_uses_settings_refresh_when_disabled() {
        let settings = update_settings(false, UpdateFrequency::Daily, None);

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, fixed_now()),
            Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS)
        );
    }

    #[test]
    fn auto_check_delay_is_due_without_previous_check() {
        let settings = update_settings(true, UpdateFrequency::Daily, None);

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, fixed_now()),
            Duration::ZERO
        );
    }

    #[test]
    fn auto_check_delay_uses_configured_frequency() {
        let now = fixed_now();
        let last_checked_at = (now - chrono::Duration::hours(24)).to_rfc3339();
        let daily = update_settings(true, UpdateFrequency::Daily, Some(last_checked_at.clone()));
        let weekly = update_settings(true, UpdateFrequency::Weekly, Some(last_checked_at));

        assert_eq!(
            next_auto_check_delay_for_settings(&daily, now),
            Duration::ZERO
        );
        assert_eq!(
            next_auto_check_delay_for_settings(&weekly, now),
            Duration::from_secs(AUTO_CHECK_SETTINGS_REFRESH_SECONDS)
        );
    }

    #[test]
    fn auto_check_delay_sleeps_until_due_when_within_refresh_window() {
        let now = fixed_now();
        let last_checked_at = (now - chrono::Duration::minutes(23 * 60 + 30)).to_rfc3339();
        let settings = update_settings(true, UpdateFrequency::Daily, Some(last_checked_at));

        assert_eq!(
            next_auto_check_delay_for_settings(&settings, now),
            Duration::from_secs(30 * 60)
        );
    }

    #[test]
    fn update_endpoints_include_enabled_channels_before_stable() {
        let endpoints = update_endpoints_from_values(
            true,
            true,
            "https://example.com/update?channel=stable",
            "https://example.com/update?channel=beta",
            "https://example.com/update?channel=nightly",
        )
        .unwrap();

        assert_eq!(
            endpoints.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://example.com/update?channel=nightly",
                "https://example.com/update?channel=beta",
                "https://example.com/update?channel=stable",
            ]
        );
    }
}
