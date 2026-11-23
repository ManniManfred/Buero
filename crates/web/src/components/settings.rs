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
                <Show when=is_desktop>
                    <crate::components::drive::DriveSection />
                    <crate::components::update::UpdateSection />
                    <hr />
                </Show>
                <Show when=|| !is_desktop()>
                    <crate::components::desktop::DesktopSection />
                    <hr />
                </Show>
                <h3>"Freigaben"</h3>
                <p class="muted small">"Links zu Dokumenten und Sammlungen, die ohne Anmeldung erreichbar sind."</p>
                <button on:click=move |_| {
                    c.show_settings.set(false);
                    c.share_target.set(Some(crate::state::ShareTarget::default()));
                }><crate::icons::Icon name="link" />"Alle Freigaben verwalten"</button>
                <hr />
                <crate::components::mail::MailSection />
                <hr />
                <crate::components::mcp::McpSection />
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
                <BackupSection />                <hr />
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
                    "Büro "{crate::util::version()}" · Copyright © 2026 Manfred Roßkamp · Freie Software unter der "
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
                            if is_desktop() {
                                crate::components::drive::sync(c, false);
                            }
                            c.api.set_token(None);
                            let _ = window().location().reload();
                        });
                    }>"Abmelden"</button>
                    <button title="Beendet alle Anmeldungen auf allen Geräten, auch diese" on:click=move |_| {
                        spawn_local(async move {
                            if !crate::dialog::confirm("Auf allen Geräten abmelden?", "Danach muss man sich überall neu anmelden, auch hier.")
                                .ok("Überall abmelden")
                                .ask_ok()
                                .await
                            {
                                return;
                            }
                            match c.api.send_empty::<()>("POST", "/api/auth/logout-all", None).await {
                                Ok(()) => {
                                    if is_desktop() {
                                        crate::components::drive::sync(c, false);
                                    }
                                    c.api.set_token(None);
                                    let _ = window().location().reload();
                                }
                                Err(e) => c.notify(e, true),
                            }
                        });
                    }>"Überall abmelden"</button>
                    <button class="primary" on:click=move |_| close()>"Schließen"</button>
                </div>
            </div>
        </div>
    }
}

/// Backup herunterladen und aus einem Backup wiederherstellen.
#[component]
fn BackupSection() -> impl IntoView {
    let c = ctx();
    let input = NodeRef::<leptos::html::Input>::new();
    let busy = RwSignal::new(false);

    // Backup über einen signierten, fünf Minuten gültigen Link laden
    // (im Desktop-Client im Standardbrowser)
    let download = move |_| {
        spawn_local(async move {
            match c.api.signed_link("/api/admin/backup-link").await {
                Ok(url) if is_desktop() => crate::api::open_external(&url),
                Ok(url) => {
                    let _ = window().location().assign(&url);
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    let restore = move |_| {
        let Some(i) = input.get() else { return };
        let Some(file) = i.files().and_then(|f| f.get(0)) else { return };
        i.set_value("");
        let msg = format!(
            "Alle Dokumente, Sammlungen, Schlagwörter und Freigaben werden durch den Stand aus „{}“ ersetzt.\n\
             Der bisherige Stand bleibt auf dem Server im Ordner „vor-wiederherstellung“.",
            file.name()
        );
        spawn_local(async move {
            if !crate::dialog::confirm("Backup einspielen?", msg).ok("Einspielen").danger().ask_ok().await {
                return;
            }
            busy.set(true);
            match c.api.post_file::<shared::RestoreResult>("/api/admin/restore", &file).await {
                Ok(r) => {
                    let mut m = format!("Backup mit {} Dokument(en) wird eingespielt, der Server startet neu …", r.documents);
                    if r.missing_files > 0 {
                        m.push_str(&format!(" Achtung: {} Datei(en) fehlen im Backup.", r.missing_files));
                    }
                    c.notify(m, r.missing_files > 0);
                    wait_and_reload(c.api);
                }
                Err(e) => {
                    busy.set(false);
                    c.notify(format!("Wiederherstellung fehlgeschlagen: {e}"), true);
                }
            }
        });
    };

    view! {
        <h3>"Datensicherung"</h3>
        <p class="muted small">
            "Das Backup enthält alle Dokumente mit ihren Versionen, Sammlungen, Schlagwörter, Freigaben "
            "und den Schlüssel für Freigabelinks. Der Suchindex wird nach dem Einspielen neu aufgebaut."
        </p>
        <button on:click=download>
            <crate::icons::Icon name="download" />"Backup herunterladen"
        </button>
        <button disabled=move || busy.get() on:click=move |_| { if let Some(i) = input.get() { i.click(); } }>
            <crate::icons::Icon name="upload" />
            {move || if busy.get() { "Wird eingespielt …" } else { "Aus Backup wiederherstellen" }}
        </button>
        <input type="file" accept=".tar,application/x-tar" style="display:none" node_ref=input on:change=restore />
    }
}

/// Wartet, bis der Server nach dem Neustart wieder antwortet, und lädt dann neu.
fn wait_and_reload(api: crate::api::Api) {
    spawn_local(async move {
        let url = api.url("/api/health");
        for _ in 0..120 {
            sleep_ms(1_000).await;
            if let Ok(r) = gloo_net::http::Request::get(&url).send().await {
                if r.ok() {
                    let _ = window().location().reload();
                    return;
                }
            }
        }
        let _ = window().location().reload();
    });
}

async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}
