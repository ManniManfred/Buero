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
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl AppState {
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
