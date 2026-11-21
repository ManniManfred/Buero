// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Tupfer Designer (tiptoi-Projekte, `.tupfer`) als Vollbild-Ebene über der Oberfläche.
//!
//! Büro liefert die Web-Version des Designers unter `/tupfer/` aus. Sie lädt das
//! Dokument selbst über die API (`?buero=<id>`, Anmeldung im Fragment `#token=…`)
//! und speichert mit „Speichern“ eine neue Version. Meldungen kommen per
//! `postMessage` (auch im Desktop-Client, wo der iframe einen anderen Ursprung hat).

use leptos::prelude::*;
use wasm_bindgen::prelude::*;

use crate::icons::Icon;
use crate::js::{tupfer_ask, tupfer_listen, tupfer_unlisten};
use crate::state::ctx;

const FRAME_ID: &str = "tupfer-frame";

/// So lange auf die Antwort „ungespeicherte Änderungen?“ warten, dann ohne Rückfrage schließen
/// (Designer lädt noch oder ist eine ältere Version ohne Büro-Anbindung)
const ASK_TIMEOUT_MS: u64 = 800;

#[component]
pub fn TupferEditor(id: i64, title: String) -> impl IntoView {
    let c = ctx();
    let saved = RwSignal::new(false);
    // Laufende Rückfrage beim Schließen (Zähler, damit eine späte Antwort nichts mehr auslöst)
    let asking = RwSignal::new(0u32);

    let mut src = c.api.url(&format!("/tupfer/?buero={id}"));
    if let Some(t) = c.api.token.get_untracked() {
        src.push_str("#token=");
        src.push_str(&String::from(js_sys::encode_uri_component(&t)));
    }

    let close_now = move || {
        tupfer_unlisten();
        c.tupfer_doc.set(None);
        if saved.get_untracked() {
            c.detail_refresh.update(|n| *n += 1);
            c.reload();
        }
    };

    let cb = Closure::<dyn FnMut(String, String, bool)>::new(move |kind: String, text: String, modified: bool| {
        match kind.as_str() {
            "gespeichert" => {
                saved.set(true);
                c.notify(text, false);
                c.detail_refresh.update(|n| *n += 1);
                c.reload();
            }
            "status" if asking.get_untracked() > 0 => {
                asking.set(0);
                if !modified
                    || window()
                        .confirm_with_message("Das Projekt hat ungespeicherte Änderungen. Trotzdem schließen?")
                        .unwrap_or(false)
                {
                    close_now();
                }
            }
            _ => {}
        }
    });
    tupfer_listen(FRAME_ID, cb.as_ref().unchecked_ref());
    // Lebt, solange die Ebene offen ist; tupfer_unlisten löst die Verbindung
    cb.forget();
    on_cleanup(tupfer_unlisten);

    let close = move || {
        if !tupfer_ask(FRAME_ID) {
            close_now();
            return;
        }
        let n = asking.get_untracked().wrapping_add(1).max(1);
        asking.set(n);
        set_timeout(
            move || {
                if asking.get_untracked() == n {
                    asking.set(0);
                    close_now();
                }
            },
            std::time::Duration::from_millis(ASK_TIMEOUT_MS),
        );
    };

    view! {
        <div class="office-overlay">
            <div class="office-bar">
                <Icon name="file-pen" />
                <span class="grow ellipsis"><b>{title}</b></span>
                <span class="muted small">"Tupfer Designer – „Speichern“ legt eine neue Version an"</span>
                <button class="primary" on:click=move |_| close()><Icon name="x" />"Schließen"</button>
            </div>
            <div class="office-frame-wrap">
                <iframe id=FRAME_ID src=src allow="clipboard-read; clipboard-write; fullscreen"></iframe>
            </div>
        </div>
    }
}
