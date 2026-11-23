// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! E-Mails in der Detailansicht: Kopfdaten, Text, Anhänge und Konversation;
//! bei Anhängen der Verweis auf die zugehörige Mail.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{EmailInfo, EmailRef};

use crate::icons::{Icon, KindIcon};
use crate::state::ctx;
use crate::util::{format_size, format_ts};

fn load(id: i64, info: RwSignal<Option<EmailInfo>>) {
    let api = ctx().api;
    spawn_local(async move {
        if let Ok(i) = api.get::<EmailInfo>(&format!("/api/documents/{id}/email")).await {
            info.set(Some(i));
        }
    });
}

/// Name ohne Adresse („Max Muster <max@…>“ → „Max Muster“)
pub(crate) fn short(sender: &str) -> String {
    match sender.split_once(" <") {
        Some((n, _)) if !n.trim().is_empty() => n.trim().trim_matches('"').to_string(),
        _ => sender.to_string(),
    }
}

/// Vorschau einer Mail
#[component]
pub fn EmailView(id: i64) -> impl IntoView {
    let c = ctx();
    let info = RwSignal::new(None::<EmailInfo>);
    let body = RwSignal::new(None::<String>);
    load(id, info);
    spawn_local(async move {
        let t = c.api.get_text(&format!("/api/documents/{id}/text")).await.unwrap_or_default();
        // Der erkannte Text beginnt mit den Kopfzeilen, danach eine Leerzeile
        let b = t.split_once("\n\n").map(|(_, b)| b.to_string()).unwrap_or(t);
        body.set(Some(b));
    });

    move || {
        let Some(i) = info.get() else {
            return view! { <div class="preview none"><p>"Lade…"</p></div> }.into_any();
        };
        let Some(h) = i.email.clone() else {
            return view! {
                <div class="preview none"><p>"Die Mail wird noch verarbeitet …"</p></div>
            }
            .into_any();
        };
        let row = |label: &'static str, v: String| {
            (!v.is_empty()).then(|| view! { <div class="mail-h"><span class="muted">{label}</span><span>{v}</span></div> })
        };
        let attachments = i.attachments.clone();
        let thread = i.thread.clone();
        view! {
            <div class="preview mail">
                <div class="mail-head">
                    <div class="mail-subject">{if h.subject.is_empty() { "(ohne Betreff)".to_string() } else { h.subject.clone() }}</div>
                    {row("Von", h.sender.clone())}
                    {row("An", h.recipients.clone())}
                    {row("Cc", h.cc.clone())}
                    {row("Datum", h.sent_at.map(format_ts).unwrap_or_default())}
                </div>
                {(!attachments.is_empty()).then(|| view! {
                    <div class="mail-attachments">
                        {attachments.into_iter().map(|a| {
                            let aid = a.id;
                            view! {
                                <button class="mail-att" title="Anhang öffnen" on:click=move |_| c.selected.set(Some(aid))>
                                    <KindIcon kind=a.kind.clone() class="sm" />
                                    <span class="ellipsis">{a.original_filename.clone()}</span>
                                    <span class="muted small">{format_size(a.size)}</span>
                                </button>
                            }
                        }).collect_view()}
                    </div>
                })}
                <div class="mail-body">{move || body.get().unwrap_or_else(|| "Lade…".into())}</div>
                {(thread.len() > 1).then(|| view! { <Thread items=thread current=id /> })}
            </div>
        }
        .into_any()
    }
}

#[component]
fn Thread(items: Vec<EmailRef>, current: i64) -> impl IntoView {
    let c = ctx();
    view! {
        <div class="mail-thread">
            <div class="mail-thread-head"><Icon name="email" class="sm" />{format!("Konversation · {} Mails", items.len())}</div>
            {items.into_iter().map(|t| {
                let tid = t.id;
                view! {
                    <button class="mail-thread-item" class:current=tid == current
                        on:click=move |_| if tid != current { c.selected.set(Some(tid)) }>
                        <span class="mail-thread-from ellipsis">{short(&t.sender)}</span>
                        <span class="ellipsis grow">{t.subject.clone()}</span>
                        <span class="muted small">{t.sent_at.map(format_ts).unwrap_or_default()}</span>
                    </button>
                }
            }).collect_view()}
        </div>
    }
}

/// Hinweis bei Anhängen: gehört zu dieser Mail
#[component]
pub fn AttachedTo(id: i64) -> impl IntoView {
    let c = ctx();
    let info = RwSignal::new(None::<EmailInfo>);
    load(id, info);
    move || {
        let mails = info.with(|i| i.as_ref().map(|i| i.attached_to.clone()).unwrap_or_default());
        (!mails.is_empty()).then(|| view! {
            <div class="notice mail-parent small">
                <Icon name="email" class="sm" />"Anhang von "
                {mails.into_iter().map(|m| {
                    let mid = m.id;
                    view! {
                        <button class="link" on:click=move |_| c.selected.set(Some(mid))>
                            {if m.subject.is_empty() { "E-Mail".to_string() } else { format!("„{}“", m.subject) }}
                        </button>
                    }
                }).collect_view()}
            </div>
        })
    }
}
