// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Hochladen in Teilstücken: für Dateien, die größer sind als das, was ein
//! vorgeschalteter Proxy in einer einzelnen Anfrage durchlässt.
//!
//! 1. `POST /api/uploads` → `{ "id": "…" }`
//! 2. `PUT /api/uploads/{id}?offset=N` mit den rohen Bytes eines Stücks
//!    (beliebig oft, in Reihenfolge; `offset` = bisher übertragene Größe)
//! 3. Abschließen über die üblichen Upload-Endpunkte (`POST /api/documents`,
//!    `POST /api/documents/{id}/versions`): statt des Felds `file` ein Feld
//!    `upload_id` mit der Kennung als Wert und dem Dateinamen als `filename`.
//!
//! `DELETE /api/uploads/{id}` verwirft einen begonnenen Upload. Liegen
//! gebliebene Teil-Uploads verfallen nach einem Tag (und beim Neustart).

use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::StreamExt;
use serde::Deserialize;
use shared::{ChunkedUpload, ChunkedUploadStatus};
use tokio::io::AsyncWriteExt;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Nicht abgeschlossene Teil-Uploads werden nach dieser Zeit entfernt
const EXPIRY: Duration = Duration::from_secs(24 * 3600);

pub async fn create(State(st): State<AppState>) -> AppResult<Json<ChunkedUpload>> {
    remove_expired(&st).await;
    let id = format!("{:032x}", rand::random::<u128>());
    let path = st.storage.chunked_path(&id).ok_or_else(|| AppError::bad_request("Ungültige Kennung"))?;
    tokio::fs::File::create(&path).await?;
    Ok(Json(ChunkedUpload { id }))
}

#[derive(Deserialize)]
pub struct ChunkParams {
    offset: u64,
}

/// Ein Stück anhängen. Bricht die Übertragung ab, wird das Stück wieder
/// entfernt, sodass es mit demselben `offset` erneut gesendet werden kann.
pub async fn put_chunk(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(p): Query<ChunkParams>,
    body: Body,
) -> AppResult<Json<ChunkedUploadStatus>> {
    let path = st.storage.chunked_path(&id).ok_or_else(AppError::not_found)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await
        .map_err(|_| AppError::new(StatusCode::NOT_FOUND, "Upload nicht gefunden (abgelaufen?)"))?;
    let len = file.metadata().await?.len();
    if len != p.offset {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            format!("Falsche Position {} (bisher empfangen: {len} Bytes)", p.offset),
        ));
    }
    let max = st.config.max_upload_bytes as u64;
    // Ans Ende schreiben (= offset, oben geprüft)
    {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(len)).await?;
    }
    let mut file_w = tokio::io::BufWriter::with_capacity(256 * 1024, &mut file);
    let mut stream = body.into_data_stream();
    let mut size = len;
    let res: AppResult<()> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::bad_request(format!("Upload fehlgeschlagen: {e}")))?;
            size += chunk.len() as u64;
            if size > max {
                return Err(AppError::bad_request(format!("Datei ist zu groß (max. {} MB)", max / 1024 / 1024)));
            }
            file_w.write_all(&chunk).await?;
        }
        file_w.flush().await?;
        Ok(())
    }
    .await;
    drop(file_w);
    if let Err(e) = res {
        // Halbes Stück verwerfen
        let _ = file.set_len(len).await;
        return Err(e);
    }
    Ok(Json(ChunkedUploadStatus { size }))
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<String>) -> AppResult<StatusCode> {
    if let Some(path) = st.storage.chunked_path(&id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Liegen gebliebene Teil-Uploads entfernen
async fn remove_expired(st: &AppState) {
    let Ok(mut rd) = tokio::fs::read_dir(st.storage.tmp_dir()).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        if !e.file_name().to_string_lossy().starts_with("chunked-") {
            continue;
        }
        let old = e
            .metadata()
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > EXPIRY);
        if old {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
}
