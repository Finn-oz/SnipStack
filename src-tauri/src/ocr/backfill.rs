//! 被动图片 OCR:复制进历史的图片在后台识别,文本写入 `search_text` 进入 FTS 索引。
//!
//! 幂等约定:`search_text` 为 NULL 表示「未处理」;识别后无论有无文字都写入
//! (无文字写空串)作已处理标记,同一图片不会被重复识别。截屏取字条目在入库时
//! 已带该标记。另有进程内 in-flight 去重,同一条目并发调度只跑一次。
//! 识别串行(引擎内部 Mutex),单任务失败只记日志,不影响剪贴板主链路。

use std::collections::HashSet;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};

use crate::clipboard::{ImageStore, CLIPBOARD_UPDATED_EVENT};
use crate::core::{AppError, Result};
use crate::settings::{SettingsStore, SnipLineBreak};

/// 正在识别中的条目 id;防止同图连续复制(去重命中同一行)触发重复识别。
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn in_flight_insert(id: &str) -> bool {
    let mut guard = IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get_or_insert_with(HashSet::new).insert(id.to_owned())
}

fn in_flight_remove(id: &str) {
    let mut guard = IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(set) = guard.as_mut() {
        set.remove(id);
    }
}

/// 调度一次图片条目的后台 OCR。设置未开启、条目已处理或已在识别中时为空操作。
pub fn schedule(app: &AppHandle, item_id: String, image_file: String) {
    let enabled = app
        .try_state::<SettingsStore>()
        .map(|store| store.snapshot().snip.ocr_copied_images)
        .unwrap_or(false);
    if !enabled {
        return;
    }
    if !in_flight_insert(&item_id) {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = backfill_one(&app, &item_id, &image_file).await;
        in_flight_remove(&item_id);
        if let Err(err) = result {
            log::warn!("image ocr backfill for {item_id} failed: {err}");
        }
    });
}

async fn backfill_one(app: &AppHandle, item_id: &str, image_file: &str) -> Result<()> {
    let pool = app.state::<crate::db::DatabaseState>().pool().await;

    // search_text 非 NULL(含空串标记)即已处理:截屏取字写入、先前识别过、或用户手动编辑过。
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT search_text FROM clipboard_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&pool)
            .await
            .map_err(|err| AppError::Other(anyhow::anyhow!("query search_text: {err}")))?;
    match existing {
        None | Some(Some(_)) => return Ok(()),
        Some(None) => {}
    }

    let path = app.state::<ImageStore>().origin_path(image_file);
    let ocr_app = app.clone();
    let text = tauri::async_runtime::spawn_blocking(move || {
        let image = image::open(&path)
            .map_err(|err| AppError::Ocr(format!("读取历史图片失败: {err}")))?
            .to_rgba8();
        let lines = super::recognize(&ocr_app, &image)?;
        // 搜索索引场景不关心排版,固定按行保留,便于搜索命中后展示上下文。
        Ok::<_, AppError>(super::join_lines(&lines, SnipLineBreak::Keep))
    })
    .await
    .map_err(|err| AppError::Other(anyhow::anyhow!("image ocr task: {err}")))??;

    // 只补空(NULL),防止与并发的截屏取字写入互相覆盖;识别为空也写入空串作已处理标记。
    // UPDATE 触发 FTS 触发器自动重建索引。
    let updated = sqlx::query(
        "UPDATE clipboard_items SET search_text = ? WHERE id = ? AND search_text IS NULL",
    )
    .bind(&text)
    .bind(item_id)
    .execute(&pool)
    .await
    .map_err(|err| AppError::Other(anyhow::anyhow!("update search_text: {err}")))?;

    if updated.rows_affected() > 0 && !text.is_empty() {
        log::info!(
            "image ocr backfill: {item_id} indexed {} chars",
            text.chars().count()
        );
        let _ = app.emit(
            CLIPBOARD_UPDATED_EVENT,
            serde_json::json!({ "id": item_id, "kind": "image" }),
        );
    }
    Ok(())
}
