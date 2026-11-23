// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::BulkUpdate;

use crate::components::upload::{pick_files, start_uploads};
use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};

#[component]
pub fn Toolbar() -> impl IntoView {
    view! {
        // Im Desktop-Client steht die Suchzeile in der Titelleiste
        <Show when=|| !crate::api::is_desktop()>
            <div class="toolbar"><SearchRow /></div>
        </Show>
        <FilterBar />
        <BulkBar />
    }
}

/// Suche, Sortierung und Hochladen
#[component]
pub fn SearchRow() -> impl IntoView {
    let c = ctx();
    let file_input = NodeRef::<leptos::html::Input>::new();

    view! {
            <button class="icon mobile-only" title="Übersicht" aria-label="Übersicht" on:click=move |_| c.dashboard.set(true)>
                <Icon name="dashboard" />
            </button>
            <div class="search">
                <Icon name="search" class="search-icon" />
                <input id="search" type="search" autocomplete="off" spellcheck="false"
                    placeholder="Dokumente durchsuchen …  (Taste /)"
                    prop:value=move || c.query.get()
                    on:input=move |ev| c.query.set(event_target_value(&ev)) />
                <Show when=move || c.loading.get()><span class="spinner"></span></Show>
            </div>
            <select class="sort" on:change=move |ev| c.sort.set(event_target_value(&ev)) prop:value=move || c.sort.get()>
                <option value="">"Sortierung: automatisch"</option>
                <option value="relevance">"Relevanz"</option>
                <option value="newest">"Neueste zuerst"</option>
                <option value="oldest">"Älteste zuerst"</option>
                <option value="date_desc">"Dokumentdatum ↓"</option>
                <option value="date_asc">"Dokumentdatum ↑"</option>
            </select>
            <button title="Dokument mit der Kamera scannen" on:click=move |_| c.scan.set(true)>
                <Icon name="camera" /><span class="btn-text">"Scannen"</span>
            </button>
            <button class="primary" on:click=move |_| { if let Some(i) = file_input.get() { i.click(); } }>
                <Icon name="upload" /><span class="btn-text">"Hochladen"</span>
            </button>
            <input type="file" multiple style="display:none" node_ref=file_input
                on:change=move |_| {
                    if let Some(i) = file_input.get() {
                        let files = pick_files(i.files());
                        i.set_value("");
                        start_uploads(c, files);
                    }
                } />
    }
}

/// Aktive Filter als entfernbare Chips
#[component]
fn FilterBar() -> impl IntoView {
    let c = ctx();
    view! {
        <div class="filterbar">
            <span class="muted small">
                {move || {
                    let n = c.total.get();
                    let t = c.took_ms.get();
                    let mut s = format!("{n} Treffer · {t:.1} ms");
                    if c.fuzzy.get() { s.push_str(" · unscharfe Suche (keine exakten Treffer)"); }
                    s
                }}
            </span>
            {move || c.collection.get().map(|id| view! {
                <span class="chip removable" on:click=move |_| c.collection.set(None)>
                    <Icon name="folder" class="sm" />{c.collection_name(id)}<Icon name="x" class="sm" />
                </span>
            })}
            {move || {
                let id = c.collection.get()?;
                c.collections.with(|cs| cs.iter().any(|x| x.parent_id == Some(id))).then(|| view! {
                    <label class="small sub-toggle" title="Auch Dokumente aus den Untersammlungen anzeigen">
                        <input type="checkbox" prop:checked=move || c.include_sub.get()
                            on:change=move |ev| c.include_sub.set(event_target_checked(&ev)) />
                        "inkl. Untersammlungen"
                    </label>
                })
            }}
            {move || c.unfiled.get().then(|| view! {
                <span class="chip removable" on:click=move |_| c.unfiled.set(false)><Icon name="inbox" class="sm" />"Eingang"<Icon name="x" class="sm" /></span>
            })}
            {move || {
                let k = c.kind.get();
                (!k.is_empty()).then(|| view! {
                    <span class="chip removable" on:click=move |_| c.kind.set(String::new())>
                        <KindIcon kind=k.clone() class="sm" />{shared::kind_label(&k)}<Icon name="x" class="sm" />
                    </span>
                })
            }}
            {move || c.tag_filter.get().into_iter().map(|id| {
                let name = c.tags.with(|ts| ts.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_default());
                view! {
                    <span class="chip removable" on:click=move |_| c.tag_filter.update(|t| t.retain(|x| *x != id))>
                        <Icon name="tag" class="sm" />{name}<Icon name="x" class="sm" />
                    </span>
                }
            }).collect_view()}
        </div>
    }
}

fn bulk(c: Ctx, body: BulkUpdate, msg: &'static str) {
    spawn_local(async move {
        match c.api.send_empty("POST", "/api/documents/bulk", Some(&body)).await {
            Ok(()) => {
                c.notify(msg, false);
                c.checked.set(Default::default());
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
}

#[component]
fn BulkBar() -> impl IntoView {
    let c = ctx();
    let ids = move || c.checked.with(|s| s.iter().copied().collect::<Vec<_>>());
    view! {
        <Show when=move || c.checked.with(|s| !s.is_empty())>
            <div class="bulkbar">
                <strong>{move || format!("{} ausgewählt", c.checked.with(|s| s.len()))}</strong>
                <select on:change=move |ev| {
                    if let Ok(coll) = event_target_value(&ev).parse::<i64>() {
                        bulk(c, BulkUpdate { ids: ids(), add_collections: vec![coll], ..Default::default() }, "Zur Sammlung hinzugefügt");
                    }
                }>
                    <option value="" selected>"+ Zu Sammlung hinzufügen …"</option>
                    {move || c.collections.with(|cs| {
                        let mut v: Vec<(i64, String)> = cs.iter().map(|x| (x.id, crate::state::collection_path(cs, x.id))).collect();
                        v.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                        v.into_iter().map(|(id, p)| view! { <option value=id.to_string()>{p}</option> }).collect_view()
                    })}
                </select>
                <select on:change=move |ev| {
                    if let Ok(tag) = event_target_value(&ev).parse::<i64>() {
                        bulk(c, BulkUpdate { ids: ids(), add_tags: vec![tag], ..Default::default() }, "Tag hinzugefügt");
                    }
                }>
                    <option value="" selected>"# Tag hinzufügen …"</option>
                    {move || c.tags.get().into_iter().map(|t| view! { <option value=t.id.to_string()>{t.name}</option> }).collect_view()}
                </select>
                {move || c.collection.get().map(|coll| view! {
                    <button on:click=move |_| bulk(c, BulkUpdate { ids: ids(), remove_collections: vec![coll], ..Default::default() }, "Aus Sammlung entfernt")>
                        "Aus dieser Sammlung entfernen"
                    </button>
                })}
                <button class="danger" on:click=move |_| {
                    let n = c.checked.with_untracked(|s| s.len());
                    let what = if n == 1 { "1 Dokument".to_string() } else { format!("{n} Dokumente") };
                    crate::dialog::confirm_then(
                        crate::dialog::confirm(format!("{what} löschen?"), "Die Dokumente werden samt aller Versionen endgültig gelöscht.")
                            .ok("Löschen")
                            .danger(),
                        move || bulk(c, BulkUpdate { ids: ids(), delete: true, ..Default::default() }, "Gelöscht"),
                    );
                }>"Löschen"</button>
                <button class="link" on:click=move |_| c.checked.set(Default::default())>"Auswahl aufheben"</button>
            </div>
        </Show>
    }
}
