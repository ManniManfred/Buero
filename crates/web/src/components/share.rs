// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Dialog zum Anlegen und Verwalten von Freigabelinks.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{Share, ShareInput};

use crate::icons::Icon;
use crate::state::{ctx, share_url, Ctx, ShareTarget};
use crate::util::format_ts;

fn select_all(ev: leptos::ev::FocusEvent) {
    event_target::<web_sys::HtmlInputElement>(&ev).select();
}

fn copy_link(c: Ctx, url: String) {
    spawn_local(async move {
        if crate::js::copy(&url).await {
            c.notify("Link kopiert", false);
        } else {
            c.notify("Automatisches Kopieren nicht möglich – bitte den Link markieren und kopieren", false);
        }
    });
}

#[component]
pub fn ShareDialog(target: ShareTarget) -> impl IntoView {
    let c = ctx();
    let manage_all = target.document_id.is_none() && target.collection_id.is_none();
    let is_collection = target.collection_id.is_some();
    let shares = RwSignal::new(None::<Vec<Share>>);

    let password = RwSignal::new(String::new());
    let expiry = RwSignal::new("30".to_string());
    let custom_date = RwSignal::new(String::new());
    let allow_upload = RwSignal::new(false);
    let note = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let query = match (target.document_id, target.collection_id) {
        (Some(d), _) => format!("?document_id={d}"),
        (_, Some(col)) => format!("?collection_id={col}"),
        _ => String::new(),
    };
    {
        let query = query.clone();
        spawn_local(async move {
            match c.api.get::<Vec<Share>>(&format!("/api/shares{query}")).await {
                Ok(v) => shares.set(Some(v)),
                Err(e) => {
                    c.notify(e, true);
                    shares.set(Some(Vec::new()));
                }
            }
        });
    }

    let close = move || c.share_target.set(None);

    let create = {
        let target = target.clone();
        move |ev: leptos::ev::SubmitEvent| {
            ev.prevent_default();
            let now = (js_sys::Date::now() / 1000.0) as i64;
            let expires_at = match expiry.get_untracked().as_str() {
                "" => None,
                "date" => {
                    let d = custom_date.get_untracked();
                    if d.is_empty() {
                        c.notify("Bitte ein Ablaufdatum wählen", true);
                        return;
                    }
                    // Ende des gewählten Tages (lokale Zeit)
                    let ms = js_sys::Date::new(&format!("{d}T23:59:59").into()).get_time();
                    Some((ms / 1000.0) as i64)
                }
                days => days.parse::<i64>().ok().map(|n| now + n * 86400),
            };
            let pw = password.get_untracked();
            let body = ShareInput {
                document_id: target.document_id,
                collection_id: target.collection_id,
                password: (!pw.is_empty()).then_some(pw),
                expires_at,
                allow_upload: allow_upload.get_untracked(),
                note: note.get_untracked(),
            };
            busy.set(true);
            spawn_local(async move {
                let r = c.api.send::<_, Share>("POST", "/api/shares", &body).await;
                busy.set(false);
                match r {
                    Ok(s) => {
                        copy_link(c, share_url(&c, &s.token));
                        shares.update(|v| v.get_or_insert_with(Vec::new).insert(0, s));
                        password.set(String::new());
                        note.set(String::new());
                    }
                    Err(e) => c.notify(e, true),
                }
            });
        }
    };

    let title = if manage_all { "Alle Freigaben".to_string() } else { format!("„{}“ freigeben", target.name) };

    view! {
        <div class="modal-bg" on:click=move |_| close()>
            <div class="card modal share-dialog" on:click=|ev| ev.stop_propagation()>
                <h2>{title}</h2>
                {(!manage_all).then(|| view! {
                    <p class="muted small">
                        {if is_collection {
                            "Wer den Link kennt, sieht alle Dokumente dieser Sammlung und ihrer Untersammlungen – auch solche, die später dazukommen."
                        } else {
                            "Wer den Link kennt, kann dieses Dokument ansehen und herunterladen – immer in der aktuellen Version."
                        }}
                    </p>
                    <form class="share-form" on:submit=create.clone()>
                        <div class="share-grid">
                            <label>"Gültig"
                                <select on:change=move |ev| expiry.set(event_target_value(&ev)) prop:value=move || expiry.get()>
                                    <option value="1">"1 Tag"</option>
                                    <option value="7">"7 Tage"</option>
                                    <option value="30">"30 Tage"</option>
                                    <option value="90">"90 Tage"</option>
                                    <option value="365">"1 Jahr"</option>
                                    <option value="date">"bis Datum …"</option>
                                    <option value="">"unbegrenzt"</option>
                                </select>
                            </label>
                            <Show when=move || expiry.get() == "date">
                                <label>"Ablaufdatum"
                                    <input type="date" prop:value=move || custom_date.get()
                                        on:input=move |ev| custom_date.set(event_target_value(&ev)) />
                                </label>
                            </Show>
                            <label>"Passwort (optional)"
                                <input type="text" autocomplete="off" placeholder="ohne Passwort"
                                    prop:value=move || password.get()
                                    on:input=move |ev| password.set(event_target_value(&ev)) />
                            </label>
                            <label>"Notiz (nur für dich)"
                                <input type="text" placeholder="z. B. Steuerberater"
                                    prop:value=move || note.get()
                                    on:input=move |ev| note.set(event_target_value(&ev)) />
                            </label>
                        </div>
                        {is_collection.then(|| view! {
                            <label class="check">
                                <input type="checkbox" prop:checked=move || allow_upload.get()
                                    on:change=move |ev| allow_upload.set(event_target_checked(&ev)) />
                                "Empfänger dürfen Dateien in diese Sammlung hochladen"
                            </label>
                        })}
                        <button class="primary" type="submit" disabled=move || busy.get()>
                            <Icon name="link" />"Link erstellen & kopieren"
                        </button>
                    </form>
                    <hr />
                })}
                <h3>{if manage_all { "Aktive Links" } else { "Bestehende Links" }}</h3>
                {move || match shares.get() {
                    None => view! { <p class="muted">"Lade…"</p> }.into_any(),
                    Some(v) if v.is_empty() => view! { <p class="muted small">"Noch keine Freigaben."</p> }.into_any(),
                    Some(v) => view! {
                        <ul class="share-list">
                            {v.into_iter().map(|s| view! { <ShareRow s=s shares=shares show_target=manage_all /> }).collect_view()}
                        </ul>
                    }.into_any(),
                }}
                <div class="row-buttons">
                    <span></span>
                    <button class="primary" on:click=move |_| close()>"Schließen"</button>
                </div>
            </div>
        </div>
    }
}

#[component]
fn ShareRow(s: Share, shares: RwSignal<Option<Vec<Share>>>, show_target: bool) -> impl IntoView {
    let c = ctx();
    let url = share_url(&c, &s.token);
    let now = (js_sys::Date::now() / 1000.0) as i64;
    let expired = s.expires_at.is_some_and(|e| e <= now);
    let id = s.id;

    let mut info: Vec<String> = Vec::new();
    if show_target {
        info.push(format!("{} „{}“", if s.collection_id.is_some() { "Sammlung" } else { "Dokument" }, s.target_name));
    }
    info.push(match s.expires_at {
        Some(e) if e <= now => format!("abgelaufen am {}", format_ts(e)),
        Some(e) => format!("gültig bis {}", format_ts(e)),
        None => "unbegrenzt gültig".into(),
    });
    if s.allow_upload {
        info.push("Upload erlaubt".into());
    }
    info.push(match s.access_count {
        0 => "noch nicht aufgerufen".to_string(),
        1 => "1× aufgerufen".to_string(),
        n => format!("{n}× aufgerufen"),
    });

    let copy_url = url.clone();
    view! {
        <li class="share-item" class:expired=expired>
            <div class="share-link">
                {s.has_password.then(|| view! { <span title="Passwortgeschützt"><Icon name="lock" class="sm" /></span> })}
                <input readonly prop:value=url.clone()
                    on:focus=select_all />
                <button class="icon" title="Link kopieren" on:click=move |_| copy_link(c, copy_url.clone())><Icon name="copy" /></button>
                <button class="icon" title="Link öffnen" on:click=move |_| crate::api::open_external(&url)><Icon name="external" /></button>
                <button class="icon danger" title="Freigabe widerrufen" on:click=move |_| {
                    spawn_local(async move {
                        if !crate::dialog::confirm("Freigabe widerrufen?", "Der Link funktioniert danach nicht mehr.")
                            .ok("Widerrufen")
                            .danger()
                            .ask_ok()
                            .await
                        {
                            return;
                        }
                        match c.api.send_empty::<()>("DELETE", &format!("/api/shares/{id}"), None).await {
                            Ok(()) => shares.update(|v| if let Some(v) = v { v.retain(|x| x.id != id) }),
                            Err(e) => c.notify(e, true),
                        }
                    });
                }><Icon name="trash" /></button>
            </div>
            <div class="small muted">
                {(!s.note.is_empty()).then(|| view! { <b>{s.note.clone()}</b>" · " })}
                {info.join(" · ")}
            </div>
        </li>
    }
}
