// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Hintergrundverarbeitung: Textextraktion, OCR, Vorschaubilder, Indexierung.

use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};

use crate::extract::{self, ExtractOptions};
use crate::state::AppState;

pub fn spawn(state: AppState, mut rx: mpsc::UnboundedReceiver<i64>) {
    let sem = Arc::new(Semaphore::new(state.config.workers));
    tokio::spawn(async move {
        while let Some(id) = rx.recv().await {
            let permit = sem.clone().acquire_owned().await.expect("semaphore");
            let st = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = process(&st, id).await {
                    tracing::error!("Verarbeitung von Dokument {id} fehlgeschlagen: {e:#}");
                    let _ = sqlx::query(
                        "UPDATE documents SET extract_status = 'failed', extract_error = ? WHERE id = ?",
                    )
                    .bind(format!("{e:#}"))
                    .bind(id)
                    .execute(&st.pool)
                    .await;
                }
            });
        }
    });
}

/// Unvollständige Aufträge nach einem Neustart wieder einreihen.
pub async fn requeue_pending(state: &AppState) -> anyhow::Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM documents WHERE extract_status IN ('pending', 'processing') ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    if !ids.is_empty() {
        tracing::info!("{} Dokumente werden (erneut) verarbeitet", ids.len());
    }
    for id in ids {
        let _ = state.jobs.send(id);
    }
    Ok(())
}

async fn process(state: &AppState, id: i64) -> anyhow::Result<()> {
    let Some(doc) = crate::db::get_doc(&state.pool, id).await? else {
        return Ok(());
    };
    sqlx::query("UPDATE documents SET extract_status = 'processing' WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;

    let started = std::time::Instant::now();
    let path = state.storage.file_path(&doc.sha256);
    let opts = ExtractOptions {
        ocr: state.config.ocr_enabled,
        ocr_lang: state.config.ocr_lang.clone(),
        tmp_dir: state.storage.tmp_dir(),
    };
    let (text, pages, err) = match extract::extract(&path, &doc.extension, &opts).await {
        Ok(e) => (e.text, e.page_count, None),
        Err(e) => (String::new(), None, Some(format!("{e:#}"))),
    };

    let thumb = state.storage.thumb_path(id);
    let has_thumb = match extract::thumbnail(&path, &doc.extension, &thumb, &state.storage.tmp_dir()).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Kein Vorschaubild für {id}: {e:#}");
            false
        }
    };

    // Datei könnte während der Verarbeitung ersetzt worden sein (neue Version)
    let current: Option<String> = sqlx::query_scalar("SELECT sha256 FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    if current.as_deref() != Some(doc.sha256.as_str()) {
        return Ok(());
    }

    sqlx::query(
        "UPDATE documents SET content = ?, page_count = ?, has_thumbnail = ?, extract_status = ?, extract_error = ? WHERE id = ?",
    )
    .bind(&text)
    .bind(pages)
    .bind(has_thumb)
    .bind(if err.is_some() { "failed" } else { "done" })
    .bind(&err)
    .bind(id)
    .execute(&state.pool)
    .await?;

    state.reindex(&[id]).await?;
    tracing::info!(
        "Dokument {id} verarbeitet ({} Zeichen) in {:.1?}",
        text.len(),
        started.elapsed()
    );
    Ok(())
}
