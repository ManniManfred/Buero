// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Einstellungen für den KI-Zugang (MCP): Zugangsschlüssel verwalten.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{ApiToken, ApiTokenCreated, ApiTokenInput, ApiTokenUpdate};

use crate::state::ctx;
use crate::util::format_ts;

#[component]
pub fn McpSection() -> impl IntoView {
    let c = ctx();
    let tokens = RwSignal::new(Vec::<ApiToken>::new());
    // Frisch angelegter Schlüssel (wird nur einmal angezeigt)
    let created = RwSignal::new(None::<ApiTokenCreated>);

    let load = move || {
        spawn_local(async move {
            match c.api.get::<Vec<ApiToken>>("/api/mcp/tokens").await {
                Ok(t) => tokens.set(t),
                Err(e) => c.notify(e, true),
            }
        });
    };
    load();

    let endpoint = move || {
        let base = c.api.base.get();
        let base = if base.is_empty() { window().location().origin().unwrap_or_default() } else { base };
        format!("{base}/mcp")
    };
    let locked = move || c.collections.with(|cs| cs.iter().filter(|x| x.ai_locked).map(|x| x.name.clone()).collect::<Vec<_>>());

    let create = move |_| {
        let Some(name) = window()
            .prompt_with_message_and_default("Name für den Zugang (z. B. „Claude Code Laptop“):", "Claude Code")
            .ok()
            .flatten()
        else {
            return;
        };
        let can_upload = window()
            .confirm_with_message("Darf dieser Zugang auch Dokumente hochladen, Tags vergeben und in Sammlungen einordnen?\n(Löschen oder Ändern geht nie.) OK = ja, Abbrechen = nur lesen")
            .unwrap_or(false);
        spawn_local(async move {
            match c.api.send::<_, ApiTokenCreated>("POST", "/api/mcp/tokens", &ApiTokenInput { name, can_upload }).await {
                Ok(t) => {
                    created.set(Some(t));
                    load();
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    let revoke = move |t: ApiToken| {
        if !window()
            .confirm_with_message(&format!("Zugang „{}“ widerrufen? Programme mit diesem Schlüssel verlieren sofort den Zugriff.", t.name))
            .unwrap_or(false)
        {
            return;
        }
        spawn_local(async move {
            match c.api.send_empty::<()>("DELETE", &format!("/api/mcp/tokens/{}", t.id), None).await {
                Ok(()) => {
                    if created.with_untracked(|n| n.as_ref().is_some_and(|n| n.info.id == t.id)) {
                        created.set(None);
                    }
                    load();
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    let set_upload = move |t: ApiToken, can_upload: bool| {
        spawn_local(async move {
            let body = ApiTokenUpdate { can_upload };
            match c.api.send_empty("PATCH", &format!("/api/mcp/tokens/{}", t.id), Some(&body)).await {
                Ok(()) => load(),
                Err(e) => {
                    c.notify(e, true);
                    load();
                }
            }
        });
    };

    let command = move |token: &str| {
        format!("claude mcp add --transport http buero {} --header \"Authorization: Bearer {token}\"", endpoint())
    };

    view! {
        <h3>"KI-Zugang (MCP)"</h3>
        <p class="muted small">
            "KI-Assistenten wie Claude können über "<code>{endpoint}</code>" Dokumente suchen und den erkannten Text lesen – "
            "nur mit einem Zugangsschlüssel von hier, normalerweise nur lesend. Schlüssel mit „Hochladen“ dürfen zusätzlich "
            "Dokumente hochladen, Tags vergeben und in Sammlungen einordnen, aber nie löschen oder ändern. Gelesene Inhalte gehen an den Anbieter des Assistenten. "
            "Sammlungen lassen sich über „Bearbeiten“ sperren; ihre Dokumente (samt Untersammlungen) liefert der Zugang nie aus."
        </p>
        <p class="small">
            {move || {
                let l = locked();
                if l.is_empty() { "Gesperrt: keine Sammlung".to_string() } else { format!("Gesperrt: {}", l.join(", ")) }
            }}
        </p>
        <Show when=move || created.with(Option::is_some)>
            {move || created.get().map(|t| {
                let cmd = command(&t.token);
                let cmd2 = cmd.clone();
                view! {
                    <div class="token-new small">
                        <strong>"Neuer Zugang „"{t.info.name.clone()}"“ – der Schlüssel wird nur jetzt angezeigt:"</strong>
                        <code>{t.token.clone()}</code>
                        "In Claude Code einrichten:"
                        <code>{cmd}</code>
                        <button on:click=move |_| {
                            let cmd = cmd2.clone();
                            spawn_local(async move {
                                if crate::js::copy(&cmd).await { c.notify("Befehl kopiert", false); }
                            });
                        }><crate::icons::Icon name="copy" />"Befehl kopieren"</button>
                        <button on:click=move |_| created.set(None)>"Ausblenden"</button>
                    </div>
                }
            })}
        </Show>
        <ul class="token-list small">
            <For each=move || tokens.get() key=|t| (t.id, t.can_upload) let:t>
                <li>
                    <crate::icons::Icon name="lock" class="sm" />
                    <span class="grow">{t.name.clone()}</span>
                    <label class="muted" title="Darf Dokumente hochladen, Tags vergeben und in Sammlungen einordnen">
                        <input type="checkbox" prop:checked=t.can_upload on:change={
                            let t = t.clone();
                            move |ev| set_upload(t.clone(), event_target_checked(&ev))
                        } />
                        "Hochladen"
                    </label>
                    <span class="muted">
                        {match t.last_used_at {
                            Some(ts) => format!("zuletzt {}", format_ts(ts)),
                            None => "nie benutzt".to_string(),
                        }}
                    </span>
                    <button class="icon" title="Widerrufen" on:click={
                        let t = t.clone();
                        move |_| revoke(t.clone())
                    }><crate::icons::Icon name="trash" class="sm" /></button>
                </li>
            </For>
        </ul>
        <button on:click=create><crate::icons::Icon name="plus" />"Zugangsschlüssel anlegen"</button>
    }
}
