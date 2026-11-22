// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Desktop-Client: Installationsdateien zum Herunterladen und Updates.
//!
//! Beide Endpunkte brauchen keine Anmeldung (die App soll sich auch vor dem
//! ersten Login holen lassen). Die Dateien selbst liegen unter `/desktop/`.

use std::path::{Path, PathBuf};

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

fn installers(dir: &Path) -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_file() && name.contains('.') && EXTENSIONS.contains(&ext.as_str()) {
                out.push((name, e.path(), meta.len()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub async fn list(State(st): State<AppState>) -> Json<Vec<DesktopDownload>> {
    Json(
        installers(&st.config.desktop_dir)
            .into_iter()
            .map(|(name, _, size)| DesktopDownload { name, size })
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
/// (`<datei>.sig`) hat; der Client prüft sie mit seinem eingebauten Schlüssel.
pub async fn update(State(st): State<AppState>, headers: HeaderMap, Query(q): Query<UpdateQuery>) -> Response {
    let version = server_version();
    if !q.version.is_empty() && !newer(version, &q.version) {
        return StatusCode::NO_CONTENT.into_response();
    }
    let found = installers(&st.config.desktop_dir).into_iter().find_map(|(name, path, _)| {
        if !matches(&name, &q.target, &q.arch, &q.bundle) {
            return None;
        }
        let mut sig = path.into_os_string();
        sig.push(".sig");
        let signature = std::fs::read_to_string(sig).ok()?;
        Some((name, signature))
    });
    let Some((name, signature)) = found else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let url = format!(
        "{}/desktop/{}",
        st.public_base(&headers),
        percent_encoding::utf8_percent_encode(&name, FILE_NAME)
    );
    Json(serde_json::json!({
        "version": version,
        "notes": "",
        "url": url,
        "signature": signature.trim(),
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
