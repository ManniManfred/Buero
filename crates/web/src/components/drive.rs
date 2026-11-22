// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: Sammlungen als Laufwerk im Dateimanager.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::json;

use crate::api::invoke;
use crate::icons::Icon;
use crate::state::{ctx, Ctx};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DriveStatus {
    pub supported: bool,
    pub unavailable: Option<String>,
    pub enabled: bool,
    pub mounted: bool,
    pub mount_point: String,
    pub last_error: Option<String>,
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
    let busy = RwSignal::new(false);

    let apply = move |r: Result<DriveStatus, String>| {
        busy.set(false);
        match r {
            Ok(s) => {
                mount_point.set(s.mount_point.clone());
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
        {move || status.get().filter(|s| s.supported).map(|s| view! {
            <h3>"Laufwerk"</h3>
            <p class="muted small">
                "Die Sammlungen erscheinen als Ordner im Dateimanager, Dokumente ohne Sammlung im Ordner "
                "„Eingang“. Öffnen, Ändern (neue Version), Hineinkopieren (Upload), Umbenennen und "
                "Verschieben wirken direkt in Büro. Löschen entfernt ein Dokument nur aus der Sammlung – "
                "im „Eingang“ wird es gelöscht."
            </p>
            {s.unavailable.clone().map(|u| {
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
                    <p class="small">"Eingebunden unter "<code>{s.mount_point.clone()}</code></p>
                    <div class="button-row">
                        <button class="primary" on:click=open><Icon name="folder" />"Im Dateimanager öffnen"</button>
                        <button on:click=unmount disabled=move || busy.get()>"Aushängen"</button>
                    </div>
                }.into_any()
            } else {
                let ok = s.unavailable.is_none();
                view! {
                    <label>"Einhängepunkt (Ordner bzw. Laufwerksbuchstabe)"
                        <input prop:value=move || mount_point.get() on:input=move |ev| mount_point.set(event_target_value(&ev)) />
                    </label>
                    <button on:click=mount disabled=move || busy.get() || !ok>
                        <Icon name="folder" />{move || if busy.get() { "Wird eingebunden …" } else { "Als Laufwerk einbinden" }}
                    </button>
                }.into_any()
            }}
            {s.last_error.clone().map(|e| view! { <p class="small error-text">{e}</p> })}
            <hr />
        })}
    }
}
