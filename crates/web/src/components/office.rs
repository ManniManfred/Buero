// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Euro-Office-Editor als Vollbild-Ebene über der Oberfläche.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::OfficeConfig;
use wasm_bindgen::prelude::*;

use crate::icons::Icon;
use crate::js::{office_close, office_open};
use crate::state::ctx;

const FRAME_ID: &str = "office-frame";

#[component]
pub fn OfficeEditor(id: i64, title: String) -> impl IntoView {
    let c = ctx();
    let error = RwSignal::new(None::<String>);
    let ready = RwSignal::new(false);
    let dirty = RwSignal::new(false);
    let can_edit = RwSignal::new(false);

    let close = move || {
        office_close();
        c.office_doc.set(None);
        if can_edit.get_untracked() {
            c.notify("Euro-Office speichert die Änderungen in wenigen Sekunden als neue Version", false);
            // Der Document Server meldet das Speichern erst nach dem Schließen –
            // Liste und Detailansicht einige Male nachladen.
            for secs in [4u64, 10, 20, 40] {
                set_timeout(
                    move || {
                        c.detail_refresh.update(|n| *n += 1);
                        c.reload();
                    },
                    std::time::Duration::from_secs(secs),
                );
            }
        }
    };
    crate::nav::guard_back(close);

    spawn_local(async move {
        match c.api.get::<OfficeConfig>(&format!("/api/office/config/{id}")).await {
            Ok(cfg) => {
                can_edit.set(cfg.can_edit);
                let cb = Closure::<dyn FnMut(String, String)>::new(move |kind: String, data: String| {
                    match kind.as_str() {
                        "ready" => ready.set(true),
                        "state" => {
                            if data == "dirty" {
                                dirty.set(true);
                            }
                        }
                        "close" => close(),
                        "error" => error.set(Some(data)),
                        _ => {}
                    }
                });
                office_open(&cfg.script_url, FRAME_ID, &cfg.config, cb.as_ref().unchecked_ref());
                // Der Editor ruft den Callback, solange er lebt
                cb.forget();
            }
            Err(e) => error.set(Some(e)),
        }
    });

    on_cleanup(office_close);

    view! {
        <div class="office-overlay">
            <div class="office-bar">
                <Icon name="file-pen" />
                <span class="grow ellipsis"><b>{title}</b></span>
                <span class="muted small">
                    {move || if !ready.get() {
                        "Euro-Office wird geladen…"
                    } else if !can_edit.get() {
                        "Nur Ansicht"
                    } else if dirty.get() {
                        "Wird beim Schließen als neue Version gespeichert"
                    } else {
                        "Bearbeiten"
                    }}
                </span>
                <button class="primary" on:click=move |_| close()><Icon name="x" />"Schließen"</button>
            </div>
            {move || error.get().map(|e| view! { <div class="notice bad">{e}</div> })}
            <div class="office-frame-wrap"><div id=FRAME_ID></div></div>
        </div>
    }
}
