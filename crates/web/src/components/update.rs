// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: Updates vom eigenen Büro-Server.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api::invoke;
use crate::icons::Icon;
use crate::state::{ctx, Ctx};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct UpdateInfo {
    /// Updates sind in diesem Build eingeschaltet (Signaturschlüssel vorhanden)
    pub available: bool,
    pub current: String,
    /// Neuere Version auf dem Server
    pub version: Option<String>,
}

/// Beim Server nach einer neueren Version fragen. `manual`: auch melden,
/// wenn es nichts Neues gibt oder die Suche fehlschlägt.
pub fn check(c: Ctx, manual: bool) {
    let server = c.api.base.get_untracked();
    spawn_local(async move {
        match invoke::<UpdateInfo>("update_check", json!({ "server": server })).await {
            Ok(info) => {
                if manual {
                    match (&info.version, info.available) {
                        (None, true) => c.notify(format!("Büro {} ist aktuell", info.current), false),
                        (None, false) => c.notify("Updates sind in dieser Version nicht eingeschaltet", true),
                        _ => {}
                    }
                }
                c.update_dismissed.set(false);
                c.update.set(Some(info));
            }
            Err(e) if manual => c.notify(e, true),
            Err(_) => {}
        }
    });
}

fn install(c: Ctx, busy: RwSignal<bool>) {
    busy.set(true);
    spawn_local(async move {
        // Bei Erfolg startet die App neu, es kommt keine Antwort mehr
        if let Err(e) = invoke::<()>("update_install", json!({})).await {
            busy.set(false);
            c.notify(e, true);
        }
    });
}

/// Hinweis unten rechts, wenn eine neue Version bereitsteht
#[component]
pub fn UpdateBanner() -> impl IntoView {
    let c = ctx();
    let busy = RwSignal::new(false);
    move || {
        let info = c.update.get()?;
        let version = info.version.clone()?;
        if c.update_dismissed.get() {
            return None;
        }
        Some(view! {
            <div class="card update-banner" role="status">
                <Icon name="download" />
                <div class="update-text">
                    <strong>{format!("Büro {version} ist verfügbar")}</strong>
                    <span class="muted small">
                        {move || if busy.get() {
                            "Wird heruntergeladen und installiert … Büro startet danach neu.".to_string()
                        } else {
                            format!("Installiert ist {}. Das Laufwerk wird dabei kurz ausgehängt.", info.current)
                        }}
                    </span>
                </div>
                <div class="button-row">
                    <button class="primary" disabled=move || busy.get() on:click=move |_| install(c, busy)>
                        "Jetzt aktualisieren"
                    </button>
                    <button disabled=move || busy.get() on:click=move |_| c.update_dismissed.set(true)>"Später"</button>
                </div>
            </div>
        })
    }
}

/// Abschnitt in den Einstellungen des Desktop-Clients
#[component]
pub fn UpdateSection() -> impl IntoView {
    let c = ctx();
    let busy = RwSignal::new(false);
    view! {
        <h3>"Updates"</h3>
        <p class="muted small">
            "Neue Versionen kommen vom Büro-Server, mit dem die App verbunden ist, und passen damit immer zu ihm. "
            "Gesucht wird nach jeder Anmeldung."
        </p>
        {move || {
            let info = c.update.get();
            let current = info.as_ref().map(|i| i.current.clone()).unwrap_or_default();
            match info.and_then(|i| i.version) {
                Some(v) => view! {
                    <p class="small">{format!("Installiert: {current} · verfügbar: {v}")}</p>
                    <div class="button-row">
                        <button class="primary" disabled=move || busy.get() on:click=move |_| install(c, busy)>
                            {move || if busy.get() { "Wird installiert …" } else { "Jetzt aktualisieren" }}
                        </button>
                    </div>
                }.into_any(),
                None => view! {
                    <div class="button-row">
                        <button on:click=move |_| check(c, true)>"Nach Updates suchen"</button>
                    </div>
                }.into_any(),
            }
        }}
    }
}
