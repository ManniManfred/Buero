// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Desktop-Client.
//!
//! Der Desktop-Client verwendet exakt dieselbe Oberfläche wie die Webseite
//! (Rust/Leptos, nach WebAssembly kompiliert) und spricht dieselbe HTTP-API an.
//! Unterschiede: Server-Adresse ist frei wählbar, Dateien werden mit dem
//! Standardprogramm des Systems geöffnet, und die Sammlungen lassen sich in
//! den Dateimanager einbinden (Windows: Sync-Ordner wie OneDrive oder Laufwerk
//! über WinFsp, Linux: FUSE).
//!
//! Updates kommen vom eigenen Büro-Server (`/api/desktop/update`), damit die
//! App zur Version des Servers passt. Installiert wird nur, was mit dem
//! Schlüssel aus `tauri.conf.json` (`plugins.updater.pubkey`) signiert ist.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Manager, RunEvent, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

/// Gespeicherte Einstellungen des Laufwerks
#[derive(Default, Clone, Serialize, Deserialize)]
struct DriveSettings {
    /// Beim Start automatisch einbinden
    enabled: bool,
    mount_point: Option<String>,
    /// „sync“ oder „drive“ (fehlt bei älteren Einstellungen: Laufwerk)
    #[serde(default)]
    mode: Option<String>,
}

impl DriveSettings {
    fn mode(&self) -> buero_vfs::Mode {
        match self.mode.as_deref().and_then(buero_vfs::Mode::parse) {
            Some(m) => m,
            // Wer schon ein Laufwerk eingerichtet hat, behält es; sonst die Empfehlung
            None if self.enabled || self.mount_point.is_some() => buero_vfs::Mode::Drive,
            None => buero_vfs::modes().first().copied().unwrap_or(buero_vfs::Mode::Drive),
        }
    }
}

struct Mounted {
    drive: buero_vfs::Drive,
    server: String,
    token: Option<String>,
}

#[derive(Default)]
struct DriveState {
    mounted: Mutex<Option<Mounted>>,
    /// Verhindert gleichzeitiges Ein-/Aushängen
    busy: tokio::sync::Mutex<()>,
}

#[derive(Serialize)]
struct ModeInfo {
    id: &'static str,
    /// Vorgeschlagener Ort
    default_path: String,
    /// Fehlende Systemvoraussetzung, sonst `None`
    unavailable: Option<String>,
}

#[derive(Serialize)]
struct DriveStatus {
    supported: bool,
    /// Fehlende Systemvoraussetzung (FUSE/WinFsp) der gewählten Art, sonst `None`
    unavailable: Option<String>,
    enabled: bool,
    mounted: bool,
    /// Gewählte bzw. eingebundene Art („sync“ / „drive“)
    mode: String,
    modes: Vec<ModeInfo>,
    mount_point: String,
    last_error: Option<String>,
}

fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("laufwerk.json"))
}

fn load_settings(app: &tauri::AppHandle) -> DriveSettings {
    settings_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_settings(app: &tauri::AppHandle, s: &DriveSettings) -> Result<(), String> {
    let p = settings_path(app).ok_or("Kein Konfigurationsordner")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(s).unwrap_or_default()).map_err(|e| e.to_string())
}

fn status(app: &tauri::AppHandle, st: &DriveState) -> DriveStatus {
    let s = load_settings(app);
    let m = st.mounted.lock().unwrap_or_else(|e| e.into_inner());
    let mode = m.as_ref().map(|m| m.drive.mode()).unwrap_or_else(|| s.mode());
    let mount_point = match &*m {
        Some(m) => m.drive.mount_point().display().to_string(),
        None => s
            .mount_point
            .clone()
            .filter(|_| s.mode() == mode)
            .unwrap_or_else(|| buero_vfs::default_mount_point(mode).display().to_string()),
    };
    DriveStatus {
        supported: buero_vfs::SUPPORTED,
        unavailable: buero_vfs::check_available(mode).err(),
        enabled: s.enabled,
        mounted: m.is_some(),
        mode: mode.as_str().into(),
        modes: buero_vfs::modes()
            .into_iter()
            .map(|md| ModeInfo {
                id: md.as_str(),
                default_path: buero_vfs::default_mount_point(md).display().to_string(),
                unavailable: buero_vfs::check_available(md).err(),
            })
            .collect(),
        mount_point,
        last_error: m.as_ref().and_then(|m| m.drive.last_error()),
    }
}

fn unmount_now(st: &DriveState) {
    let m = st.mounted.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(m);
}

async fn do_mount(
    app: &tauri::AppHandle,
    st: &DriveState,
    server: String,
    token: Option<String>,
    mode: buero_vfs::Mode,
    mount_point: Option<String>,
) -> Result<(), String> {
    unmount_now(st);
    let mp = mount_point
        .filter(|m| !m.trim().is_empty())
        .map(|m| PathBuf::from(m.trim()))
        .unwrap_or_else(|| buero_vfs::default_mount_point(mode));
    let cache_dir = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("laufwerk");
    let opts = buero_vfs::Options { server: server.clone(), token: token.clone(), mode, mount_point: mp, cache_dir };
    let drive = tauri::async_runtime::spawn_blocking(move || buero_vfs::mount(opts))
        .await
        .map_err(|e| e.to_string())??;
    *st.mounted.lock().unwrap_or_else(|e| e.into_inner()) = Some(Mounted { drive, server, token });
    Ok(())
}

#[tauri::command]
fn drive_status(app: tauri::AppHandle, st: State<'_, DriveState>) -> DriveStatus {
    status(&app, &st)
}

/// Einbinden und für künftige Starts merken
#[tauri::command]
async fn drive_mount(
    app: tauri::AppHandle,
    st: State<'_, DriveState>,
    server: String,
    token: Option<String>,
    mode: Option<String>,
    mount_point: Option<String>,
) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    let mode = mode.as_deref().and_then(buero_vfs::Mode::parse).unwrap_or_else(|| load_settings(&app).mode());
    do_mount(&app, &st, server, token, mode, mount_point.clone()).await?;
    let actual = st
        .mounted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.drive.mount_point().display().to_string());
    save_settings(
        &app,
        &DriveSettings { enabled: true, mount_point: actual.or(mount_point), mode: Some(mode.as_str().into()) },
    )?;
    Ok(status(&app, &st))
}

/// Aushängen und nicht mehr automatisch einbinden
#[tauri::command]
async fn drive_unmount(app: tauri::AppHandle, st: State<'_, DriveState>) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    unmount_now(&st);
    let mut s = load_settings(&app);
    s.enabled = false;
    save_settings(&app, &s)?;
    Ok(status(&app, &st))
}

/// Nach der Anmeldung: eingeschaltetes Laufwerk einbinden bzw. bei neuer
/// Anmeldung/anderem Server neu einbinden. `token: None` = abgemeldet.
#[tauri::command]
async fn drive_sync(
    app: tauri::AppHandle,
    st: State<'_, DriveState>,
    server: String,
    token: Option<String>,
    logged_in: bool,
) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    if !logged_in {
        unmount_now(&st);
        return Ok(status(&app, &st));
    }
    let s = load_settings(&app);
    let current = st
        .mounted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| (m.server.clone(), m.token.clone()));
    let mode = s.mode();
    if s.enabled && current != Some((server.clone(), token.clone())) && buero_vfs::check_available(mode).is_ok() {
        do_mount(&app, &st, server, token, mode, s.mount_point).await?;
    }
    Ok(status(&app, &st))
}

/// Laufwerk im Dateimanager öffnen
#[tauri::command]
fn drive_open(app: tauri::AppHandle, st: State<'_, DriveState>) -> Result<(), String> {
    let mp = st
        .mounted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.drive.mount_point().to_path_buf())
        .ok_or("Nicht eingebunden")?;
    let mut p = mp.display().to_string();
    // „B:“ allein wäre das aktuelle Verzeichnis auf B:
    if p.len() == 2 && p.ends_with(':') {
        p.push('\\');
    }
    app.opener().open_path(p, None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

/// Gefundenes, noch nicht installiertes Update
#[derive(Default)]
struct UpdateState(Mutex<Option<tauri_plugin_updater::Update>>);

#[derive(Serialize)]
struct UpdateInfo {
    /// Ohne Signaturschlüssel im Build gibt es keine Updates
    available: bool,
    current: String,
    /// Neue Version, falls es eine gibt
    version: Option<String>,
}

/// Öffentlicher Schlüssel aus `plugins.updater.pubkey` (leer = Updates aus)
fn updater_pubkey(app: &tauri::AppHandle) -> Option<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Beim Server nach einer neueren Version fragen
#[tauri::command]
async fn update_check(app: tauri::AppHandle, st: State<'_, UpdateState>, server: String) -> Result<UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    if updater_pubkey(&app).is_none() || server.trim().is_empty() {
        return Ok(UpdateInfo { available: false, current, version: None });
    }
    let endpoint = format!(
        "{}/api/desktop/update?target={{{{target}}}}&arch={{{{arch}}}}&bundle={{{{bundle_type}}}}&version={{{{current_version}}}}",
        server.trim().trim_end_matches('/')
    );
    let url = endpoint.parse().map_err(|e| format!("Ungültige Server-Adresse: {e}"))?;
    let update = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("Suche nach Updates fehlgeschlagen: {e}"))?;
    let version = update.as_ref().map(|u| u.version.clone());
    *st.0.lock().unwrap_or_else(|e| e.into_inner()) = update;
    Ok(UpdateInfo { available: true, current, version })
}

/// Gefundenes Update herunterladen, installieren und neu starten
#[tauri::command]
async fn update_install(
    app: tauri::AppHandle,
    st: State<'_, UpdateState>,
    drive: State<'_, DriveState>,
) -> Result<(), String> {
    let update = st.0.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or("Kein Update gefunden")?;
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| format!("Download fehlgeschlagen: {e}"))?;
    // Vor dem Installieren aushängen: unter Windows beendet der Installer die App
    {
        let _busy = drive.busy.lock().await;
        unmount_now(&drive);
    }
    update.install(bytes).map_err(|e| format!("Installation fehlgeschlagen: {e}"))?;
    app.restart()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DriveState::default())
        .manage(UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            drive_status,
            drive_mount,
            drive_unmount,
            drive_sync,
            drive_open,
            update_check,
            update_install
        ])
        .build(tauri::generate_context!())
        .expect("Fehler beim Starten von Büro")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Sauber aushängen, sonst bleibt ein toter Einhängepunkt zurück
                unmount_now(&app.state::<DriveState>());
            }
        });
}
