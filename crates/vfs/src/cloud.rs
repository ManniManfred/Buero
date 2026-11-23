// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Windows: Sammlungen als Sync-Ordner über die Cloud Files API – so wie
//! OneDrive oder Nextcloud.
//!
//! Der Ordner ist ein normaler Ordner auf der Platte (mit eigenem Eintrag im
//! Explorer). Dokumente liegen dort als Platzhalter und werden erst beim
//! Öffnen geladen; „Immer auf diesem Gerät behalten“ und „Speicherplatz
//! freigeben“ funktionieren wie gewohnt. Kein Zusatzprogramm nötig.
//!
//! Was wann hoch- oder heruntergeladen wird, entscheidet der Abgleich in
//! [`crate::sync`]; hier steckt nur die Anbindung an Windows:
//! - [`WinLocal`]: Platzhalter lesen, anlegen, aktualisieren
//! - [`Filter`]: Rückrufe von Windows (Inhalt laden, Löschen/Umbenennen erlauben)
//! - ein Hintergrund-Thread, der bei Änderungen und regelmäßig abgleicht

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::ext::FileExt;
use cloud_filter::filter::{info, ticket, Request, StateWatchEvent, SyncFilter};
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder::{ConvertOptions, PinState, Placeholder, UpdateOptions};
use cloud_filter::placeholder_create_info::PlaceholderCreateInfo;
use cloud_filter::root::{
    Connection, HydrationPolicy, HydrationType, PopulationType, SecurityId, Session, SyncRootId, SyncRootIdBuilder,
    SyncRootInfo,
};
use cloud_filter::utility::{FileTime, WriteAt};

use crate::client::Client;
use crate::core::INBOX_NAME;
use crate::sync::{Engine, LocalEntry, LocalFs, Meta};

const PROVIDER: &str = "Buero";
const ACCOUNT: &str = "Buero";
const DISPLAY_NAME: &str = "Büro";
/// `FILE_SHARE_READ`: andere dürfen lesen, aber nicht schreiben
const SHARE_READ: u32 = 0x1;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const ERROR_SHARING_VIOLATION: i32 = 32;
const CHUNK: usize = 64 * 1024;
/// So oft wird ohne lokale Änderungen beim Server nachgesehen
const POLL: Duration = Duration::from_secs(20);
/// Nach einer Änderung so lange auf Ruhe warten (Speichern besteht oft aus
/// mehreren Schritten)
const QUIET: Duration = Duration::from_millis(1500);

fn win(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Wird der Sync-Ordner von diesem Windows unterstützt?
pub fn check_available() -> Result<(), String> {
    match cloud_filter::root::is_supported() {
        Ok(true) => Ok(()),
        _ => Err("Der Sync-Ordner braucht Windows 10 (Version 1709) oder neuer.".into()),
    }
}

/// Pfad in der Form, die auch die Rückrufe liefern (`C:\…` statt `\\?\C:\…`)
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    let Some(Component::Prefix(prefix)) = path.components().next() else { return Ok(path) };
    let mut out = match prefix.kind() {
        Prefix::VerbatimDisk(d) => PathBuf::from(format!("{}:\\", char::from(d))),
        Prefix::VerbatimUNC(server, share) => PathBuf::from(r"\\").join(server).join(share),
        _ => return Ok(path),
    };
    out.extend(path.components().skip(2));
    Ok(out)
}

fn sync_root_id() -> Result<SyncRootId, String> {
    let sid = SecurityId::current_user().map_err(text)?;
    Ok(SyncRootIdBuilder::new(PROVIDER).user_security_id(sid).account_name(ACCOUNT).build())
}

/// Ordner bei Windows anmelden. `true`: neu angemeldet (Index verwerfen).
fn register(root: &Path, server: &str) -> Result<bool, String> {
    let id = sync_root_id()?;
    if id.is_registered().unwrap_or(false) {
        let info = id.info().map_err(text)?;
        let same_path = canonical(&info.path()).ok().as_deref() == Some(root);
        if same_path && info.blob() == server.as_bytes() {
            return Ok(false);
        }
        // Anderer Ordner oder anderer Server: alte Anmeldung lösen (nur online
        // vorhandene Platzhalter verschwinden, geladene Dateien bleiben)
        id.unregister().map_err(|e| format!("Bisherigen Sync-Ordner lösen: {e}"))?;
    }
    if fs::read_dir(root).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!(
            "Der Ordner „{}“ ist nicht leer. Bitte einen neuen oder leeren Ordner wählen.",
            root.display()
        ));
    }
    let icon = std::env::current_exe()
        .map(|p| format!("{},0", p.display()))
        .unwrap_or_else(|_| "%SystemRoot%\\system32\\imageres.dll,-1043".into());
    let info = SyncRootInfo::default()
        .with_display_name(DISPLAY_NAME)
        .with_hydration_type(HydrationType::Full)
        .with_hydration_policy(HydrationPolicy::AutoDehydrationAllowed)
        .with_allow_pinning(true)
        .with_population_type(PopulationType::AlwaysFull)
        .with_icon(icon)
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_blob(server.as_bytes())
        .with_path(root)
        .map_err(text)?;
    id.register(info).map_err(|e| format!("Sync-Ordner bei Windows anmelden: {e}"))?;
    Ok(true)
}

/// Verbundener Sync-Ordner. Beim Verwerfen wird die Verbindung getrennt; der
/// Ordner bleibt angemeldet (geladene Dateien bleiben nutzbar).
pub struct Mount {
    connection: Option<Connection<Filter>>,
    stop: Arc<AtomicBool>,
    wake: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        drop(self.connection.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn mount(
    client: Client,
    server: &str,
    root: &Path,
    cache: &Path,
    errors: Arc<Mutex<Option<String>>>,
) -> Result<Mount, String> {
    check_available()?;
    fs::create_dir_all(root).map_err(|e| format!("Ordner „{}“ anlegen: {e}", root.display()))?;
    let root = canonical(root).map_err(text)?;
    let fresh = register(&root, server)?;
    let docs = cache.join("docs");
    let uploads = cache.join("upload");
    for d in [&docs, &uploads] {
        fs::create_dir_all(d).map_err(|e| format!("Zwischenspeicher: {e}"))?;
    }
    let index = cache.join("sync-index.json");
    if fresh {
        let _ = fs::remove_file(&index);
    }

    let (tx, rx) = mpsc::channel();
    let filter = Filter { client: client.clone(), root: root.clone(), docs, wake: tx.clone(), download: Mutex::new(()) };
    let connection = Session::new().connect(&root, filter).map_err(|e| format!("Sync-Ordner verbinden: {e}"))?;
    let local = WinLocal { root, uploads, counter: AtomicU64::new(0) };
    let engine = Engine::new(client, local, Some(index));
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let thread = std::thread::Builder::new()
        .name("buero-sync".into())
        .spawn(move || run(engine, rx, stop2, errors))
        .map_err(text)?;
    let _ = tx.send(());
    Ok(Mount { connection: Some(connection), stop, wake: tx, thread: Some(thread) })
}

/// Abgleich im Hintergrund: nach Änderungen (mit kurzer Ruhepause) und regelmäßig
fn run(mut engine: Engine<Client, WinLocal>, rx: Receiver<()>, stop: Arc<AtomicBool>, errors: Arc<Mutex<Option<String>>>) {
    let mut next = Duration::ZERO;
    loop {
        match rx.recv_timeout(next) {
            Ok(()) => loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                match rx.recv_timeout(QUIET) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let res = engine.reconcile();
        let msg = match &res {
            Ok(p) if p.errors.is_empty() => None,
            Ok(p) => {
                for e in &p.errors {
                    log::warn!("Büro-Sync: {e}");
                }
                Some(p.errors.join("\n"))
            }
            Err(e) => {
                log::warn!("Büro-Sync: {e}");
                Some(e.clone())
            }
        };
        if let Ok(mut e) = errors.lock() {
            *e = msg;
        }
        next = match res {
            Ok(p) if p.pending => Duration::from_secs(3),
            Ok(_) => POLL,
            Err(_) => Duration::from_secs(30),
        };
    }
}

// ---------------------------------------------------------------------------
// Lokaler Ordner
// ---------------------------------------------------------------------------

pub struct WinLocal {
    root: PathBuf,
    uploads: PathBuf,
    counter: AtomicU64,
}

/// Öffnen nur für Attribute: lädt keinen Inhalt und kollidiert mit keiner Sperre
fn open_attr(path: &Path) -> std::io::Result<Placeholder> {
    OpenOptions::new().access_mode(0).custom_flags(BACKUP_SEMANTICS).open(path).map(Placeholder::from)
}

/// Zum Lesen öffnen und Schreiben anderer verhindern
fn open_guard(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).share_mode(SHARE_READ).open(path)
}

fn file_time(secs: i64) -> FileTime {
    FileTime::from_unix_time_secs(secs).unwrap_or_else(|_| FileTime::now())
}

impl WinLocal {
    fn full(&self, p: &[String]) -> PathBuf {
        let mut x = self.root.clone();
        x.extend(p);
        x
    }

    fn split<'a>(&self, p: &'a [String]) -> std::io::Result<(PathBuf, &'a str)> {
        let (name, parent) = p.split_last().ok_or_else(|| win("leerer Pfad"))?;
        Ok((self.full(parent), name.as_str()))
    }
}

impl LocalFs for WinLocal {
    fn scan(&self) -> std::io::Result<Vec<LocalEntry>> {
        let mut out = Vec::new();
        let mut stack: Vec<Vec<String>> = vec![Vec::new()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(self.full(&dir))? {
                let entry = entry?;
                let ft = entry.file_type()?;
                if ft.is_symlink() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
                let mut path = dir.clone();
                path.push(name);
                let is_dir = ft.is_dir();
                let info = open_attr(&entry.path())?.info().map_err(win)?;
                let (meta, dirty) = match &info {
                    Some(i) => (Meta::from_blob(i.blob()), !is_dir && (!i.is_in_sync() || i.modified_data_size() > 0)),
                    None => (None, !is_dir),
                };
                if is_dir {
                    stack.push(path.clone());
                }
                out.push(LocalEntry { path, is_dir, meta, dirty });
            }
        }
        Ok(out)
    }

    fn create_dir(&self, path: &[String], meta: &Meta) -> std::io::Result<()> {
        let (parent, name) = self.split(path)?;
        let now = FileTime::now();
        PlaceholderCreateInfo::new(name)
            .metadata(Metadata::directory().created(now).written(now).changed(now).accessed(now))
            .mark_in_sync()
            .blob(meta.to_blob())
            .create(parent)
            .map_err(win)?;
        Ok(())
    }

    fn create_file(&self, path: &[String], meta: &Meta, size: u64, mtime: i64) -> std::io::Result<()> {
        let (parent, name) = self.split(path)?;
        let t = file_time(mtime);
        PlaceholderCreateInfo::new(name)
            .metadata(Metadata::file().size(size).created(t).written(t).changed(t).accessed(t))
            .mark_in_sync()
            .blob(meta.to_blob())
            .create(parent)
            .map_err(win)?;
        Ok(())
    }

    fn replace_file(&self, path: &[String], meta: &Meta, size: u64, mtime: i64) -> std::io::Result<()> {
        let full = self.full(path);
        let pinned = open_attr(&full)?.info().map_err(win)?.is_some_and(|i| i.pin_state() == PinState::Pinned);
        let t = file_time(mtime);
        let blob = meta.to_blob();
        // Exklusiv: schlägt fehl, solange die Datei geöffnet ist (dann später erneut)
        let mut ph = Placeholder::options().exclusive().write_access().open(&full).map_err(win)?;
        ph.update(
            UpdateOptions::default()
                .metadata(Metadata::file().size(size).written(t).changed(t))
                .blob(&blob)
                .mark_in_sync()
                .dehydrate(),
            None,
        )
        .map_err(win)?;
        drop(ph);
        if pinned {
            // „Immer auf diesem Gerät behalten“: neue Fassung gleich laden
            let _ = open_attr(&full).and_then(|mut p| p.hydrate(..).map_err(win));
        }
        Ok(())
    }

    fn snapshot(&self, path: &[String]) -> std::io::Result<(PathBuf, u64)> {
        let mut src = open_guard(&self.full(path))?;
        let usn = src.usn().map_err(win)?;
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let copy = self.uploads.join(format!("{}-{n}", std::process::id()));
        let mut dst = File::create(&copy)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
        Ok((copy, usn as u64))
    }

    fn mark_synced(&self, path: &[String], meta: &Meta, stamp: Option<u64>) -> std::io::Result<bool> {
        let full = self.full(path);
        let is_dir = full.is_dir();
        // Während des Markierens darf niemand schreiben
        let guard = if is_dir {
            None
        } else {
            match open_guard(&full) {
                Ok(f) => Some(f),
                // Gerade zum Schreiben geöffnet: später erneut
                Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => return Ok(false),
                Err(e) => return Err(e),
            }
        };
        let unchanged = match (stamp, &guard) {
            (Some(s), Some(g)) => g.usn().map_err(win)? as u64 == s,
            _ => true,
        };
        let blob = meta.to_blob();
        let mut ph = open_attr(&full)?;
        if ph.info().map_err(win)?.is_none() {
            let mut o = ConvertOptions::default().blob(blob.clone());
            if unchanged {
                o = o.mark_in_sync();
            }
            ph.convert_to_placeholder(o, None).map_err(win)?;
        } else {
            let mut o = UpdateOptions::default().blob(&blob);
            if unchanged {
                o = o.mark_in_sync();
            }
            ph.update(o, None).map_err(win)?;
        }
        drop(guard);
        Ok(unchanged)
    }

    fn rename(&self, from: &[String], to: &[String]) -> std::io::Result<()> {
        fs::rename(self.full(from), self.full(to))
    }

    fn remove(&self, path: &[String], is_dir: bool) -> std::io::Result<()> {
        if is_dir {
            fs::remove_dir(self.full(path))
        } else {
            fs::remove_file(self.full(path))
        }
    }
}

// ---------------------------------------------------------------------------
// Rückrufe von Windows
// ---------------------------------------------------------------------------

pub struct Filter {
    client: Client,
    root: PathBuf,
    /// Zwischenspeicher geladener Fassungen (`<id>-v<version>`)
    docs: PathBuf,
    wake: Sender<()>,
    download: Mutex<()>,
}

impl Filter {
    fn wake(&self) {
        let _ = self.wake.send(());
    }

    fn is_inbox(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root)
            .ok()
            .is_some_and(|r| r.components().count() == 1 && r.as_os_str().to_string_lossy().eq_ignore_ascii_case(INBOX_NAME))
    }

    /// Fassung eines Dokuments im Zwischenspeicher bereitstellen
    fn cached(&self, id: i64, version: i64) -> Result<PathBuf, String> {
        let path = self.docs.join(format!("{id}-v{version}"));
        if path.exists() {
            return Ok(path);
        }
        let _d = self.download.lock().unwrap_or_else(|e| e.into_inner());
        if path.exists() {
            return Ok(path);
        }
        // Ältere Fassungen wegräumen
        if let Ok(rd) = fs::read_dir(&self.docs) {
            let prefix = format!("{id}-v");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        self.client.download(id, &path).map_err(text)?;
        Ok(path)
    }
}

impl SyncFilter for Filter {
    fn fetch_data(&self, request: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        let Some(Meta::Doc { id, version, .. }) = Meta::from_blob(request.file_blob()) else {
            return Err(CloudErrorKind::InvalidRequest);
        };
        let path = self.cached(id, version).map_err(|e| {
            log::warn!("Büro-Sync: Laden fehlgeschlagen: {e}");
            CloudErrorKind::NetworkUnavailable
        })?;
        let mut file = File::open(&path).map_err(|_| CloudErrorKind::Unsuccessful)?;
        let len = file.metadata().map_err(|_| CloudErrorKind::Unsuccessful)?.len();
        if len != request.file_size() {
            // Am Server gibt es inzwischen eine andere Fassung: Platzhalter erneuern
            let _ = fs::remove_file(&path);
            self.wake();
            return Err(CloudErrorKind::NotInSync);
        }
        let range = info.required_file_range();
        let end = range.end.min(len);
        file.seek(SeekFrom::Start(range.start)).map_err(|_| CloudErrorKind::Unsuccessful)?;
        let mut pos = range.start;
        let mut buf = vec![0u8; CHUNK];
        while pos < end {
            let n = ((end - pos) as usize).min(CHUNK);
            file.read_exact(&mut buf[..n]).map_err(|_| CloudErrorKind::Unsuccessful)?;
            ticket.write_at(&buf[..n], pos).map_err(|e| {
                log::warn!("Büro-Sync: Übertragen fehlgeschlagen: {e}");
                CloudErrorKind::Unsuccessful
            })?;
            pos += n as u64;
            let _ = ticket.report_progress(end - range.start, pos - range.start);
        }
        Ok(())
    }

    fn dehydrate(&self, _request: Request, ticket: ticket::Dehydrate, _info: info::Dehydrate) -> CResult<()> {
        // Windows prüft selbst, dass nur Abgeglichenes freigegeben wird
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn delete(&self, request: Request, ticket: ticket::Delete, info: info::Delete) -> CResult<()> {
        if !info.is_undelete() && self.is_inbox(&request.path()) {
            return Err(CloudErrorKind::AccessDenied);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn deleted(&self, _request: Request, _info: info::Deleted) {
        self.wake();
    }

    fn rename(&self, request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
        if self.is_inbox(&request.path()) {
            return Err(CloudErrorKind::AccessDenied);
        }
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }

    fn renamed(&self, _request: Request, _info: info::Renamed) {
        self.wake();
    }

    fn closed(&self, _request: Request, _info: info::Closed) {
        self.wake();
    }

    fn state_changed(&self, event: StateWatchEvent) {
        if let StateWatchEvent::Stopped(e) = &event {
            log::warn!("Büro-Sync: Überwachung des Ordners beendet: {e}");
        }
        self.wake();
    }
}
