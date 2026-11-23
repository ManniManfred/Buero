// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: Sammlungen im Dateimanager (Sync-Ordner oder Laufwerk).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api::invoke;
use crate::icons::Icon;
use crate::state::{ctx, Ctx};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModeInfo {
    pub id: String,
    pub default_path: String,
    pub unavailable: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FolderStatus {
    pub collection_id: i64,
    pub name: String,
    pub path: String,
    pub connected: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DriveStatus {
    pub supported: bool,
    pub unavailable: Option<String>,
    pub enabled: bool,
    pub mounted: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub modes: Vec<ModeInfo>,
    pub mount_point: String,
    pub last_error: Option<String>,
    #[serde(default)]
    pub folders_unavailable: Option<String>,
    #[serde(default)]
    pub folders: Vec<FolderStatus>,
}

fn mode_label(id: &str) -> &'static str {
    match id {
        "sync" => "Sync-Ordner wie OneDrive (empfohlen)",
        _ => "Laufwerk (FUSE)",
    }
}

/// Nach An- oder Abmeldung: Laufwerk (falls eingeschaltet) passend ein-/aushängen.
pub fn sync(c: Ctx, logged_in: bool) {
    let server = c.api.base.get_untracked();
    let token = c.api.token.get_untracked();
    spawn_local(async move {
        let r = invoke::<DriveStatus>("drive_sync", json!({ "server": server, "token": token, "loggedIn": logged_in })).await;
        if let Err(e) = r {
            c.notify(format!("Laufwerk: {e}"), true);
        }
    });
}

#[component]
pub fn DriveSection() -> impl IntoView {
    let c = ctx();
    let status = RwSignal::new(None::<DriveStatus>);
    let mount_point = RwSignal::new(String::new());
    let mode = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let apply = move |r: Result<DriveStatus, String>| {
        busy.set(false);
        match r {
            Ok(s) => {
                mount_point.set(s.mount_point.clone());
                mode.set(s.mode.clone());
                status.set(Some(s));
            }
            Err(e) => c.notify(format!("Laufwerk: {e}"), true),
        }
    };
    spawn_local(async move { apply(invoke("drive_status", json!({})).await) });

    let mount = move |_| {
        busy.set(true);
        let args = json!({
            "server": c.api.base.get_untracked(),
            "token": c.api.token.get_untracked(),
            "mode": Some(mode.get_untracked()).filter(|m| !m.is_empty()),
            "mountPoint": mount_point.get_untracked(),
        });
        spawn_local(async move { apply(invoke("drive_mount", args).await) });
    };
    let unmount = move |_| {
        busy.set(true);
        spawn_local(async move { apply(invoke("drive_unmount", json!({})).await) });
    };
    let open = move |_| {
        spawn_local(async move {
            if let Err(e) = invoke::<()>("drive_open", json!({})).await {
                c.notify(e, true);
            }
        });
    };

    view! {
        {move || status.get().filter(|s| s.supported).map(|s| {
            // Gewählte Art (vor dem Einbinden umschaltbar)
            let cur = move || {
                let m = mode.get();
                status.with(|st| st.as_ref().and_then(|st| st.modes.iter().find(|x| x.id == m).cloned()))
            };
            let unavailable = move || if s.mounted { None } else { cur().and_then(|m| m.unavailable) };
            let is_sync = move || mode.get() == "sync";
            view! {
            <h3>{move || if is_sync() { "Sync-Ordner" } else { "Laufwerk" }}</h3>
            <p class="muted small">
                "Die Sammlungen erscheinen als Ordner im Dateimanager, Dokumente ohne Sammlung im Ordner "
                "„Eingang“. Öffnen, Ändern (neue Version), Hineinkopieren (Upload), Umbenennen und "
                "Verschieben wirken direkt in Büro. Löschen entfernt ein Dokument nur aus der Sammlung – "
                "im „Eingang“ wird es gelöscht."
            </p>
            <Show when=is_sync>
                <p class="muted small">
                    "Dateien werden erst beim Öffnen geladen. Per Rechtsklick „Immer auf diesem Gerät behalten“ "
                    "sind sie auch ohne Verbindung da, „Speicherplatz freigeben“ entfernt die lokale Kopie wieder."
                </p>
            </Show>
            {(!s.mounted && s.modes.len() > 1).then(|| {
                let modes = s.modes.clone();
                view! {
                    <label>"Art"
                        <select prop:value=move || mode.get() on:change=move |ev| {
                            let m = event_target_value(&ev);
                            // Vorgeschlagenen Ort der neuen Art übernehmen
                            if let Some(d) = status.with_untracked(|st| st.as_ref().and_then(|st| st.modes.iter().find(|x| x.id == m).map(|x| x.default_path.clone()))) {
                                mount_point.set(d);
                            }
                            mode.set(m);
                        }>
                            {modes.into_iter().map(|m| {
                                let id = m.id.clone();
                                view! { <option value=id.clone() selected=move || mode.get() == id>{mode_label(&m.id)}</option> }
                            }).collect_view()}
                        </select>
                    </label>
                }
            })}
            {move || unavailable().map(|u| view! { <p class="small error-text">{u}</p> })}
            {if s.mounted {
                view! {
                    <p class="small">{move || if is_sync() { "Verbunden mit " } else { "Eingebunden unter " }}<code>{s.mount_point.clone()}</code></p>
                    <div class="button-row">
                        <button class="primary" on:click=open><Icon name="folder" />"Im Dateimanager öffnen"</button>
                        <button on:click=unmount disabled=move || busy.get()>{move || if is_sync() { "Trennen" } else { "Aushängen" }}</button>
                    </div>
                    <Show when=is_sync>
                        <p class="muted small">"Nach dem Trennen bleiben geladene Dateien im Ordner, es wird aber nichts mehr abgeglichen."</p>
                    </Show>
                }.into_any()
            } else {
                view! {
                    <label>{move || if is_sync() { "Ordner (neu oder leer)" } else { "Einhängepunkt (Ordner)" }}
                        <input prop:value=move || mount_point.get() on:input=move |ev| mount_point.set(event_target_value(&ev)) />
                    </label>
                    <button on:click=mount disabled=move || busy.get() || unavailable().is_some()>
                        <Icon name="folder" />{move || match (busy.get(), is_sync()) {
                            (true, _) => "Wird eingerichtet …",
                            (false, true) => "Sync-Ordner einrichten",
                            (false, false) => "Als Laufwerk einbinden",
                        }}
                    </button>
                }.into_any()
            }}
            {s.last_error.clone().map(|e| view! {
                <div class="small error-text">{e.lines().map(|l| view! { <p>{l.to_string()}</p> }).collect_view()}</div>
            })}
            {s.folders_unavailable.is_none().then(|| view! { <FolderSection status=status /> })}
            <Show when=move || status.with(|st| st.as_ref().is_some_and(|st| (st.mounted && st.mode == "sync") || !st.folders.is_empty()))>
                <button on:click=move |_| spawn_local(async move {
                    if let Err(e) = invoke::<()>("activity_open", json!({})).await {
                        c.notify(e, true);
                    }
                })>
                    <Icon name="history" />"Aktivität und Fehlerprotokoll"
                </button>
            </Show>
            <hr />
        }})}
    }
}

/// Mit der Anmeldung am Computer starten (nur im Infobereich, wenn etwas
/// abzugleichen ist)
#[component]
pub fn AutostartSection() -> impl IntoView {
    let c = ctx();
    // `None`: unbekannt (z. B. vom System nicht unterstützt) – dann nichts zeigen
    let on = RwSignal::new(None::<bool>);
    spawn_local(async move { on.set(invoke::<bool>("autostart_get", json!({})).await.ok()) });
    let toggle = move |ev: leptos::ev::Event| {
        let want = event_target_checked(&ev);
        spawn_local(async move {
            match invoke::<bool>("autostart_set", json!({ "enabled": want })).await {
                Ok(v) => on.set(Some(v)),
                Err(e) => {
                    // Schalter auf den tatsächlichen Stand zurücksetzen
                    on.set(on.get_untracked());
                    c.notify(e, true);
                }
            }
        });
    };
    view! {
        <Show when=move || on.get().is_some()>
            <label class="dialog-check">
                <input type="checkbox" prop:checked=move || on.get().unwrap_or(false) on:change=toggle />
                "Beim Anmelden am Computer starten"
            </label>
            <p class="muted small">
                "Büro startet dann unsichtbar im Infobereich und gleicht Sync-Ordner im Hintergrund ab. "
                "Abgeglichen wird nur, solange Büro läuft."
            </p>
        </Show>
    }
}

/// Weitere Sync-Ordner, je einer Sammlung zugeordnet
#[component]
fn FolderSection(status: RwSignal<Option<DriveStatus>>) -> impl IntoView {
    let c = ctx();
    let coll = RwSignal::new(String::new());
    let path = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let folders = move || status.with(|s| s.as_ref().map(|s| s.folders.clone()).unwrap_or_default());

    let apply = move |r: Result<DriveStatus, String>| {
        busy.set(false);
        match r {
            Ok(s) => status.set(Some(s)),
            Err(e) => c.notify(format!("Sync-Ordner: {e}"), true),
        }
    };
    let add = move |_| {
        let Ok(id) = coll.get_untracked().parse::<i64>() else { return };
        let name = c.collections.with_untracked(|cs| cs.iter().find(|x| x.id == id).map(|x| x.name.clone())).unwrap_or_default();
        busy.set(true);
        let args = json!({
            "server": c.api.base.get_untracked(),
            "token": c.api.token.get_untracked(),
            "collectionId": id,
            "name": name,
            "path": Some(path.get_untracked()).filter(|p| !p.trim().is_empty()),
        });
        spawn_local(async move {
            let r = invoke::<DriveStatus>("folder_add", args).await;
            if r.is_ok() {
                coll.set(String::new());
                path.set(String::new());
            }
            apply(r)
        });
    };
    let remove = move |id: i64| {
        busy.set(true);
        spawn_local(async move { apply(invoke("folder_remove", json!({ "collectionId": id })).await) });
    };
    let open = move |id: i64| {
        spawn_local(async move {
            if let Err(e) = invoke::<()>("drive_open", json!({ "collectionId": id })).await {
                c.notify(e, true);
            }
        });
    };
    // Sammlungen mit vollem Pfad, ohne die schon eingerichteten
    let options = move || {
        let used: Vec<i64> = folders().iter().map(|f| f.collection_id).collect();
        let mut v: Vec<(i64, String)> = c.collections.with(|cs| {
            cs.iter().filter(|x| !used.contains(&x.id)).map(|x| (x.id, crate::state::collection_path(cs, x.id))).collect()
        });
        v.sort_by_key(|(_, p)| p.to_lowercase());
        v
    };

    view! {
        <h3>"Sync-Ordner für einzelne Sammlungen"</h3>
        <p class="muted small">
            "Zusätzlich lässt sich jede Sammlung mit einem eigenen Ordner abgleichen, z. B. „Steuer“ mit "
            "„Dokumente\\Steuer“. Der Ordner entspricht dann der Sammlung, Unterordner ihren Unter-Sammlungen; "
            "neue Dateien landen in dieser Sammlung."
        </p>
        <Show when=move || !folders().is_empty()>
            <ul class="folder-list">
                <For each=folders key=|f| (f.collection_id, f.path.clone(), f.connected, f.last_error.clone()) let:f>
                    {
                        let id = f.collection_id;
                        let name = c.collections.with_untracked(|cs| cs.iter().any(|x| x.id == id))
                            .then(|| c.collection_name(id))
                            .unwrap_or(f.name.clone());
                        view! {
                            <li>
                                <div class="folder-head">
                                    <Icon name="folder" />
                                    <strong>{name}</strong>
                                    <span class=if f.connected { "pill ok" } else { "pill" }>
                                        {if f.connected { "verbunden" } else { "getrennt" }}
                                    </span>
                                </div>
                                <code class="small">{f.path.clone()}</code>
                                {f.last_error.clone().map(|e| view! {
                                    <div class="small error-text">{e.lines().map(|l| view! { <p>{l.to_string()}</p> }).collect_view()}</div>
                                })}
                                <div class="button-row">
                                    <Show when=move || f.connected>
                                        <button on:click=move |_| open(id)><Icon name="external" />"Öffnen"</button>
                                    </Show>
                                    <button on:click=move |_| remove(id) disabled=move || busy.get()
                                        title="Abgleich beenden und bei Windows abmelden. Geladene Dateien bleiben im Ordner.">
                                        "Entfernen"
                                    </button>
                                </div>
                            </li>
                        }
                    }
                </For>
            </ul>
        </Show>
        <div class="folder-add">
            <label>"Sammlung"
                <select prop:value=move || coll.get() on:change=move |ev| coll.set(event_target_value(&ev))>
                    <option value="">"– Sammlung wählen –"</option>
                    {move || options().into_iter().map(|(id, p)| view! { <option value=id.to_string()>{p}</option> }).collect_view()}
                </select>
            </label>
            <label>"Ordner (neu oder leer, leer lassen für Vorschlag)"
                <input prop:value=move || path.get() on:input=move |ev| path.set(event_target_value(&ev))
                    placeholder=move || {
                        let n = coll.get().parse::<i64>().ok()
                            .and_then(|id| c.collections.with(|cs| cs.iter().find(|x| x.id == id).map(|x| x.name.clone())));
                        let base = status.with(|s| s.as_ref().and_then(|s| s.modes.iter().find(|m| m.id == "sync").map(|m| m.default_path.clone()))).unwrap_or_default();
                        match n {
                            Some(n) if !base.is_empty() => format!("{base} - {n}"),
                            _ => String::new(),
                        }
                    } />
            </label>
            <button on:click=add disabled=move || busy.get() || coll.get().is_empty()>
                <Icon name="folder" />{move || if busy.get() { "Wird eingerichtet …" } else { "Sync-Ordner hinzufügen" }}
            </button>
        </div>
    }
}
