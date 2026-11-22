// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Büro – Sammlungen als virtuelles Laufwerk.
//!
//! Linux: FUSE (`fusermount3`), Windows: WinFsp. Die Struktur der Sammlungen
//! erscheint als Ordnerbaum, Dateien werden erst beim Öffnen geladen.

pub mod client;
mod core;

#[cfg(target_os = "linux")]
mod fuse;
#[cfg(windows)]
mod winfs;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::core::INBOX_NAME;
use crate::core::Vfs;

/// Einstellungen zum Einbinden
pub struct Options {
    /// Server-Adresse, z. B. `https://buero.example.com`
    pub server: String,
    pub token: Option<String>,
    /// Linux: Ordner, Windows: Laufwerksbuchstabe (`B:`) oder leerer Ordner
    pub mount_point: PathBuf,
    /// Zwischenspeicher für geladene Dateien
    pub cache_dir: PathBuf,
}

/// Eingebundenes Laufwerk. Beim Verwerfen wird es ausgehängt.
pub struct Drive {
    vfs: Arc<Vfs>,
    mount_point: PathBuf,
    #[cfg(target_os = "linux")]
    _mount: fuse::Mount,
    #[cfg(windows)]
    _mount: winfs::Mount,
}

impl Drive {
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    /// Letzter Fehler beim Hochladen oder Ändern
    pub fn last_error(&self) -> Option<String> {
        self.vfs.last_error()
    }
}

/// Wird das Laufwerk auf diesem System unterstützt?
pub const SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

/// Vorgeschlagener Einhängepunkt
pub fn default_mount_point() -> PathBuf {
    #[cfg(windows)]
    {
        winfs::free_drive_letter().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("B:"))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join("Büro")
    }
}

/// Prüft, ob die Systemvoraussetzungen erfüllt sind (FUSE bzw. WinFsp).
pub fn check_available() -> Result<(), String> {
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

/// Bindet die Sammlungen ein.
pub fn mount(o: Options) -> Result<Drive, String> {
    check_available()?;
    let client = client::Client::new(&o.server, o.token);
    client.ping().map_err(|e| e.to_string())?;
    let vfs = Arc::new(
        Vfs::new(client, o.cache_dir, cfg!(windows)).map_err(|e| format!("Zwischenspeicher: {e}"))?,
    );
    vfs.refresh(true).map_err(|e| format!("Laden fehlgeschlagen: {e:?}"))?;
    #[cfg(target_os = "linux")]
    {
        let m = fuse::mount(vfs.clone(), &o.mount_point)?;
        Ok(Drive { vfs, mount_point: o.mount_point, _mount: m })
    }
    #[cfg(windows)]
    {
        let m = winfs::mount(vfs.clone(), &o.mount_point)?;
        Ok(Drive { vfs, mount_point: o.mount_point, _mount: m })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = vfs;
        Err("Das Laufwerk gibt es nur unter Linux und Windows.".into())
    }
}
