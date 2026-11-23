// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{BulkUpdate, Collection, CollectionInput, TagInput};

use crate::dialog::{self, choice, Choice};
use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};

pub const DRAG_MIME: &str = "application/x-buero-docs";

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
    let (title, text) = match parent {
        Some(p) => ("Neue Untersammlung".to_string(), format!("In „{}“", c.collection_name(p))),
        None => ("Neue Sammlung".to_string(), String::new()),
    };
    spawn_local(async move {
        let Some(name) = dialog::prompt(title, text).placeholder("Name").ok("Anlegen").ask_text().await else { return };
        let body = CollectionInput { name: Some(name), parent_id: parent, color: None, ai_locked: None };
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
        // In der Übersicht: Eingang zeigen statt umschalten
        c.unfiled.set(c.dashboard.get_untracked() || !c.unfiled.get_untracked());
    };

    view! {
        <aside class="sidebar">
            <div class="brand">
                <span class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"<span class="brand-version" title="Version">{crate::util::version()}</span></span>
                <button class="icon" title="Einstellungen" on:click=move |_| c.show_settings.set(true)><Icon name="settings" /></button>
            </div>
            <nav>
                <div class="nav-item" class:active=move || c.dashboard.get() on:click=move |_| c.dashboard.set(true)
                    title="Speicherplatz und Kennzahlen">
                    <span class="label"><Icon name="dashboard" />"Übersicht"</span>
                </div>
                <div class="nav-item" class:active=move || !c.dashboard.get() && c.collection.get().is_none() && !c.unfiled.get()
                    on:click=select_all>
                    <span class="label"><Icon name="layers" />"Alle Dokumente"</span>
                    <span class="count">{move || c.stats.get().documents}</span>
                </div>
                <div class="nav-item" class:active=move || !c.dashboard.get() && c.unfiled.get() on:click=select_unfiled
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
                    spawn_local(async move {
                        let Some(name) = dialog::prompt("Neuer Tag", "").placeholder("Name").ok("Anlegen").ask_text().await else { return };
                        match c.api.send::<_, shared::Tag>("POST", "/api/tags", &TagInput { name: Some(name), color: None }).await {
                            Ok(_) => c.load_meta(),
                            Err(e) => c.notify(e, true),
                        }
                    });
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
                        {col.ai_locked.then(|| view! {
                            <span class="ai-lock" title="Für den KI-Zugang (MCP) gesperrt – samt Untersammlungen"><Icon name="lock" class="sm" /></span>
                        })}
                        <span class="count">{col.document_count}</span>
                        <span class="row-actions">
                            <button class="icon" title="Sammlung freigeben" on:click={
                                let name = name.clone();
                                move |ev| {
                                    ev.stop_propagation();
                                    c.share_target.set(Some(crate::state::ShareTarget { document_id: None, collection_id: Some(id), name: name.clone() }));
                                }
                            }><Icon name="link" class="sm" /></button>
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
    spawn_local(async move {
        let locked = c.collections.with_untracked(|cs| cs.iter().any(|x| x.id == id && x.ai_locked));
        let color = c.collections.with_untracked(|cs| cs.iter().find(|x| x.id == id).and_then(|x| x.color.clone())).unwrap_or_default();
        let lock = if locked { choice("lock", "Für KI-Zugang freigeben", "lock") } else { choice("lock", "Für KI-Zugang sperren", "lock") };
        let Some(action) = dialog::menu(
            format!("Sammlung „{name}“"),
            vec![
                choice("rename", "Umbenennen", "pencil"),
                choice("color", "Farbe ändern", "tag"),
                choice("move", "Verschieben", "folder"),
                lock,
                Choice { danger: true, ..choice("delete", "Löschen", "trash") },
            ],
        )
        .ask()
        .await
        else {
            return;
        };
        let mut body = CollectionInput::default();
        match action.value.as_str() {
            "lock" => {
                let d = if locked {
                    dialog::confirm(
                        "Für KI-Zugang freigeben?",
                        format!("Dokumente in „{name}“ werden dann für KI-Assistenten auffindbar und lesbar."),
                    )
                    .ok("Freigeben")
                } else {
                    dialog::confirm(
                        "Für KI-Zugang sperren?",
                        format!("Dokumente in „{name}“ samt Untersammlungen werden KI-Assistenten nie angezeigt – auch nicht, wenn sie zusätzlich in anderen Sammlungen liegen."),
                    )
                    .ok("Sperren")
                };
                if !d.ask_ok().await {
                    return;
                }
                body.ai_locked = Some(!locked);
            }
            "rename" => {
                body.name = dialog::prompt("Sammlung umbenennen", "Neuer Name").value(name.clone()).ok("Umbenennen").ask_text().await;
            }
            "color" => {
                let Some(a) = dialog::confirm(format!("Farbe für „{name}“"), "").colors().value(color).ok("Übernehmen").ask().await else {
                    return;
                };
                body.color = Some(a.value);
            }
            "move" => {
                // Nicht in sich selbst oder eigene Untersammlungen
                let options = c.collections.with_untracked(|cs| {
                    let mut blocked = vec![id];
                    let mut i = 0;
                    while i < blocked.len() {
                        let p = blocked[i];
                        blocked.extend(cs.iter().filter(|x| x.parent_id == Some(p)).map(|x| x.id));
                        i += 1;
                    }
                    let mut o: Vec<(String, String)> = cs
                        .iter()
                        .filter(|x| !blocked.contains(&x.id))
                        .map(|x| (x.id.to_string(), crate::state::collection_path(cs, x.id)))
                        .collect();
                    o.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                    o.insert(0, ("0".into(), "– Oberste Ebene –".into()));
                    o
                });
                let current = c.collections.with_untracked(|cs| cs.iter().find(|x| x.id == id).and_then(|x| x.parent_id)).unwrap_or(0);
                let Some(a) = dialog::confirm(format!("„{name}“ verschieben"), "Neue übergeordnete Sammlung")
                    .select(options)
                    .value(current.to_string())
                    .ok("Verschieben")
                    .ask()
                    .await
                else {
                    return;
                };
                body.parent_id = a.value.parse().ok();
            }
            "delete" => {
                if dialog::confirm(format!("Sammlung „{name}“ löschen?"), "Untersammlungen werden mitgelöscht. Die Dokumente selbst bleiben erhalten.")
                    .ok("Löschen")
                    .danger()
                    .ask_ok()
                    .await
                {
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
                }
                return;
            }
            _ => return,
        }
        if body.name.is_none() && body.color.is_none() && body.parent_id.is_none() && body.ai_locked.is_none() {
            return;
        }
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
    spawn_local(async move {
        let color = c.tags.with_untracked(|ts| ts.iter().find(|t| t.id == id).and_then(|t| t.color.clone())).unwrap_or_default();
        let Some(action) = dialog::menu(
            format!("Tag „{name}“"),
            vec![
                choice("rename", "Umbenennen", "pencil"),
                choice("color", "Farbe ändern", "tag"),
                Choice { danger: true, ..choice("delete", "Löschen", "trash") },
            ],
        )
        .ask()
        .await
        else {
            return;
        };
        let body = match action.value.as_str() {
            "rename" => match dialog::prompt("Tag umbenennen", "Neuer Name").value(name.clone()).ok("Umbenennen").ask_text().await {
                Some(n) => TagInput { name: Some(n), color: None },
                None => return,
            },
            "color" => match dialog::confirm(format!("Farbe für „{name}“"), "").colors().value(color).ok("Übernehmen").ask().await {
                Some(a) => TagInput { name: None, color: Some(a.value) },
                None => return,
            },
            "delete" => {
                if dialog::confirm(format!("Tag „{name}“ löschen?"), "Die Dokumente bleiben erhalten, sie verlieren nur das Tag.")
                    .ok("Löschen")
                    .danger()
                    .ask_ok()
                    .await
                {
                    match c.api.send_empty::<()>("DELETE", &format!("/api/tags/{id}"), None).await {
                        Ok(()) => {
                            c.tag_filter.update(|t| t.retain(|x| *x != id));
                            c.load_meta();
                            c.reload();
                        }
                        Err(e) => c.notify(e, true),
                    }
                }
                return;
            }
            _ => return,
        };
        match c.api.send::<_, shared::Tag>("PATCH", &format!("/api/tags/{id}"), &body).await {
            Ok(_) => {
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
}
