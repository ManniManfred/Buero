// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Sammlungen im Dateimanager.
//!
//! Zwei Arten:
//! - **Sync-Ordner** (Windows, wie OneDrive/Nextcloud): ein normaler Ordner mit
//!   Platzhaltern, die erst beim Öffnen geladen werden (Cloud Files API)
//! - **Laufwerk** (nur Linux, FUSE): virtuelles Dateisystem
//!
//! Die Struktur der Sammlungen erscheint in beiden Fällen als Ordnerbaum.
//! Sync-Ordner können außerdem genau einer Sammlung zugeordnet sein (mehrere
//! Ordner gleichzeitig möglich).

#[cfg_attr(not(windows), allow(dead_code))]
mod activity;
pub mod client;
// Unter Windows nutzt nur der Sync-Ordner Teile davon (das Laufwerk gibt es dort nicht)
#[cfg_attr(windows, allow(dead_code))]
mod core;
pub mod sync;

#[cfg(windows)]
mod cloud;
#[cfg(target_os = "linux")]
mod fuse;

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;

pub use crate::activity::{ActivityEntry, SyncState};
pub use crate::core::INBOX_NAME;
pub use crate::sync::ActKind;
#[cfg(target_os = "linux")]
use crate::core::Vfs;

/// Art der Einbindung
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Echter Ordner mit Platzhaltern (nur Windows)
    SyncFolder,
    /// Virtuelles Laufwerk (nur Linux, FUSE)
    Drive,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::SyncFolder => "sync",
            Mode::Drive => "drive",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "sync" => Some(Mode::SyncFolder),
            "drive" => Some(Mode::Drive),
            _ => None,
        }
    }
}

/// Auf diesem System mögliche Arten, die empfohlene zuerst
pub fn modes() -> Vec<Mode> {
    if cfg!(windows) {
        vec![Mode::SyncFolder]
    } else if cfg!(target_os = "linux") {
        vec![Mode::Drive]
    } else {
        Vec::new()
    }
}

/// Einstellungen zum Einbinden
pub struct Options {
    /// Server-Adresse, z. B. `https://buero.example.com`
    pub server: String,
    pub token: Option<String>,
    pub mode: Mode,
    /// Sync-Ordner (Windows) bzw. Einhängepunkt (Linux)
    pub mount_point: PathBuf,
    /// Zwischenspeicher für geladene Dateien
    pub cache_dir: PathBuf,
    /// Nur Sync-Ordner: zugeordnete Sammlung (ID, Name); `None` = alle Sammlungen
    pub collection: Option<(i64, String)>,
}

#[allow(clippy::large_enum_variant)]
enum Inner {
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    Vfs { vfs: Arc<Vfs>, mount: fuse::Mount },
    /// Andere Systeme: nichts einbindbar
    #[cfg(not(any(target_os = "linux", windows)))]
    #[allow(dead_code)]
    Unsupported,
    #[cfg(windows)]
    #[allow(dead_code)]
    Sync { mount: cloud::Mount, log: activity::SharedLog },
}

/// Eingebundene Sammlungen. Beim Verwerfen wird ausgehängt bzw. getrennt.
pub struct Drive {
    mode: Mode,
    collection: Option<i64>,
    mount_point: PathBuf,
    inner: Inner,
}

impl Drive {
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Zugeordnete Sammlung (nur Sync-Ordner)
    pub fn collection(&self) -> Option<i64> {
        self.collection
    }

    /// Letzter Fehler beim Hochladen oder Abgleichen
    pub fn last_error(&self) -> Option<String> {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Vfs { vfs, .. } => vfs.last_error(),
            #[cfg(windows)]
            Inner::Sync { log, .. } => {
                let l = log.lock().ok()?;
                (!l.errors().is_empty()).then(|| l.errors().join("\n"))
            }
            #[cfg(not(any(target_os = "linux", windows)))]
            Inner::Unsupported => None,
        }
    }

    /// Zustand und letzte Aktivitäten (nur Sync-Ordner)
    pub fn activity(&self) -> Option<SyncState> {
        match &self.inner {
            #[cfg(windows)]
            Inner::Sync { log, .. } => log.lock().ok().map(|l| l.state()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Sofort abgleichen (nur Sync-Ordner)
    pub fn sync_now(&self) {
        #[cfg(windows)]
        {
            let Inner::Sync { mount, .. } = &self.inner;
            mount.sync_now();
        }
    }
}

/// Protokolldatei der Sync-Ordner (Aktivitäten und Fehler)
pub fn log_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("sync.log")
}

/// Wird das Einbinden auf diesem System unterstützt?
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

/// Vorgeschlagener Ort
pub fn default_mount_point(mode: Mode) -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/tmp" }));
    match mode {
        Mode::SyncFolder | Mode::Drive => home.join("Büro"),
    }
}

/// Prüft, ob die Systemvoraussetzungen erfüllt sind (FUSE bzw. Windows-Version).
pub fn check_available(mode: Mode) -> Result<(), String> {
    match mode {
        Mode::SyncFolder => {
            #[cfg(windows)]
            {
                cloud::check_available()
            }
            #[cfg(not(windows))]
            {
                Err("Den Sync-Ordner gibt es nur unter Windows.".into())
            }
        }
        Mode::Drive => {
            #[cfg(target_os = "linux")]
            {
                let found = ["fusermount3", "fusermount"].iter().any(|b| {
                    std::env::var_os("PATH")
                        .map(|p| std::env::split_paths(&p).any(|d| d.join(b).exists()))
                        .unwrap_or(false)
                });
                if found && Path::new("/dev/fuse").exists() {
                    Ok(())
                } else {
                    Err("FUSE ist nicht installiert (Paket „fuse3“).".into())
                }
            }
            #[cfg(windows)]
            {
                Err("Unter Windows gibt es nur den Sync-Ordner.".into())
            }
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                Err("Das Laufwerk gibt es nur unter Linux.".into())
            }
        }
    }
}

/// Bindet die Sammlungen ein.
pub fn mount(o: Options) -> Result<Drive, String> {
    check_available(o.mode)?;
    let client = client::Client::new(&o.server, o.token);
    client.ping().map_err(|e| e.to_string())?;
    if o.mode == Mode::SyncFolder {
        #[cfg(windows)]
        {
            let coll = o.collection.as_ref().map(|(c, _)| *c);
            let cache = o.cache_dir.join(match coll {
                Some(c) => format!("sync-{c}"),
                None => "sync".into(),
            });
            let name = o.collection.as_ref().map(|(_, n)| n.clone()).unwrap_or_else(|| "Alle Sammlungen".into());
            let log = activity::ActivityLog::new(name, Some(log_file(&o.cache_dir)));
            let mount = cloud::mount(client, &o.server, &o.mount_point, &cache, o.collection, log.clone())?;
            return Ok(Drive {
                mode: o.mode,
                collection: coll,
                mount_point: o.mount_point,
                inner: Inner::Sync { mount, log },
            });
        }
        #[cfg(not(windows))]
        {
            return Err("Den Sync-Ordner gibt es nur unter Windows.".into());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let vfs = Arc::new(Vfs::new(client, o.cache_dir, false).map_err(|e| format!("Zwischenspeicher: {e}"))?);
        vfs.refresh(true).map_err(|e| format!("Laden fehlgeschlagen: {e:?}"))?;
        let mount = fuse::mount(vfs.clone(), &o.mount_point)?;
        Ok(Drive { mode: o.mode, collection: None, mount_point: o.mount_point, inner: Inner::Vfs { vfs, mount } })
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Unerreichbar: check_available lehnt das Laufwerk hier ab
        let _ = client;
        Err("Das Laufwerk gibt es nur unter Linux.".into())
    }
}

/// Sync-Ordner bei Windows abmelden, nachdem er getrennt wurde (Ordner
/// entfernt). Geladene Dateien bleiben, nur online vorhandene verschwinden.
pub fn forget_sync_folder(collection: Option<i64>) -> Result<(), String> {
    #[cfg(windows)]
    {
        cloud::unregister(collection)
    }
    #[cfg(not(windows))]
    {
        let _ = collection;
        Ok(())
    }
}

/// Vorgeschlagener Ort für den Sync-Ordner einer Sammlung
pub fn default_folder_for(name: &str) -> PathBuf {
    let base = default_mount_point(Mode::SyncFolder);
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_end_matches('.').to_string();
    let file = format!("{} - {}", base.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), clean);
    base.with_file_name(file)
}
