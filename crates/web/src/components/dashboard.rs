// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Übersicht: Speicherplatz und Kennzahlen.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{Dashboard, DocumentBrief};

use crate::icons::{Icon, KindIcon};
use crate::state::{ctx, Ctx};
use crate::util::{format_size, format_ts};

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez"];

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 * 100.0 / whole as f64 }
}

fn pct_label(p: f64) -> String {
    if p > 0.0 && p < 1.0 { "< 1 %".into() } else { format!("{p:.0} %") }
}

fn size(b: u64) -> String {
    format_size(b as i64)
}

/// Nur Dokumente anzeigen, die zur Auswahl passen (Übersicht schließen)
fn show_docs(c: Ctx, f: impl FnOnce()) {
    c.collection.set(None);
    c.tag_filter.set(Vec::new());
    c.kind.set(String::new());
    c.unfiled.set(false);
    f();
    c.dashboard.set(false);
}

#[component]
pub fn DashboardView() -> impl IntoView {
    let c = ctx();
    let data = RwSignal::new(None::<Result<Dashboard, String>>);
    let load = move || {
        spawn_local(async move {
            let r = c.api.get::<Dashboard>("/api/dashboard").await;
            data.set(Some(r));
        });
    };
    load();

    view! {
        <div class="dashboard">
            <div class="dashboard-head">
                <button class="icon mobile-only" title="Sammlungen und Filter" aria-label="Sammlungen und Filter"
                    on:click=move |_| c.sidebar_open.set(true)>
                    <Icon name="menu" />
                </button>
                <button class="icon mobile-only" title="Zu den Dokumenten" aria-label="Zu den Dokumenten"
                    on:click=move |_| c.dashboard.set(false)>
                    <Icon name="search" />
                </button>
                <h2>"Übersicht"</h2>
                <button on:click=move |_| { load(); c.load_meta(); } title="Aktualisieren">
                    <Icon name="history" /><span class="btn-text">"Aktualisieren"</span>
                </button>
            </div>
            {move || match data.get() {
                None => view! { <p class="muted">"Lade…"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="error-text">{e}</p> }.into_any(),
                Some(Ok(d)) => view! {
                    <StorageCard d=d.clone() />
                    <Tiles d=d.clone() />
                    <div class="dash-grid">
                        <KindsCard d=d.clone() />
                        <MonthsCard d=d.clone() />
                        <DocList title="Größte Dokumente" docs=d.largest.clone() show_size=true />
                        <DocList title="Zuletzt hinzugefügt" docs=d.recent.clone() show_size=false />
                    </div>
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn StorageCard(d: Dashboard) -> impl IntoView {
    let s = &d.storage;
    let buero = s.total();
    let docs = s.files;
    let extra = buero - docs;
    let card = match (d.disk_total, d.disk_free) {
        (Some(total), Some(free)) if total > 0 => {
            let used_other = total.saturating_sub(free).saturating_sub(buero);
            let free_pct = pct(free, total);
            let parts = [
                ("seg-docs", "Dokumente (alle Fassungen)", docs),
                ("seg-extra", "Suchindex, Vorschaubilder, Datenbank", extra),
                ("seg-other", "Andere Daten auf dem Datenträger", used_other),
                ("seg-free", "Frei", free),
            ];
            let low = free_pct < 10.0;
            view! {
                <div class="card dash-card">
                    <h3>"Speicherplatz"</h3>
                    <p class="hero-figure">{size(free)}<span class="hero-unit">" frei"</span></p>
                    <p class="muted">
                        {format!("von {} · Büro belegt {} ({})", size(total), size(buero), pct_label(pct(buero, total)))}
                    </p>
                    <Show when=move || low>
                        <p class="status-warning">
                            <Icon name="alert" />{format!("Nur noch {} des Datenträgers frei", pct_label(free_pct))}
                        </p>
                    </Show>
                    <div class="seg-bar" role="img"
                        aria-label={parts.iter().map(|(_, n, v)| format!("{n}: {}", size(*v))).collect::<Vec<_>>().join(", ")}>
                        {parts.iter().filter(|p| p.2 > 0).map(|(cls, name, v)| {
                            let p = pct(*v, total);
                            let label = if p >= 8.0 { pct_label(p) } else { String::new() };
                            view! {
                                <div class=format!("seg {cls}") style=format!("flex-grow: {v}")
                                    title=format!("{name}: {} ({})", size(*v), pct_label(p))>{label}</div>
                            }
                        }).collect_view()}
                    </div>
                    <ul class="legend">
                        {parts.iter().map(|(cls, name, v)| view! {
                            <li><span class=format!("swatch {cls}")></span>{*name}<span class="muted">{size(*v)}</span></li>
                        }).collect_view()}
                    </ul>
                    <StorageTable d=d.clone() />
                </div>
            }
            .into_any()
        }
        _ => view! {
            <div class="card dash-card">
                <h3>"Speicherplatz"</h3>
                <p class="hero-figure">{size(buero)}<span class="hero-unit">" belegt"</span></p>
                <p class="muted">"Freier Platz des Datenträgers ist nicht bekannt."</p>
                <StorageTable d=d.clone() />
            </div>
        }
        .into_any(),
    };
    card
}

/// Aufschlüsselung als Tabelle (auch als zugängliche Alternative zum Balken)
#[component]
fn StorageTable(d: Dashboard) -> impl IntoView {
    let s = d.storage;
    let rows = [
        ("Dokumente (alle Fassungen, gleiche Inhalte nur einmal)", s.files),
        ("Suchindex", s.index),
        ("Vorschaubilder", s.thumbnails),
        ("Datenbank", s.database),
        ("Sonstiges", s.other),
    ];
    view! {
        <details class="storage-details">
            <summary>"Aufschlüsselung"</summary>
            <table class="num-table">
                {rows.into_iter().filter(|r| r.1 > 0 || r.0 != "Sonstiges").map(|(n, v)| view! {
                    <tr><td>{n}</td><td class="num">{size(v)}</td></tr>
                }).collect_view()}
                <tr class="sum"><td>"Büro gesamt"</td><td class="num">{size(s.total())}</td></tr>
            </table>
            <p class="muted small">
                {format!("Aktuelle Fassungen: {} · ältere Fassungen: {} ({})",
                    format_size(d.documents_size), d.old_versions, format_size(d.old_versions_size))}
            </p>
        </details>
    }
}

#[component]
fn Tiles(d: Dashboard) -> impl IntoView {
    let c = ctx();
    let tile = |label: &'static str, value: i64, on: Option<Box<dyn Fn() + 'static>>| {
        let clickable = on.is_some();
        view! {
            <div class="card stat-tile" class:clickable=clickable on:click=move |_| { if let Some(f) = &on { f() } }>
                <span class="stat-label">{label}</span>
                <span class="stat-value">{value.to_string()}</span>
            </div>
        }
    };
    view! {
        <div class="stat-tiles">
            {tile("Dokumente", d.documents, Some(Box::new(move || show_docs(c, || ()))))}
            {tile("Im Eingang", d.unfiled, Some(Box::new(move || show_docs(c, || c.unfiled.set(true)))))}
            {tile("Sammlungen", d.collections, None)}
            {tile("Tags", d.tags, None)}
            {tile("Freigaben", d.shares, Some(Box::new(move || c.share_target.set(Some(crate::state::ShareTarget::default())))))}
            {(d.pending > 0).then(|| tile("In Verarbeitung", d.pending, None))}
            {(d.failed > 0).then(|| tile("Texterkennung fehlgeschlagen", d.failed, None))}
        </div>
    }
}

#[component]
fn KindsCard(d: Dashboard) -> impl IntoView {
    let c = ctx();
    let max = d.kinds.iter().map(|k| k.size).max().unwrap_or(0).max(1);
    view! {
        <div class="card dash-card">
            <h3>"Nach Art (Speicherplatz)"</h3>
            <Show when={let empty = d.kinds.is_empty(); move || empty}>
                <p class="muted small">"Noch keine Dokumente."</p>
            </Show>
            <div class="hbars">
                {d.kinds.iter().map(|k| {
                    let kind = k.kind.clone();
                    let w = k.size as f64 * 100.0 / max as f64;
                    let tip = format!("{}: {} Dokumente, {}", shared::kind_label(&k.kind), k.count, format_size(k.size));
                    view! {
                        <div class="hbar-row" title=tip on:click=move |_| { let k = kind.clone(); show_docs(c, move || c.kind.set(k)) }>
                            <span class="hbar-label"><KindIcon kind=k.kind.clone() />{shared::kind_label(&k.kind)}</span>
                            <span class="hbar-track"><span class="hbar" style=format!("width: max({w:.1}%, 2px)")></span></span>
                            <span class="hbar-value">{format_size(k.size)}<span class="muted">{format!(" · {}", k.count)}</span></span>
                        </div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

#[component]
fn MonthsCard(d: Dashboard) -> impl IntoView {
    let max = d.months.iter().map(|m| m.count).max().unwrap_or(0).max(1);
    let total: i64 = d.months.iter().map(|m| m.count).sum();
    view! {
        <div class="card dash-card">
            <h3>"Neue Dokumente pro Monat"</h3>
            <p class="muted small">{format!("{total} in den letzten 12 Monaten")}</p>
            <div class="vbars" role="img" aria-label={d.months.iter().map(|m| format!("{}: {}", m.month, m.count)).collect::<Vec<_>>().join(", ")}>
                {d.months.iter().map(|m| {
                    let h = m.count as f64 * 100.0 / max as f64;
                    let (y, mo) = m.month.split_once('-').unwrap_or(("", ""));
                    let name = mo.parse::<usize>().ok().and_then(|i| MONTHS.get(i.wrapping_sub(1))).copied().unwrap_or("");
                    let tip = format!("{name} {y}: {} Dokumente, {}", m.count, format_size(m.size));
                    view! {
                        <div class="vbar-col" title=tip>
                            <span class="vbar-value">{(m.count > 0 && m.count == max).then(|| m.count.to_string())}</span>
                            <span class="vbar-track">
                                <span class="vbar" style=format!("height: {h:.1}%") class:zero=m.count == 0></span>
                            </span>
                            <span class="vbar-label">{name}</span>
                        </div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

#[component]
fn DocList(title: &'static str, docs: Vec<DocumentBrief>, show_size: bool) -> impl IntoView {
    let c = ctx();
    view! {
        <div class="card dash-card">
            <h3>{title}</h3>
            <Show when={let empty = docs.is_empty(); move || empty}>
                <p class="muted small">"Noch keine Dokumente."</p>
            </Show>
            <ul class="dash-docs">
                {docs.into_iter().map(|doc| {
                    let id = doc.id;
                    let meta = if show_size { format_size(doc.size) } else { format_ts(doc.created_at) };
                    view! {
                        <li on:click=move |_| c.selected.set(Some(id)) title="Details anzeigen">
                            <KindIcon kind=doc.kind.clone() />
                            <span class="dash-doc-title">{doc.title.clone()}</span>
                            <span class="muted small">{meta}</span>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </div>
    }
}
