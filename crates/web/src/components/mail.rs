// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Einstellungen: Mails per IMAP abrufen (mehrere Postfächer und Ordner).

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{MailAccount, MailAccountInput, MailFolder, ServerFolder};

use crate::dialog;
use crate::icons::Icon;
use crate::state::{collection_path, ctx};

#[component]
pub fn MailSection() -> impl IntoView {
    let c = ctx();
    let accounts = RwSignal::new(Vec::<MailAccount>::new());
    // Postfach in Bearbeitung: Some(None) = neu
    let editing = RwSignal::new(None::<Option<MailAccount>>);

    let load = move || {
        spawn_local(async move {
            match c.api.get::<Vec<MailAccount>>("/api/mail/accounts").await {
                Ok(a) => {
                    // Abruf fertig: neue Sammlungen und Dokumente anzeigen
                    let was = accounts.with_untracked(|o| o.iter().any(|x| x.running));
                    let now = a.iter().any(|x| x.running);
                    accounts.set(a);
                    if was && !now {
                        c.load_meta();
                        c.reload();
                    }
                }
                Err(e) => c.notify(e, true),
            }
        });
    };
    load();
    // Solange ein Abruf läuft, Stand nachladen
    let handle = set_interval_with_handle(
        move || {
            if accounts.with_untracked(|a| a.iter().any(|x| x.running)) {
                load();
            }
        },
        std::time::Duration::from_millis(1500),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });

    let run = move |id: i64| {
        spawn_local(async move {
            match c.api.send_empty::<()>("POST", &format!("/api/mail/accounts/{id}/run"), None).await {
                Ok(()) => {
                    accounts.update(|a| a.iter_mut().filter(|x| x.id == id).for_each(|x| x.running = true));
                    c.notify("Abruf gestartet", false);
                }
                Err(e) => c.notify(e, true),
            }
        });
    };
    let cancel = move |id: i64| {
        spawn_local(async move {
            match c.api.send_empty::<()>("POST", &format!("/api/mail/accounts/{id}/cancel"), None).await {
                Ok(()) => {
                    c.notify("Abruf wird abgebrochen – Übernommenes bleibt erhalten", false);
                    load();
                }
                Err(e) => c.notify(e, true),
            }
        });
    };
    let remove = move |a: MailAccount| {
        spawn_local(async move {
            if !dialog::confirm(format!("Postfach „{}“ entfernen?", a.name), "Büro ruft dann keine Mails mehr daraus ab. Bereits übernommene Mails bleiben erhalten.")
                .ok("Entfernen")
                .danger()
                .ask_ok()
                .await
            {
                return;
            }
            match c.api.send_empty::<()>("DELETE", &format!("/api/mail/accounts/{}", a.id), None).await {
                Ok(()) => load(),
                Err(e) => c.notify(e, true),
            }
        });
    };

    view! {
        <h3>"E-Mails abrufen"</h3>
        <p class="muted small">
            "Büro holt Mails per IMAP aus ausgewählten Ordnern ab – nur lesend, im Postfach wird nichts verschoben oder als gelesen markiert. "
            "Je Ordner: „Alle Mails“ (z. B. ein eigener Ordner „Büro“, in den du Mails ziehst) oder „Nur Antworten“ "
            "(z. B. „Gesendet“ und Posteingang: nur Mails zu Konversationen, die schon in Büro sind). Anhänge werden eigene Dokumente."
        </p>
        <ul class="mail-accounts">
            <For each=move || accounts.get() key=|a| format!("{}|{}|{:?}|{:?}|{}|{}|{}", a.id, a.running, a.progress, a.last_run, a.enabled, a.name, a.folders.len()) let:a>
                <li class="mail-account">
                    <div class="grow">
                        <div><strong>{a.name.clone()}</strong>" "<span class="muted small">{format!("{}@{}", a.username, a.host)}</span></div>
                        <div class="small muted">
                            {if !a.enabled { "Abruf ausgeschaltet".to_string() }
                             else if a.running { "Abruf läuft …".to_string() }
                             else { match a.last_run {
                                Some(t) => format!("Zuletzt {} · {} neu · {} Ordner", crate::util::format_ts(t), a.last_imported, a.folders.len()),
                                None => format!("Noch nicht abgerufen · {} Ordner", a.folders.len()),
                             } }}
                        </div>
                        {a.progress.clone().filter(|_| a.running).map(|p| {
                            let pct = if p.total > 0 { (p.done * 100 / p.total).clamp(0, 100) } else { 0 };
                            view! {
                                <div class="mail-progress small">
                                    <div class="muted">
                                        {if p.folder.is_empty() { p.phase.clone() } else {
                                            format!("Ordner {} von {}: „{}“ · {}", p.folder_index, p.folder_count, p.folder, p.phase)
                                        }}
                                        {(p.total > 0).then(|| format!(" · {} von {}", p.done, p.total))}
                                        {format!(" · {} übernommen", p.imported)}
                                    </div>
                                    <div class="progress" class:indeterminate=p.total == 0>
                                        <div class="progress-bar" style=format!("width: {pct}%")></div>
                                    </div>
                                </div>
                            }
                        })}
                        {a.last_error.clone().filter(|_| !a.running).map(|e| view! { <div class="small bad-text">{e}</div> })}
                    </div>
                    {if a.running {
                        let cancelling = a.progress.as_ref().is_some_and(|p| p.phase.starts_with("Wird abgebrochen"));
                        view! {
                            <button class="danger" title="Abruf nach der aktuellen Mail beenden; Übernommenes bleibt"
                                disabled=cancelling on:click=move |_| cancel(a.id)>
                                <Icon name="x" />"Abbrechen"
                            </button>
                        }.into_any()
                    } else {
                        view! {
                            <button title="Jetzt abrufen" disabled=a.folders.is_empty() on:click=move |_| run(a.id)>
                                <Icon name="download" />"Abrufen"
                            </button>
                        }.into_any()
                    }}
                    <button class="icon" title="Bearbeiten" on:click={
                        let a = a.clone();
                        move |_| editing.set(Some(Some(a.clone())))
                    }><Icon name="pencil" /></button>
                    <button class="icon" title="Entfernen" on:click={
                        let a = a.clone();
                        move |_| remove(a.clone())
                    }><Icon name="trash" /></button>
                </li>
            </For>
        </ul>
        {move || match editing.get() {
            Some(a) => view! { <AccountEditor account=a on_done=Callback::new(move |_| { editing.set(None); load(); }) /> }.into_any(),
            None => view! {
                <button on:click=move |_| editing.set(Some(None))><Icon name="plus" />"Postfach hinzufügen"</button>
            }.into_any(),
        }}
    }
}

/// Auswahl für einen Ordner: "" = nicht abrufen, "all", "replies"
#[derive(Clone)]
struct FolderChoice {
    name: String,
    special: String,
    mode: RwSignal<String>,
    collection: RwSignal<i64>,
}

#[component]
fn AccountEditor(account: Option<MailAccount>, on_done: Callback<()>) -> impl IntoView {
    let c = ctx();
    let id = RwSignal::new(account.as_ref().map(|a| a.id));
    let name = RwSignal::new(account.as_ref().map(|a| a.name.clone()).unwrap_or_default());
    let host = RwSignal::new(account.as_ref().map(|a| a.host.clone()).unwrap_or_default());
    let port = RwSignal::new(account.as_ref().map(|a| a.port).unwrap_or(993).to_string());
    let user = RwSignal::new(account.as_ref().map(|a| a.username.clone()).unwrap_or_default());
    let password = RwSignal::new(String::new());
    let has_password = account.as_ref().is_some_and(|a| a.has_password);
    let interval = RwSignal::new(account.as_ref().map(|a| a.interval_min).unwrap_or(15).to_string());
    let collection = RwSignal::new(account.as_ref().and_then(|a| a.collection_id).unwrap_or(0).to_string());
    let enabled = RwSignal::new(account.as_ref().map(|a| a.enabled).unwrap_or(true));
    let configured: Vec<MailFolder> = account.as_ref().map(|a| a.folders.clone()).unwrap_or_default();
    // Bisher gewählte Ordner, bis die Liste vom Server geladen ist
    let choices = RwSignal::new(
        configured
            .iter()
            .map(|f| FolderChoice {
                name: f.name.clone(),
                special: String::new(),
                mode: RwSignal::new(f.mode.clone()),
                // 0 = eigene Unter-Sammlung, -1 = direkt in die Sammlung des Postfachs
                collection: RwSignal::new(f.collection_id.unwrap_or(if f.own_collection { 0 } else { -1 })),
            })
            .collect::<Vec<_>>(),
    );
    let busy = RwSignal::new(false);
    let loaded = RwSignal::new(false);

    let input = move || MailAccountInput {
        name: Some(name.get_untracked()),
        host: Some(host.get_untracked()),
        port: port.get_untracked().trim().parse().ok(),
        username: Some(user.get_untracked()),
        password: Some(password.get_untracked()).filter(|p| !p.is_empty()),
        enabled: Some(enabled.get_untracked()),
        interval_min: interval.get_untracked().parse().ok(),
        collection_id: collection.get_untracked().parse().ok(),
        folders: None,
    };
    let folders = move || -> Vec<MailFolder> {
        choices.with_untracked(|cs| {
            cs.iter()
                .filter(|f| !f.mode.get_untracked().is_empty())
                .map(|f| MailFolder {
                    name: f.name.clone(),
                    mode: f.mode.get_untracked(),
                    collection_id: Some(f.collection.get_untracked()).filter(|c| *c > 0),
                    own_collection: f.collection.get_untracked() != -1,
                })
                .collect()
        })
    };

    // Speichern (neu anlegen oder ändern); liefert die ID
    let save = move |with_folders: bool| async move {
        let mut body = input();
        if with_folders {
            body.folders = Some(folders());
        }
        match id.get_untracked() {
            Some(i) => c.api.send_empty("PATCH", &format!("/api/mail/accounts/{i}"), Some(&body)).await.map(|_| i),
            None => {
                let i = c.api.send::<_, i64>("POST", "/api/mail/accounts", &body).await?;
                id.set(Some(i));
                Ok(i)
            }
        }
    };

    let load_folders = move |_| {
        busy.set(true);
        spawn_local(async move {
            let res = async {
                let i = save(false).await?;
                c.api.send::<_, Vec<ServerFolder>>("POST", &format!("/api/mail/accounts/{i}/folders"), &()).await
            }
            .await;
            busy.set(false);
            match res {
                Ok(list) => {
                    let fresh = choices.with_untracked(|cs| cs.is_empty());
                    let old = choices.get_untracked();
                    let mut new: Vec<FolderChoice> = list
                        .into_iter()
                        .map(|f| {
                            let prev = old.iter().find(|o| o.name == f.name);
                            // Vorschlag beim ersten Laden: Posteingang/Gesendet nur Antworten, „Büro“ alles
                            let lname = shared::imap_folder_label(&f.name).to_lowercase();
                            let suggested = if !fresh {
                                ""
                            } else if f.special == "inbox" || f.special == "sent" {
                                "replies"
                            } else if lname.ends_with("büro") || lname.ends_with("buero") {
                                "all"
                            } else {
                                ""
                            };
                            FolderChoice {
                                mode: RwSignal::new(prev.map(|p| p.mode.get_untracked()).unwrap_or_else(|| suggested.to_string())),
                                collection: RwSignal::new(prev.map(|p| p.collection.get_untracked()).unwrap_or(0)),
                                name: f.name,
                                special: f.special,
                            }
                        })
                        .collect();
                    // Gewählte Ordner, die es auf dem Server nicht (mehr) gibt, behalten
                    for o in old {
                        if !new.iter().any(|n| n.name == o.name) {
                            new.push(o);
                        }
                    }
                    choices.set(new);
                    loaded.set(true);
                    c.notify("Verbindung in Ordnung", false);
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    let old_collection = account.as_ref().map(|a| a.collection_id.unwrap_or(0));
    let submit = move |_| {
        busy.set(true);
        spawn_local(async move {
            let res = save(true).await;
            busy.set(false);
            match res {
                Ok(_) => {
                    c.notify("Postfach gespeichert", false);
                    // Sammlung geändert: bisher abgerufene Mails mitnehmen?
                    let new: i64 = collection.get_untracked().parse().unwrap_or(0);
                    if let Some(old) = old_collection.filter(|o| *o != new) {
                        let n = c.api.get::<i64>(&format!("/api/mail/in-collection/{old}")).await.unwrap_or(0);
                        let name = |id: i64| if id > 0 { format!("„{}“", c.collection_name(id)) } else { "dem Eingang (ohne Sammlung)".to_string() };
                        if n > 0
                            && dialog::confirm(
                                "Bisherige Mails verschieben?",
                                format!(
                                    "In {} liegen {n} Mail(s). Sollen sie samt ihren Anhängen nach {} verschoben werden?",
                                    name(old),
                                    if new > 0 { name(new) } else { "„Eingang“".to_string() }
                                ),
                            )
                            .ok("Verschieben")
                            .ask_ok()
                            .await
                        {
                            let body = shared::MailMove { from: old, to: new };
                            match c.api.send::<_, i64>("POST", "/api/mail/move", &body).await {
                                Ok(n) => {
                                    c.notify(format!("{n} Mail(s) verschoben"), false);
                                    c.load_meta();
                                    c.reload();
                                }
                                Err(e) => c.notify(e, true),
                            }
                        }
                    }
                    on_done.run(());
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    let coll_options = move || {
        c.collections.with(|cs| {
            let mut o: Vec<(i64, String)> = cs.iter().map(|x| (x.id, collection_path(cs, x.id))).collect();
            o.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
            o
        })
    };

    view! {
        <div class="mail-editor card">
            <h4>{if account.is_some() { "Postfach bearbeiten" } else { "Neues Postfach" }}</h4>
            <div class="mail-grid">
                <label>"Server (IMAP)"
                    <input placeholder="imap.beispiel.de" prop:value=move || host.get() on:input=move |ev| host.set(event_target_value(&ev)) />
                </label>
                <label class="narrow">"Port"
                    <input inputmode="numeric" prop:value=move || port.get() on:input=move |ev| port.set(event_target_value(&ev)) />
                </label>
                <label>"Benutzer"
                    <input autocomplete="off" placeholder="name@beispiel.de" prop:value=move || user.get() on:input=move |ev| user.set(event_target_value(&ev)) />
                </label>
                <label>"Passwort"
                    <input type="password" autocomplete="new-password"
                        placeholder=if has_password { "unverändert" } else { "App-Passwort empfohlen" }
                        prop:value=move || password.get() on:input=move |ev| password.set(event_target_value(&ev)) />
                </label>
                <label>"Name"
                    <input placeholder="z. B. Privat" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
                </label>
                <label>"Abrufen alle"
                    <select prop:value=move || interval.get() on:change=move |ev| interval.set(event_target_value(&ev))>
                        {[(5, "5 Minuten"), (15, "15 Minuten"), (30, "30 Minuten"), (60, "Stunde"), (240, "4 Stunden"), (1440, "Tag")]
                            .into_iter()
                            .map(|(v, l)| view! { <option value=v.to_string() selected=move || interval.get() == v.to_string()>{l}</option> })
                            .collect_view()}
                    </select>
                </label>
                <label class="wide">"Übernommene Mails in Sammlung"
                    <select prop:value=move || collection.get() on:change=move |ev| collection.set(event_target_value(&ev))>
                        <option value="0">"– keine (Eingang) –"</option>
                        {move || coll_options().into_iter().map(|(v, l)| {
                            let v = v.to_string();
                            let v2 = v.clone();
                            view! { <option value=v selected=move || collection.get() == v2>{l}</option> }
                        }).collect_view()}
                    </select>
                </label>
            </div>
            <label class="dialog-check">
                <input type="checkbox" prop:checked=move || enabled.get() on:change=move |ev| enabled.set(event_target_checked(&ev)) />
                "Automatisch abrufen"
            </label>

            <div class="mail-folders-head">
                <strong>"Ordner"</strong>
                <button disabled=move || busy.get() on:click=load_folders>
                    <Icon name="rotate" />{move || if loaded.get() { "Neu laden" } else { "Verbindung testen & Ordner laden" }}
                </button>
            </div>
            <Show when=move || !choices.with(|c| c.is_empty())
                fallback=|| view! { <p class="muted small">"Erst Server und Zugangsdaten eintragen, dann die Ordner vom Server laden."</p> }>
                <ul class="mail-folders">
                    {move || choices.get().into_iter().map(|f| {
                        let mode = f.mode;
                        let coll = f.collection;
                        let label = match f.special.as_str() {
                            "inbox" => " (Posteingang)",
                            "sent" => " (Gesendet)",
                            "drafts" => " (Entwürfe)",
                            "trash" => " (Papierkorb)",
                            "junk" => " (Spam)",
                            "archive" => " (Archiv)",
                            _ => "",
                        };
                        view! {
                            <li class:active=move || !mode.get().is_empty()>
                                <span class="grow ellipsis" title=shared::imap_folder_label(&f.name)>{shared::imap_folder_label(&f.name)}<span class="muted">{label}</span></span>
                                <select prop:value=move || mode.get() on:change=move |ev| mode.set(event_target_value(&ev))>
                                    <option value="" selected=move || mode.get().is_empty()>"nicht abrufen"</option>
                                    <option value="all" selected=move || mode.get() == "all">"Alle Mails"</option>
                                    <option value="replies" selected=move || mode.get() == "replies">"Nur Antworten"</option>
                                </select>
                                <Show when=move || !mode.get().is_empty()>
                                    <select title="Wohin die Mails dieses Ordners kommen. Unter-Sammlungen werden in der Sammlung des Postfachs angelegt (ohne Sammlung: Eingang)."
                                        on:change=move |ev| coll.set(event_target_value(&ev).parse().unwrap_or(0))>
                                        <option value="0" selected=move || coll.get() == 0>{format!("↳ {} (Unter-Sammlung)", shared::mail_folder_collection_name(&f.name))}</option>
                                        <option value="-1" selected=move || coll.get() == -1>"Sammlung des Postfachs"</option>
                                        {move || coll_options().into_iter().map(|(v, l)| {
                                            view! { <option value=v.to_string() selected=move || coll.get() == v>{l}</option> }
                                        }).collect_view()}
                                    </select>
                                </Show>
                            </li>
                        }
                    }).collect_view()}
                </ul>
            </Show>
            <p class="muted small">
                "Das Passwort wird auf dem Büro-Server gespeichert (auch im Backup). Viele Anbieter verlangen für IMAP ein eigenes App-Passwort "
                "oder dass IMAP in den Einstellungen des Postfachs eingeschaltet ist. Verbindung nur verschlüsselt (TLS, meist Port 993)."
            </p>
            <div class="dialog-actions">
                <button on:click=move |_| on_done.run(())>"Abbrechen"</button>
                <button class="primary" disabled=move || busy.get() on:click=submit>"Speichern"</button>
            </div>
        </div>
    }
}
