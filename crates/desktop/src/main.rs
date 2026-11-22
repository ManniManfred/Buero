// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Desktop-Client.
//!
//! Der Desktop-Client verwendet exakt dieselbe Oberfläche wie die Webseite
//! (Rust/Leptos, nach WebAssembly kompiliert) und spricht dieselbe HTTP-API an.
//! Unterschiede: Server-Adresse ist frei wählbar, Dateien werden mit dem
//! Standardprogramm des Systems geöffnet, und die Sammlungen lassen sich als
//! Laufwerk ins System einbinden (Linux: FUSE, Windows: WinFsp).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Manager, RunEvent, State};
use tauri_plugin_opener::OpenerExt;

/// Gespeicherte Einstellungen des Laufwerks
#[derive(Default, Clone, Serialize, Deserialize)]
struct DriveSettings {
    /// Beim Start automatisch einbinden
    enabled: bool,
    mount_point: Option<String>,
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
struct DriveStatus {
    supported: bool,
    /// Fehlende Systemvoraussetzung (FUSE/WinFsp), sonst `None`
    unavailable: Option<String>,
    enabled: bool,
    mounted: bool,
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
    let mount_point = match &*m {
        Some(m) => m.drive.mount_point().display().to_string(),
        None => s.mount_point.clone().unwrap_or_else(|| buero_vfs::default_mount_point().display().to_string()),
    };
    DriveStatus {
        supported: buero_vfs::SUPPORTED,
        unavailable: buero_vfs::check_available().err(),
        enabled: s.enabled,
        mounted: m.is_some(),
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
    mount_point: Option<String>,
) -> Result<(), String> {
    unmount_now(st);
    let mp = mount_point
        .filter(|m| !m.trim().is_empty())
        .map(|m| PathBuf::from(m.trim()))
        .unwrap_or_else(buero_vfs::default_mount_point);
    let cache_dir = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("laufwerk");
    let opts = buero_vfs::Options { server: server.clone(), token: token.clone(), mount_point: mp, cache_dir };
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
    mount_point: Option<String>,
) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    do_mount(&app, &st, server, token, mount_point.clone()).await?;
    let actual = st
        .mounted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.drive.mount_point().display().to_string());
    save_settings(&app, &DriveSettings { enabled: true, mount_point: actual.or(mount_point) })?;
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
    if s.enabled && current != Some((server.clone(), token.clone())) && buero_vfs::check_available().is_ok() {
        do_mount(&app, &st, server, token, s.mount_point).await?;
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
        .ok_or("Laufwerk ist nicht eingebunden")?;
    let mut p = mp.display().to_string();
    // „B:“ allein wäre das aktuelle Verzeichnis auf B:
    if p.len() == 2 && p.ends_with(':') {
        p.push('\\');
    }
    app.opener().open_path(p, None::<&str>).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DriveState::default())
        .invoke_handler(tauri::generate_handler![drive_status, drive_mount, drive_unmount, drive_sync, drive_open])
        .build(tauri::generate_context!())
        .expect("Fehler beim Starten von Büro")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Sauber aushängen, sonst bleibt ein toter Einhängepunkt zurück
                unmount_now(&app.state::<DriveState>());
            }
        });
}
