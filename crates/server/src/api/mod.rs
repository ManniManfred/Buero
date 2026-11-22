// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use shared::{DesktopDownload, Stats};

use crate::error::AppResult;
use crate::state::AppState;

mod auth;
pub(crate) mod backup;
mod collections;
pub(crate) mod documents;
mod office;
mod shares;
mod tags;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/documents", get(documents::list).post(documents::upload))
        .route("/api/documents/bulk", post(documents::bulk))
        .route(
            "/api/documents/{id}",
            get(documents::get).patch(documents::update).delete(documents::delete),
        )
        .route("/api/documents/{id}/file", get(documents::file))
        .route("/api/documents/{id}/thumbnail", get(documents::thumbnail))
        .route("/api/documents/{id}/text", get(documents::text))
        .route("/api/documents/{id}/versions", post(documents::new_version))
        .route("/api/documents/{id}/reprocess", post(documents::reprocess))
        .route("/api/collections", get(collections::list).post(collections::create))
        .route(
            "/api/collections/{id}",
            axum::routing::patch(collections::update).delete(collections::delete),
        )
        .route("/api/tags", get(tags::list).post(tags::create))
        .route("/api/tags/{id}", axum::routing::patch(tags::update).delete(tags::delete))
        .route("/api/shares", get(shares::list).post(shares::create))
        .route("/api/shares/{id}", axum::routing::delete(shares::delete))
        .route("/api/features", get(office::features))
        .route("/api/office/config/{id}", get(office::config))
        .route("/api/stats", get(stats))
        .route("/api/admin/reindex", post(reindex))
        .route("/api/admin/reprocess-failed", post(reprocess_failed))
        .route("/api/admin/backup", get(backup::backup))
        .route("/api/admin/restore", post(backup::restore).layer(DefaultBodyLimit::disable()))
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(state.clone(), crate::auth::require_auth));

    let public = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/login", post(auth::login))
        .route("/api/desktop", get(desktop_downloads))
        // Freigabelinks (Zugriff ohne Anmeldung, geprüft über Token/Passwort)
        .route("/api/public/shares/{token}", get(shares::public_get))
        .route("/api/public/shares/{token}/unlock", post(shares::public_unlock))
        .route("/api/public/shares/{token}/upload", post(shares::public_upload))
        .route("/api/public/shares/{token}/documents/{doc}/file", get(shares::public_file))
        .route("/api/public/shares/{token}/documents/{doc}/thumbnail", get(shares::public_thumbnail))
        // Euro-Office Document Server (geprüft über signierte Links / JWT)
        .route("/api/office/file/{id}", get(office::file))
        .route("/api/office/callback/{id}", post(office::callback));

    let web = &state.config.web_dir;
    let spa = ServeDir::new(web)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(ServeFile::new(web.join("index.html")));

    // Tupfer Designer (WebAssembly) zum Bearbeiten von .tupfer-Projekten; spricht selbst die API an
    let tupfer = ServeDir::new(&state.config.tupfer_dir).precompressed_br().precompressed_gzip();

    Router::new()
        .merge(public)
        .merge(protected)
        .nest_service("/tupfer", tupfer)
        .nest_service("/desktop", ServeDir::new(&state.config.desktop_dir))
        .fallback_service(spa)
        .layer(DefaultBodyLimit::max(state.config.max_upload_bytes + 1024 * 1024))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn stats(State(st): State<AppState>) -> AppResult<Json<Stats>> {
    let (documents, total_size, pending, failed): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), IFNULL(SUM(size), 0), \
         IFNULL(SUM(extract_status IN ('pending','processing')), 0), \
         IFNULL(SUM(extract_status = 'failed'), 0) FROM documents",
    )
    .fetch_one(&st.pool)
    .await?;
    let unfiled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM documents d WHERE NOT EXISTS (SELECT 1 FROM document_collections dc WHERE dc.document_id = d.id)",
    )
    .fetch_one(&st.pool)
    .await?;
    Ok(Json(Stats { documents, total_size, pending, failed, unfiled }))
}

/// Endungen der Installationsdateien, die angeboten werden (Signaturen,
/// Update-Archive u. Ä. im selben Ordner werden ausgelassen)
const DESKTOP_EXTENSIONS: &[&str] = &["exe", "msi", "deb", "rpm", "appimage"];

/// Installationsdateien des Desktop-Clients (ohne Anmeldung, damit man sich die
/// App auch vor dem ersten Login holen kann)
async fn desktop_downloads(State(st): State<AppState>) -> Json<Vec<DesktopDownload>> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&st.config.desktop_dir) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_file() && name.contains('.') && DESKTOP_EXTENSIONS.contains(&ext.as_str()) {
                out.push(DesktopDownload { name, size: meta.len() });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Json(out)
}

async fn reindex(State(st): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    let n = st.reindex_all().await?;
    Ok(Json(serde_json::json!({ "indexed": n })))
}

async fn reprocess_failed(State(st): State<AppState>) -> AppResult<StatusCode> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM documents WHERE extract_status = 'failed'")
        .fetch_all(&st.pool)
        .await?;
    for id in ids {
        sqlx::query("UPDATE documents SET extract_status = 'pending' WHERE id = ?").bind(id).execute(&st.pool).await?;
        let _ = st.jobs.send(id);
    }
    Ok(StatusCode::ACCEPTED)
}
