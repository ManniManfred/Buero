// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Download des Desktop-Clients (nur in der Weboberfläche).

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::DesktopDownload;

use crate::icons::Icon;
use crate::state::ctx;

/// Hier liegen alle Versionen, falls der Server selbst keine Dateien anbietet
const RELEASES_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases");

#[derive(Clone, Copy, PartialEq)]
enum Os {
    Windows,
    Linux,
    Other,
}

fn current_os() -> Os {
    let ua = window().navigator().user_agent().unwrap_or_default();
    if ua.contains("Windows") {
        Os::Windows
    } else if ua.contains("Linux") && !ua.contains("Android") {
        Os::Linux
    } else {
        Os::Other
    }
}

/// Betriebssystem und Beschreibung zu einer Installationsdatei
fn describe(name: &str) -> (Os, &'static str) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "exe" => (Os::Windows, "Windows – Installationsprogramm"),
        "msi" => (Os::Windows, "Windows – MSI-Paket (für die Verteilung im Netzwerk)"),
        "deb" => (Os::Linux, "Linux – Debian, Ubuntu, Mint (.deb)"),
        "rpm" => (Os::Linux, "Linux – Fedora, openSUSE (.rpm)"),
        "appimage" => (Os::Linux, "Linux – AppImage (ohne Installation)"),
        _ => (Os::Other, "Sonstige"),
    }
}

fn file_url(api: crate::api::Api, name: &str) -> String {
    api.url(&format!("/desktop/{}", String::from(js_sys::encode_uri_component(name))))
}

fn load(files: RwSignal<Option<Vec<DesktopDownload>>>) {
    // Kontext vorher holen: in der asynchronen Aufgabe gibt es keinen
    let api = ctx().api;
    spawn_local(async move {
        let r = api.get::<Vec<DesktopDownload>>("/api/desktop").await;
        files.set(Some(r.unwrap_or_default()));
    });
}

/// Abschnitt in den Einstellungen: alle angebotenen Installationsdateien,
/// passende für das eigene System zuerst.
#[component]
pub fn DesktopSection() -> impl IntoView {
    let api = ctx().api;
    let files = RwSignal::new(None::<Vec<DesktopDownload>>);
    load(files);
    let os = current_os();

    let list = move || {
        let Some(mut files) = files.get() else {
            return view! { <p class="muted small">"Lädt …"</p> }.into_any();
        };
        if files.is_empty() {
            return view! {
                <p class="muted small">
                    "Dieser Server bietet keine Installationsdateien an. Alle Versionen gibt es auf "
                    <a href=RELEASES_URL target="_blank" rel="noopener">"GitHub"</a>"."
                </p>
            }
            .into_any();
        }
        files.sort_by_key(|f| describe(&f.name).0 != os);
        view! {
            <ul class="downloads">
                {files
                    .into_iter()
                    .map(|f| {
                        let (fos, label) = describe(&f.name);
                        let size = crate::util::format_size(f.size as i64);
                        view! {
                            <li class:recommended=fos == os && os != Os::Other>
                                <a href=file_url(api, &f.name) download=f.name.clone()>
                                    <Icon name="download" />
                                    <span class="download-label">{label}</span>
                                </a>
                                <span class="muted small">{f.name.clone()}" · "{size}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        }
        .into_any()
    };

    view! {
        <h3>"Desktop-App"</h3>
        <p class="muted small">
            "Büro als Programm für Windows und Linux: öffnet Dateien mit den installierten Programmen "
            "und bindet die Sammlungen als Laufwerk im Dateimanager ein."
        </p>
        {list}
    }
}

/// Datei, die für das eigene System empfohlen wird: Windows-Installer bzw.
/// unter Linux das AppImage (läuft auf jeder Distribution)
fn preferred(files: &[DesktopDownload], os: Os) -> Option<&DesktopDownload> {
    let ext = match os {
        Os::Windows => ".exe",
        Os::Linux => ".appimage",
        Os::Other => return None,
    };
    files
        .iter()
        .find(|f| f.name.to_ascii_lowercase().ends_with(ext))
        .or_else(|| files.iter().find(|f| describe(&f.name).0 == os))
}

/// Unauffälliger Link unter der Anmeldung, nur wenn es eine passende Datei gibt
#[component]
pub fn DesktopLoginLink() -> impl IntoView {
    let api = ctx().api;
    let files = RwSignal::new(None::<Vec<DesktopDownload>>);
    load(files);
    let os = current_os();
    move || {
        let files = files.get()?;
        let f = preferred(&files, os)?;
        let label = if os == Os::Windows { "Desktop-App für Windows herunterladen" } else { "Desktop-App für Linux herunterladen" };
        Some(view! {
            <a class="small muted login-download" href=file_url(api, &f.name) download=f.name.clone()>
                <Icon name="download" />{label}
            </a>
        })
    }
}
