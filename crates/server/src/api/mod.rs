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

use shared::Stats;

use crate::error::AppResult;
use crate::state::AppState;

mod auth;
pub(crate) mod backup;
mod collections;
mod dashboard;
pub(crate) mod links;
mod mail;
mod mcp;
mod desktop;
pub(crate) mod documents;
mod office;
mod shares;
mod tags;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/documents", get(documents::list).post(documents::upload))
        .route("/api/documents/bulk", post(documents::bulk))
        .route("/api/documents/new", post(documents::create_blank))
        .route(
            "/api/documents/{id}",
            get(documents::get).patch(documents::update).delete(documents::delete),
        )
        .route("/api/documents/{id}/file", get(documents::file))
        .route("/api/documents/{id}/thumbnail", get(documents::thumbnail))
        .route("/api/documents/{id}/text", get(documents::text))
        .route("/api/documents/{id}/versions", post(documents::new_version))
        .route("/api/documents/{id}/reprocess", post(documents::reprocess))
        .route("/api/documents/{id}/email", get(mail::email_info))
        .route("/api/documents/{id}/links", get(links::get).post(links::add))
        .route("/api/documents/{id}/links/{target}", axum::routing::delete(links::remove))
        .route("/api/mail/accounts", get(mail::list).post(mail::create))
        .route("/api/mail/accounts/{id}", axum::routing::patch(mail::update).delete(mail::delete))
        .route("/api/mail/accounts/{id}/folders", post(mail::folders))
        .route("/api/mail/accounts/{id}/run", post(mail::run))
        .route("/api/mail/accounts/{id}/cancel", post(mail::cancel))
        .route("/api/mail/in-collection/{id}", get(mail::count_in))
        .route("/api/mail/move", post(mail::move_mails))
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
        .route("/api/dashboard", get(dashboard::dashboard))
        .route("/api/admin/reindex", post(reindex))
        .route("/api/admin/reprocess-failed", post(reprocess_failed))
        .route("/api/admin/backup", get(backup::backup))
        .route("/api/admin/restore", post(backup::restore).layer(DefaultBodyLimit::disable()))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/logout-all", post(auth::logout_all))
        .route("/api/auth/file-key", get(auth::file_key))
        .route("/api/admin/backup-link", post(auth::backup_link))
        .route("/api/mcp/tokens", get(mcp::list_tokens).post(mcp::create_token))
        .route("/api/mcp/tokens/{id}", axum::routing::patch(mcp::update_token).delete(mcp::delete_token))
        .route_layer(middleware::from_fn_with_state(state.clone(), crate::auth::require_auth));

    let public = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/login", post(auth::login))
        .route("/api/desktop", get(desktop::list))
        .route("/api/desktop/update", get(desktop::update))
        // KI-Zugang, geprüft über eigene Zugangsschlüssel (api_tokens)
        .route("/mcp", get(mcp::get_mcp).post(mcp::post_mcp))
        .route("/mcp/upload", post(mcp::upload_multipart))
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
        .layer(middleware::from_fn_with_state(state.clone(), crate::security::security_headers))
        .layer(cors())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Fremde Seiten dürfen die API nicht aus dem Browser heraus aufrufen – nur der
/// Desktop-Client (Tauri), dessen Oberfläche von einem eigenen Ursprung kommt.
fn cors() -> CorsLayer {
    use axum::http::HeaderValue;
    use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin};
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(crate::security::DESKTOP_ORIGINS.map(HeaderValue::from_static)))
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .max_age(std::time::Duration::from_secs(3600))
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
