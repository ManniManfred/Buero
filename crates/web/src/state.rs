// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{Collection, DocumentSummary, Features, ListResponse, Stats, Tag};

use crate::api::Api;

#[derive(Clone, Debug, PartialEq)]
pub struct UploadItem {
    pub key: u32,
    pub name: String,
    /// waiting | uploading | done | duplicate | error
    pub status: String,
    pub message: String,
    pub doc_id: Option<i64>,
}

/// Ziel des Freigabe-Dialogs. Ohne Dokument und Sammlung: alle Freigaben verwalten.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShareTarget {
    pub document_id: Option<i64>,
    pub collection_id: Option<i64>,
    pub name: String,
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub api: Api,
    pub collections: RwSignal<Vec<Collection>>,
    pub tags: RwSignal<Vec<Tag>>,
    pub stats: RwSignal<Stats>,

    pub query: RwSignal<String>,
    pub collection: RwSignal<Option<i64>>,
    pub tag_filter: RwSignal<Vec<i64>>,
    pub kind: RwSignal<String>,
    pub unfiled: RwSignal<bool>,
    pub sort: RwSignal<String>,

    pub results: RwSignal<Vec<DocumentSummary>>,
    pub total: RwSignal<u64>,
    pub took_ms: RwSignal<f64>,
    pub fuzzy: RwSignal<bool>,
    pub loading: RwSignal<bool>,
    pub request_seq: RwSignal<u32>,

    pub selected: RwSignal<Option<i64>>,
    pub checked: RwSignal<HashSet<i64>>,
    pub uploads: RwSignal<Vec<UploadItem>>,
    pub toast: RwSignal<Option<(String, bool)>>,
    pub show_settings: RwSignal<bool>,
    /// Zähler, der ein Neuladen der Liste auslöst
    pub refresh: RwSignal<u32>,
    /// Zähler, der ein Neuladen der Detailansicht auslöst
    pub detail_refresh: RwSignal<u32>,
    pub features: RwSignal<Features>,
    pub share_target: RwSignal<Option<ShareTarget>>,
    /// Dokument, das gerade in Euro-Office geöffnet ist
    pub office_doc: RwSignal<Option<i64>>,
    /// Dokument, das gerade im Tupfer Designer geöffnet ist
    pub tupfer_doc: RwSignal<Option<i64>>,
}

pub const PAGE: usize = 50;

impl Ctx {
    pub fn new() -> Self {
        Ctx {
            api: Api::new(),
            collections: RwSignal::new(Vec::new()),
            tags: RwSignal::new(Vec::new()),
            stats: RwSignal::new(Stats::default()),
            query: RwSignal::new(String::new()),
            collection: RwSignal::new(None),
            tag_filter: RwSignal::new(Vec::new()),
            kind: RwSignal::new(String::new()),
            unfiled: RwSignal::new(false),
            sort: RwSignal::new(String::new()),
            results: RwSignal::new(Vec::new()),
            total: RwSignal::new(0),
            took_ms: RwSignal::new(0.0),
            fuzzy: RwSignal::new(false),
            loading: RwSignal::new(false),
            request_seq: RwSignal::new(0),
            selected: RwSignal::new(None),
            checked: RwSignal::new(HashSet::new()),
            uploads: RwSignal::new(Vec::new()),
            toast: RwSignal::new(None),
            show_settings: RwSignal::new(false),
            refresh: RwSignal::new(0),
            detail_refresh: RwSignal::new(0),
            features: RwSignal::new(Features::default()),
            share_target: RwSignal::new(None),
            office_doc: RwSignal::new(None),
            tupfer_doc: RwSignal::new(None),
        }
    }

    pub fn notify(&self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        self.toast.set(Some((msg.clone(), error)));
        let toast = self.toast;
        set_timeout(
            move || {
                if toast.get_untracked().map(|(m, _)| m == msg).unwrap_or(false) {
                    toast.set(None);
                }
            },
            std::time::Duration::from_millis(if error { 6000 } else { 3000 }),
        );
    }

    pub fn reload(&self) {
        self.refresh.update(|r| *r += 1);
    }

    pub fn load_meta(&self) {
        let c = *self;
        spawn_local(async move {
            if let Ok(v) = c.api.get::<Vec<Collection>>("/api/collections").await {
                c.collections.set(v);
            }
            if let Ok(v) = c.api.get::<Vec<Tag>>("/api/tags").await {
                c.tags.set(v);
            }
            if let Ok(v) = c.api.get::<Stats>("/api/stats").await {
                c.stats.set(v);
            }
            if let Ok(v) = c.api.get::<Features>("/api/features").await {
                if v != c.features.get_untracked() {
                    c.features.set(v);
                }
            }
        });
    }

    fn query_string(&self, offset: usize) -> String {
        let mut parts = vec![format!("limit={PAGE}"), format!("offset={offset}")];
        let enc = |s: &str| String::from(js_sys::encode_uri_component(s));
        let q = self.query.get_untracked();
        if !q.trim().is_empty() {
            parts.push(format!("q={}", enc(&q)));
        }
        if let Some(c) = self.collection.get_untracked() {
            parts.push(format!("collections={c}"));
        }
        let tags = self.tag_filter.get_untracked();
        if !tags.is_empty() {
            parts.push(format!(
                "tags={}",
                tags.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(",")
            ));
        }
        let k = self.kind.get_untracked();
        if !k.is_empty() {
            parts.push(format!("kind={k}"));
        }
        if self.unfiled.get_untracked() {
            parts.push("unfiled=true".into());
        }
        let s = self.sort.get_untracked();
        if !s.is_empty() {
            parts.push(format!("sort={s}"));
        }
        parts.join("&")
    }

    /// Suche ausführen. `append`: nächste Seite anhängen.
    pub fn search(&self, append: bool) {
        let c = *self;
        let offset = if append { c.results.get_untracked().len() } else { 0 };
        let qs = c.query_string(offset);
        c.request_seq.update(|s| *s += 1);
        let seq = c.request_seq.get_untracked();
        c.loading.set(true);
        spawn_local(async move {
            let res = c.api.get::<ListResponse>(&format!("/api/documents?{qs}")).await;
            // Veraltete Antworten (schnelles Tippen) verwerfen
            if c.request_seq.get_untracked() != seq {
                return;
            }
            c.loading.set(false);
            match res {
                Ok(r) => {
                    c.total.set(r.total);
                    c.took_ms.set(r.took_ms);
                    c.fuzzy.set(r.fuzzy);
                    if append {
                        c.results.update(|v| v.extend(r.items));
                    } else {
                        c.results.set(r.items);
                    }
                }
                Err(e) => c.notify(e, true),
            }
        });
    }

    pub fn collection_name(&self, id: i64) -> String {
        self.collections.with_untracked(|cs| collection_path(cs, id))
    }
}

/// "Eltern / Kind / Enkel"
pub fn collection_path(cs: &[Collection], id: i64) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(id);
    let mut guard = 0;
    while let Some(c) = cur {
        let Some(col) = cs.iter().find(|x| x.id == c) else { break };
        parts.push(col.name.clone());
        cur = col.parent_id;
        guard += 1;
        if guard > 32 {
            break;
        }
    }
    parts.reverse();
    parts.join(" / ")
}

/// Öffentlicher Link einer Freigabe
pub fn share_url(c: &Ctx, token: &str) -> String {
    let base = c.api.base.get_untracked();
    let base = if base.is_empty() { window().location().origin().unwrap_or_default() } else { base };
    format!("{base}/s/{token}")
}

pub fn ctx() -> Ctx {
    expect_context::<Ctx>()
}
