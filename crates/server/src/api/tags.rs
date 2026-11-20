use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use shared::{Tag, TagInput};

use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Tag>>> {
    let rows: Vec<(i64, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT t.id, t.name, t.color, (SELECT COUNT(*) FROM document_tags dt WHERE dt.tag_id = t.id) \
         FROM tags t ORDER BY t.name COLLATE NOCASE",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, color, document_count)| Tag { id, name, color, document_count })
            .collect(),
    ))
}

async fn get_one(st: &AppState, id: i64) -> AppResult<Tag> {
    list(State(st.clone())).await?.0.into_iter().find(|t| t.id == id).ok_or_else(AppError::not_found)
}

/// Legt einen Tag an – existiert der Name schon, wird der vorhandene geliefert.
pub async fn create(State(st): State<AppState>, Json(i): Json<TagInput>) -> AppResult<Json<Tag>> {
    let name = i.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
        .ok_or_else(|| AppError::bad_request("Name fehlt"))?;
    sqlx::query("INSERT OR IGNORE INTO tags (name, color) VALUES (?, ?)")
        .bind(name)
        .bind(i.color.filter(|c| !c.is_empty()))
        .execute(&st.pool)
        .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM tags WHERE name = ? COLLATE NOCASE")
        .bind(name)
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(get_one(&st, id).await?))
}

pub async fn update(State(st): State<AppState>, Path(id): Path<i64>, Json(i): Json<TagInput>) -> AppResult<Json<Tag>> {
    if let Some(name) = i.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE tags SET name = ? WHERE id = ?").bind(name).bind(id).execute(&st.pool).await
            .map_err(|e| match &e {
                sqlx::Error::Database(d) if d.is_unique_violation() =>
                    AppError::new(StatusCode::CONFLICT, "Tag existiert bereits"),
                _ => e.into(),
            })?;
    }
    if let Some(c) = i.color {
        sqlx::query("UPDATE tags SET color = ? WHERE id = ?")
            .bind(Some(c).filter(|c| !c.is_empty())).bind(id).execute(&st.pool).await?;
    }
    let docs = db::docs_with_tag(&st.pool, id).await?;
    st.reindex(&docs).await?;
    Ok(Json(get_one(&st, id).await?))
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let docs = db::docs_with_tag(&st.pool, id).await?;
    let n = sqlx::query("DELETE FROM tags WHERE id = ?").bind(id).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    st.reindex(&docs).await?;
    Ok(StatusCode::NO_CONTENT)
}
