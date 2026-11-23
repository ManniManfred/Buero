// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Verknüpfungen zwischen Dokumenten: Unterdokumente (genau ein
//! übergeordnetes Dokument, z. B. Mail-Anhänge) und gegenseitige Verweise.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use shared::{DocumentLinks, LinkInput};

use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util::now;

pub async fn get(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<DocumentLinks>> {
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    let parent = match doc.parent_id {
        Some(p) => db::summary(&st.pool, p).await?,
        None => None,
    };
    let child_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM documents WHERE parent_id = ? ORDER BY IFNULL(document_date, ''), title COLLATE NOCASE",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    let related_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT CASE WHEN a_id = ? THEN b_id ELSE a_id END FROM document_links WHERE a_id = ? OR b_id = ? ORDER BY created_at",
    )
    .bind(id)
    .bind(id)
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(DocumentLinks {
        parent,
        children: db::summaries(&st.pool, &child_ids).await?,
        related: db::summaries(&st.pool, &related_ids).await?,
    }))
}

/// `child` unter `parent` hängen (mit Prüfung auf Kreise)
pub async fn set_parent(st: &AppState, child: i64, parent: i64) -> AppResult<()> {
    if child == parent {
        return Err(AppError::bad_request("Ein Dokument kann nicht unter sich selbst hängen"));
    }
    // Kreis: parent darf kein Unterdokument von child sein
    let mut cur = Some(parent);
    let mut depth = 0;
    while let Some(c) = cur {
        if c == child {
            return Err(AppError::bad_request("Das würde einen Kreis ergeben (das Dokument hängt bereits darunter)"));
        }
        cur = sqlx::query_scalar::<_, Option<i64>>("SELECT parent_id FROM documents WHERE id = ?")
            .bind(c)
            .fetch_optional(&st.pool)
            .await?
            .flatten();
        depth += 1;
        if depth > 100 {
            break;
        }
    }
    sqlx::query("UPDATE documents SET parent_id = ? WHERE id = ?").bind(parent).bind(child).execute(&st.pool).await?;
    // Ein Verweis zwischen beiden ist damit überflüssig
    sqlx::query("DELETE FROM document_links WHERE a_id = ? AND b_id = ?")
        .bind(child.min(parent))
        .bind(child.max(parent))
        .execute(&st.pool)
        .await?;
    Ok(())
}

pub async fn relate(st: &AppState, a: i64, b: i64) -> AppResult<()> {
    if a == b {
        return Err(AppError::bad_request("Ein Dokument kann nicht auf sich selbst verweisen"));
    }
    sqlx::query("INSERT OR IGNORE INTO document_links (a_id, b_id, created_at) VALUES (?, ?, ?)")
        .bind(a.min(b))
        .bind(a.max(b))
        .bind(now())
        .execute(&st.pool)
        .await?;
    Ok(())
}

pub async fn add(State(st): State<AppState>, Path(id): Path<i64>, Json(i): Json<LinkInput>) -> AppResult<StatusCode> {
    for d in [id, i.target] {
        db::get_doc(&st.pool, d).await?.ok_or_else(AppError::not_found)?;
    }
    match i.kind.as_str() {
        "child" => set_parent(&st, i.target, id).await?,
        "parent" => set_parent(&st, id, i.target).await?,
        "related" => relate(&st, id, i.target).await?,
        _ => return Err(AppError::bad_request("Unbekannte Art der Verknüpfung")),
    }
    st.reindex(&[id, i.target]).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Verknüpfung zwischen zwei Dokumenten lösen (Unterdokument oder Verweis)
pub async fn remove(State(st): State<AppState>, Path((id, target)): Path<(i64, i64)>) -> AppResult<StatusCode> {
    sqlx::query("UPDATE documents SET parent_id = NULL WHERE (id = ? AND parent_id = ?) OR (id = ? AND parent_id = ?)")
        .bind(id)
        .bind(target)
        .bind(target)
        .bind(id)
        .execute(&st.pool)
        .await?;
    sqlx::query("DELETE FROM document_links WHERE a_id = ? AND b_id = ?")
        .bind(id.min(target))
        .bind(id.max(target))
        .execute(&st.pool)
        .await?;
    st.reindex(&[id, target]).await?;
    Ok(StatusCode::NO_CONTENT)
}
