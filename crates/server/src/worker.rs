// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Hintergrundverarbeitung: Textextraktion, OCR, Vorschaubilder, Indexierung.

use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};

use crate::extract::{self, ExtractOptions};
use crate::state::AppState;

/// Startet die Verarbeitung; endet mit `state.restart` erst, wenn alle
/// laufenden Aufträge fertig sind.
pub fn spawn(state: AppState, mut rx: mpsc::UnboundedReceiver<i64>) -> tokio::task::JoinHandle<()> {
    let workers = state.config.workers;
    let sem = Arc::new(Semaphore::new(workers));
    tokio::spawn(async move {
        loop {
            let id = tokio::select! {
                _ = state.restart.cancelled() => break,
                id = rx.recv() => match id {
                    Some(id) => id,
                    None => break,
                },
            };
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
        let _ = sem.acquire_many(workers as u32).await;
    })
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
        ocr_max_pages: state.config.ocr_max_pages,
    };
    let (text, pages, err) = match extract::extract(&path, &doc.extension, &opts).await {
        Ok(e) => (e.text, e.page_count, None),
        Err(e) => (String::new(), None, Some(format!("{e:#}"))),
    };

    let thumb = state.storage.thumb_path(id);
    let mut has_thumb = match extract::thumbnail(&path, &doc.extension, &thumb, &state.storage.tmp_dir()).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Kein Vorschaubild für {id}: {e:#}");
            false
        }
    };
    // Office-Dateien: PDF-Vorschau über Euro-Office, daraus das Vorschaubild.
    // Ohne bekannte Adresse (kein BUERO_PUBLIC_URL, noch keine Anfrage seit dem
    // Start) entsteht sie beim ersten Ansehen.
    if !has_thumb && crate::preview::available(state, &doc) {
        if let Some(base) = state.background_base() {
            match crate::preview::ensure(state, &doc, &base).await {
                Ok(pdf) => {
                    has_thumb = extract::thumbnail(&pdf, "pdf", &thumb, &state.storage.tmp_dir())
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!("Kein Vorschaubild für {id}: {e:#}");
                            false
                        })
                }
                Err(e) => tracing::warn!("Keine PDF-Vorschau für {id}: {e:#}"),
            }
        }
    }

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

    if err.is_none() && crate::autotitle::scannable(&doc.extension) && crate::autotitle::is_scan_name(&doc.title) {
        if let Err(e) = name_scan(state, &doc, &text).await {
            tracing::warn!("Kein Titel aus dem Inhalt für {id}: {e:#}");
        }
    }
    if doc.extension.eq_ignore_ascii_case("eml") {
        if let Err(e) = crate::email::register(state, id).await {
            tracing::warn!("E-Mail {id} nicht vollständig erfasst: {e:#}");
        }
    }
    state.reindex(&[id]).await?;
    tracing::info!(
        "Dokument {id} verarbeitet ({} Zeichen) in {:.1?}",
        text.len(),
        started.elapsed()
    );
    Ok(())
}

/// Scan mit automatischem Namen: Titel und Datum aus dem Inhalt. Der Titel
/// wird nur ersetzt, wenn ihn niemand inzwischen geändert hat, das Datum nur,
/// wenn noch keins gesetzt ist.
async fn name_scan(state: &AppState, doc: &crate::db::DocRow, text: &str) -> anyhow::Result<()> {
    let today = crate::util::fmt_date(crate::util::now());
    let guess = crate::autotitle::guess(text, &today);
    if let Some(t) = &guess.title {
        let done = sqlx::query("UPDATE documents SET title = ?, updated_at = ? WHERE id = ? AND title = ?")
            .bind(t)
            .bind(crate::util::now())
            .bind(doc.id)
            .bind(&doc.title)
            .execute(&state.pool)
            .await?;
        if done.rows_affected() > 0 {
            tracing::info!("Dokument {}: Titel „{t}“ aus dem Inhalt", doc.id);
        }
    }
    if let Some(d) = &guess.date {
        sqlx::query("UPDATE documents SET document_date = ? WHERE id = ? AND IFNULL(document_date, '') = ''")
            .bind(d)
            .bind(doc.id)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}
