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
mod collections;
mod documents;
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
        .route("/api/stats", get(stats))
        .route("/api/admin/reindex", post(reindex))
        .route("/api/admin/reprocess-failed", post(reprocess_failed))
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(state.clone(), crate::auth::require_auth));

    let public = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/login", post(auth::login));

    let web = &state.config.web_dir;
    let spa = ServeDir::new(web)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(ServeFile::new(web.join("index.html")));

    Router::new()
        .merge(public)
        .merge(protected)
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
