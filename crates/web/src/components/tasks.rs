// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Laufende Hintergrundaufgaben des Servers: Hinweis unten in der
//! Seitenleiste, aufklappbar zu einer Liste.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{BackgroundTask, BackgroundTasks};

use crate::icons::Icon;
use crate::state::ctx;

/// Takt der Abfrage; ohne laufende Aufgaben und zugeklappt nur jeden dritten
const TICK_MS: u64 = 2000;

#[component]
pub fn TaskIndicator() -> impl IntoView {
    let c = ctx();
    let data = RwSignal::new(BackgroundTasks::default());
    let open = RwSignal::new(false);
    let loading = StoredValue::new(false);
    let tick = StoredValue::new(0u32);

    let load = move || {
        if loading.get_value() {
            return;
        }
        loading.set_value(true);
        spawn_local(async move {
            if let Ok(d) = c.api.get::<BackgroundTasks>("/api/tasks").await {
                data.try_set(d);
            }
            loading.set_value(false);
        });
    };
    load();
    let handle = set_interval_with_handle(
        move || {
            if document().hidden() {
                return;
            }
            let n = tick.get_value().wrapping_add(1);
            tick.set_value(n);
            let busy = data.with_untracked(|d| !d.tasks.is_empty() || d.queued > 0);
            if busy || open.get_untracked() || n % 3 == 0 {
                load();
            }
        },
        std::time::Duration::from_millis(TICK_MS),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });

    let count = move || data.with(|d| d.tasks.len());
    let queued = move || data.with(|d| d.queued);
    let label = move || {
        let (n, q) = (count(), queued());
        let mut t = match n {
            0 => String::new(),
            1 => "1 Aufgabe läuft".to_string(),
            n => format!("{n} Aufgaben laufen"),
        };
        if q > 0 {
            if !t.is_empty() {
                t.push_str(" · ");
            }
            t.push_str(&format!("{q} warten"));
        }
        t
    };

    view! {
        <Show when=move || { count() > 0 || queued() > 0 }>
            <div class="tasks">
                <button class="tasks-toggle" class:open=move || open.get()
                    title="Hintergrundaufgaben anzeigen"
                    on:click=move |_| { open.update(|o| *o = !*o); load(); }>
                    <span class="tasks-spin"></span>
                    <span>{label}</span>
                </button>
                <Show when=move || open.get()>
                    <div class="tasks-panel" role="dialog" aria-label="Hintergrundaufgaben">
                        <div class="tasks-head">
                            <strong>"Hintergrundaufgaben"</strong>
                            <button class="icon" title="Schließen" aria-label="Schließen"
                                on:click=move |_| open.set(false)><Icon name="x" /></button>
                        </div>
                        <ul>
                            {move || {
                                let d = data.get();
                                d.tasks.into_iter().map(|t| task_row(t, d.now, open)).collect_view()
                            }}
                        </ul>
                        <Show when=move || { queued() > 0 }>
                            <p class="muted small">{move || match queued() {
                                1 => "1 weiteres Dokument wartet auf die Verarbeitung.".to_string(),
                                q => format!("{q} weitere Dokumente warten auf die Verarbeitung."),
                            }}</p>
                        </Show>
                    </div>
                </Show>
            </div>
        </Show>
    }
}

fn task_row(t: BackgroundTask, now: i64, open: RwSignal<bool>) -> impl IntoView {
    let c = ctx();
    let icon = match t.kind.as_str() {
        "mail" => "email",
        "preview" => "pdf",
        "index" => "search",
        _ => "layers",
    };
    let doc = t.document_id;
    let since = duration(now - t.started_at);
    view! {
        <li class:link=doc.is_some()
            title=if doc.is_some() { "Dokument anzeigen" } else { "" }
            on:click=move |_| if let Some(id) = doc {
                c.selected.set(Some(id));
                open.set(false);
            }>
            <Icon name=icon />
            <div class="task-text">
                <span class="task-title">{t.title}</span>
                <span class="muted small">{t.phase}</span>
            </div>
            <span class="muted small task-time">{since}</span>
        </li>
    }
}

/// „5 s“, „2 min“, „1 h 5 min“
fn duration(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}
