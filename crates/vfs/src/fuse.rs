// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Linux: Einbinden über FUSE (ohne libfuse, über `fusermount3`).

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, MountOption,
    OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};

use crate::core::{Attr, Error, Kind, Vfs};

const TTL: Duration = Duration::from_secs(1);

struct Fs {
    vfs: Arc<Vfs>,
    uid: u32,
    gid: u32,
}

fn errno(e: Error) -> Errno {
    match e {
        Error::NotFound => Errno::ENOENT,
        Error::Exists => Errno::EEXIST,
        Error::NotEmpty => Errno::ENOTEMPTY,
        Error::NotDir => Errno::ENOTDIR,
        Error::IsDir => Errno::EISDIR,
        Error::Perm => Errno::EACCES,
        Error::Invalid => Errno::EINVAL,
        Error::Io(msg) => {
            log::warn!("Büro-Laufwerk: {msg}");
            Errno::EIO
        }
    }
}

fn name(n: &OsStr) -> Result<&str, Errno> {
    n.to_str().ok_or(Errno::EINVAL)
}

impl Fs {
    fn attr(&self, a: &Attr) -> FileAttr {
        let dir = a.kind == Kind::Dir;
        FileAttr {
            ino: INodeNo(a.ino),
            size: a.size,
            blocks: a.size.div_ceil(512),
            atime: a.mtime,
            mtime: a.mtime,
            ctime: a.ctime,
            crtime: a.crtime,
            kind: if dir { FileType::Directory } else { FileType::RegularFile },
            perm: if dir { 0o755 } else { 0o644 },
            nlink: if dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
            blksize: 4096,
        }
    }
}

impl Filesystem for Fs {
    fn lookup(&self, _req: &Request, parent: INodeNo, n: &OsStr, reply: ReplyEntry) {
        let res = name(n).and_then(|n| self.vfs.lookup(parent.0, n).map_err(errno));
        match res {
            Ok(a) => reply.entry(&TTL, &self.attr(&a), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        let res = match fh {
            Some(f) => self.vfs.handle_attr(f.0).or_else(|_| self.vfs.getattr(ino.0)),
            None => self.vfs.getattr(ino.0),
        };
        match res {
            Ok(a) => reply.attr(&TTL, &self.attr(&a)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        _kill_suid_gid: bool,
        reply: ReplyAttr,
    ) {
        // Rechte und Zeiten werden nicht gespeichert, nur die Größe zählt
        let res = match size {
            Some(s) => self.vfs.set_size(ino.0, fh.map(|f| f.0), s),
            None => self.vfs.getattr(ino.0),
        };
        match res {
            Ok(a) => reply.attr(&TTL, &self.attr(&a)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        n: &OsStr,
        _mode: u32,
        _umask: u32,
        _owner: fuser::Owner,
        reply: ReplyEntry,
    ) {
        match name(n).and_then(|n| self.vfs.mkdir(parent.0, n).map_err(errno)) {
            Ok(a) => reply.entry(&TTL, &self.attr(&a), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, n: &OsStr, reply: ReplyEmpty) {
        match name(n).and_then(|n| self.vfs.unlink(parent.0, n).map_err(errno)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, n: &OsStr, reply: ReplyEmpty) {
        match name(n).and_then(|n| self.vfs.rmdir(parent.0, n).map_err(errno)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        n: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        _owner: Option<fuser::Owner>,
        reply: ReplyEmpty,
    ) {
        if flags.contains(RenameFlags::RENAME_EXCHANGE) {
            return reply.error(Errno::EINVAL);
        }
        let replace = !flags.contains(RenameFlags::RENAME_NOREPLACE);
        let res = name(n).and_then(|n| {
            let nn = name(newname)?;
            self.vfs.rename(parent.0, n, newparent.0, nn, replace).map_err(errno)
        });
        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, _kill: bool, reply: ReplyOpen) {
        let acc = flags.0 & libc::O_ACCMODE;
        let write = acc == libc::O_WRONLY || acc == libc::O_RDWR;
        let truncate = write && flags.0 & libc::O_TRUNC != 0;
        match self.vfs.open(ino.0, write, truncate) {
            Ok(fh) => reply.opened(FileHandle(fh), FopenFlags::empty()),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let mut buf = vec![0u8; size as usize];
        match self.vfs.read(fh.0, offset, &mut buf) {
            Ok(n) => reply.data(&buf[..n]),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _wf: WriteFlags,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.vfs.write(fh.0, offset, data) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn flush(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _lock: LockOwner, reply: ReplyEmpty) {
        match self.vfs.flush(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        match self.vfs.flush(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.vfs.release(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        match self.vfs.getattr(ino.0) {
            Ok(a) if a.kind == Kind::Dir => reply.opened(FileHandle(0), FopenFlags::empty()),
            Ok(_) => reply.error(Errno::ENOTDIR),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let entries = match self.vfs.readdir(ino.0) {
            Ok(e) => e,
            Err(e) => return reply.error(errno(e)),
        };
        let parent = self.vfs.parent(ino.0);
        let all = [(ino.0, FileType::Directory, ".".to_string()), (parent, FileType::Directory, "..".to_string())]
            .into_iter()
            .chain(entries.into_iter().map(|(n, a)| {
                (a.ino, if a.kind == Kind::Dir { FileType::Directory } else { FileType::RegularFile }, n)
            }));
        for (i, (ino, kind, n)) in all.enumerate().skip(offset as usize) {
            if reply.add(INodeNo(ino), (i + 1) as u64, kind, n) {
                break;
            }
        }
        reply.ok()
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        // Fester, großzügiger Wert, damit Dateimanager Kopieren erlauben
        let blocks = 1u64 << 28; // 1 TiB bei 4 KiB-Blöcken
        reply.statfs(blocks, blocks, blocks, 1 << 20, 1 << 20, 4096, 255, 4096);
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        n: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        _kill: bool,
        _owner: fuser::Owner,
        reply: ReplyCreate,
    ) {
        match name(n).and_then(|n| self.vfs.create(parent.0, n).map_err(errno)) {
            Ok((a, fh)) => reply.created(&TTL, &self.attr(&a), Generation(0), FileHandle(fh), FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
}

/// Eingebundenes Laufwerk; beim Verwerfen wird es ausgehängt.
pub struct Mount {
    _session: fuser::BackgroundSession,
}

/// Hängt eine evtl. noch von einem Absturz eingebundene Instanz aus.
fn cleanup_stale(mount_point: &Path) {
    // „Transport endpoint is not connected“ → lässt sich nicht lesen
    if std::fs::read_dir(mount_point).is_err() {
        let _ = std::process::Command::new("fusermount3").arg("-uz").arg(mount_point).status();
        let _ = std::process::Command::new("fusermount").arg("-uz").arg(mount_point).status();
    }
}

pub fn mount(vfs: Arc<Vfs>, mount_point: &Path) -> Result<Mount, String> {
    cleanup_stale(mount_point);
    std::fs::create_dir_all(mount_point).map_err(|e| format!("Ordner {} anlegen: {e}", mount_point.display()))?;
    let not_empty = std::fs::read_dir(mount_point).map(|mut d| d.next().is_some()).unwrap_or(false);
    if not_empty {
        return Err(format!("Der Ordner {} ist nicht leer", mount_point.display()));
    }
    let mut cfg = fuser::Config::default();
    cfg.mount_options = vec![
        MountOption::FSName("buero".into()),
        MountOption::Subtype("buero".into()),
        MountOption::NoExec,
        MountOption::NoAtime,
    ];
    cfg.n_threads = Some(4);
    // SAFETY: getuid/getgid haben keine Vorbedingungen
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let fs = Fs { vfs, uid, gid };
    let session = fuser::spawn_mount(fs, mount_point, &cfg).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "FUSE ist nicht verfügbar (Paket „fuse3“ installieren)".to_string()
        } else {
            format!("Einbinden fehlgeschlagen: {e}")
        }
    })?;
    Ok(Mount { _session: session })
}
