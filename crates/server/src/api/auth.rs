// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::Json;
use rand::RngCore;

use shared::{AuthStatus, LoginRequest, LoginResponse};

use crate::auth::{constant_time_eq, hash_token, is_valid, token_from_request};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util::now;

pub async fn status(State(st): State<AppState>, req: Request) -> Json<AuthStatus> {
    let required = st.config.password.is_some();
    let authenticated = match token_from_request(&req) {
        Some(t) => is_valid(&st, &t).await,
        None => !required,
    };
    Json(AuthStatus { auth_required: required, authenticated })
}

pub async fn login(State(st): State<AppState>, Json(r): Json<LoginRequest>) -> AppResult<Json<LoginResponse>> {
    let Some(pw) = &st.config.password else {
        return Ok(Json(LoginResponse { token: String::new() }));
    };
    if !constant_time_eq(pw.as_bytes(), r.password.as_bytes()) {
        // Brute-Force bremsen
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        return Err(AppError::new(StatusCode::UNAUTHORIZED, "Falsches Passwort"));
    }
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let ts = now();
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(ts).execute(&st.pool).await?;
    sqlx::query("INSERT INTO sessions (token_hash, created_at, expires_at) VALUES (?, ?, ?)")
        .bind(hash_token(&token))
        .bind(ts)
        .bind(ts + st.config.session_days * 86400)
        .execute(&st.pool)
        .await?;
    Ok(Json(LoginResponse { token }))
}

pub async fn logout(State(st): State<AppState>, req: Request) -> AppResult<StatusCode> {
    if let Some(t) = token_from_request(&req) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(hash_token(&t)).execute(&st.pool).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
