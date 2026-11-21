// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::is_desktop;
use crate::state::ctx;

/// Pflichtangabe nach § 13 AGPL: Bezugsquelle des Quellcodes
const SOURCE_URL: &str = env!("CARGO_PKG_REPOSITORY");

#[component]
pub fn Settings() -> impl IntoView {
    let c = ctx();
    let server = RwSignal::new(c.api.base.get_untracked());
    let close = move || c.show_settings.set(false);
    view! {
        <div class="modal-bg" on:click=move |_| close()>
            <div class="card modal" on:click=|ev| ev.stop_propagation()>
                <h2>"Einstellungen"</h2>
                <Show when=is_desktop>
                    <label>"Server-Adresse"
                        <input type="url" prop:value=move || server.get() on:input=move |ev| server.set(event_target_value(&ev)) />
                    </label>
                    <button on:click=move |_| {
                        c.api.set_server(&server.get_untracked());
                        let _ = window().location().reload();
                    }>"Speichern & neu verbinden"</button>
                    <hr />
                </Show>
                <h3>"Freigaben"</h3>
                <p class="muted small">"Links zu Dokumenten und Sammlungen, die ohne Anmeldung erreichbar sind."</p>
                <button on:click=move |_| {
                    c.show_settings.set(false);
                    c.share_target.set(Some(crate::state::ShareTarget::default()));
                }><crate::icons::Icon name="link" />"Alle Freigaben verwalten"</button>
                <hr />
                <h3>"Wartung"</h3>
                <p class="muted small">"Suchindex aus der Datenbank neu aufbauen (z. B. nach einem Update)."</p>
                <button on:click=move |_| {
                    spawn_local(async move {
                        match c.api.send_empty::<()>("POST", "/api/admin/reindex", None).await {
                            Ok(()) => { c.notify("Suchindex neu aufgebaut", false); c.reload(); }
                            Err(e) => c.notify(e, true),
                        }
                    });
                }>"Suchindex neu aufbauen"</button>
                <button on:click=move |_| {
                    spawn_local(async move {
                        match c.api.send_empty::<()>("POST", "/api/admin/reprocess-failed", None).await {
                            Ok(()) => { c.notify("Fehlgeschlagene Dokumente werden erneut verarbeitet", false); c.reload(); }
                            Err(e) => c.notify(e, true),
                        }
                    });
                }>"Fehlgeschlagene erneut verarbeiten"</button>
                <hr />
                <h3>"Suche – Tipps"</h3>
                <ul class="small tips">
                    <li><code>"rechnung strom"</code>" – alle Wörter müssen vorkommen, das letzte auch als Wortanfang"</li>
                    <li><code>"\"kaution 1500\""</code>" – genaue Wortfolge"</li>
                    <li><code>"title:vertrag"</code>" – nur im Titel (auch: tags:, filename:, content:)"</li>
                    <li><code>"versicherung -kfz"</code>" – Wort ausschließen"</li>
                    <li><code>"miete OR pacht"</code>" – eines von beiden"</li>
                    <li>"Tippfehler werden automatisch toleriert, wenn es keine exakten Treffer gibt."</li>
                </ul>
                <hr />
                <p class="small muted">
                    "Büro · Copyright © 2026 Manfred Roßkamp · Freie Software unter der "
                    <a href="#" on:click=|ev| { ev.prevent_default(); crate::api::open_external("https://www.gnu.org/licenses/agpl-3.0.html"); }>"GNU AGPL v3"</a>
                    " · "
                    <a href="#" on:click=|ev| { ev.prevent_default(); crate::api::open_external(SOURCE_URL); }>"Quellcode"</a>
                    " · "
                    <a href="#" on:click=move |ev| { ev.prevent_default(); crate::api::open_external(&c.api.url("/licenses.html")); }>
                        "Lizenzen von Drittanbietern"
                    </a>
                </p>
                <div class="row-buttons">
                    <button on:click=move |_| {
                        spawn_local(async move {
                            let _ = c.api.send_empty::<()>("POST", "/api/auth/logout", None).await;
                            c.api.set_token(None);
                            let _ = window().location().reload();
                        });
                    }>"Abmelden"</button>
                    <button class="primary" on:click=move |_| close()>"Schließen"</button>
                </div>
            </div>
        </div>
    }
}
