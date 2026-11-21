// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Dokumentenverwaltung: HTTP-Server

mod api;
mod auth;
mod config;
mod db;
mod error;
mod extract;
mod search;
mod sign;
mod state;
mod storage;
mod util;
mod worker;

use std::sync::Arc;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tantivy=warn,sqlx=warn")),
        )
        .init();

    let config = config::Config::from_env();
    tokio::fs::create_dir_all(&config.data_dir).await?;
    if config.password.is_none() {
        tracing::warn!("BUERO_PASSWORD ist nicht gesetzt – die Anwendung ist OHNE Anmeldung erreichbar!");
    }

    let pool = db::connect(&config.data_dir).await?;
    let storage = storage::Storage::new(&config.data_dir).await?;
    let (search, fresh_index) = search::SearchIndex::open(&config.data_dir.join("index"))?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let secret = sign::load_or_create_secret(&config.data_dir).await?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("buero/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()?;
    match (&config.office_url, &config.office_jwt_secret) {
        (Some(u), Some(_)) => tracing::info!("Euro-Office angebunden: {u}"),
        (Some(u), None) => tracing::warn!("Euro-Office ({u}) ohne BUERO_OFFICE_JWT_SECRET – nur sinnvoll, wenn JWT dort abgeschaltet ist"),
        _ => {}
    }

    let state = state::AppState {
        inner: Arc::new(state::Inner { config: config.clone(), pool, storage, search, jobs: tx, secret, http }),
    };

    if fresh_index {
        let n = state.reindex_all().await?;
        tracing::info!("Suchindex neu aufgebaut ({n} Dokumente)");
    }
    worker::spawn(state.clone(), rx);
    worker::requeue_pending(&state).await?;

    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("Büro-Server läuft auf http://{}", config.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
