use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use shared::{Collection, CollectionInput};

use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util::now;

pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Collection>>> {
    let rows: Vec<(i64, Option<i64>, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT c.id, c.parent_id, c.name, c.color, \
         (SELECT COUNT(*) FROM document_collections dc WHERE dc.collection_id = c.id) \
         FROM collections c ORDER BY c.name COLLATE NOCASE",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, parent_id, name, color, document_count)| Collection {
                id,
                parent_id,
                name,
                color,
                document_count,
            })
            .collect(),
    ))
}

fn map_unique(e: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(d) = &e {
        if d.is_unique_violation() {
            return AppError::new(StatusCode::CONFLICT, "Eine Sammlung mit diesem Namen existiert hier bereits");
        }
    }
    e.into()
}

async fn get_one(st: &AppState, id: i64) -> AppResult<Collection> {
    list(State(st.clone()))
        .await?
        .0
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(AppError::not_found)
}

pub async fn create(State(st): State<AppState>, Json(i): Json<CollectionInput>) -> AppResult<Json<Collection>> {
    let name = i.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
        .ok_or_else(|| AppError::bad_request("Name fehlt"))?;
    let parent = i.parent_id.filter(|p| *p > 0);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO collections (parent_id, name, color, created_at) VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(parent)
    .bind(name)
    .bind(i.color.filter(|c| !c.is_empty()))
    .bind(now())
    .fetch_one(&st.pool)
    .await
    .map_err(map_unique)?;
    Ok(Json(get_one(&st, id).await?))
}

pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(i): Json<CollectionInput>,
) -> AppResult<Json<Collection>> {
    let tree = db::collection_tree(&st.pool).await?;
    if !tree.contains_key(&id) {
        return Err(AppError::not_found());
    }
    let subtree = db::descendants(&tree, id);
    if let Some(name) = i.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE collections SET name = ? WHERE id = ?")
            .bind(name).bind(id).execute(&st.pool).await.map_err(map_unique)?;
    }
    if let Some(p) = i.parent_id {
        let parent = if p > 0 { Some(p) } else { None };
        if let Some(p) = parent {
            if subtree.contains(&p) {
                return Err(AppError::bad_request("Eine Sammlung kann nicht in sich selbst verschoben werden"));
            }
            if !tree.contains_key(&p) {
                return Err(AppError::bad_request("Übergeordnete Sammlung existiert nicht"));
            }
        }
        sqlx::query("UPDATE collections SET parent_id = ? WHERE id = ?")
            .bind(parent).bind(id).execute(&st.pool).await.map_err(map_unique)?;
    }
    if let Some(c) = i.color {
        sqlx::query("UPDATE collections SET color = ? WHERE id = ?")
            .bind(Some(c).filter(|c| !c.is_empty())).bind(id).execute(&st.pool).await?;
    }
    // Namen und Vorfahren stecken im Index -> betroffene Dokumente neu indexieren
    let docs = db::docs_in_collections(&st.pool, &subtree).await?;
    st.reindex(&docs).await?;
    Ok(Json(get_one(&st, id).await?))
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let tree = db::collection_tree(&st.pool).await?;
    let subtree = db::descendants(&tree, id);
    let docs = db::docs_in_collections(&st.pool, &subtree).await?;
    let n = sqlx::query("DELETE FROM collections WHERE id = ?").bind(id).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    st.reindex(&docs).await?;
    Ok(StatusCode::NO_CONTENT)
}
