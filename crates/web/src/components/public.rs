// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Öffentliche Seite eines Freigabelinks (`/s/<token>`), ohne Anmeldung.

use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{ApiError, PublicDocument, PublicShare, ShareKey, ShareUnlock};

use crate::icons::{Icon, KindIcon};
use crate::util::{format_date, format_size, format_ts};

#[derive(Clone, PartialEq)]
enum PState {
    Loading,
    NeedPassword,
    Error(String),
    Ready(PublicShare),
}

fn enc(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

fn session_key(token: &str) -> String {
    format!("buero.share.{token}")
}

fn session() -> Option<web_sys::Storage> {
    window().session_storage().ok().flatten()
}

async fn error_text(r: gloo_net::http::Response) -> String {
    let status = r.status();
    match r.json::<ApiError>().await {
        Ok(e) => e.error,
        Err(_) => format!("Fehler {status}"),
    }
}

#[component]
pub fn PublicPage(token: String) -> impl IntoView {
    let token = StoredValue::new(token);
    let key = RwSignal::new(
        session().and_then(|s| s.get_item(&session_key(&token.get_value())).ok().flatten()).unwrap_or_default(),
    );
    let state = RwSignal::new(PState::Loading);
    let message = RwSignal::new(None::<(String, bool)>);

    let base = move || format!("/api/public/shares/{}", enc(&token.get_value()));
    let key_q = move || {
        let k = key.get_untracked();
        if k.is_empty() { String::new() } else { format!("key={}", enc(&k)) }
    };

    let load = move || {
        spawn_local(async move {
            let q = key_q();
            let url = if q.is_empty() { base() } else { format!("{}?{q}", base()) };
            let s = match Request::get(&url).send().await {
                Ok(r) if r.ok() => match r.json::<PublicShare>().await {
                    Ok(p) => PState::Ready(p),
                    Err(e) => PState::Error(e.to_string()),
                },
                Ok(r) if r.status() == 403 => PState::NeedPassword,
                Ok(r) => PState::Error(error_text(r).await),
                Err(e) => PState::Error(format!("Server nicht erreichbar: {e}")),
            };
            if let PState::Ready(p) = &s {
                document().set_title(&format!("{} – Büro", p.title));
            }
            state.set(s);
        });
    };
    load();

    view! {
        <div class="public">
            <header class="public-head">
                <span class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"</span>
                <span class="muted small">"Freigabe"</span>
            </header>
            {move || message.get().map(|(m, err)| view! {
                <div class="notice" class:bad=err on:click=move |_| message.set(None)>{m}</div>
            })}
            {move || match state.get() {
                PState::Loading => view! { <div class="center muted">"Lade…"</div> }.into_any(),
                PState::Error(e) => view! {
                    <div class="center"><div class="card public-msg"><h2>"Nicht verfügbar"</h2><p>{e}</p></div></div>
                }.into_any(),
                PState::NeedPassword => view! { <Unlock token=token key=key on_done=Callback::new(move |_| load()) /> }.into_any(),
                PState::Ready(p) => view! {
                    <ShareView p=p token=token key_q=Callback::new(move |_| key_q()) message=message reload=Callback::new(move |_| load()) />
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn Unlock(token: StoredValue<String>, key: RwSignal<String>, on_done: Callback<()>) -> impl IntoView {
    let pw = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        spawn_local(async move {
            let url = format!("/api/public/shares/{}/unlock", enc(&token.get_value()));
            let body = ShareUnlock { password: pw.get_untracked() };
            let r = match Request::post(&url).json(&body) {
                Ok(req) => req.send().await,
                Err(e) => Err(e),
            };
            busy.set(false);
            match r {
                Ok(r) if r.ok() => {
                    if let Ok(k) = r.json::<ShareKey>().await {
                        if let Some(s) = session() {
                            let _ = s.set_item(&session_key(&token.get_value()), &k.key);
                        }
                        key.set(k.key);
                        on_done.run(());
                    }
                }
                Ok(r) => error.set(Some(error_text(r).await)),
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };
    view! {
        <div class="center">
            <form class="card login" on:submit=submit>
                <h2><Icon name="lock" />" Passwortgeschützt"</h2>
                <p class="muted small">"Bitte das Passwort eingeben, das du zusammen mit dem Link bekommen hast."</p>
                <input type="password" placeholder="Passwort" autofocus
                    prop:value=move || pw.get()
                    on:input=move |ev| pw.set(event_target_value(&ev)) />
                {move || error.get().map(|e| view! { <div class="notice bad">{e}</div> })}
                <button class="primary" type="submit" disabled=move || busy.get()>"Öffnen"</button>
            </form>
        </div>
    }
}

#[component]
fn ShareView(
    p: PublicShare,
    token: StoredValue<String>,
    key_q: Callback<(), String>,
    message: RwSignal<Option<(String, bool)>>,
    reload: Callback<()>,
) -> impl IntoView {
    // endpoint: "file" oder "thumbnail"
    let file_url = move |id: i64, endpoint: &str, extra: &str| {
        let mut q: Vec<String> = Vec::new();
        let k = key_q.run(());
        if !k.is_empty() {
            q.push(k);
        }
        if !extra.is_empty() {
            q.push(extra.to_string());
        }
        let qs = if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) };
        format!("/api/public/shares/{}/documents/{id}/{endpoint}{qs}", enc(&token.get_value()))
    };

    let single = (!p.is_collection).then(|| p.documents.first().cloned()).flatten();
    let upload_input = NodeRef::<leptos::html::Input>::new();
    let uploading = RwSignal::new(false);

    let do_upload = move |_| {
        let Some(input) = upload_input.get() else { return };
        let Some(files) = input.files() else { return };
        if files.length() == 0 {
            return;
        }
        let form = web_sys::FormData::new().unwrap();
        for i in 0..files.length() {
            if let Some(f) = files.get(i) {
                let _ = form.append_with_blob_and_filename("file", &f, &f.name());
            }
        }
        input.set_value("");
        uploading.set(true);
        spawn_local(async move {
            let k = key_q.run(());
            let url = format!(
                "/api/public/shares/{}/upload{}",
                enc(&token.get_value()),
                if k.is_empty() { String::new() } else { format!("?{k}") }
            );
            let r = match Request::post(&url).body(form) {
                Ok(req) => req.send().await,
                Err(e) => Err(e),
            };
            uploading.set(false);
            match r {
                Ok(r) if r.ok() => {
                    let v: serde_json::Value = r.json().await.unwrap_or_default();
                    let stored = v.get("stored").and_then(|x| x.as_u64()).unwrap_or(0);
                    let dup = v.get("duplicates").and_then(|x| x.as_u64()).unwrap_or(0);
                    let mut m = match stored {
                        1 => "1 Datei hochgeladen.".to_string(),
                        n => format!("{n} Dateien hochgeladen."),
                    };
                    if dup > 0 {
                        m.push_str(&format!(" {dup} war(en) bereits vorhanden."));
                    }
                    message.set(Some((m, false)));
                    reload.run(());
                }
                Ok(r) => message.set(Some((error_text(r).await, true))),
                Err(e) => message.set(Some((e.to_string(), true))),
            }
        });
    };

    view! {
        <main class="public-main">
            <div class="public-title">
                <h1>
                    {if p.is_collection { view! { <Icon name="folder" /> }.into_any() } else { view! { <Icon name="file" /> }.into_any() }}
                    {p.title.clone()}
                </h1>
                <span class="muted small">
                    {[
                        p.is_collection.then(|| format!("{} Dokument(e)", p.documents.len())),
                        p.expires_at.map(|e| format!("Link gültig bis {}", format_ts(e))),
                    ].into_iter().flatten().collect::<Vec<_>>().join(" · ")}
                </span>
            </div>

            {p.allow_upload.then(|| view! {
                <div class="card public-upload">
                    <span><Icon name="upload" />" Dateien für den Empfänger hochladen"</span>
                    <input type="file" multiple node_ref=upload_input on:change=do_upload />
                    {move || uploading.get().then(|| view! { <span class="muted small">"Lädt hoch…"</span> })}
                </div>
            })}

            {single.map(|d| {
                let url = file_url(d.id, "file", "");
                let previewable = d.kind == "pdf" || d.kind == "image" || d.mime.starts_with("text/");
                previewable.then(|| if d.kind == "image" {
                    view! { <div class="public-preview img"><img src=url alt="" /></div> }.into_any()
                } else {
                    view! { <iframe class="public-preview" src=url></iframe> }.into_any()
                })
            })}

            <ul class="public-list">
                {p.documents.iter().cloned().map(|d: PublicDocument| {
                    let open = file_url(d.id, "file", "");
                    let download = file_url(d.id, "file", "download=1");
                    let thumb = d.has_thumbnail.then(|| file_url(d.id, "thumbnail", ""));
                    view! {
                        <li class="card public-item">
                            <div class="thumb">
                                {match thumb {
                                    Some(t) => view! { <img src=t alt="" loading="lazy" /> }.into_any(),
                                    None => view! { <KindIcon kind=d.kind.clone() class="kind-icon" /> }.into_any(),
                                }}
                            </div>
                            <div class="row-body">
                                <span class="title">{d.title.clone()}</span>
                                <span class="muted small ellipsis">
                                    {d.original_filename.clone()}" · "{format_size(d.size)}
                                    {d.document_date.as_deref().map(|x| format!(" · {}", format_date(x)))}
                                </span>
                            </div>
                            <div class="public-actions">
                                <a class="button" href=open target="_blank" rel="noopener"><Icon name="external" />"Öffnen"</a>
                                <a class="button" href=download><Icon name="download" />"Herunterladen"</a>
                            </div>
                        </li>
                    }
                }).collect_view()}
            </ul>
            {(p.is_collection && p.documents.is_empty()).then(|| view! { <p class="muted center">"Diese Sammlung ist noch leer."</p> })}
        </main>
    }
}
