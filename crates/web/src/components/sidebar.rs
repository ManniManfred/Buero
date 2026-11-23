// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{BulkUpdate, Collection, CollectionInput, TagInput};

use crate::dialog::{self, choice, Choice};
use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};

pub const DRAG_MIME: &str = "application/x-buero-docs";
/// Sammlung, die im Baum gezogen wird
const COLL_MIME: &str = "application/x-buero-collection";

thread_local! {
    /// Gerade gezogene Sammlung (beim Überfahren ist der Inhalt des Drags nicht lesbar)
    static DRAGGED: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

fn has_type(ev: &web_sys::DragEvent, t: &str) -> bool {
    ev.data_transfer().map(|d| d.types().includes(&t.into(), 0)).unwrap_or(false)
}

/// Darf die gezogene Sammlung nach `target` (`None`: ganz oben)?
fn can_move_to(c: Ctx, target: Option<i64>) -> Option<i64> {
    let dragged = DRAGGED.with(|d| d.get())?;
    c.collections.with_untracked(|cs| {
        let current_parent = cs.iter().find(|x| x.id == dragged)?.parent_id;
        if current_parent == target {
            return None;
        }
        // Nicht in sich selbst oder eine eigene Untersammlung
        let mut cur = target;
        let mut guard = 0;
        while let Some(t) = cur {
            if t == dragged || guard > 64 {
                return None;
            }
            cur = cs.iter().find(|x| x.id == t).and_then(|x| x.parent_id);
            guard += 1;
        }
        Some(dragged)
    })
}

fn move_collection(c: Ctx, id: i64, target: Option<i64>, open: Option<RwSignal<HashMap<i64, bool>>>) {
    let name = c.collections.with_untracked(|cs| cs.iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
    spawn_local(async move {
        let input = CollectionInput { name: None, parent_id: Some(target.unwrap_or(0)), color: None, ai_locked: None };
        match c.api.send::<_, Collection>("PATCH", &format!("/api/collections/{id}"), &input).await {
            Ok(_) => {
                if let (Some(t), Some(open)) = (target, open) {
                    open.update(|o| {
                        o.insert(t, true);
                    });
                }
                let to = match target {
                    Some(t) => format!("nach „{}“", c.collection_name(t)),
                    None => "nach ganz oben".to_string(),
                };
                c.notify(format!("„{name}“ {to} verschoben"), false);
                c.load_meta();
                c.reload();
            }
            Err(e) => c.notify(e, true),
        }
    });
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
        let with_children = match crate::components::links::ask_children(crate::components::links::child_count(c, &ids)).await {
            Some(w) => w,
            None => return,
        };
        let body = BulkUpdate { ids, add_collections: vec![coll], with_children, ..Default::default() };
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
    // Auf-/zugeklappt per Klick; sonst offen bis Ebene 2 (siehe `tree_level`)
    let open = RwSignal::new(HashMap::<i64, bool>::new());
    let filter = RwSignal::new(String::new());
    let root_over = RwSignal::new(false);
    // Gewählte Sammlung sichtbar machen: Vorfahren aufklappen
    Effect::new(move |_| {
        let Some(id) = c.collection.get() else { return };
        let ancestors = c.collections.with_untracked(|cs| {
            let mut out = Vec::new();
            let mut cur = cs.iter().find(|x| x.id == id).and_then(|x| x.parent_id);
            while let Some(p) = cur {
                if out.contains(&p) {
                    break;
                }
                out.push(p);
                cur = cs.iter().find(|x| x.id == p).and_then(|x| x.parent_id);
            }
            out
        });
        let closed = |a: &i64| !open.with_untracked(|o| o.get(a).copied().unwrap_or_else(|| !depth_closed(c, *a)));
        if ancestors.iter().any(closed) {
            open.update(|o| {
                for a in ancestors {
                    o.insert(a, true);
                }
            });
        }
    });

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

            <div class="section-head" class:drop-target=move || root_over.get()
                title="Sammlungen lassen sich im Baum per Drag & Drop verschieben; hierher ziehen = ganz nach oben"
                on:dragover=move |ev| {
                    if has_type(&ev, COLL_MIME) && can_move_to(c, None).is_some() { ev.prevent_default(); root_over.set(true); }
                }
                on:dragleave=move |_| root_over.set(false)
                on:drop=move |ev| {
                    root_over.set(false);
                    if let Some(id) = can_move_to(c, None) { ev.prevent_default(); move_collection(c, id, None, None); }
                }>
                <span>"Sammlungen"</span>
                <button class="icon" title="Neue Sammlung" on:click=move |_| create_collection(c, None)><Icon name="plus" class="sm" /></button>
            </div>
            <Show when=move || c.collections.with(|v| v.len() > 5)>
                <div class="tree-search">
                    <Icon name="search" class="sm" />
                    <input type="search" placeholder="Sammlung suchen …" spellcheck="false"
                        prop:value=move || filter.get()
                        on:input=move |ev| filter.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Escape" { ev.stop_propagation(); filter.set(String::new()); } />
                    <Show when=move || !filter.with(String::is_empty)>
                        <button class="icon" title="Suche löschen" on:click=move |_| filter.set(String::new())><Icon name="x" class="sm" /></button>
                    </Show>
                </div>
            </Show>
            <div class="tree">
                {move || {
                    let visible = filter.with(|f| matching(c, f));
                    let empty = visible.as_ref().is_some_and(HashSet::is_empty);
                    view! {
                        {tree_level(c, open, visible, None, 0)}
                        {empty.then(|| view! { <p class="muted small">"Keine passende Sammlung"</p> })}
                    }
                }}
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

/// Standard beim Start: alles zugeklappt, nur die oberste Ebene sichtbar
/// (aufgeklappt wird nur der Weg zur gewählten Sammlung)
fn open_by_default(_depth: usize) -> bool {
    false
}

/// Ist eine Sammlung ohne Klick zugeklappt?
fn depth_closed(c: Ctx, id: i64) -> bool {
    let depth = c.collections.with_untracked(|cs| {
        let mut d = 0;
        let mut cur = cs.iter().find(|x| x.id == id).and_then(|x| x.parent_id);
        while let Some(p) = cur {
            d += 1;
            if d > 64 {
                break;
            }
            cur = cs.iter().find(|x| x.id == p).and_then(|x| x.parent_id);
        }
        d
    });
    !open_by_default(depth)
}

/// Für Vergleiche: klein, ohne Umlaute
fn norm(s: &str) -> String {
    s.to_lowercase().replace('ä', "a").replace('ö', "o").replace('ü', "u").replace('ß', "ss")
}

/// Bei Suche: passende Sammlungen samt ihrer Vorfahren (`None` = keine Suche)
fn matching(c: Ctx, filter: &str) -> Option<HashSet<i64>> {
    let f = norm(filter.trim());
    if f.is_empty() {
        return None;
    }
    c.collections.with(|cs| {
        let mut out = HashSet::new();
        for col in cs.iter().filter(|x| norm(&x.name).contains(&f)) {
            let mut cur = Some(col.id);
            while let Some(id) = cur {
                if !out.insert(id) {
                    break;
                }
                cur = cs.iter().find(|x| x.id == id).and_then(|x| x.parent_id);
            }
        }
        Some(out)
    })
}

fn tree_level(
    c: Ctx,
    open: RwSignal<HashMap<i64, bool>>,
    visible: Option<HashSet<i64>>,
    parent: Option<i64>,
    depth: usize,
) -> AnyView {
    let children: Vec<Collection> = c.collections.with(|cs| {
        cs.iter()
            .filter(|x| x.parent_id == parent && visible.as_ref().is_none_or(|v| v.contains(&x.id)))
            .cloned()
            .collect()
    });
    let searching = visible.is_some();
    children
        .into_iter()
        .map(|col| {
            let id = col.id;
            let has_children = c.collections.with(|cs| cs.iter().any(|x| x.parent_id == Some(id)));
            // Bei Suche alles aufgeklappt, was passt
            let is_open = move || searching || open.with(|o| o.get(&id).copied().unwrap_or(open_by_default(depth)));
            let drag_over = RwSignal::new(false);
            let name = col.name.clone();
            let color = col.color.clone().unwrap_or_default();
            view! {
                <div class="tree-node">
                    <div class="nav-item tree-row"
                        draggable="true"
                        class:active=move || c.collection.get() == Some(id)
                        class:drop-target=move || drag_over.get()
                        on:dragstart=move |ev| {
                            ev.stop_propagation();
                            if let Some(dt) = ev.data_transfer() {
                                let _ = dt.set_data(COLL_MIME, &id.to_string());
                                dt.set_effect_allowed("move");
                            }
                            DRAGGED.with(|d| d.set(Some(id)));
                        }
                        on:dragend=move |_| DRAGGED.with(|d| d.set(None))
                        style=format!("padding-left: {}px", 8 + depth * 14)
                        on:click=move |_| {
                            c.unfiled.set(false);
                            c.collection.set(if c.collection.get_untracked() == Some(id) { None } else { Some(id) });
                        }
                        on:dragover=move |ev| {
                            let ok = has_type(&ev, DRAG_MIME) || (has_type(&ev, COLL_MIME) && can_move_to(c, Some(id)).is_some());
                            if ok { ev.prevent_default(); drag_over.set(true); }
                        }
                        on:dragleave=move |_| drag_over.set(false)
                        on:drop=move |ev| {
                            drag_over.set(false);
                            if has_type(&ev, COLL_MIME) {
                                if let Some(dragged) = can_move_to(c, Some(id)) {
                                    ev.prevent_default();
                                    move_collection(c, dragged, Some(id), Some(open));
                                }
                            } else {
                                drop_docs_on_collection(c, &ev, id);
                            }
                        }
                    >
                        <span class="twisty" on:click=move |ev| {
                            ev.stop_propagation();
                            if !searching {
                                let now = is_open();
                                open.update(|o| { o.insert(id, !now); });
                            }
                        }>
                            {move || has_children.then(|| view! { <Icon name=if is_open() { "chevron-down" } else { "chevron-right" } class="sm" /> })}
                        </span>
                        <span class="coll-icon" style=(!color.is_empty()).then(|| format!("color:{color}"))>
                            {move || view! { <Icon name=if has_children && is_open() { "folder-open" } else { "folder" } class="sm" /> }}
                        </span>
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
                    {
                        let visible = visible.clone();
                        move || (has_children && is_open()).then(|| tree_level(c, open, visible.clone(), Some(id), depth + 1))
                    }
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
