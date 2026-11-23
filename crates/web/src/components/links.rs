// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Verknüpfungen zwischen Dokumenten: Unterdokumente (z. B. Mail-Anhänge)
//! und Verweise – in der Detailansicht, in der Liste und per Drag & Drop.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{DocumentLinks, DocumentSummary, LinkInput, ListResponse};

use crate::dialog::{self, choice};
use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};
use crate::util::{format_size, format_ts};

/// Unterdokumente der gewählten Dokumente laut aktueller Liste
pub fn child_count(c: Ctx, ids: &[i64]) -> i64 {
    c.results.with_untracked(|r| r.iter().filter(|d| ids.contains(&d.id)).map(|d| d.child_count).sum())
}

/// Bei Sammlungsänderungen fragen, ob Unterdokumente mitgehen.
/// `None` = abgebrochen.
pub async fn ask_children(n: i64) -> Option<bool> {
    if n <= 0 {
        return Some(false);
    }
    let what = if n == 1 { "1 Unterdokument".to_string() } else { format!("{n} Unterdokumente") };
    let a = dialog::menu(
        "Unterdokumente mitnehmen?",
        vec![choice("with", format!("Mit {what} (z. B. Anhänge)"), "layers"), choice("only", "Nur das Dokument selbst", "file-pen")],
    )
    .text(format!("Dazu gehören {what}."))
    .ask()
    .await?;
    Some(a.value == "with")
}

pub async fn link(c: Ctx, id: i64, target: i64, kind: &str) -> bool {
    let body = LinkInput { target, kind: kind.into() };
    match c.api.send_empty("POST", &format!("/api/documents/{id}/links"), Some(&body)).await {
        Ok(()) => true,
        Err(e) => {
            c.notify(e, true);
            false
        }
    }
}

/// Dokumente (per Drag & Drop) mit einem anderen verknüpfen
pub fn drop_on_document(c: Ctx, ids: Vec<i64>, target: i64, target_title: String) {
    let ids: Vec<i64> = ids.into_iter().filter(|i| *i != target).collect();
    if ids.is_empty() {
        return;
    }
    spawn_local(async move {
        let what = if ids.len() == 1 { "Dokument".to_string() } else { format!("{} Dokumente", ids.len()) };
        let Some(a) = dialog::menu(
            format!("{what} verknüpfen"),
            vec![
                choice("child", format!("Als Unterdokument von „{target_title}“"), "layers"),
                choice("related", format!("Als Verweis auf „{target_title}“"), "link"),
            ],
        )
        .ask()
        .await
        else {
            return;
        };
        let mut ok = 0;
        for id in &ids {
            if link(c, target, *id, &a.value).await {
                ok += 1;
            }
        }
        if ok > 0 {
            c.notify(if a.value == "child" { "Als Unterdokument verknüpft" } else { "Verweis angelegt" }, false);
            c.reload();
            c.detail_refresh.update(|n| *n += 1);
        }
    });
}

#[component]
fn LinkRow(doc: DocumentSummary, #[prop(into)] on_remove: Callback<i64>, remove_title: &'static str) -> impl IntoView {
    let c = ctx();
    let id = doc.id;
    view! {
        <li class="link-row">
            <button class="link-open" title="Öffnen" on:click=move |_| c.selected.set(Some(id))>
                <KindIcon kind=doc.kind.clone() class="sm" />
                <span class="ellipsis">{doc.title.clone()}</span>
            </button>
            <span class="muted small nowrap">
                {doc.document_date.clone().unwrap_or_else(|| format_ts(doc.created_at))}
            </span>
            <button class="icon" title=remove_title on:click=move |_| on_remove.run(id)><Icon name="x" class="sm" /></button>
        </li>
    }
}

/// Bereich „Verknüpfungen“ in der Detailansicht
#[component]
pub fn LinksSection(id: i64, title: String) -> impl IntoView {
    let c = ctx();
    let links = RwSignal::new(None::<DocumentLinks>);
    let picking = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if let Ok(l) = c.api.get::<DocumentLinks>(&format!("/api/documents/{id}/links")).await {
                links.set(Some(l));
            }
        });
    };
    load();
    let unlink = move |target: i64| {
        spawn_local(async move {
            match c.api.send_empty::<()>("DELETE", &format!("/api/documents/{id}/links/{target}"), None).await {
                Ok(()) => {
                    load();
                    c.reload();
                }
                Err(e) => c.notify(e, true),
            }
        });
    };
    let title2 = title.clone();

    view! {
        <div class="links">
            <div class="links-head">
                <span>"Verknüpfungen"</span>
                <button class="link" on:click=move |_| picking.set(true)><Icon name="plus" class="sm" />"Verknüpfen …"</button>
            </div>
            {move || links.get().map(|l| {
                let empty = l.parent.is_none() && l.children.is_empty() && l.related.is_empty();
                view! {
                    {empty.then(|| view! { <p class="muted small">"Keine. Dokumente lassen sich auch per Drag & Drop aufeinander ziehen."</p> })}
                    {l.parent.clone().map(|p| view! {
                        <div class="links-group">
                            <div class="muted small">"Gehört zu"</div>
                            <ul><LinkRow doc=p on_remove=unlink remove_title="Nicht mehr unterordnen" /></ul>
                        </div>
                    })}
                    {(!l.children.is_empty()).then(|| view! {
                        <div class="links-group">
                            <div class="muted small">{format!("Unterdokumente ({})", l.children.len())}</div>
                            <ul>{l.children.clone().into_iter().map(|d| view! { <LinkRow doc=d on_remove=unlink remove_title="Nicht mehr unterordnen" /> }).collect_view()}</ul>
                        </div>
                    })}
                    {(!l.related.is_empty()).then(|| view! {
                        <div class="links-group">
                            <div class="muted small">"Verweise"</div>
                            <ul>{l.related.clone().into_iter().map(|d| view! { <LinkRow doc=d on_remove=unlink remove_title="Verweis entfernen" /> }).collect_view()}</ul>
                        </div>
                    })}
                }
            })}
            <Show when=move || picking.get()>
                <LinkPicker id=id title=title2.clone() on_close=Callback::new(move |changed: bool| {
                    picking.set(false);
                    if changed {
                        load();
                        c.reload();
                    }
                }) />
            </Show>
        </div>
    }
}

/// Dialog: Dokument suchen und verknüpfen
#[component]
fn LinkPicker(id: i64, title: String, on_close: Callback<bool>) -> impl IntoView {
    let c = ctx();
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<DocumentSummary>::new());
    let chosen = RwSignal::new(None::<i64>);
    let kind = RwSignal::new("child".to_string());
    let seq = RwSignal::new(0u32);
    let input = NodeRef::<leptos::html::Input>::new();
    request_animation_frame(move || {
        if let Some(i) = input.get_untracked() {
            let _ = i.focus();
        }
    });

    let search = move || {
        let q = query.get_untracked();
        let n = seq.get_untracked() + 1;
        seq.set(n);
        spawn_local(async move {
            let enc = String::from(js_sys::encode_uri_component(&q));
            if let Ok(r) = c.api.get::<ListResponse>(&format!("/api/documents?limit=12&q={enc}")).await {
                if seq.get_untracked() == n {
                    results.set(r.items.into_iter().filter(|d| d.id != id).collect());
                }
            }
        });
    };
    search();

    let submit = move || {
        let Some(target) = chosen.get_untracked() else { return };
        let k = kind.get_untracked();
        spawn_local(async move {
            if link(c, id, target, &k).await {
                c.notify("Verknüpft", false);
                on_close.run(true);
            }
        });
    };
    let handle = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" {
            ev.prevent_default();
            on_close.run(false);
        }
    });
    on_cleanup(move || handle.remove());

    view! {
        <div class="modal-bg dialog-bg" on:click=move |_| on_close.run(false)>
            <div class="card modal dialog link-picker" on:click=|ev| ev.stop_propagation()>
                <div class="dialog-head">
                    <h3>"Verknüpfen"</h3>
                    <button class="icon" title="Schließen" on:click=move |_| on_close.run(false)><Icon name="x" /></button>
                </div>
                <input node_ref=input type="search" placeholder="Dokument suchen …"
                    prop:value=move || query.get()
                    on:input=move |ev| { query.set(event_target_value(&ev)); search(); } />
                <ul class="picker-results">
                    {move || results.get().into_iter().map(|d| {
                        let did = d.id;
                        view! {
                            <li class:active=move || chosen.get() == Some(did) on:click=move |_| chosen.set(Some(did))
                                on:dblclick=move |_| { chosen.set(Some(did)); submit(); }>
                                <KindIcon kind=d.kind.clone() class="sm" />
                                <span class="ellipsis grow">{d.title.clone()}</span>
                                <span class="muted small nowrap">{format_size(d.size)}</span>
                            </li>
                        }
                    }).collect_view()}
                </ul>
                <label>"Art der Verknüpfung"
                    <select prop:value=move || kind.get() on:change=move |ev| kind.set(event_target_value(&ev))>
                        <option value="child">{format!("Gewähltes wird Unterdokument von „{title}“")}</option>
                        <option value="parent">{format!("„{title}“ wird Unterdokument des gewählten")}</option>
                        <option value="related">"Gegenseitiger Verweis"</option>
                    </select>
                </label>
                <div class="dialog-actions">
                    <button on:click=move |_| on_close.run(false)>"Abbrechen"</button>
                    <button class="primary" disabled=move || chosen.get().is_none() on:click=move |_| submit()>"Verknüpfen"</button>
                </div>
            </div>
        </div>
    }
}

/// Aufklappbare Unterdokumente unter einer Zeile der Dokumentliste
#[component]
pub fn ChildToggle(id: i64, count: i64) -> impl IntoView {
    let c = ctx();
    let open = RwSignal::new(false);
    let children = RwSignal::new(None::<Vec<DocumentSummary>>);
    let toggle = move |ev: leptos::ev::MouseEvent| {
        ev.stop_propagation();
        open.update(|o| *o = !*o);
        if open.get_untracked() && children.with_untracked(Option::is_none) {
            spawn_local(async move {
                if let Ok(l) = c.api.get::<DocumentLinks>(&format!("/api/documents/{id}/links")).await {
                    children.set(Some(l.children));
                }
            });
        }
    };
    let label = if count == 1 { "1 Unterdokument".to_string() } else { format!("{count} Unterdokumente") };
    view! {
        <div class="child-toggle" on:click=|ev| ev.stop_propagation()>
            <button class="link small" on:click=toggle>
                {move || if open.get() { view! { <Icon name="chevron-down" class="sm" /> } } else { view! { <Icon name="chevron-right" class="sm" /> } }}
                <Icon name="layers" class="sm" />{label}
            </button>
            <Show when=move || open.get()>
                <ul class="child-list">
                    {move || match children.get() {
                        None => view! { <li class="muted small">"Lade…"</li> }.into_any(),
                        Some(list) => list.into_iter().map(|d| {
                            let did = d.id;
                            view! {
                                <li class:selected=move || c.selected.get() == Some(did) on:click=move |_| c.selected.set(Some(did))>
                                    <KindIcon kind=d.kind.clone() class="sm" />
                                    <span class="ellipsis grow">{d.title.clone()}</span>
                                    <span class="muted small nowrap">{format_size(d.size)}</span>
                                </li>
                            }
                        }).collect_view().into_any(),
                    }}
                </ul>
            </Show>
        </div>
    }
}
