// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Anbindung an Euro-Office (OnlyOffice-kompatibler Document Server).
//!
//! Ablauf: Die Oberfläche holt sich unter `/api/office/config/{id}` eine
//! signierte Editor-Konfiguration und bettet damit den Editor des Document
//! Servers ein. Der Document Server lädt die Datei über einen signierten Link
//! (`/api/office/file/{id}`) und meldet Änderungen an
//! `/api/office/callback/{id}`. Büro lädt die gespeicherte Datei dann herunter
//! und legt sie als neue Version ab.

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use futures_util::TryStreamExt;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use shared::{office_document_type, office_editable, Features, OfficeConfig};

use super::documents::{add_version, guess_mime, serve_document, Upload};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::sign;
use crate::state::AppState;
use crate::util::now;

/// Gültigkeit des Datei-Links für den Document Server
const FILE_LINK_SECS: i64 = 24 * 3600;

pub async fn features(State(st): State<AppState>) -> Json<Features> {
    Json(Features {
        office: st.config.office_url.is_some(),
        tupfer: st.config.tupfer_dir.join("index.html").is_file(),
    })
}

pub async fn config(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> AppResult<Json<OfficeConfig>> {
    let office = st
        .config
        .office_url
        .clone()
        .ok_or_else(|| AppError::bad_request("Euro-Office ist nicht eingerichtet (BUERO_OFFICE_URL)"))?;
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    let ext = doc.extension.to_ascii_lowercase();
    let doc_type = office_document_type(&ext)
        .ok_or_else(|| AppError::bad_request("Dieser Dateityp kann nicht in Euro-Office geöffnet werden"))?;
    let can_edit = office_editable(&ext);

    // Schlüssel der Bearbeitungssitzung: bleibt gleich, bis der Document Server
    // das Schließen meldet – so landen mehrere Fenster in derselben Sitzung.
    let key: String = match sqlx::query_scalar::<_, Option<String>>("SELECT office_key FROM documents WHERE id = ?")
        .bind(id)
        .fetch_one(&st.pool)
        .await?
    {
        Some(k) => k,
        None => {
            let mut r = [0u8; 6];
            rand::thread_rng().fill_bytes(&mut r);
            let k = format!("{id}-{}-{}", doc.version, hex::encode(r));
            sqlx::query("UPDATE documents SET office_key = ? WHERE id = ?").bind(&k).bind(id).execute(&st.pool).await?;
            k
        }
    };

    let base = st.public_base(&headers);
    let file_url = file_link(&st, &base, id, doc.version);
    let cb_sig = sign::sign(&st.secret, &format!("office-cb:{id}"));
    let callback_url = format!("{base}/api/office/callback/{id}?sig={cb_sig}");

    let mut cfg = json!({
        "type": "desktop",
        "width": "100%",
        "height": "100%",
        "documentType": doc_type,
        "document": {
            "fileType": ext,
            "key": key,
            "title": doc.original_filename,
            "url": file_url,
            "permissions": {
                "edit": can_edit,
                "download": true,
                "print": true,
                "review": can_edit,
                "comment": can_edit,
            },
        },
        "editorConfig": {
            "mode": if can_edit { "edit" } else { "view" },
            "lang": "de",
            "region": "de-DE",
            "callbackUrl": callback_url,
            "user": { "id": "buero", "name": "Büro" },
            "customization": {
                // „Speichern“-Knopf: legt sofort eine neue Version an
                "forcesave": true,
                "compactHeader": true,
                "uiTheme": "default-light",
            },
        },
    });
    if let Some(secret) = &st.config.office_jwt_secret {
        let token = sign::jwt_encode(secret, &cfg);
        cfg["token"] = Value::String(token);
    }
    Ok(Json(OfficeConfig {
        script_url: format!("{office}/web-apps/apps/api/documents/api.js"),
        config: cfg.to_string(),
        can_edit,
    }))
}

/// Signierter, zeitlich begrenzter Link, über den der Document Server eine
/// Fassung des Dokuments abholt
pub(crate) fn file_link(st: &AppState, base: &str, id: i64, version: i64) -> String {
    let exp = now() + FILE_LINK_SECS;
    let sig = sign::sign(&st.secret, &format!("office-file:{id}:{version}:{exp}"));
    format!("{base}/api/office/file/{id}?v={version}&exp={exp}&sig={sig}")
}

#[derive(Deserialize)]
pub struct FileParams {
    v: i64,
    exp: i64,
    sig: String,
}

/// Datei für den Document Server (signierter, zeitlich begrenzter Link)
pub async fn file(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(p): Query<FileParams>,
    req: Request,
) -> AppResult<Response> {
    if p.exp < now() || !sign::verify(&st.secret, &format!("office-file:{id}:{}:{}", p.v, p.exp), &p.sig) {
        return Err(AppError::new(StatusCode::FORBIDDEN, "Ungültiger oder abgelaufener Link"));
    }
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    serve_document(&st, &doc, Some(p.v), true, req).await
}

#[derive(Deserialize)]
pub struct CallbackParams {
    sig: String,
}

/// Rückmeldung des Document Servers. Muss immer mit `{"error": 0}` antworten,
/// sonst zeigt der Editor einen Fehler an.
pub async fn callback(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(p): Query<CallbackParams>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !sign::verify(&st.secret, &format!("office-cb:{id}"), &p.sig) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": 1, "message": "Ungültige Signatur" })));
    }
    let body = match &st.config.office_jwt_secret {
        Some(secret) => match verify_jwt(secret, &headers, &body) {
            Some(b) => b,
            None => {
                tracing::warn!("Euro-Office-Rückmeldung für Dokument {id} mit ungültigem JWT abgelehnt");
                return (StatusCode::FORBIDDEN, Json(json!({ "error": 1, "message": "Ungültiges Token" })));
            }
        },
        None => body,
    };
    match handle_callback(&st, id, &body).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "error": 0 }))),
        Err(e) => {
            tracing::error!("Euro-Office-Rückmeldung für Dokument {id}: {}", e.message);
            (StatusCode::OK, Json(json!({ "error": 1, "message": e.message })))
        }
    }
}

/// Der Document Server schickt das JWT entweder im Feld `token` (dann sind die
/// Nutzdaten die komplette Rückmeldung) oder im Authorization-Header (dann
/// stecken sie unter `payload`).
fn verify_jwt(secret: &str, headers: &HeaderMap, body: &Value) -> Option<Value> {
    if let Some(t) = body.get("token").and_then(|t| t.as_str()) {
        return sign::jwt_decode(secret, t);
    }
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let t = auth.strip_prefix("Bearer ")?;
    let payload = sign::jwt_decode(secret, t)?;
    Some(payload.get("payload").cloned().unwrap_or(payload))
}

async fn handle_callback(st: &AppState, id: i64, body: &Value) -> AppResult<()> {
    let status = body.get("status").and_then(|s| s.as_i64()).unwrap_or(0);
    let key = body.get("key").and_then(|k| k.as_str()).unwrap_or("");
    match status {
        // 2 = geschlossen, speichern · 6 = Speichern-Knopf (Sitzung läuft weiter)
        2 | 6 => {
            let url = body
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or_else(|| AppError::bad_request("Rückmeldung ohne Datei-URL"))?;
            let filetype = body.get("filetype").and_then(|f| f.as_str());
            save_from_url(st, id, url, filetype).await?;
            if status == 2 {
                end_session(st, id, key).await?;
            }
            tracing::info!("Dokument {id} aus Euro-Office gespeichert");
        }
        // 4 = geschlossen ohne Änderungen
        4 => end_session(st, id, key).await?,
        3 | 7 => tracing::warn!("Euro-Office meldet Fehler beim Speichern von Dokument {id} (Status {status})"),
        _ => {}
    }
    Ok(())
}

async fn end_session(st: &AppState, id: i64, key: &str) -> AppResult<()> {
    sqlx::query("UPDATE documents SET office_key = NULL WHERE id = ? AND office_key = ?")
        .bind(id)
        .bind(key)
        .execute(&st.pool)
        .await?;
    Ok(())
}

async fn save_from_url(st: &AppState, id: i64, url: &str, filetype: Option<&str>) -> AppResult<()> {
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    let resp = st
        .http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, format!("Download vom Document Server: {e}")))?;
    let stream = resp.bytes_stream().map_err(|e| e.to_string());
    let staged = st
        .storage
        .stage(Box::pin(stream), st.config.max_upload_bytes)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    // Speichert der Document Server in einem anderen Format, Endung anpassen
    let mut filename = doc.original_filename.clone();
    if let Some(ft) = filetype.map(str::to_ascii_lowercase).filter(|ft| !ft.is_empty() && *ft != doc.extension) {
        let stem = match filename.rfind('.') {
            Some(i) if i > 0 => &filename[..i],
            _ => filename.as_str(),
        };
        filename = format!("{stem}.{ft}");
    }
    let mime = guess_mime(&filename, Some(&doc.mime));
    add_version(st, &doc, Upload { staged, filename, mime }, None).await?;
    Ok(())
}
