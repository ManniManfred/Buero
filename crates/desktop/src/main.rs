// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Desktop-Client.
//!
//! Der Desktop-Client verwendet exakt dieselbe Oberfläche wie die Webseite
//! (Rust/Leptos, nach WebAssembly kompiliert) und spricht dieselbe HTTP-API an.
//! Unterschiede: Server-Adresse ist frei wählbar, Dateien werden mit dem
//! Standardprogramm des Systems geöffnet, und die Sammlungen lassen sich in
//! den Dateimanager einbinden (Windows: Sync-Ordner wie OneDrive, Linux:
//! Laufwerk über FUSE).
//!
//! Solange ein Laufwerk oder Sync-Ordner verbunden ist, muss Büro laufen:
//! „Schließen“ blendet das Fenster dann nur aus, Büro bleibt im Infobereich.
//!
//! Updates kommen vom eigenen Büro-Server (`/api/desktop/update`), damit die
//! App zur Version des Servers passt. Installiert wird nur, was mit dem
//! Schlüssel aus `tauri.conf.json` (`plugins.updater.pubkey`) signiert ist.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
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
    /// Weitere Sync-Ordner, je einer Sammlung zugeordnet
    #[serde(default)]
    folders: Vec<FolderSetting>,
}

/// Sync-Ordner für eine Sammlung
#[derive(Clone, Serialize, Deserialize)]
struct FolderSetting {
    collection_id: i64,
    /// Name der Sammlung beim Einrichten (für Anzeige und Explorer)
    name: String,
    path: String,
}

impl DriveSettings {
    /// Gespeicherte Art (ältere Einstellungen ohne Art: wer schon eingebunden
    /// hat, hatte ein Laufwerk)
    fn saved_mode(&self) -> Option<buero_vfs::Mode> {
        match self.mode.as_deref().and_then(buero_vfs::Mode::parse) {
            Some(m) => Some(m),
            None if self.enabled || self.mount_point.is_some() => Some(buero_vfs::Mode::Drive),
            None => None,
        }
    }

    fn mode(&self) -> buero_vfs::Mode {
        let modes = buero_vfs::modes();
        // Nicht (mehr) vorhandene Art (z. B. das frühere WinFsp-Laufwerk): die Empfehlung
        self.saved_mode()
            .filter(|m| modes.contains(m))
            .or_else(|| modes.first().copied())
            .unwrap_or(buero_vfs::Mode::Drive)
    }

    /// Gespeicherter Ort, sofern er zur Art passt (ein früherer Laufwerks-
    /// buchstabe taugt nicht als Sync-Ordner)
    fn mount_point(&self) -> Option<String> {
        self.mount_point.clone().filter(|_| self.saved_mode() == Some(self.mode()))
    }

    /// Beim Start einbinden? Nicht, wenn die eingerichtete Art weggefallen ist
    /// (dann soll nicht ungefragt ein Sync-Ordner entstehen)
    fn enabled(&self) -> bool {
        self.enabled && self.saved_mode() == Some(self.mode())
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
    /// Sync-Ordner je Sammlung (Schlüssel: Sammlung)
    folders: Mutex<std::collections::HashMap<i64, Mounted>>,
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
struct FolderStatus {
    collection_id: i64,
    name: String,
    path: String,
    connected: bool,
    last_error: Option<String>,
}

#[derive(Serialize)]
struct DriveStatus {
    supported: bool,
    /// Fehlende Systemvoraussetzung (FUSE bzw. Windows-Version) der gewählten Art, sonst `None`
    unavailable: Option<String>,
    enabled: bool,
    mounted: bool,
    /// Gewählte bzw. eingebundene Art („sync“ / „drive“)
    mode: String,
    modes: Vec<ModeInfo>,
    mount_point: String,
    last_error: Option<String>,
    /// Sync-Ordner je Sammlung möglich (Windows); sonst Grund
    folders_unavailable: Option<String>,
    folders: Vec<FolderStatus>,
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
            .mount_point()
            .filter(|_| s.mode() == mode)
            .unwrap_or_else(|| buero_vfs::default_mount_point(mode).display().to_string()),
    };
    DriveStatus {
        supported: buero_vfs::SUPPORTED,
        unavailable: buero_vfs::check_available(mode).err(),
        enabled: s.enabled(),
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
        folders_unavailable: buero_vfs::check_available(buero_vfs::Mode::SyncFolder).err(),
        folders: {
            let f = st.folders.lock().unwrap_or_else(|e| e.into_inner());
            s.folders
                .iter()
                .map(|x| {
                    let m = f.get(&x.collection_id);
                    FolderStatus {
                        collection_id: x.collection_id,
                        name: x.name.clone(),
                        path: x.path.clone(),
                        connected: m.is_some(),
                        last_error: m.and_then(|m| m.drive.last_error()),
                    }
                })
                .collect()
        },
    }
}

fn unmount_now(st: &DriveState) {
    let m = st.mounted.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(m);
}

/// Alle Sync-Ordner der Sammlungen trennen
fn disconnect_folders(st: &DriveState) {
    let all: Vec<Mounted> = st.folders.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, m)| m).collect();
    drop(all);
}

fn cache_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_cache_dir().map_err(|e| e.to_string())?.join("laufwerk"))
}

/// Sync-Ordner einer Sammlung verbinden
async fn connect_folder(
    app: &tauri::AppHandle,
    st: &DriveState,
    server: String,
    token: Option<String>,
    f: &FolderSetting,
) -> Result<(), String> {
    let old = st.folders.lock().unwrap_or_else(|e| e.into_inner()).remove(&f.collection_id);
    drop(old);
    let opts = buero_vfs::Options {
        server: server.clone(),
        token: token.clone(),
        mode: buero_vfs::Mode::SyncFolder,
        mount_point: PathBuf::from(f.path.trim()),
        cache_dir: cache_dir(app)?,
        collection: Some((f.collection_id, f.name.clone())),
    };
    let drive = tauri::async_runtime::spawn_blocking(move || buero_vfs::mount(opts))
        .await
        .map_err(|e| e.to_string())??;
    st.folders
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(f.collection_id, Mounted { drive, server, token });
    Ok(())
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
    let opts = buero_vfs::Options {
        server: server.clone(),
        token: token.clone(),
        mode,
        mount_point: mp,
        cache_dir: cache_dir(app)?,
        collection: None,
    };
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
    let mut s = load_settings(&app);
    s.enabled = true;
    s.mount_point = actual.or(mount_point);
    s.mode = Some(mode.as_str().into());
    save_settings(&app, &s)?;
    Ok(status(&app, &st))
}

/// Sync-Ordner für eine Sammlung einrichten (bzw. Ort ändern) und merken
#[tauri::command]
async fn folder_add(
    app: tauri::AppHandle,
    st: State<'_, DriveState>,
    server: String,
    token: Option<String>,
    collection_id: i64,
    name: String,
    path: Option<String>,
) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    let path = path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| buero_vfs::default_folder_for(&name).display().to_string());
    let mut s = load_settings(&app);
    let taken = |p: &str| {
        let p = p.trim_end_matches(['\\', '/']).to_lowercase();
        s.folders.iter().any(|f| f.collection_id != collection_id && f.path.trim_end_matches(['\\', '/']).to_lowercase() == p)
            || (s.enabled()
                && s.mode() == buero_vfs::Mode::SyncFolder
                && s.mount_point().map(|m| m.trim_end_matches(['\\', '/']).to_lowercase()) == Some(p))
    };
    if taken(&path) {
        return Err(format!("„{path}“ wird schon für einen anderen Sync-Ordner verwendet."));
    }
    let f = FolderSetting { collection_id, name, path };
    connect_folder(&app, &st, server, token, &f).await?;
    s.folders.retain(|x| x.collection_id != collection_id);
    s.folders.push(f);
    save_settings(&app, &s)?;
    Ok(status(&app, &st))
}

/// Sync-Ordner einer Sammlung trennen, bei Windows abmelden und vergessen
#[tauri::command]
async fn folder_remove(app: tauri::AppHandle, st: State<'_, DriveState>, collection_id: i64) -> Result<DriveStatus, String> {
    let _busy = st.busy.lock().await;
    let old = st.folders.lock().unwrap_or_else(|e| e.into_inner()).remove(&collection_id);
    drop(old);
    let mut s = load_settings(&app);
    s.folders.retain(|x| x.collection_id != collection_id);
    save_settings(&app, &s)?;
    tauri::async_runtime::spawn_blocking(move || buero_vfs::forget_sync_folder(Some(collection_id)))
        .await
        .map_err(|e| e.to_string())??;
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
        disconnect_folders(&st);
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
    if s.enabled() && current != Some((server.clone(), token.clone())) && buero_vfs::check_available(mode).is_ok() {
        do_mount(&app, &st, server.clone(), token.clone(), mode, s.mount_point()).await?;
    }
    // Sync-Ordner der Sammlungen; ein Fehler hält die anderen nicht auf
    if buero_vfs::check_available(buero_vfs::Mode::SyncFolder).is_ok() {
        let mut problems = Vec::new();
        for f in &s.folders {
            let cur = st
                .folders
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&f.collection_id)
                .map(|m| (m.server.clone(), m.token.clone()));
            if cur != Some((server.clone(), token.clone())) {
                if let Err(e) = connect_folder(&app, &st, server.clone(), token.clone(), f).await {
                    problems.push(format!("{}: {e}", f.name));
                }
            }
        }
        if !problems.is_empty() {
            return Err(problems.join("\n"));
        }
    }
    Ok(status(&app, &st))
}

/// Laufwerk bzw. Sync-Ordner (`collection_id`) im Dateimanager öffnen
#[tauri::command]
fn drive_open(app: tauri::AppHandle, st: State<'_, DriveState>, collection_id: Option<i64>) -> Result<(), String> {
    if let Some(c) = collection_id {
        let p = st
            .folders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&c)
            .map(|m| m.drive.mount_point().display().to_string())
            .ok_or("Nicht verbunden")?;
        return app.opener().open_path(p, None::<&str>).map_err(|e| e.to_string());
    }
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
// Aktivität der Sync-Ordner
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct FolderActivity {
    /// `None`: Sync-Ordner für alle Sammlungen
    collection_id: Option<i64>,
    name: String,
    path: String,
    connected: bool,
    /// Zustand und letzte Aktivitäten (nur wenn verbunden)
    state: Option<buero_vfs::SyncState>,
}

/// Alle eingerichteten Sync-Ordner mit Zustand und letzten Aktivitäten
#[tauri::command]
fn sync_activity(app: tauri::AppHandle, st: State<'_, DriveState>) -> Vec<FolderActivity> {
    let s = load_settings(&app);
    let mut out = Vec::new();
    if let Some(m) = st.mounted.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if let Some(state) = m.drive.activity() {
            out.push(FolderActivity {
                collection_id: None,
                name: "Alle Sammlungen".into(),
                path: m.drive.mount_point().display().to_string(),
                connected: true,
                state: Some(state),
            });
        }
    }
    let folders = st.folders.lock().unwrap_or_else(|e| e.into_inner());
    for f in &s.folders {
        let m = folders.get(&f.collection_id);
        out.push(FolderActivity {
            collection_id: Some(f.collection_id),
            name: f.name.clone(),
            path: f.path.clone(),
            connected: m.is_some(),
            state: m.and_then(|m| m.drive.activity()),
        });
    }
    out
}

/// Sofort abgleichen (`collection_id`: nur diesen Ordner, sonst alle)
#[tauri::command]
fn sync_now(st: State<'_, DriveState>, collection_id: Option<i64>) {
    if collection_id.is_none() {
        if let Some(m) = st.mounted.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            m.drive.sync_now();
        }
    }
    for (c, m) in st.folders.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        if collection_id.is_none_or(|x| x == *c) {
            m.drive.sync_now();
        }
    }
}

/// Protokolldatei im Standardprogramm öffnen
#[tauri::command]
fn sync_log_open(app: tauri::AppHandle) -> Result<(), String> {
    let file = buero_vfs::log_file(&cache_dir(&app)?);
    if !file.exists() {
        return Err("Noch keine Einträge im Protokoll".into());
    }
    app.opener().open_path(file.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

/// Aktivitätsfenster zeigen (bzw. anlegen)
fn show_activity(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(ACTIVITY_WINDOW) {
        w.show()?;
        w.unminimize()?;
        return w.set_focus();
    }
    // Dieselbe Oberfläche wie das Hauptfenster, aber nur die Aktivitätsansicht
    tauri::WebviewWindowBuilder::new(app, ACTIVITY_WINDOW, tauri::WebviewUrl::App("index.html".into()))
        .title("Büro – Synchronisierung")
        .inner_size(560.0, 680.0)
        .min_inner_size(380.0, 360.0)
        .initialization_script("window.__BUERO_VIEW__ = 'activity';")
        .build()?;
    Ok(())
}

const ACTIVITY_WINDOW: &str = "activity";

/// Aktivitätsfenster öffnen (async: unter Windows dürfen Fenster nicht aus
/// einem synchronen Befehl heraus entstehen)
#[tauri::command]
async fn activity_open(app: tauri::AppHandle) -> Result<(), String> {
    show_activity(&app).map_err(|e| e.to_string())
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
        disconnect_folders(&drive);
    }
    update.install(bytes).map_err(|e| format!("Installation fehlgeschlagen: {e}"))?;
    app.restart()
}

// ---------------------------------------------------------------------------
// Infobereich (Tray)
// ---------------------------------------------------------------------------

/// Ist ein Laufwerk oder Sync-Ordner verbunden? Dann muss Büro weiterlaufen,
/// sonst wird nichts mehr abgeglichen und Platzhalter lassen sich nicht laden.
fn sync_active(st: &DriveState) -> bool {
    st.mounted.lock().unwrap_or_else(|e| e.into_inner()).is_some()
        || !st.folders.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// Hauptfenster zeigen und nach vorn holen
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Fenster zeigen (die Oberfläche ruft das, wenn sie Eingaben braucht, z. B.
/// nach einem versteckten Autostart ohne gültige Anmeldung)
#[tauri::command]
fn show_window(app: tauri::AppHandle) {
    show_main(&app);
}

// ---------------------------------------------------------------------------
// Autostart
// ---------------------------------------------------------------------------

/// Kennzeichen des Starts bei der Anmeldung (wird im Autostart-Eintrag hinterlegt)
const AUTOSTART_ARG: &str = "--autostart";

/// Startet Büro mit der Anmeldung am System?
#[tauri::command]
fn autostart_get(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// Autostart ein- oder ausschalten
#[tauri::command]
fn autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| format!("Autostart: {e}"))?;
    al.is_enabled().map_err(|e| e.to_string())
}

/// Beim Autostart nur im Infobereich starten, wenn es etwas abzugleichen gibt;
/// sonst (und bei jedem normalen Start) das Fenster zeigen.
fn start_hidden(app: &tauri::AppHandle) -> bool {
    let s = load_settings(app);
    std::env::args().any(|a| a == AUTOSTART_ARG) && (s.enabled() || !s.folders.is_empty())
}

fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Büro öffnen", true, None::<&str>)?;
    let activity = MenuItem::with_id(app, "activity", "Synchronisierung …", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &activity, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Büro")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "activity" => {
                let _ = show_activity(app);
            }
            // Beim Beenden trennt RunEvent::Exit Laufwerk und Sync-Ordner
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Muss als erstes Plugin stehen
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().args([AUTOSTART_ARG]).build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(DriveState::default())
        .manage(UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            drive_status,
            drive_mount,
            drive_unmount,
            drive_sync,
            drive_open,
            folder_add,
            folder_remove,
            update_check,
            update_install,
            show_window,
            autostart_get,
            autostart_set,
            sync_activity,
            sync_now,
            sync_log_open,
            activity_open
        ])
        .setup(|app| {
            setup_tray(app.handle())?;
            // Das Fenster startet unsichtbar (tauri.conf.json), damit es beim
            // Autostart nicht kurz aufblitzt
            if !start_hidden(app.handle()) {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Schließen bei verbundenem Sync-Ordner: nur ausblenden, Büro läuft im
            // Infobereich weiter (Beenden über dessen Menü)
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && sync_active(&window.app_handle().state::<DriveState>()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Fehler beim Starten von Büro")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Sauber aushängen, sonst bleibt ein toter Einhängepunkt zurück
                unmount_now(&app.state::<DriveState>());
                disconnect_folders(&app.state::<DriveState>());
            }
        });
}
