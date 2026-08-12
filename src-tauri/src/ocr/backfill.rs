//! 被动图片 OCR:复制进历史的图片在后台识别,文本写入 `search_text` 进入 FTS 索引。
//!
//! 只补空:`search_text` 已有值(截屏取字条目、先前已识别)的行不重复识别、不覆盖。
//! 识别串行(引擎内部 Mutex),单任务失败只记日志,不影响剪贴板主链路。

use tauri::{AppHandle, Emitter, Manager};

use crate::clipboard::{ImageStore, CLIPBOARD_UPDATED_EVENT};
use crate::core::{AppError, Result};
use crate::settings::{SettingsStore, SnipLineBreak};

/// 调度一次图片条目的后台 OCR。设置未开启或条目已有搜索文本时为空操作。
pub fn schedule(app: &AppHandle, item_id: String, image_file: String) {
    let enabled = app
        .try_state::<SettingsStore>()
        .map(|store| store.snapshot().snip.ocr_copied_images)
        .unwrap_or(false);
    if !enabled {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = backfill_one(&app, &item_id, &image_file).await {
            log::warn!("image ocr backfill for {item_id} failed: {err}");
        }
    });
}

async fn backfill_one(app: &AppHandle, item_id: &str, image_file: &str) -> Result<()> {
    let pool = app.state::<crate::db::DatabaseState>().pool().await;

    // 已有搜索文本(截屏取字写入、或去重命中的旧行)则不再识别。
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT search_text FROM clipboard_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&pool)
            .await
            .map_err(|err| AppError::Other(anyhow::anyhow!("query search_text: {err}")))?;
    match existing {
        None => return Ok(()),
        Some(Some(text)) if !text.is_empty() => return Ok(()),
        _ => {}
    }

    let path = app.state::<ImageStore>().origin_path(image_file);
    let ocr_app = app.clone();
    let text = tauri::async_runtime::spawn_blocking(move || {
        let image = image::open(&path)
            .map_err(|err| AppError::Ocr(format!("读取历史图片失败: {err}")))?
            .to_rgba8();
        let lines = super::recognize(&ocr_app, image)?;
        // 搜索索引场景不关心排版,固定按行保留,便于搜索命中后展示上下文。
        Ok::<_, AppError>(super::join_lines(&lines, SnipLineBreak::Keep))
    })
    .await
    .map_err(|err| AppError::Other(anyhow::anyhow!("image ocr task: {err}")))??;

    if text.is_empty() {
        return Ok(());
    }

    // 只补空,防止与并发的截屏取字写入互相覆盖;UPDATE 触发 FTS 触发器自动重建索引。
    let updated = sqlx::query(
        "UPDATE clipboard_items SET search_text = ? WHERE id = ? AND (search_text IS NULL OR search_text = '')",
    )
    .bind(&text)
    .bind(item_id)
    .execute(&pool)
    .await
    .map_err(|err| AppError::Other(anyhow::anyhow!("update search_text: {err}")))?;

    if updated.rows_affected() > 0 {
        log::info!(
            "image ocr backfill: {item_id} indexed {} chars",
            text.chars().count()
        );
        let _ = app.emit(
            CLIPBOARD_UPDATED_EVENT,
            serde_json::json!({ "id": item_id }),
        );
    }
    Ok(())
}
