// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Dokumentenverwaltung: HTTP-Server

mod api;
mod autotitle;
mod auth;
mod config;
mod db;
mod email;
mod error;
mod extract;
mod mail;
mod preview;
mod search;
mod security;
mod sign;
mod state;
mod storage;
mod templates;
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

    loop {
        if api::backup::apply_pending(&config.data_dir).await? {
            tracing::info!(
                "Wiederherstellung übernommen – bisheriger Stand liegt in {}/",
                api::backup::PREVIOUS_DIR
            );
        }
        if !run(&config).await? {
            return Ok(());
        }
        tracing::info!("Server startet für die Wiederherstellung neu …");
    }
}

/// Startet den Server; `true`, wenn er für eine Wiederherstellung neu starten soll.
async fn run(config: &config::Config) -> anyhow::Result<bool> {
    let pool = db::connect(&config.data_dir).await?;
    let storage = storage::Storage::new(&config.data_dir).await?;
    let (search, fresh_index) = search::SearchIndex::open(&config.data_dir.join("index"))?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let secret = sign::load_or_create_secret(&config.data_dir).await?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("buero/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    match (&config.office_url, &config.office_jwt_secret) {
        (Some(u), Some(_)) => tracing::info!("Euro-Office angebunden: {u}"),
        (Some(u), None) => tracing::warn!("Euro-Office ({u}) ohne BUERO_OFFICE_JWT_SECRET – nur sinnvoll, wenn JWT dort abgeschaltet ist"),
        _ => {}
    }

    let epoch = security::init_auth(&config.data_dir, config.password.as_deref(), &pool).await?;
    let csp = security::build_csp(&config.web_dir, config.office_url.as_deref());
    let restart = tokio_util::sync::CancellationToken::new();
    let state = state::AppState {
        inner: Arc::new(state::Inner {
            config: config.clone(),
            pool,
            storage,
            search,
            jobs: tx,
            secret,
            http,
            restart: restart.clone(),
            restoring: Default::default(),
            epoch: std::sync::RwLock::new(epoch),
            csp,
            login_limit: security::Limiter::new("Anmeldung"),
            share_limit: security::Limiter::new("Freigabe-Passwort"),
            share_uploads: Default::default(),
            seen_base: Default::default(),
        }),
    };

    if fresh_index {
        let n = state.reindex_all().await?;
        tracing::info!("Suchindex neu aufgebaut ({n} Dokumente)");
    }
    let worker = worker::spawn(state.clone(), rx);
    mail::spawn(state.clone());
    worker::requeue_pending(&state).await?;

    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("Büro-Server läuft auf http://{}", config.bind);
    let stop = restart.clone();
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = stop.cancelled() => {}
            }
        })
        .await?;

    // Laufende Verarbeitungen abwarten, danach Datenbank und Index freigeben
    let restarting = restart.is_cancelled() && state.restoring.load(std::sync::atomic::Ordering::SeqCst);
    restart.cancel();
    let _ = worker.await;
    state.pool.close().await;
    Ok(restarting)
}
