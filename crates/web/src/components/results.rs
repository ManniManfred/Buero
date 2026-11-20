// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use leptos::prelude::*;
use shared::DocumentSummary;

use crate::components::sidebar::DRAG_MIME;
use crate::state::ctx;
use crate::util::{format_date, format_size, format_ts, kind_icon, status_label};

#[component]
pub fn Results() -> impl IntoView {
    let c = ctx();
    let all_checked = move || {
        let n = c.results.with(|r| r.len());
        n > 0 && c.checked.with(|s| s.len()) >= n
    };
    view! {
        <div class="results">
            <Show when=move || !c.results.with(|r| r.is_empty())
                fallback=move || view! {
                    <div class="empty">
                        {move || if c.loading.get() { "Suche …".to_string() }
                            else if c.query.with(|q| q.trim().is_empty()) { "Keine Dokumente. Dateien einfach hierher ziehen oder „Hochladen“ klicken.".to_string() }
                            else { "Nichts gefunden.".to_string() }}
                    </div>
                }>
                <div class="result-head">
                    <input type="checkbox" title="Alle auswählen" prop:checked=all_checked
                        on:change=move |ev| {
                            if event_target_checked(&ev) {
                                c.checked.set(c.results.with_untracked(|r| r.iter().map(|d| d.id).collect()));
                            } else {
                                c.checked.set(Default::default());
                            }
                        } />
                </div>
                <For each=move || c.results.get() key=|d| (d.id, d.updated_at, d.extract_status.clone(), d.snippet.clone(), d.collection_ids.clone(), d.tag_ids.clone()) let:doc>
                    <ResultRow doc=doc />
                </For>
                <Show when=move || (c.results.with(|r| r.len()) as u64) < c.total.get()>
                    <button class="more" on:click=move |_| c.search(true)>
                        {move || format!("Weitere laden ({} von {})", c.results.with(|r| r.len()), c.total.get())}
                    </button>
                </Show>
            </Show>
        </div>
    }
}

#[component]
fn ResultRow(doc: DocumentSummary) -> impl IntoView {
    let c = ctx();
    let id = doc.id;
    let is_checked = move || c.checked.with(|s| s.contains(&id));
    let thumb = doc
        .has_thumbnail
        .then(|| c.api.file_url(&format!("/api/documents/{id}/thumbnail?v={}", doc.updated_at)));
    let date = doc
        .document_date
        .as_deref()
        .map(format_date)
        .unwrap_or_else(|| format_ts(doc.created_at));
    let status = status_label(&doc.extract_status);
    let kind = doc.kind.clone();
    let collections = doc.collection_ids.clone();
    let tags = doc.tag_ids.clone();
    let snippet = doc.snippet.clone();

    view! {
        <div class="row" class:selected=move || c.selected.get() == Some(id) class:checked=is_checked
            draggable="true"
            on:dragstart=move |ev| {
                // Mehrfachauswahl ziehen, sonst nur dieses Dokument
                let mut ids: Vec<i64> = c.checked.with_untracked(|s| s.iter().copied().collect());
                if !ids.contains(&id) { ids = vec![id]; }
                if let Some(dt) = ev.data_transfer() {
                    let s = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
                    let _ = dt.set_data(DRAG_MIME, &s);
                    dt.set_effect_allowed("copy");
                }
            }
            on:click=move |_| c.selected.set(Some(id))>
            <input type="checkbox" prop:checked=is_checked
                on:click=move |ev| ev.stop_propagation()
                on:change=move |ev| {
                    let on = event_target_checked(&ev);
                    c.checked.update(|s| { if on { s.insert(id); } else { s.remove(&id); } });
                } />
            <div class="thumb">
                {match thumb {
                    Some(src) => view! { <img src=src loading="lazy" alt="" /> }.into_any(),
                    None => view! { <span class="kind-icon">{kind_icon(&kind)}</span> }.into_any(),
                }}
            </div>
            <div class="row-body">
                <div class="row-title">
                    <span class="title">{doc.title.clone()}</span>
                    {(!status.is_empty()).then(|| view! { <span class="badge" class:bad=doc.extract_status == "failed">{status}</span> })}
                </div>
                {snippet.map(|s| view! { <div class="snippet" inner_html=s></div> })}
                <div class="meta">
                    <span>{date}</span>
                    <span>{format_size(doc.size)}</span>
                    {doc.page_count.filter(|p| *p > 1).map(|p| view! { <span>{format!("{p} Seiten")}</span> })}
                    <span class="ellipsis filename">{doc.original_filename.clone()}</span>
                    {(doc.version > 1).then(|| view! { <span>{format!("v{}", doc.version)}</span> })}
                </div>
                <div class="chips">
                    {collections.into_iter().map(|cid| view! {
                        <span class="chip coll" on:click=move |ev| { ev.stop_propagation(); c.unfiled.set(false); c.collection.set(Some(cid)); }>
                            "🗂 "{move || c.collection_name(cid)}
                        </span>
                    }).collect_view()}
                    {tags.into_iter().map(|tid| {
                        let tag = move || c.tags.with(|ts| ts.iter().find(|t| t.id == tid).cloned());
                        view! {
                            <span class="chip tag"
                                style=move || tag().and_then(|t| t.color).map(|col| format!("--tag: {col}")).unwrap_or_default()
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    c.tag_filter.update(|f| if !f.contains(&tid) { f.push(tid) });
                                }>"#"{move || tag().map(|t| t.name).unwrap_or_default()}</span>
                        }
                    }).collect_view()}
                </div>
            </div>
        </div>
    }
}
