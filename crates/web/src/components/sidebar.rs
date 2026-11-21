// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{BulkUpdate, Collection, CollectionInput, TagInput};

use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};
use crate::util::COLORS;

pub const DRAG_MIME: &str = "application/x-buero-docs";

fn prompt(msg: &str, default: &str) -> Option<String> {
    window()
        .prompt_with_message_and_default(msg, default)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn confirm(msg: &str) -> bool {
    window().confirm_with_message(msg).unwrap_or(false)
}

/// Dokumente (per Drag & Drop aus der Liste) einer Sammlung hinzufügen
fn drop_docs_on_collection(c: Ctx, ev: &web_sys::DragEvent, coll: i64) {
    let Some(dt) = ev.data_transfer() else { return };
    let Ok(data) = dt.get_data(DRAG_MIME) else { return };
    let ids: Vec<i64> = data.split(',').filter_map(|s| s.parse().ok()).collect();
    if ids.is_empty() {
        return;
    }
    ev.prevent_default();
    let n = ids.len();
    spawn_local(async move {
        let body = BulkUpdate { ids, add_collections: vec![coll], ..Default::default() };
        match c.api.send_empty("POST", "/api/documents/bulk", Some(&body)).await {
            Ok(()) => {
                c.notify(format!("{n} Dokument(e) zu „{}“ hinzugefügt", c.collection_name(coll)), false);
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
}

fn create_collection(c: Ctx, parent: Option<i64>) {
    let label = match parent {
        Some(p) => format!("Neue Untersammlung in „{}“:", c.collection_name(p)),
        None => "Name der neuen Sammlung:".to_string(),
    };
    let Some(name) = prompt(&label, "") else { return };
    spawn_local(async move {
        let body = CollectionInput { name: Some(name), parent_id: parent, color: None };
        match c.api.send::<_, Collection>("POST", "/api/collections", &body).await {
            Ok(_) => c.load_meta(),
            Err(e) => c.notify(e, true),
        }
    });
}

#[component]
pub fn Sidebar() -> impl IntoView {
    let c = ctx();
    let collapsed = RwSignal::new(HashSet::<i64>::new());

    let select_all = move |_| {
        c.collection.set(None);
        c.unfiled.set(false);
        c.tag_filter.set(Vec::new());
        c.kind.set(String::new());
    };
    let select_unfiled = move |_| {
        c.collection.set(None);
        c.unfiled.set(!c.unfiled.get_untracked());
    };

    view! {
        <aside class="sidebar">
            <div class="brand">
                <span class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"</span>
                <button class="icon" title="Einstellungen" on:click=move |_| c.show_settings.set(true)><Icon name="settings" /></button>
            </div>
            <nav>
                <div class="nav-item" class:active=move || c.collection.get().is_none() && !c.unfiled.get()
                    on:click=select_all>
                    <span class="label"><Icon name="layers" />"Alle Dokumente"</span>
                    <span class="count">{move || c.stats.get().documents}</span>
                </div>
                <div class="nav-item" class:active=move || c.unfiled.get() on:click=select_unfiled
                    title="Dokumente, die noch keiner Sammlung zugeordnet sind">
                    <span class="label"><Icon name="inbox" />"Eingang"</span>
                    <span class="count">{move || c.stats.get().unfiled}</span>
                </div>
            </nav>

            <div class="section-head">
                <span>"Sammlungen"</span>
                <button class="icon" title="Neue Sammlung" on:click=move |_| create_collection(c, None)><Icon name="plus" class="sm" /></button>
            </div>
            <div class="tree">
                {move || tree_level(c, collapsed, None, 0)}
                <Show when=move || c.collections.with(|v| v.is_empty())>
                    <p class="muted small">"Noch keine Sammlungen. Ein Dokument kann in beliebig vielen Sammlungen liegen."</p>
                </Show>
            </div>

            <div class="section-head"><span>"Tags"</span>
                <button class="icon" title="Neuer Tag" on:click=move |_| {
                    if let Some(name) = prompt("Neuer Tag:", "") {
                        spawn_local(async move {
                            match c.api.send::<_, shared::Tag>("POST", "/api/tags", &TagInput { name: Some(name), color: None }).await {
                                Ok(_) => c.load_meta(),
                                Err(e) => c.notify(e, true),
                            }
                        });
                    }
                }><Icon name="plus" class="sm" /></button>
            </div>
            <div class="tag-cloud">
                <For each=move || c.tags.get() key=|t| (t.id, t.name.clone(), t.document_count, t.color.clone()) let:t>
                    <TagFilterChip tag=t />
                </For>
            </div>

            <div class="section-head"><span>"Dateiart"</span></div>
            <div class="kinds">
                {shared::KINDS.iter().map(|(k, label)| {
                    let k = k.to_string();
                    let k2 = k.clone();
                    view! {
                        <div class="nav-item small" class:active=move || c.kind.get() == k
                            on:click=move |_| {
                                let cur = c.kind.get_untracked();
                                c.kind.set(if cur == k2 { String::new() } else { k2.clone() });
                            }>
                            <span class="label"><KindIcon kind=k2.clone() />{*label}</span>
                        </div>
                    }
                }).collect_view()}
            </div>
            <div class="sidebar-foot muted small">
                {move || {
                    let s = c.stats.get();
                    let mut t = format!("{} Dokumente · {}", s.documents, crate::util::format_size(s.total_size));
                    if s.pending > 0 { t.push_str(&format!(" · {} in Arbeit", s.pending)); }
                    t
                }}
            </div>
        </aside>
    }
}

fn tree_level(c: Ctx, collapsed: RwSignal<HashSet<i64>>, parent: Option<i64>, depth: usize) -> AnyView {
    let children: Vec<Collection> = c
        .collections
        .with(|cs| cs.iter().filter(|x| x.parent_id == parent).cloned().collect());
    children
        .into_iter()
        .map(|col| {
            let id = col.id;
            let has_children = c.collections.with(|cs| cs.iter().any(|x| x.parent_id == Some(id)));
            let is_open = move || !collapsed.with(|s| s.contains(&id));
            let drag_over = RwSignal::new(false);
            let name = col.name.clone();
            let color = col.color.clone().unwrap_or_default();
            view! {
                <div class="tree-node">
                    <div class="nav-item tree-row"
                        class:active=move || c.collection.get() == Some(id)
                        class:drop-target=move || drag_over.get()
                        style=format!("padding-left: {}px", 8 + depth * 14)
                        on:click=move |_| {
                            c.unfiled.set(false);
                            c.collection.set(if c.collection.get_untracked() == Some(id) { None } else { Some(id) });
                        }
                        on:dragover=move |ev| {
                            let ok = ev.data_transfer().map(|d| d.types().includes(&DRAG_MIME.into(), 0)).unwrap_or(false);
                            if ok { ev.prevent_default(); drag_over.set(true); }
                        }
                        on:dragleave=move |_| drag_over.set(false)
                        on:drop=move |ev| { drag_over.set(false); drop_docs_on_collection(c, &ev, id); }
                    >
                        <span class="twisty" on:click=move |ev| {
                            ev.stop_propagation();
                            collapsed.update(|s| { if !s.remove(&id) { s.insert(id); } });
                        }>
                            {move || has_children.then(|| view! { <Icon name=if is_open() { "chevron-down" } else { "chevron-right" } class="sm" /> })}
                        </span>
                        <span class="dot" style=format!("background:{}", if color.is_empty() { "var(--muted)".to_string() } else { color.clone() })></span>
                        <span class="grow ellipsis">{name.clone()}</span>
                        <span class="count">{col.document_count}</span>
                        <span class="row-actions">
                            <button class="icon" title="Untersammlung anlegen"
                                on:click=move |ev| { ev.stop_propagation(); create_collection(c, Some(id)); }><Icon name="plus" class="sm" /></button>
                            <button class="icon" title="Bearbeiten" on:click={
                                let name = name.clone();
                                move |ev| { ev.stop_propagation(); edit_collection(c, id, name.clone()); }
                            }><Icon name="pencil" class="sm" /></button>
                        </span>
                    </div>
                    {move || (has_children && is_open()).then(|| tree_level(c, collapsed, Some(id), depth + 1))}
                </div>
            }
        })
        .collect_view()
        .into_any()
}

fn edit_collection(c: Ctx, id: i64, name: String) {
    let Some(action) = prompt(
        &format!(
            "Sammlung „{name}“ bearbeiten:\n\n1 = Umbenennen\n2 = Farbe ändern\n3 = Verschieben\n4 = Löschen"
        ),
        "1",
    ) else {
        return;
    };
    let mut body = CollectionInput::default();
    match action.as_str() {
        "1" => body.name = prompt("Neuer Name:", &name),
        "2" => {
            let list = COLORS.iter().enumerate().skip(1).map(|(i, c)| format!("{i} = {c}")).collect::<Vec<_>>().join("\n");
            if let Some(v) = prompt(&format!("Farbe wählen (0 = keine):\n{list}"), "0") {
                body.color = Some(v.parse::<usize>().ok().and_then(|i| COLORS.get(i)).unwrap_or(&"").to_string());
            }
        }
        "3" => {
            let opts = c.collections.with_untracked(|cs| {
                cs.iter()
                    .filter(|x| x.id != id)
                    .map(|x| format!("{} = {}", x.id, crate::state::collection_path(cs, x.id)))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            if let Some(v) = prompt(&format!("Nummer der neuen übergeordneten Sammlung (0 = oberste Ebene):\n{opts}"), "0") {
                body.parent_id = v.parse().ok();
            }
        }
        "4" => {
            if confirm(&format!("Sammlung „{name}“ inklusive Untersammlungen löschen?\nDie Dokumente selbst bleiben erhalten.")) {
                spawn_local(async move {
                    match c.api.send_empty::<()>("DELETE", &format!("/api/collections/{id}"), None).await {
                        Ok(()) => {
                            if c.collection.get_untracked() == Some(id) {
                                c.collection.set(None);
                            }
                            c.load_meta();
                            c.reload();
                        }
                        Err(e) => c.notify(e, true),
                    }
                });
            }
            return;
        }
        _ => return,
    }
    if body.name.is_none() && body.color.is_none() && body.parent_id.is_none() {
        return;
    }
    spawn_local(async move {
        match c.api.send::<_, Collection>("PATCH", &format!("/api/collections/{id}"), &body).await {
            Ok(_) => {
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
}

#[component]
fn TagFilterChip(tag: shared::Tag) -> impl IntoView {
    let c = ctx();
    let id = tag.id;
    let name = tag.name.clone();
    let active = move || c.tag_filter.with(|t| t.contains(&id));
    let style = tag.color.clone().map(|col| format!("--tag: {col}")).unwrap_or_default();
    view! {
        <span class="chip tag" class:active=active style=style
            title="Klicken: filtern · Rechtsklick: bearbeiten"
            on:click=move |_| c.tag_filter.update(|t| { if let Some(p) = t.iter().position(|x| *x == id) { t.remove(p); } else { t.push(id); } })
            on:contextmenu={
                let name = name.clone();
                move |ev| { ev.prevent_default(); edit_tag(c, id, name.clone()); }
            }>
            "#"{name.clone()}<span class="count">{tag.document_count}</span>
        </span>
    }
}

fn edit_tag(c: Ctx, id: i64, name: String) {
    let Some(action) = prompt(&format!("Tag „{name}“:\n1 = Umbenennen\n2 = Farbe\n3 = Löschen"), "1") else { return };
    let body = match action.as_str() {
        "1" => match prompt("Neuer Name:", &name) {
            Some(n) => TagInput { name: Some(n), color: None },
            None => return,
        },
        "2" => {
            let list = COLORS.iter().enumerate().skip(1).map(|(i, c)| format!("{i} = {c}")).collect::<Vec<_>>().join("\n");
            match prompt(&format!("Farbe (0 = keine):\n{list}"), "0") {
                Some(v) => TagInput { name: None, color: Some(v.parse::<usize>().ok().and_then(|i| COLORS.get(i)).unwrap_or(&"").to_string()) },
                None => return,
            }
        }
        "3" => {
            if confirm(&format!("Tag „{name}“ löschen?")) {
                spawn_local(async move {
                    match c.api.send_empty::<()>("DELETE", &format!("/api/tags/{id}"), None).await {
                        Ok(()) => {
                            c.tag_filter.update(|t| t.retain(|x| *x != id));
                            c.load_meta();
                            c.reload();
                        }
                        Err(e) => c.notify(e, true),
                    }
                });
            }
            return;
        }
        _ => return,
    };
    spawn_local(async move {
        match c.api.send::<_, shared::Tag>("PATCH", &format!("/api/tags/{id}"), &body).await {
            Ok(_) => {
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
}
