// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Sammlungen im Dateimanager.
//!
//! Zwei Arten:
//! - **Sync-Ordner** (Windows, wie OneDrive/Nextcloud): ein normaler Ordner mit
//!   Platzhaltern, die erst beim Öffnen geladen werden (Cloud Files API)
//! - **Laufwerk** (Linux: FUSE, Windows: WinFsp): virtuelles Dateisystem
//!
//! Die Struktur der Sammlungen erscheint in beiden Fällen als Ordnerbaum.

pub mod client;
mod core;
pub mod sync;

#[cfg(windows)]
mod cloud;
#[cfg(target_os = "linux")]
mod fuse;
#[cfg(windows)]
mod winfs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(windows)]
use std::sync::Mutex;

pub use crate::core::INBOX_NAME;
use crate::core::Vfs;

/// Art der Einbindung
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Echter Ordner mit Platzhaltern (nur Windows)
    SyncFolder,
    /// Virtuelles Laufwerk (FUSE / WinFsp)
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
        vec![Mode::SyncFolder, Mode::Drive]
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
    /// Sync-Ordner bzw. Einhängepunkt (Linux: Ordner, Windows: `B:` oder leerer Ordner)
    pub mount_point: PathBuf,
    /// Zwischenspeicher für geladene Dateien
    pub cache_dir: PathBuf,
}

#[allow(clippy::large_enum_variant)]
enum Inner {
    #[allow(dead_code)]
    Vfs {
        vfs: Arc<Vfs>,
        #[cfg(target_os = "linux")]
        mount: fuse::Mount,
        #[cfg(windows)]
        mount: winfs::Mount,
    },
    #[cfg(windows)]
    #[allow(dead_code)]
    Sync { mount: cloud::Mount, errors: Arc<Mutex<Option<String>>> },
}

/// Eingebundene Sammlungen. Beim Verwerfen wird ausgehängt bzw. getrennt.
pub struct Drive {
    mode: Mode,
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

    /// Letzter Fehler beim Hochladen oder Abgleichen
    pub fn last_error(&self) -> Option<String> {
        match &self.inner {
            Inner::Vfs { vfs, .. } => vfs.last_error(),
            #[cfg(windows)]
            Inner::Sync { errors, .. } => errors.lock().ok()?.clone(),
        }
    }
}

/// Wird das Einbinden auf diesem System unterstützt?
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

/// Vorgeschlagener Ort
pub fn default_mount_point(mode: Mode) -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/tmp" }));
    match mode {
        Mode::SyncFolder => home.join("Büro"),
        Mode::Drive => {
            #[cfg(windows)]
            {
                winfs::free_drive_letter().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("B:"))
            }
            #[cfg(not(windows))]
            {
                home.join("Büro")
            }
        }
    }
}

/// Prüft, ob die Systemvoraussetzungen erfüllt sind (FUSE, WinFsp bzw. Windows-Version).
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
                winfs::check_available()
            }
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                Err("Das Laufwerk gibt es nur unter Linux und Windows.".into())
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
            let errors = Arc::new(Mutex::new(None));
            let mount = cloud::mount(client, &o.server, &o.mount_point, &o.cache_dir.join("sync"), errors.clone())?;
            return Ok(Drive { mode: o.mode, mount_point: o.mount_point, inner: Inner::Sync { mount, errors } });
        }
        #[cfg(not(windows))]
        {
            return Err("Den Sync-Ordner gibt es nur unter Windows.".into());
        }
    }
    let vfs = Arc::new(
        Vfs::new(client, o.cache_dir, cfg!(windows)).map_err(|e| format!("Zwischenspeicher: {e}"))?,
    );
    vfs.refresh(true).map_err(|e| format!("Laden fehlgeschlagen: {e:?}"))?;
    #[cfg(target_os = "linux")]
    {
        let mount = fuse::mount(vfs.clone(), &o.mount_point)?;
        Ok(Drive { mode: o.mode, mount_point: o.mount_point, inner: Inner::Vfs { vfs, mount } })
    }
    #[cfg(windows)]
    {
        let mount = winfs::mount(vfs.clone(), &o.mount_point)?;
        Ok(Drive { mode: o.mode, mount_point: o.mount_point, inner: Inner::Vfs { vfs, mount } })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = vfs;
        Err("Das Laufwerk gibt es nur unter Linux und Windows.".into())
    }
}
