// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Windows: Einbinden als Laufwerk über WinFsp (https://winfsp.dev).

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    HLOCAL, LocalFree, NTSTATUS, STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY, STATUS_END_OF_FILE,
    STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_PARAMETER, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION,
    STATUS_OBJECT_NAME_NOT_FOUND, STATUS_UNEXPECTED_IO_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{GetSecurityDescriptorLength, PSECURITY_DESCRIPTOR};
use winfsp::constants::FspCleanupFlags;
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, ModificationDescriptor, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, FineGuard, VolumeParams};
use winfsp::{FspError, U16CStr};

use crate::core::{Attr, Error, Ino, Kind, Vfs, ROOT_INO};

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const FILE_DIRECTORY_FILE: u32 = 0x01;
const FILE_WRITE_DATA: u32 = 0x02;
const FILE_APPEND_DATA: u32 = 0x04;
/// Alle Vollzugriff: System, Administratoren, Jeder
const SDDL: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)";

fn status(e: Error) -> FspError {
    let s: NTSTATUS = match e {
        Error::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        Error::Exists => STATUS_OBJECT_NAME_COLLISION,
        Error::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        Error::NotDir => STATUS_NOT_A_DIRECTORY,
        Error::IsDir => STATUS_FILE_IS_A_DIRECTORY,
        Error::Perm => STATUS_ACCESS_DENIED,
        Error::Invalid => STATUS_INVALID_PARAMETER,
        Error::Io(msg) => {
            log::warn!("Büro-Laufwerk: {msg}");
            STATUS_UNEXPECTED_IO_ERROR
        }
    };
    s.into()
}

type R<T> = winfsp::Result<T>;

/// FILETIME: 100 ns seit 1601
fn filetime(t: SystemTime) -> u64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(d.subsec_nanos() / 100)
}

fn fill(fi: &mut FileInfo, a: &Attr) {
    let dir = a.kind == Kind::Dir;
    fi.file_attributes = if dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_ARCHIVE };
    fi.reparse_tag = 0;
    fi.file_size = a.size;
    fi.allocation_size = a.size.div_ceil(4096) * 4096;
    fi.creation_time = filetime(a.crtime);
    fi.last_access_time = filetime(a.mtime);
    fi.last_write_time = filetime(a.mtime);
    fi.change_time = filetime(a.ctime);
    fi.index_number = a.ino;
    fi.hard_links = 0;
    fi.ea_size = 0;
}

/// Pfad in übergeordneten Pfad und Namen zerlegen (`\a\b` → `\a`, `b`)
fn split(path: &str) -> (&str, &str) {
    let p = path.trim_end_matches('\\');
    match p.rfind('\\') {
        Some(i) => (&p[..i], &p[i + 1..]),
        None => ("", p),
    }
}

fn wide(s: &U16CStr) -> String {
    s.to_string_lossy()
}

/// Geöffnete Datei bzw. geöffneter Ordner
pub struct Ctx {
    ino: Ino,
    dir: bool,
    path: Mutex<String>,
    wants_write: bool,
    /// Handle im Kern (erst beim ersten Lesen/Schreiben, damit reines
    /// Abfragen von Eigenschaften nichts herunterlädt) und ob beschreibbar
    fh: Mutex<Option<(u64, bool)>>,
    dir_buffer: DirBuffer,
    deleted: AtomicBool,
}

struct Fs {
    vfs: Arc<Vfs>,
    sd: Vec<u8>,
}

impl Fs {
    fn ctx(&self, path: String, a: &Attr, wants_write: bool, fh: Option<(u64, bool)>) -> Ctx {
        Ctx {
            ino: a.ino,
            dir: a.kind == Kind::Dir,
            path: Mutex::new(path),
            wants_write,
            fh: Mutex::new(fh),
            dir_buffer: DirBuffer::new(),
            deleted: AtomicBool::new(false),
        }
    }

    /// Kern-Handle holen bzw. öffnen
    fn handle(&self, c: &Ctx, write: bool) -> R<u64> {
        let mut fh = c.fh.lock().unwrap_or_else(|e| e.into_inner());
        match *fh {
            Some((h, w)) if w || !write => return Ok(h),
            Some((h, _)) => {
                // Nur lesend geöffnet, jetzt wird geschrieben
                let _ = self.vfs.release(h);
            }
            None => {}
        }
        let w = write || c.wants_write;
        let h = self.vfs.open(c.ino, w, false).map_err(status)?;
        *fh = Some((h, w));
        Ok(h)
    }

    fn attr(&self, c: &Ctx) -> R<Attr> {
        let fh = c.fh.lock().unwrap_or_else(|e| e.into_inner()).map(|(h, _)| h);
        match fh {
            Some(h) => self.vfs.handle_attr(h).or_else(|_| self.vfs.getattr(c.ino)),
            None => self.vfs.getattr(c.ino),
        }
        .map_err(status)
    }

    fn copy_sd(&self, buf: Option<&mut [c_void]>) -> u64 {
        if let Some(buf) = buf {
            if buf.len() >= self.sd.len() {
                // SAFETY: Zielpuffer ist mindestens so groß wie der Deskriptor
                unsafe { std::ptr::copy_nonoverlapping(self.sd.as_ptr(), buf.as_mut_ptr().cast::<u8>(), self.sd.len()) };
            }
        }
        self.sd.len() as u64
    }
}

impl FileSystemContext for Fs {
    type FileContext = Ctx;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> R<FileSecurity> {
        let a = self.vfs.resolve(&wide(file_name)).map_err(status)?;
        let attributes = if a.kind == Kind::Dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_ARCHIVE };
        Ok(FileSecurity { reparse: false, sz_security_descriptor: self.copy_sd(security_descriptor), attributes })
    }

    fn open(&self, file_name: &U16CStr, _opts: u32, access: u32, file_info: &mut OpenFileInfo) -> R<Ctx> {
        let path = wide(file_name);
        let a = self.vfs.resolve(&path).map_err(status)?;
        fill(file_info.as_mut(), &a);
        Ok(self.ctx(path, &a, access & (FILE_WRITE_DATA | FILE_APPEND_DATA) != 0, None))
    }

    fn close(&self, c: Ctx) {
        let fh = c.fh.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((h, _)) = fh {
            if c.deleted.load(Ordering::SeqCst) {
                self.vfs.discard(h);
            } else {
                let _ = self.vfs.release(h);
            }
        }
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _access: u32,
        _attrs: u32,
        _sd: Option<&[c_void]>,
        _alloc: u64,
        _extra: Option<&[u8]>,
        _extra_is_reparse: bool,
        file_info: &mut OpenFileInfo,
    ) -> R<Ctx> {
        let path = wide(file_name);
        let (parent, name) = split(&path);
        let p = self.vfs.resolve(parent).map_err(status)?;
        if create_options & FILE_DIRECTORY_FILE != 0 {
            let a = self.vfs.mkdir(p.ino, name).map_err(status)?;
            fill(file_info.as_mut(), &a);
            Ok(self.ctx(path, &a, false, None))
        } else {
            let (a, fh) = self.vfs.create(p.ino, name).map_err(status)?;
            fill(file_info.as_mut(), &a);
            Ok(self.ctx(path, &a, true, Some((fh, true))))
        }
    }

    fn cleanup(&self, c: &Ctx, _file_name: Option<&U16CStr>, flags: u32) {
        if FspCleanupFlags::FspCleanupDelete.is_flagged(flags) {
            let path = c.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let (parent, name) = split(&path);
            if let Some((h, _)) = c.fh.lock().unwrap_or_else(|e| e.into_inner()).take() {
                self.vfs.discard(h);
            }
            let res = self.vfs.resolve(parent).and_then(|p| {
                if c.dir {
                    self.vfs.rmdir(p.ino, name)
                } else {
                    self.vfs.unlink(p.ino, name)
                }
            });
            if let Err(e) = res {
                log::warn!("Büro-Laufwerk: Löschen von {path} fehlgeschlagen: {e:?}");
            }
            c.deleted.store(true, Ordering::SeqCst);
            return;
        }
        // Beim Schließen hochladen (Fehler werden im Kern gemeldet)
        let fh = c.fh.lock().unwrap_or_else(|e| e.into_inner()).map(|(h, _)| h);
        if let Some(h) = fh {
            let _ = self.vfs.flush(h);
        }
    }

    fn flush(&self, c: Option<&Ctx>, file_info: &mut FileInfo) -> R<()> {
        let Some(c) = c else { return Ok(()) };
        let fh = c.fh.lock().unwrap_or_else(|e| e.into_inner()).map(|(h, _)| h);
        if let Some(h) = fh {
            self.vfs.flush(h).map_err(status)?;
        }
        fill(file_info, &self.attr(c)?);
        Ok(())
    }

    fn get_file_info(&self, c: &Ctx, file_info: &mut FileInfo) -> R<()> {
        fill(file_info, &self.attr(c)?);
        Ok(())
    }

    fn get_security(&self, _c: &Ctx, security_descriptor: Option<&mut [c_void]>) -> R<u64> {
        Ok(self.copy_sd(security_descriptor))
    }

    fn set_security(&self, _c: &Ctx, _info: u32, _d: ModificationDescriptor) -> R<()> {
        // Rechte werden nicht gespeichert
        Ok(())
    }

    fn overwrite(
        &self,
        c: &Ctx,
        _attrs: u32,
        _replace: bool,
        _alloc: u64,
        _extra: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> R<()> {
        let mut fh = c.fh.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((h, _)) = fh.take() {
            let _ = self.vfs.release(h);
        }
        let h = self.vfs.open(c.ino, true, true).map_err(status)?;
        *fh = Some((h, true));
        drop(fh);
        fill(file_info, &self.attr(c)?);
        Ok(())
    }

    fn read_directory(&self, c: &Ctx, _pattern: Option<&U16CStr>, marker: DirMarker, buffer: &mut [u8]) -> R<u32> {
        if marker.is_none() {
            let entries = self.vfs.readdir(c.ino).map_err(status)?;
            let lock = c.dir_buffer.acquire(true, None)?;
            let mut info: DirInfo = DirInfo::new();
            if c.ino != ROOT_INO {
                let me = self.vfs.getattr(c.ino).map_err(status)?;
                let parent = self.vfs.getattr(self.vfs.parent(c.ino)).map_err(status)?;
                for (n, a) in [(".", &me), ("..", &parent)] {
                    info.reset();
                    fill(info.file_info_mut(), a);
                    info.set_name(n)?;
                    lock.write(&mut info)?;
                }
            }
            for (n, a) in &entries {
                info.reset();
                fill(info.file_info_mut(), a);
                if info.set_name(n).is_err() {
                    continue;
                }
                lock.write(&mut info)?;
            }
        }
        Ok(c.dir_buffer.read(marker, buffer))
    }

    fn rename(&self, c: &Ctx, file_name: &U16CStr, new_file_name: &U16CStr, replace_if_exists: bool) -> R<()> {
        let from = wide(file_name);
        let to = wide(new_file_name);
        let (p, n) = split(&from);
        let (np, nn) = split(&to);
        let pa = self.vfs.resolve(p).map_err(status)?;
        let npa = self.vfs.resolve(np).map_err(status)?;
        self.vfs.rename(pa.ino, n, npa.ino, nn, replace_if_exists).map_err(status)?;
        *c.path.lock().unwrap_or_else(|e| e.into_inner()) = to;
        Ok(())
    }

    fn set_basic_info(
        &self,
        c: &Ctx,
        _attrs: u32,
        _ctime: u64,
        _atime: u64,
        _wtime: u64,
        _chtime: u64,
        file_info: &mut FileInfo,
    ) -> R<()> {
        // Attribute und Zeiten werden nicht gespeichert
        fill(file_info, &self.attr(c)?);
        Ok(())
    }

    fn set_delete(&self, c: &Ctx, _file_name: &U16CStr, delete_file: bool) -> R<()> {
        if delete_file {
            self.vfs.can_delete(c.ino).map_err(status)?;
        }
        Ok(())
    }

    fn set_file_size(&self, c: &Ctx, new_size: u64, set_allocation_size: bool, file_info: &mut FileInfo) -> R<()> {
        let current = self.attr(c)?;
        // Belegter Platz zählt nicht, außer er wird kleiner als die Datei
        if !set_allocation_size || new_size < current.size {
            let h = self.handle(c, true)?;
            self.vfs.set_size(c.ino, Some(h), new_size).map_err(status)?;
        }
        fill(file_info, &self.attr(c)?);
        Ok(())
    }

    fn read(&self, c: &Ctx, buffer: &mut [u8], offset: u64) -> R<u32> {
        let h = self.handle(c, false)?;
        let n = self.vfs.read(h, offset, buffer).map_err(status)?;
        if n == 0 && !buffer.is_empty() {
            return Err(STATUS_END_OF_FILE.into());
        }
        Ok(n as u32)
    }

    fn write(
        &self,
        c: &Ctx,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> R<u32> {
        let h = self.handle(c, true)?;
        let size = self.vfs.handle_attr(h).map_err(status)?.size;
        let offset = if write_to_eof { size } else { offset };
        let data = if constrained_io {
            if offset >= size {
                fill(file_info, &self.attr(c)?);
                return Ok(0);
            }
            &buffer[..buffer.len().min((size - offset) as usize)]
        } else {
            buffer
        };
        let n = self.vfs.write(h, offset, data).map_err(status)?;
        fill(file_info, &self.attr(c)?);
        Ok(n as u32)
    }

    fn get_volume_info(&self, out: &mut VolumeInfo) -> R<()> {
        out.total_size = 1 << 40;
        out.free_size = 1 << 40;
        out.set_volume_label("Büro");
        Ok(())
    }
}

/// Eingebundenes Laufwerk; beim Verwerfen wird es ausgehängt.
pub struct Mount {
    host: FileSystemHost<Fs, FineGuard>,
}

// SAFETY: Der Host wird nach dem Start nur noch zum Anhalten/Aushängen benutzt;
// WinFsp erlaubt das von jedem Thread aus.
unsafe impl Send for Mount {}

impl Drop for Mount {
    fn drop(&mut self) {
        self.host.stop();
        self.host.unmount();
    }
}

pub fn check_available() -> Result<(), String> {
    winfsp::winfsp_init()
        .map(|_| ())
        .map_err(|_| "WinFsp ist nicht installiert. Bitte von https://winfsp.dev/rel/ installieren.".to_string())
}

/// Freier Laufwerksbuchstabe: bevorzugt `B:`, sonst von `Z:` abwärts
pub fn free_drive_letter() -> Option<String> {
    // SAFETY: keine Vorbedingungen
    let used = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    std::iter::once(b'B')
        .chain((b'D'..=b'Z').rev())
        .find(|l| used & (1 << (l - b'A')) == 0)
        .map(|l| format!("{}:", l as char))
}

fn security_descriptor() -> Result<Vec<u8>, String> {
    let sddl: Vec<u16> = SDDL.encode_utf16().chain(std::iter::once(0)).collect();
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: gültiger, nullterminierter String; psd wird von Windows belegt
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut psd, None)
            .map_err(|e| format!("Sicherheitsbeschreibung: {e}"))?;
        let len = GetSecurityDescriptorLength(psd) as usize;
        let bytes = std::slice::from_raw_parts(psd.0.cast::<u8>(), len).to_vec();
        let _ = LocalFree(Some(HLOCAL(psd.0)));
        Ok(bytes)
    }
}

pub fn mount(vfs: Arc<Vfs>, mount_point: &Path) -> Result<Mount, String> {
    check_available()?;
    let mut params = VolumeParams::new();
    params
        .filesystem_name("Buero")
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .volume_creation_time(filetime(SystemTime::now()))
        .volume_serial_number(0x6275_6572)
        .file_info_timeout(1000)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(true)
        .post_cleanup_when_modified_only(true)
        .flush_and_purge_on_cleanup(true);
    let fs = Fs { vfs, sd: security_descriptor()? };
    let mut host: FileSystemHost<Fs, FineGuard> = FileSystemHost::new(params, fs).map_err(|e| format!("WinFsp: {e}"))?;
    let mp = mount_point.as_os_str().to_os_string();
    host.mount(&mp).map_err(|e| format!("Einbinden unter {} fehlgeschlagen: {e}", mount_point.display()))?;
    if let Err(e) = FileSystemHost::<Fs, FineGuard>::start(&mut host) {
        host.unmount();
        return Err(format!("WinFsp starten: {e}"));
    }
    Ok(Mount { host })
}
