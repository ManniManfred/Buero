// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Einstellungen für den Mail-Abruf (IMAP) und E-Mail-Angaben zu Dokumenten.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use shared::{EmailInfo, MailAccount, MailAccountInput, MailFolder, ServerFolder};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn bad(e: anyhow::Error) -> AppError {
    AppError::bad_request(format!("{e:#}"))
}

pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<MailAccount>>> {
    type Row = (i64, String, String, i64, String, bool, bool, i64, Option<i64>, Option<i64>, Option<String>, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, name, host, port, username, password <> '', enabled <> 0, interval_min, collection_id, \
         last_run, last_error, last_imported FROM mail_accounts ORDER BY id",
    )
    .fetch_all(&st.pool)
    .await?;
    let folders: Vec<(i64, String, String, Option<i64>)> =
        sqlx::query_as("SELECT account_id, name, mode, collection_id FROM mail_folders ORDER BY id")
            .fetch_all(&st.pool)
            .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, name, host, port, username, has_password, enabled, interval_min, collection_id, last_run, last_error, last_imported)| {
                    MailAccount {
                        id,
                        name,
                        host,
                        port,
                        username,
                        has_password,
                        enabled,
                        interval_min,
                        collection_id,
                        last_run,
                        last_error,
                        last_imported,
                        running: crate::mail::is_running(id),
                        folders: folders
                            .iter()
                            .filter(|f| f.0 == id)
                            .map(|f| MailFolder { name: f.1.clone(), mode: f.2.clone(), collection_id: f.3 })
                            .collect(),
                    }
                },
            )
            .collect(),
    ))
}

pub async fn create(State(st): State<AppState>, Json(i): Json<MailAccountInput>) -> AppResult<Json<i64>> {
    let host = i.host.as_deref().map(str::trim).filter(|h| !h.is_empty()).ok_or_else(|| AppError::bad_request("Server fehlt"))?;
    let user = i.username.as_deref().map(str::trim).filter(|h| !h.is_empty()).ok_or_else(|| AppError::bad_request("Benutzername fehlt"))?;
    let name = i.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(user);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO mail_accounts (name, host, port, username, password, interval_min, collection_id, enabled) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(name)
    .bind(host)
    .bind(i.port.unwrap_or(993))
    .bind(user)
    .bind(i.password.unwrap_or_default())
    .bind(i.interval_min.unwrap_or(15).clamp(5, 24 * 60))
    .bind(i.collection_id.filter(|c| *c > 0))
    .bind(i.enabled.unwrap_or(true))
    .fetch_one(&st.pool)
    .await?;
    if let Some(f) = i.folders {
        set_folders(&st, id, f).await?;
    }
    Ok(Json(id))
}

pub async fn update(State(st): State<AppState>, Path(id): Path<i64>, Json(i): Json<MailAccountInput>) -> AppResult<StatusCode> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM mail_accounts WHERE id = ?").bind(id).fetch_optional(&st.pool).await?;
    if exists.is_none() {
        return Err(AppError::not_found());
    }
    let set = |col: &'static str| format!("UPDATE mail_accounts SET {col} = ? WHERE id = ?");
    if let Some(v) = i.name.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        sqlx::query(&set("name")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.host.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        sqlx::query(&set("host")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.port {
        sqlx::query(&set("port")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.username.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        sqlx::query(&set("username")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.password.filter(|v| !v.is_empty()) {
        sqlx::query(&set("password")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.enabled {
        sqlx::query(&set("enabled")).bind(v).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.interval_min {
        sqlx::query(&set("interval_min")).bind(v.clamp(5, 24 * 60)).bind(id).execute(&st.pool).await?;
    }
    if let Some(v) = i.collection_id {
        sqlx::query(&set("collection_id")).bind(Some(v).filter(|c| *c > 0)).bind(id).execute(&st.pool).await?;
    }
    if let Some(f) = i.folders {
        set_folders(&st, id, f).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Ordnerliste ersetzen; Abrufstand bleibender Ordner bleibt erhalten
async fn set_folders(st: &AppState, account: i64, folders: Vec<MailFolder>) -> AppResult<()> {
    let names: Vec<String> = folders.iter().map(|f| f.name.clone()).collect();
    let existing: Vec<String> = sqlx::query_scalar("SELECT name FROM mail_folders WHERE account_id = ?")
        .bind(account)
        .fetch_all(&st.pool)
        .await?;
    for e in existing.iter().filter(|e| !names.contains(e)) {
        sqlx::query("DELETE FROM mail_folders WHERE account_id = ? AND name = ?").bind(account).bind(e).execute(&st.pool).await?;
    }
    for f in folders {
        let mode = if f.mode == "replies" { "replies" } else { "all" };
        sqlx::query(
            "INSERT INTO mail_folders (account_id, name, mode, collection_id) VALUES (?, ?, ?, ?) \
             ON CONFLICT (account_id, name) DO UPDATE SET mode = excluded.mode, collection_id = excluded.collection_id",
        )
        .bind(account)
        .bind(&f.name)
        .bind(mode)
        .bind(f.collection_id.filter(|c| *c > 0))
        .execute(&st.pool)
        .await?;
    }
    Ok(())
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let n = sqlx::query("DELETE FROM mail_accounts WHERE id = ?").bind(id).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Verbindung prüfen und die Ordner des Servers auflisten
pub async fn folders(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Vec<ServerFolder>>> {
    let a = crate::mail::load_account(&st, id).await.map_err(bad)?.ok_or_else(AppError::not_found)?;
    Ok(Json(crate::mail::list_folders(&a).await.map_err(bad)?))
}

/// Jetzt abrufen (läuft im Hintergrund weiter)
pub async fn run(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    if crate::mail::is_running(id) {
        return Err(AppError::new(StatusCode::CONFLICT, "Abruf läuft bereits"));
    }
    tokio::spawn(async move {
        if let Err(e) = crate::mail::run(&st, id).await {
            tracing::warn!("Mail-Abruf Postfach {id}: {e:#}");
        }
    });
    Ok(StatusCode::ACCEPTED)
}

pub async fn email_info(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<EmailInfo>> {
    Ok(Json(crate::email::info(&st, id).await?))
}
