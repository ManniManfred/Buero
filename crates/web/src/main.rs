// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Oberfläche (Web & Desktop)

mod api;
mod components;
mod dialog;
mod player;
mod icons;
mod js;
mod markdown;
mod nav;
mod state;
mod util;

use leptos::prelude::*;
use leptos::task::spawn_local;

use components::{detail::DetailPanel, results::Results, sidebar::Sidebar, toolbar::Toolbar, upload::{DropOverlay, UploadPanel}};
use icons::Icon;
use state::{ctx, Ctx};

fn main() {
    console_error_panic_hook::set_once();
    // Freigabelinks: /s/<token> zeigt die öffentliche Seite ohne Anmeldung
    let path = window().location().pathname().unwrap_or_default();
    if let Some(token) = path.strip_prefix("/s/").map(|t| t.trim_end_matches('/').to_string()).filter(|t| !t.is_empty()) {
        leptos::mount::mount_to_body(move || view! { <components::public::PublicPage token=token.clone() /> });
        return;
    }
    // Desktop-Client: eigenes Fenster mit der Aktivität der Sync-Ordner
    if components::activity::is_activity_window() {
        leptos::mount::mount_to_body(components::activity::ActivityWindow);
        return;
    }
    leptos::mount::mount_to_body(App);
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Loading,
    NeedServer,
    Login,
    Ready,
}

#[component]
fn App() -> impl IntoView {
    let c = Ctx::new();
    provide_context(c);
    player::provide();
    let phase = RwSignal::new(Phase::Loading);

    let check = move || {
        if api::is_desktop() && c.api.base.get_untracked().is_empty() {
            phase.set(Phase::NeedServer);
            return;
        }
        spawn_local(async move {
            match c.api.get::<shared::AuthStatus>("/api/auth/status").await {
                Ok(s) if s.authenticated => {
                    c.api.unauthorized.set(false);
                    // Schlüssel für Vorschau & Co. vor dem ersten Bild holen
                    if let Err(e) = c.api.load_file_key().await {
                        c.notify(e, true);
                    }
                    phase.set(Phase::Ready);
                    if api::is_desktop() {
                        components::drive::sync(c, true);
                        components::update::check(c, false);
                    }
                }
                Ok(_) => phase.set(Phase::Login),
                Err(e) => {
                    c.notify(e, true);
                    phase.set(if api::is_desktop() { Phase::NeedServer } else { Phase::Login });
                }
            }
        });
    };
    check();

    // 401 während der Arbeit -> zurück zur Anmeldung
    Effect::new(move |_| {
        if c.api.unauthorized.get() && phase.get_untracked() == Phase::Ready {
            c.api.set_token(None);
            if api::is_desktop() {
                components::drive::sync(c, false);
            }
            phase.set(Phase::Login);
        }
    });

    // Desktop: Nach einem Autostart ist das Fenster versteckt. Braucht es eine
    // Eingabe (Server, Anmeldung), muss es sichtbar werden.
    Effect::new(move |_| {
        if api::is_desktop() && matches!(phase.get(), Phase::NeedServer | Phase::Login) {
            spawn_local(async move {
                let _ = api::invoke::<()>("show_window", serde_json::json!({})).await;
            });
        }
    });

    if api::is_desktop() {
        // Fenster ohne Systemrahmen: eigene Titelleiste (Abstände über CSS)
        let _ = document().document_element().map(|e| e.class_list().add_1("desktop"));
    }

    view! {
        <Show when=api::is_desktop>
            <components::titlebar::TitleBar ready=Signal::derive(move || phase.get() == Phase::Ready) />
        </Show>
        {move || match phase.get() {
            Phase::Loading => view! { <div class="center muted">"Lade…"</div> }.into_any(),
            Phase::NeedServer => view! { <ServerSetup on_done=Callback::new(move |_| check()) /> }.into_any(),
            Phase::Login => view! { <Login on_done=Callback::new(move |_| check()) /> }.into_any(),
            Phase::Ready => view! { <Main /> }.into_any(),
        }}
        <Show when=api::is_desktop>
            <components::update::UpdateBanner />
        </Show>
        <Toast />
        <dialog::DialogHost />
    }
}

#[component]
fn Toast() -> impl IntoView {
    let c = ctx();
    view! {
        {move || c.toast.get().map(|(m, err)| view! {
            <div class="toast" class:error=err on:click=move |_| c.toast.set(None)>{m}</div>
        })}
    }
}

#[component]
fn ServerSetup(on_done: Callback<()>) -> impl IntoView {
    let c = ctx();
    let url = RwSignal::new(c.api.base.get_untracked());
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let mut u = url.get_untracked().trim().to_string();
        if !u.starts_with("http://") && !u.starts_with("https://") {
            u = format!("https://{u}");
        }
        c.api.set_server(&u);
        on_done.run(());
    };
    view! {
        <div class="center">
            <form class="card login" on:submit=submit>
                <h1 class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"</h1>
                <p class="muted">"Adresse deines Büro-Servers (z. B. aus CapRover):"</p>
                <input type="url" placeholder="https://buero.example.com" required
                    prop:value=move || url.get()
                    on:input=move |ev| url.set(event_target_value(&ev)) />
                <button class="primary" type="submit">"Verbinden"</button>
            </form>
        </div>
    }
}

#[component]
fn Login(on_done: Callback<()>) -> impl IntoView {
    let c = ctx();
    let pw = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        spawn_local(async move {
            let r = c
                .api
                .send::<_, shared::LoginResponse>(
                    "POST",
                    "/api/auth/login",
                    &shared::LoginRequest { password: pw.get_untracked() },
                )
                .await;
            busy.set(false);
            match r {
                Ok(t) => {
                    c.api.set_token(Some(t.token));
                    on_done.run(());
                }
                Err(e) => c.notify(e, true),
            }
        });
    };
    view! {
        <div class="center login-page">
            <form class="card login" on:submit=submit>
                <h1 class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"</h1>
                <input type="password" placeholder="Passwort" autofocus
                    prop:value=move || pw.get()
                    on:input=move |ev| pw.set(event_target_value(&ev)) />
                <button class="primary" type="submit" disabled=move || busy.get()>"Anmelden"</button>
                <Show when=api::is_desktop>
                    <button type="button" class="link" on:click=move |_| { c.api.set_server(""); on_done.run(()); }>
                        "Anderen Server wählen"
                    </button>
                </Show>
            </form>
            <Show when=|| !api::is_desktop()>
                <crate::components::desktop::DesktopLoginLink />
            </Show>
        </div>
    }
}

#[component]
fn Main() -> impl IntoView {
    let c = ctx();
    c.load_meta();
    // Ansicht <-> Adresse und Browser-Verlauf (vor der ersten Suche)
    nav::init(c);

    // Filter/Sortierung geändert -> sofort suchen; Suchtext -> kurz entprellt
    Effect::new(move |_| {
        c.collection.track();
        c.include_sub.track();
        c.tag_filter.track();
        c.kind.track();
        c.unfiled.track();
        c.sort.track();
        c.refresh.track();
        c.checked.set(Default::default());
        c.search(false);
    });
    Effect::new(move |prev: Option<()>| {
        let q = c.query.get();
        if prev.is_none() {
            return;
        }
        set_timeout(
            move || {
                if c.query.get_untracked() == q {
                    c.search(false);
                }
            },
            std::time::Duration::from_millis(120),
        );
    });

    // Auswahl in der Seitenleiste oder Suche: Übersicht schließen
    Effect::new(move |prev: Option<()>| {
        c.collection.track();
        c.tag_filter.track();
        c.kind.track();
        c.unfiled.track();
        c.query.track();
        if prev.is_some() {
            c.dashboard.set(false);
        }
    });

    // Datei-Schlüssel stündlich erneuern (er gilt 6–12 Stunden)
    let key_refresh = set_interval_with_handle(
        move || {
            spawn_local(async move {
                let _ = c.api.load_file_key().await;
            });
        },
        std::time::Duration::from_secs(3600),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = key_refresh {
            h.clear();
        }
    });

    // Laufende Verarbeitung: Liste regelmäßig aktualisieren
    let poll = set_interval_with_handle(
        move || {
            let busy = c.results.with_untracked(|r| {
                r.iter().any(|d| d.extract_status == "pending" || d.extract_status == "processing")
            });
            if busy {
                c.search(false);
                c.load_meta();
            }
        },
        std::time::Duration::from_secs(3),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = poll {
            h.clear();
        }
    });

    // Tastenkürzel: "/" fokussiert Suche, Esc schließt Detailansicht
    let _ = window_event_listener(leptos::ev::keydown, move |ev| {
        let tag = ev
            .target()
            .and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok())
            .map(|e| e.tag_name())
            .unwrap_or_default();
        let typing = matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT");
        if ev.key() == "/" && !typing {
            ev.prevent_default();
            if let Some(el) = document().get_element_by_id("search") {
                if let Ok(i) = wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlInputElement>(el) {
                    let _ = i.focus();
                    i.select();
                }
            }
        } else if ev.key() == "Escape" && !crate::dialog::is_open() {
            c.selected.set(None);
        }
    });

    view! {
        <div class="layout" class:with-detail=move || c.selected.get().is_some()
            class:has-player=move || player::player().doc.get().is_some()>
            <Sidebar />
            <main class="main">
                <Show when=move || !c.dashboard.get()
                    fallback=|| view! { <crate::components::dashboard::DashboardView /> }>
                    <Toolbar />
                    <Results />
                </Show>
            </main>
            {move || c.selected.get().map(|id| view! { <DetailPanel id=id /> })}
        </div>
        <UploadPanel />
        <player::PlayerBar />
        <DropOverlay />
        <Show when=move || c.show_settings.get()>
            <components::settings::Settings />
        </Show>
        <Show when=move || c.scan.get()>
            <components::scan::Scanner />
        </Show>
        {move || c.share_target.get().map(|t| view! { <components::share::ShareDialog target=t /> })}
        {move || c.office_doc.get().map(|id| {
            let title = c.results.with_untracked(|r| r.iter().find(|d| d.id == id).map(|d| d.title.clone()))
                .unwrap_or_else(|| c.editor_title.get_untracked());
            view! { <components::office::OfficeEditor id=id title=title /> }
        })}
        {move || c.tupfer_doc.get().map(|id| {
            let title = c.results.with_untracked(|r| r.iter().find(|d| d.id == id).map(|d| d.title.clone()))
                .unwrap_or_else(|| c.editor_title.get_untracked());
            view! { <components::tupfer::TupferEditor id=id title=title /> }
        })}
        {move || c.text_doc.get().map(|id| view! { <components::editor::TextEditor id=id /> })}
    }
}
