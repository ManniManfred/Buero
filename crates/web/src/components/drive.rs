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
}

fn mode_label(id: &str) -> &'static str {
    match id {
        "sync" => "Sync-Ordner wie OneDrive (empfohlen)",
        _ => "Laufwerk mit eigenem Buchstaben (benötigt WinFsp)",
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
            {move || unavailable().map(|u| {
                let winfsp = u.contains("WinFsp");
                view! {
                    <p class="small error-text">{u}</p>
                    <Show when=move || winfsp>
                        <button on:click=|_| crate::api::open_external("https://winfsp.dev/rel/")>
                            <Icon name="external" />"WinFsp herunterladen"
                        </button>
                    </Show>
                }
            })}
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
                    <label>{move || if is_sync() { "Ordner (neu oder leer)" } else { "Einhängepunkt (Ordner bzw. Laufwerksbuchstabe)" }}
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
            <hr />
        }})}
    }
}
