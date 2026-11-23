// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: Installationsdateien zum Herunterladen und Updates.
//!
//! Beide Endpunkte brauchen keine Anmeldung (die App soll sich auch vor dem
//! ersten Login holen lassen).
//!
//! Die Dateien kommen aus dem GitHub-Release zur Server-Version (`v1.2.3`,
//! Repository aus `BUERO_DESKTOP_REPO`); Downloads und Updates zeigen direkt
//! dorthin. Liegen Installationsdateien in `BUERO_DESKTOP_DIR`, werden
//! stattdessen diese unter `/desktop/` angeboten (z. B. ohne Internet).

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use shared::DesktopDownload;

use crate::state::AppState;

/// Zeichen, die im Dateinamen der Download-Adresse kodiert werden
const FILE_NAME: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Endungen der Installationsdateien, die angeboten werden (Signaturen,
/// Update-Archive u. Ä. im selben Ordner werden ausgelassen)
const EXTENSIONS: &[&str] = &["exe", "msi", "deb", "rpm", "appimage"];

/// Version des Servers; die Installationsdateien im Docker-Image gehören dazu.
pub fn server_version() -> &'static str {
    match option_env!("BUERO_VERSION").map(|v| v.trim().trim_start_matches('v')) {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Eine angebotene Installationsdatei
#[derive(Clone)]
struct Installer {
    name: String,
    size: u64,
    /// Adresse im GitHub-Release; `None` = lokal unter `/desktop/`
    url: Option<String>,
    /// Inhalt der Signatur (`<datei>.sig`), falls vorhanden
    signature: Option<String>,
}

fn is_installer(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    name.contains('.') && EXTENSIONS.contains(&ext.as_str())
}

fn local_installers(dir: &Path) -> Vec<Installer> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_file() && is_installer(&name) {
                let mut sig = e.path().into_os_string();
                sig.push(".sig");
                let signature = std::fs::read_to_string(sig).ok().map(|s| s.trim().to_string());
                out.push(Installer { name, size: meta.len(), url: None, signature });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Zwischenspeicher für das GitHub-Release (Abfragen ohne Anmeldung sind
/// auf 60 pro Stunde begrenzt)
static RELEASE_CACHE: Mutex<Option<(Instant, Vec<Installer>)>> = Mutex::new(None);

#[derive(Deserialize)]
struct GhRelease {
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

async fn fetch_release(st: &AppState, repo: &str) -> anyhow::Result<Vec<Installer>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/v{}", server_version());
    let resp = st
        .http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // Release (noch) nicht veröffentlicht
        return Ok(Vec::new());
    }
    let release: GhRelease = resp.error_for_status()?.json().await?;
    let mut out = Vec::new();
    for a in release.assets.iter().filter(|a| is_installer(&a.name)) {
        let sig_name = format!("{}.sig", a.name);
        let signature = match release.assets.iter().find(|s| s.name == sig_name) {
            Some(s) => {
                let r = st.http.get(&s.browser_download_url).timeout(Duration::from_secs(20)).send().await?;
                Some(r.error_for_status()?.text().await?.trim().to_string())
            }
            None => None,
        };
        out.push(Installer { name: a.name.clone(), size: a.size, url: Some(a.browser_download_url.clone()), signature });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Liegen Installer für Windows und Linux vor?
fn complete(list: &[Installer]) -> bool {
    let has = |exts: &[&str]| list.iter().any(|i| exts.iter().any(|e| i.name.to_lowercase().ends_with(e)));
    has(&[".exe", ".msi"]) && has(&[".deb", ".rpm", ".appimage"])
}

async fn installers(st: &AppState) -> Vec<Installer> {
    let local = local_installers(&st.config.desktop_dir);
    let Some(repo) = st.config.desktop_repo.as_deref().filter(|_| local.is_empty()) else {
        return local;
    };
    if let Some((at, list)) = RELEASE_CACHE.lock().unwrap().as_ref() {
        // Vollständig: länger merken; fehlt das Release oder ein Teil davon noch
        // (Windows- und Linux-Build laden nacheinander hoch), bald erneut fragen
        let ttl = if complete(list) { Duration::from_secs(1800) } else { Duration::from_secs(120) };
        if at.elapsed() < ttl {
            return list.clone();
        }
    }
    let list = match fetch_release(st, repo).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("Desktop-Release von GitHub ({repo}) nicht abrufbar: {e}");
            Vec::new()
        }
    };
    *RELEASE_CACHE.lock().unwrap() = Some((Instant::now(), list.clone()));
    list
}

pub async fn list(State(st): State<AppState>) -> Json<Vec<DesktopDownload>> {
    Json(
        installers(&st)
            .await
            .into_iter()
            .map(|i| DesktopDownload { name: i.name, size: i.size, url: i.url })
            .collect(),
    )
}

/// Platzhalter des Tauri-Updaters: `{{target}}`, `{{arch}}`, `{{bundle_type}}`, `{{current_version}}`
#[derive(Deserialize)]
pub struct UpdateQuery {
    target: String,
    arch: String,
    #[serde(default)]
    bundle: String,
    #[serde(default)]
    version: String,
}

/// `1.2.3` → `[1, 2, 3]` (Zusätze wie `-beta` werden ignoriert)
fn parse_version(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

fn newer(server: &str, client: &str) -> bool {
    let (mut s, mut c) = (parse_version(server), parse_version(client));
    let n = s.len().max(c.len());
    s.resize(n, 0);
    c.resize(n, 0);
    s > c
}

/// Passt die Datei zum Paketformat und zur Architektur des Clients?
fn matches(name: &str, target: &str, arch: &str, bundle: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let bundle = match (bundle, target) {
        ("", "windows") => "nsis",
        ("", "linux") => "appimage",
        (b, _) => b,
    };
    let format_ok = match bundle {
        "nsis" => n.ends_with("-setup.exe"),
        "msi" => n.ends_with(".msi"),
        "deb" => n.ends_with(".deb"),
        "rpm" => n.ends_with(".rpm"),
        "appimage" => n.ends_with(".appimage"),
        _ => false,
    };
    let arch_ok = match arch {
        "x86_64" => ["x64", "amd64", "x86_64"].iter().any(|a| n.contains(a)),
        "aarch64" => ["arm64", "aarch64"].iter().any(|a| n.contains(a)),
        _ => false,
    };
    format_ok && arch_ok
}

/// Update-Auskunft im Format des Tauri-Updaters. 204 = kein Update.
/// Angeboten wird nur, was neben der Installationsdatei eine Signatur
/// (`<datei>.sig`) hat; der Client prüft sie mit seinem eingebauten Schlüssel
/// (die Signatur schützt auch bei Downloads von GitHub).
pub async fn update(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<UpdateQuery>) -> Response {
    let version = server_version();
    if !q.version.is_empty() && !newer(version, &q.version) {
        return StatusCode::NO_CONTENT.into_response();
    }
    let found = installers(&st)
        .await
        .into_iter()
        .find(|i| i.signature.is_some() && matches(&i.name, &q.target, &q.arch, &q.bundle));
    let Some(Installer { name, url, signature: Some(signature), .. }) = found else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let url = url.unwrap_or_else(|| {
        format!("{}/desktop/{}", st.public_base(&headers), percent_encoding::utf8_percent_encode(&name, FILE_NAME))
    });
    Json(serde_json::json!({
        "version": version,
        "notes": "",
        "url": url,
        "signature": signature,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.4.0", "0.3.9"));
        assert!(newer("1.0", "0.99.0"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(!newer("0.4.0", "v0.5.0"));
        assert!(newer("0.10.0", "0.9.1"));
    }

    #[test]
    fn files() {
        assert!(matches("Buero_0.4.0_x64-setup.exe", "windows", "x86_64", "nsis"));
        assert!(matches("Buero_0.4.0_x64-setup.exe", "windows", "x86_64", ""));
        assert!(!matches("Buero_0.4.0_x64_en-US.msi", "windows", "x86_64", "nsis"));
        assert!(matches("Buero_0.4.0_x64_en-US.msi", "windows", "x86_64", "msi"));
        assert!(matches("Buero_0.4.0_amd64.AppImage", "linux", "x86_64", "appimage"));
        assert!(matches("Buero-0.4.0-1.x86_64.rpm", "linux", "x86_64", "rpm"));
        assert!(!matches("Buero_0.4.0_amd64.deb", "linux", "aarch64", "deb"));
    }
}
