use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::BulkUpdate;

use crate::components::upload::{pick_files, start_uploads};
use crate::state::{ctx, Ctx};

#[component]
pub fn Toolbar() -> impl IntoView {
    let c = ctx();
    let file_input = NodeRef::<leptos::html::Input>::new();

    view! {
        <div class="toolbar">
            <div class="search">
                <span class="search-icon">"🔍"</span>
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
            <button class="primary" on:click=move |_| { if let Some(i) = file_input.get() { i.click(); } }>
                "⬆ Hochladen"
            </button>
            <input type="file" multiple style="display:none" node_ref=file_input
                on:change=move |_| {
                    if let Some(i) = file_input.get() {
                        let files = pick_files(i.files());
                        i.set_value("");
                        start_uploads(c, files);
                    }
                } />
        </div>
        <FilterBar />
        <BulkBar />
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
                    "🗂 "{c.collection_name(id)}" ✕"
                </span>
            })}
            {move || c.unfiled.get().then(|| view! {
                <span class="chip removable" on:click=move |_| c.unfiled.set(false)>"📥 Eingang ✕"</span>
            })}
            {move || {
                let k = c.kind.get();
                (!k.is_empty()).then(|| view! {
                    <span class="chip removable" on:click=move |_| c.kind.set(String::new())>
                        {shared::kind_label(&k)}" ✕"
                    </span>
                })
            }}
            {move || c.tag_filter.get().into_iter().map(|id| {
                let name = c.tags.with(|ts| ts.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_default());
                view! {
                    <span class="chip removable" on:click=move |_| c.tag_filter.update(|t| t.retain(|x| *x != id))>
                        "#"{name}" ✕"
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
                    <option value="" selected>"➕ Zu Sammlung hinzufügen …"</option>
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
                    if window().confirm_with_message(&format!("{n} Dokument(e) endgültig löschen?")).unwrap_or(false) {
                        bulk(c, BulkUpdate { ids: ids(), delete: true, ..Default::default() }, "Gelöscht");
                    }
                }>"Löschen"</button>
                <button class="link" on:click=move |_| c.checked.set(Default::default())>"Auswahl aufheben"</button>
            </div>
        </Show>
    }
}
