// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::search::SearchIndex;
use crate::storage::Storage;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: Config,
    pub pool: SqlitePool,
    pub storage: Storage,
    pub search: SearchIndex,
    pub jobs: mpsc::UnboundedSender<i64>,
    /// Server-Schlüssel für signierte Links (`secret.key` im Datenverzeichnis)
    pub secret: Vec<u8>,
    pub http: reqwest::Client,
    /// Wird ausgelöst, um den Server im selben Prozess neu zu starten (nach einer Wiederherstellung)
    pub restart: tokio_util::sync::CancellationToken,
    /// Eine Wiederherstellung ist vorbereitet oder läuft
    pub restoring: std::sync::atomic::AtomicBool,
    /// Sitzungs-Epoche (ändert sich bei „Überall abmelden“ und Passwortwechsel)
    pub epoch: std::sync::RwLock<String>,
    /// Content-Security-Policy der Oberfläche
    pub csp: axum::http::HeaderValue,
    /// Bremse für falsche Anmelde-Passwörter
    pub login_limit: crate::security::Limiter,
    /// Bremse für falsche Passwörter von Freigabelinks
    pub share_limit: crate::security::Limiter,
    /// Uploads pro Freigabelink und Stunde
    pub share_uploads: crate::security::UploadCounter,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl AppState {
    /// Öffentliche Basis-Adresse von Büro: `BUERO_PUBLIC_URL` oder aus den
    /// (Proxy-)Headern der Anfrage.
    pub fn public_base(&self, headers: &axum::http::HeaderMap) -> String {
        if let Some(u) = &self.config.public_url {
            return u.clone();
        }
        let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim().to_string());
        let host = h("x-forwarded-host").or_else(|| h("host")).unwrap_or_else(|| "localhost".into());
        let proto = h("x-forwarded-proto").unwrap_or_else(|| "http".into());
        format!("{proto}://{host}")
    }

    /// Indexeinträge der Dokumente neu schreiben und sofort sichtbar machen.
    pub async fn reindex(&self, ids: &[i64]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let docs = crate::db::index_docs(&self.pool, ids).await?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            this.search.upsert(&docs)?;
            this.search.commit()
        })
        .await??;
        Ok(())
    }

    pub async fn reindex_all(&self) -> anyhow::Result<usize> {
        let ids = crate::db::all_doc_ids(&self.pool).await?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.search.clear()).await??;
        for chunk in ids.chunks(500) {
            let docs = crate::db::index_docs(&self.pool, chunk).await?;
            let this = self.clone();
            tokio::task::spawn_blocking(move || this.search.upsert(&docs)).await??;
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.search.commit()).await??;
        Ok(ids.len())
    }
}
