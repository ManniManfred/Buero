// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Freigabelinks: einzelne Dokumente oder ganze Sammlungen (inkl.
//! Untersammlungen) ohne Anmeldung zugänglich machen – optional mit Passwort,
//! Ablaufdatum und Upload-Möglichkeit.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use shared::{PublicDocument, PublicShare, Share, ShareInput, ShareKey, ShareUnlock};

use super::documents::{read_upload_form, serve, serve_document, store_uploads};
use crate::auth::constant_time_eq;
use crate::db;
use crate::security;
use crate::error::{AppError, AppResult};
use crate::sign;
use crate::state::AppState;
use crate::util::now;

#[derive(FromRow, Clone)]
struct ShareRow {
    id: i64,
    token: String,
    document_id: Option<i64>,
    collection_id: Option<i64>,
    password_hash: Option<String>,
    allow_upload: bool,
    expires_at: Option<i64>,
    note: String,
    created_at: i64,
    access_count: i64,
    last_access_at: Option<i64>,
}

const SHARE_COLS: &str = "id, token, document_id, collection_id, password_hash, allow_upload, expires_at, \
    note, created_at, access_count, last_access_at";

/// Neue Passwörter: Argon2id. Ältere Freigaben haben noch `salz$sha256`.
async fn check_password(stored: &str, pw: &str) -> bool {
    if stored.starts_with("$argon2") {
        return security::verify_password_async(pw.to_string(), stored.to_string()).await;
    }
    let Some((salt, h)) = stored.split_once('$') else { return false };
    let calc = hex::encode(Sha256::digest(format!("{salt}{pw}").as_bytes()));
    constant_time_eq(calc.as_bytes(), h.as_bytes())
}

/// Zugangsschlüssel nach Passworteingabe. Ändert sich mit dem Passwort, also
/// werden alte Schlüssel ungültig, wenn die Freigabe neu angelegt wird.
fn access_key(st: &AppState, row: &ShareRow) -> Option<String> {
    row.password_hash
        .as_ref()
        .map(|h| sign::sign(&st.secret, &format!("share:{}:{h}", row.token)))
}

async fn target_name(st: &AppState, row: &ShareRow) -> AppResult<String> {
    let name: Option<String> = if let Some(d) = row.document_id {
        sqlx::query_scalar("SELECT title FROM documents WHERE id = ?").bind(d).fetch_optional(&st.pool).await?
    } else {
        sqlx::query_scalar("SELECT name FROM collections WHERE id = ?")
            .bind(row.collection_id)
            .fetch_optional(&st.pool)
            .await?
    };
    Ok(name.unwrap_or_default())
}

async fn to_share(st: &AppState, row: ShareRow) -> AppResult<Share> {
    Ok(Share {
        target_name: target_name(st, &row).await?,
        id: row.id,
        token: row.token,
        document_id: row.document_id,
        collection_id: row.collection_id,
        has_password: row.password_hash.is_some(),
        allow_upload: row.allow_upload,
        expires_at: row.expires_at,
        note: row.note,
        created_at: row.created_at,
        access_count: row.access_count,
        last_access_at: row.last_access_at,
    })
}

// ---------------------------------------------------------------------------
// Verwaltung (angemeldet)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListParams {
    document_id: Option<i64>,
    collection_id: Option<i64>,
}

pub async fn list(State(st): State<AppState>, Query(p): Query<ListParams>) -> AppResult<Json<Vec<Share>>> {
    let rows: Vec<ShareRow> = sqlx::query_as(&format!(
        "SELECT {SHARE_COLS} FROM shares WHERE (?1 IS NULL OR document_id = ?1) AND (?2 IS NULL OR collection_id = ?2) \
         ORDER BY created_at DESC"
    ))
    .bind(p.document_id)
    .bind(p.collection_id)
    .fetch_all(&st.pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(to_share(&st, r).await?);
    }
    Ok(Json(out))
}

pub async fn create(State(st): State<AppState>, Json(i): Json<ShareInput>) -> AppResult<Json<Share>> {
    match (i.document_id, i.collection_id) {
        (Some(d), None) => {
            db::get_doc(&st.pool, d).await?.ok_or_else(AppError::not_found)?;
        }
        (None, Some(c)) => {
            let exists: Option<i64> =
                sqlx::query_scalar("SELECT id FROM collections WHERE id = ?").bind(c).fetch_optional(&st.pool).await?;
            exists.ok_or_else(AppError::not_found)?;
        }
        _ => return Err(AppError::bad_request("Entweder ein Dokument oder eine Sammlung angeben")),
    }
    if let Some(e) = i.expires_at {
        if e <= now() {
            return Err(AppError::bad_request("Das Ablaufdatum liegt in der Vergangenheit"));
        }
    }
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let pw = match i.password.clone().filter(|p| !p.is_empty()) {
        Some(p) => Some(tokio::task::spawn_blocking(move || security::hash_password(&p)).await.map_err(anyhow::Error::from)?),
        None => None,
    };
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO shares (token, document_id, collection_id, password_hash, allow_upload, expires_at, note, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&token)
    .bind(i.document_id)
    .bind(i.collection_id)
    .bind(pw)
    .bind(i.allow_upload && i.collection_id.is_some())
    .bind(i.expires_at)
    .bind(i.note.trim())
    .bind(now())
    .fetch_one(&st.pool)
    .await?;
    let row: ShareRow = sqlx::query_as(&format!("SELECT {SHARE_COLS} FROM shares WHERE id = ?"))
        .bind(id)
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(to_share(&st, row).await?))
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let n = sqlx::query("DELETE FROM shares WHERE id = ?").bind(id).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Öffentlicher Zugriff über den Link
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct KeyParams {
    key: Option<String>,
    #[serde(default)]
    download: bool,
}

fn key_from(headers: &HeaderMap, p: &KeyParams) -> Option<String> {
    headers
        .get("x-share-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| p.key.clone())
}

/// Freigabe laden und prüfen (Ablauf). `key`: Zugangsschlüssel bei Passwortschutz.
async fn open_share(st: &AppState, token: &str, key: Option<&str>) -> AppResult<ShareRow> {
    let row: Option<ShareRow> = sqlx::query_as(&format!("SELECT {SHARE_COLS} FROM shares WHERE token = ?"))
        .bind(token)
        .fetch_optional(&st.pool)
        .await?;
    let row = row.ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "Diesen Link gibt es nicht (mehr)"))?;
    if row.expires_at.is_some_and(|e| e <= now()) {
        return Err(AppError::new(StatusCode::GONE, "Dieser Link ist abgelaufen"));
    }
    if let Some(expected) = access_key(st, &row) {
        let ok = key.is_some_and(|k| constant_time_eq(k.as_bytes(), expected.as_bytes()));
        if !ok {
            return Err(AppError::new(StatusCode::FORBIDDEN, "Passwort erforderlich"));
        }
    }
    Ok(row)
}

/// IDs aller Dokumente, die über die Freigabe sichtbar sind.
async fn shared_doc_ids(st: &AppState, row: &ShareRow) -> AppResult<Vec<i64>> {
    if let Some(d) = row.document_id {
        return Ok(vec![d]);
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE sub(id) AS (SELECT ? UNION SELECT c.id FROM collections c JOIN sub ON c.parent_id = sub.id) \
         SELECT DISTINCT dc.document_id FROM document_collections dc JOIN sub ON dc.collection_id = sub.id",
    )
    .bind(row.collection_id)
    .fetch_all(&st.pool)
    .await?;
    Ok(ids)
}

async fn ensure_doc_in_share(st: &AppState, row: &ShareRow, doc_id: i64) -> AppResult<db::DocRow> {
    if !shared_doc_ids(st, row).await?.contains(&doc_id) {
        return Err(AppError::not_found());
    }
    db::get_doc(&st.pool, doc_id).await?.ok_or_else(AppError::not_found)
}

pub async fn public_get(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Query(p): Query<KeyParams>,
    headers: HeaderMap,
) -> AppResult<Json<PublicShare>> {
    let key = key_from(&headers, &p);
    let row = open_share(&st, &token, key.as_deref()).await?;
    sqlx::query("UPDATE shares SET access_count = access_count + 1, last_access_at = ? WHERE id = ?")
        .bind(now())
        .bind(row.id)
        .execute(&st.pool)
        .await?;
    let ids = shared_doc_ids(&st, &row).await?;
    let mut docs: Vec<PublicDocument> = db::summaries(&st.pool, &ids)
        .await?
        .into_iter()
        .map(|s| PublicDocument {
            id: s.id,
            title: s.title,
            original_filename: s.original_filename,
            kind: s.kind,
            mime: s.mime,
            size: s.size,
            document_date: s.document_date,
            updated_at: s.updated_at,
            has_thumbnail: s.has_thumbnail,
        })
        .collect();
    docs.sort_by(|a, b| {
        b.document_date
            .cmp(&a.document_date)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    Ok(Json(PublicShare {
        title: target_name(&st, &row).await?,
        is_collection: row.collection_id.is_some(),
        allow_upload: row.allow_upload,
        expires_at: row.expires_at,
        documents: docs,
    }))
}

pub async fn public_unlock(
    State(st): State<AppState>,
    Path(token): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(u): Json<ShareUnlock>,
) -> AppResult<Json<ShareKey>> {
    let row: Option<ShareRow> = sqlx::query_as(&format!("SELECT {SHARE_COLS} FROM shares WHERE token = ?"))
        .bind(&token)
        .fetch_optional(&st.pool)
        .await?;
    let row = row.ok_or_else(AppError::not_found)?;
    if row.expires_at.is_some_and(|e| e <= now()) {
        return Err(AppError::new(StatusCode::GONE, "Dieser Link ist abgelaufen"));
    }
    let Some(h) = &row.password_hash else {
        return Ok(Json(ShareKey { key: String::new() }));
    };
    // Ausprobieren bremsen: Sperre pro Adresse nach mehreren Fehlversuchen
    let attempt = st.share_limit.begin(security::client_ip(peer, &headers))?;
    if check_password(h, &u.password).await {
        attempt.success();
        Ok(Json(ShareKey { key: access_key(&st, &row).unwrap_or_default() }))
    } else {
        attempt.failed().await;
        Err(AppError::new(StatusCode::FORBIDDEN, "Falsches Passwort"))
    }
}

pub async fn public_file(
    State(st): State<AppState>,
    Path((token, doc_id)): Path<(String, i64)>,
    Query(p): Query<KeyParams>,
    req: Request,
) -> AppResult<Response> {
    let key = key_from(req.headers(), &p);
    let row = open_share(&st, &token, key.as_deref()).await?;
    let doc = ensure_doc_in_share(&st, &row, doc_id).await?;
    serve_document(&st, &doc, None, p.download, req).await
}

pub async fn public_thumbnail(
    State(st): State<AppState>,
    Path((token, doc_id)): Path<(String, i64)>,
    Query(p): Query<KeyParams>,
    req: Request,
) -> AppResult<Response> {
    let key = key_from(req.headers(), &p);
    let row = open_share(&st, &token, key.as_deref()).await?;
    ensure_doc_in_share(&st, &row, doc_id).await?;
    let path = st.storage.thumb_path(doc_id);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(AppError::not_found());
    }
    serve(path, "image/jpeg", req).await
}

pub async fn public_upload(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Query(p): Query<KeyParams>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> AppResult<Json<serde_json::Value>> {
    let key = key_from(&headers, &p);
    let row = open_share(&st, &token, key.as_deref()).await?;
    let Some(collection) = row.collection_id.filter(|_| row.allow_upload) else {
        return Err(AppError::new(StatusCode::FORBIDDEN, "Hochladen ist für diesen Link nicht erlaubt"));
    };
    let limit = st.config.share_uploads_per_hour;
    let too_many = || {
        AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Über diesen Link sind höchstens {limit} Dateien pro Stunde möglich – bitte später erneut versuchen"),
        )
    };
    if st.share_uploads.used(row.id) >= limit {
        return Err(too_many());
    }
    let mut form = read_upload_form(&st, &mut mp, st.config.share_max_upload_bytes).await?;
    let n = form.uploads.len() as u32;
    if st.share_uploads.used(row.id) + n > limit {
        for u in &form.uploads {
            st.storage.discard(&u.staged).await;
        }
        return Err(too_many());
    }
    st.share_uploads.add(row.id, n);
    // Empfänger bestimmen nur die Dateien – Ziel ist immer die freigegebene Sammlung
    form.collections = vec![collection];
    form.tags.clear();
    form.title = None;
    form.date = None;
    form.link_duplicates = false;
    let results = store_uploads(&st, form).await?;
    let stored = results.iter().filter(|r| !r.duplicate).count();
    tracing::info!("Freigabe {}: {stored} Datei(en) hochgeladen ({} bereits vorhanden)", row.id, results.len() - stored);
    // Nicht verraten, ob eine Datei schon im Archiv liegt
    Ok(Json(serde_json::json!({ "stored": results.len(), "duplicates": 0 })))
}
