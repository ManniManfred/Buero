// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Einfache Einzelbenutzer-Anmeldung: Passwort aus `BUERO_PASSWORD`,
//! danach Bearer-Token im `Authorization`-Header. Dateien, die der Browser
//! direkt lädt (Vorschau, Vorschaubilder, Download), nutzen stattdessen
//! kurzlebige signierte Schlüssel (siehe `security`), damit das Sitzungs-Token
//! nie in einer URL – und damit in Protokollen oder im Verlauf – landet.

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::AppState;
use crate::util::now;

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let ha = Sha256::digest(a);
    let hb = Sha256::digest(b);
    ha.iter().zip(hb.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn token_from_request(req: &Request) -> Option<String> {
    let v = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

pub async fn is_valid(state: &AppState, token: &str) -> bool {
    if state.config.password.is_none() {
        return true;
    }
    let res: Option<i64> =
        sqlx::query_scalar("SELECT expires_at FROM sessions WHERE token_hash = ?")
            .bind(hash_token(token))
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    matches!(res, Some(exp) if exp > now())
}

pub async fn require_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if state.config.password.is_none() {
        state.remember_base(req.headers());
        return Ok(next.run(req).await);
    }
    match token_from_request(&req) {
        Some(t) if is_valid(&state, &t).await => {
            state.remember_base(req.headers());
            Ok(next.run(req).await)
        }
        _ if crate::security::signed_access(&state, &req) => Ok(next.run(req).await),
        _ => Err(AppError::unauthorized()),
    }
}
