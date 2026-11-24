// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: eigenes Fenster mit Zustand, letzten Aktivitäten und
//! Fehlern der Sync-Ordner (aus dem Infobereich oder den Einstellungen).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api::invoke;
use crate::icons::Icon;

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Entry {
    time: i64,
    kind: String,
    path: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct State {
    running: bool,
    last_sync: Option<i64>,
    pending: bool,
    errors: Vec<String>,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Folder {
    collection_id: Option<i64>,
    name: String,
    path: String,
    connected: bool,
    state: Option<State>,
}

/// So oft wird der Zustand neu geholt
const REFRESH_MS: u64 = 2000;

/// Soll diese Seite die Aktivitätsansicht zeigen? (setzt der Desktop-Client
/// beim Öffnen des Fensters)
pub fn is_activity_window() -> bool {
    js_sys::Reflect::get(&window(), &"__BUERO_VIEW__".into())
        .ok()
        .and_then(|v| v.as_string())
        .is_some_and(|v| v == "activity")
}

fn now() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// Uhrzeit (heute) bzw. Datum und Uhrzeit in Ortszeit
fn when(ts: i64) -> String {
    let d = js_sys::Date::new(&((ts as f64) * 1000.0).into());
    let today = js_sys::Date::new_0();
    let time = format!("{:02}:{:02}:{:02}", d.get_hours(), d.get_minutes(), d.get_seconds());
    if d.get_date() == today.get_date() && d.get_month() == today.get_month() && d.get_full_year() == today.get_full_year() {
        time
    } else {
        format!("{:02}.{:02}. {time}", d.get_date(), d.get_month() + 1)
    }
}

fn ago(ts: i64) -> String {
    match (now() - ts).max(0) {
        s if s < 10 => "gerade eben".into(),
        s if s < 60 => format!("vor {s} Sekunden"),
        s if s < 120 => "vor einer Minute".into(),
        s if s < 3600 => format!("vor {} Minuten", s / 60),
        _ => format!("um {}", when(ts)),
    }
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "upload" => "upload",
        "download" => "download",
        "change" => "pencil",
        "remove" => "trash",
        _ => "alert",
    }
}

fn kind_title(kind: &str) -> &'static str {
    match kind {
        "upload" => "Zum Server übertragen",
        "download" => "Vom Server übernommen",
        "change" => "Umbenannt oder verschoben",
        "remove" => "Entfernt",
        _ => "Fehler",
    }
}

#[component]
pub fn ActivityWindow() -> impl IntoView {
    let folders = RwSignal::new(None::<Vec<Folder>>);
    let only_errors = RwSignal::new(false);
    let message = RwSignal::new(None::<String>);

    let load = move || {
        spawn_local(async move {
            match invoke::<Vec<Folder>>("sync_activity", json!({})).await {
                Ok(f) => folders.set(Some(f)),
                Err(e) => message.set(Some(e)),
            }
        })
    };
    load();
    // Laufend aktualisieren, solange das Fenster offen ist
    let handle = set_interval_with_handle(load, std::time::Duration::from_millis(REFRESH_MS)).ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });

    let sync_all = move |_| {
        spawn_local(async move {
            let _ = invoke::<()>("sync_now", json!({ "collectionId": null })).await;
        });
    };
    let open_log = move |_| {
        spawn_local(async move {
            if let Err(e) = invoke::<()>("sync_log_open", json!({})).await {
                message.set(Some(e));
            }
        });
    };

    view! {
        <div class="activity-page">
            <header class="activity-head">
                <h2><Icon name="history" />"Synchronisierung"</h2>
                <div class="button-row">
                    <button on:click=sync_all title="Alle Sync-Ordner sofort abgleichen"><Icon name="rotate" />"Jetzt abgleichen"</button>
                    <button on:click=open_log title="Protokolldatei mit allen Aktivitäten und Fehlern öffnen"><Icon name="external" />"Protokoll"</button>
                </div>
            </header>
            <label class="dialog-check small">
                <input type="checkbox" prop:checked=move || only_errors.get() on:change=move |ev| only_errors.set(event_target_checked(&ev)) />
                "Nur Fehler zeigen"
            </label>
            {move || message.get().map(|m| view! { <p class="small error-text">{m}</p> })}
            {move || match folders.get() {
                None => view! { <p class="muted">"Wird geladen …"</p> }.into_any(),
                Some(list) if list.is_empty() => view! {
                    <p class="muted">"Es ist kein Sync-Ordner eingerichtet. Das geht in Büro unter Einstellungen."</p>
                }.into_any(),
                Some(list) => list
                    .into_iter()
                    .map(|f| view! { <FolderCard folder=f only_errors=only_errors message=message /> })
                    .collect_view()
                    .into_any(),
            }}
        </div>
    }
}

#[component]
fn FolderCard(folder: Folder, only_errors: RwSignal<bool>, message: RwSignal<Option<String>>) -> impl IntoView {
    let Folder { collection_id, name, path, connected, state } = folder;
    let (status, class) = match &state {
        _ if !connected => ("getrennt".to_string(), "pill"),
        None => ("verbunden".to_string(), "pill ok"),
        Some(s) if s.running => ("wird abgeglichen …".to_string(), "pill busy"),
        Some(s) if !s.errors.is_empty() => (
            if s.errors.len() == 1 { "1 Problem".to_string() } else { format!("{} Probleme", s.errors.len()) },
            "pill bad",
        ),
        Some(s) if s.pending => ("gleich wieder …".to_string(), "pill busy"),
        Some(_) => ("aktuell".to_string(), "pill ok"),
    };
    let last = state.as_ref().and_then(|s| s.last_sync).map(|t| format!("Zuletzt abgeglichen {}", ago(t)));
    let errors = state.as_ref().map(|s| s.errors.clone()).unwrap_or_default();
    let entries = state.map(|s| s.entries).unwrap_or_default();
    let sync = move |_| {
        spawn_local(async move {
            let _ = invoke::<()>("sync_now", json!({ "collectionId": collection_id })).await;
        });
    };
    let open = move |_| {
        spawn_local(async move {
            if let Err(e) = invoke::<()>("drive_open", json!({ "collectionId": collection_id })).await {
                message.set(Some(e));
            }
        });
    };

    view! {
        <section class="card activity-card">
            <div class="folder-head">
                <Icon name="folder" />
                <strong>{name}</strong>
                <span class=class>{status}</span>
                <Show when=move || connected>
                    <button class="icon" title="Ordner im Explorer öffnen" on:click=open><Icon name="folder-open" /></button>
                    <button class="icon" title="Diesen Ordner sofort abgleichen" on:click=sync><Icon name="rotate" /></button>
                </Show>
            </div>
            <code class="small">{path}</code>
            {last.map(|l| view! { <p class="small muted">{l}</p> })}
            {(!errors.is_empty()).then(|| view! {
                <div class="activity-errors">
                    <strong class="small">"Aktuelle Probleme"</strong>
                    <ul>{errors.into_iter().map(|e| view! { <li class="small">{e}</li> }).collect_view()}</ul>
                </div>
            })}
            {move || {
                let shown: Vec<Entry> = entries.iter().filter(|e| !only_errors.get() || e.kind == "error").cloned().collect();
                if shown.is_empty() {
                    let text = if only_errors.get() { "Keine Fehler protokolliert." } else { "Noch keine Aktivitäten." };
                    return view! { <p class="small muted">{text}</p> }.into_any();
                }
                view! {
                    <ul class="activity-list">
                        {shown.into_iter().map(|e| {
                            let err = e.kind == "error";
                            view! {
                                <li class:error-text=err>
                                    <span class="activity-icon" title=kind_title(&e.kind)><Icon name=kind_icon(&e.kind) /></span>
                                    <span class="activity-time small muted">{when(e.time)}</span>
                                    <span class="activity-what small">
                                        {(!e.path.is_empty()).then(|| view! { <span class="activity-path">{e.path.clone()}</span> })}
                                        <span>{e.text.clone()}</span>
                                    </span>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </section>
    }
}
