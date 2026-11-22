// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Plattformunabhängiger Kern des Laufwerks.
//!
//! Abbildung:
//! - Wurzel: Ordner `Eingang` (Dokumente ohne Sammlung) und alle obersten Sammlungen
//! - Sammlung: Untersammlungen als Ordner, Dokumente der Sammlung als Dateien.
//!   Liegt ein Dokument in mehreren Sammlungen, erscheint es in jedem dieser Ordner.
//!
//! Schreiben:
//! - Datei ändern → neue Version
//! - neue Datei → Upload in die Sammlung des Ordners (Wurzel/Eingang: ohne Sammlung)
//! - Umbenennen → Titel ändern, Verschieben → Sammlung wechseln
//! - Löschen → aus der Sammlung entfernen, im Eingang: Dokument löschen
//! - Ordner anlegen/umbenennen/verschieben/löschen (leer) → Sammlung entsprechend
//!
//! Temporäre Dateien (Sperrdateien, `~$…`, `*.tmp`, versteckte Dateien) bleiben
//! nur lokal. Damit funktionieren die üblichen Speichermuster von Office &
//! Co. (in temporäre Datei schreiben, Original wegbenennen, umbenennen, löschen).

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shared::{Collection, CollectionInput, DocumentSummary, DocumentUpdate};

use crate::client::{ApiError, Client};

pub type Ino = u64;
pub const ROOT_INO: Ino = 1;
/// Name des Ordners für Dokumente ohne Sammlung
pub const INBOX_NAME: &str = "Eingang";
/// So lange gilt der geladene Stand, bevor er neu geholt wird
const REFRESH_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Dir {
    Root,
    Inbox,
    Coll(i64),
}

impl Dir {
    fn collection(self) -> Option<i64> {
        match self {
            Dir::Coll(c) => Some(c),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    Dir(Dir),
    Doc(Dir, i64),
    Local(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub ino: Ino,
    pub kind: Kind,
    pub size: u64,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,
}

#[derive(Debug, Clone)]
pub enum Error {
    NotFound,
    Exists,
    NotEmpty,
    NotDir,
    IsDir,
    /// Nicht erlaubt (z. B. Ordner im Eingang anlegen)
    Perm,
    Invalid,
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        log::warn!("Büro-Laufwerk: {e}");
        match e.status {
            Some(404) => Error::NotFound,
            Some(409) => Error::Exists,
            Some(400) => Error::Invalid,
            Some(401) | Some(403) => Error::Perm,
            _ => Error::Io(e.message),
        }
    }
}

/// Dateien, die nicht hochgeladen werden, sondern nur lokal existieren
pub fn is_temp(name: &str) -> bool {
    let l = name.to_lowercase();
    l.starts_with('.')
        || l.starts_with('~')
        || l.ends_with('~')
        || [".tmp", ".temp", ".swp", ".swx", ".part", ".crdownload", ".partial"].iter().any(|e| l.ends_with(e))
        || l == "desktop.ini"
        || l == "thumbs.db"
}

/// Zeichen ersetzen, die unter Windows (oder Linux) nicht in Dateinamen erlaubt sind.
/// Gilt auf beiden Systemen, damit die Namen überall gleich aussehen.
pub fn sanitize_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let s = s.trim_start().to_string();
    if s.is_empty() || s == "." || s == ".." {
        "_".into()
    } else {
        s
    }
}

fn extension(filename: &str) -> Option<&str> {
    let (stem, ext) = filename.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 10 && !ext.contains(' ')).then_some(ext)
}

/// Dateiname eines Dokuments: Titel + Endung der Originaldatei
fn doc_file_name(d: &DocumentSummary) -> String {
    let title = d.title.trim();
    let base = if title.is_empty() { d.original_filename.as_str() } else { title };
    let mut name = sanitize_name(base);
    if let Some(ext) = extension(&d.original_filename) {
        if !name.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase())) {
            name.push('.');
            name.push_str(ext);
        }
    }
    name
}

/// Titel aus einem neuen Dateinamen (Endung der Originaldatei abschneiden)
fn title_from_name(name: &str, original_filename: &str) -> String {
    if let Some(ext) = extension(original_filename) {
        let suffix = format!(".{}", ext.to_lowercase());
        if name.to_lowercase().ends_with(&suffix) && name.len() > suffix.len() {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    name.to_string()
}

fn stem(name: &str) -> &str {
    match extension(name) {
        Some(ext) => &name[..name.len() - ext.len() - 1],
        None => name,
    }
}

fn with_suffix(name: &str, suffix: &str) -> String {
    match extension(name) {
        Some(ext) => format!("{} ({suffix}).{ext}", stem(name)),
        None => format!("{name} ({suffix})"),
    }
}

fn ts(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Entry {
    Dir(Dir),
    Doc(i64),
}

/// Geladener Stand vom Server, als Ordnerbaum aufbereitet
#[derive(Default)]
struct Snapshot {
    colls: HashMap<i64, Collection>,
    docs: HashMap<i64, DocumentSummary>,
    entries: HashMap<Dir, Vec<(String, Entry)>>,
}

impl Snapshot {
    fn build(colls: Vec<Collection>, docs: Vec<DocumentSummary>) -> Self {
        let colls: HashMap<i64, Collection> = colls.into_iter().map(|c| (c.id, c)).collect();
        let mut entries: HashMap<Dir, Vec<(String, Entry)>> = HashMap::new();
        entries.insert(Dir::Root, vec![(INBOX_NAME.to_string(), Entry::Dir(Dir::Inbox))]);
        entries.insert(Dir::Inbox, Vec::new());

        let mut sorted: Vec<&Collection> = colls.values().collect();
        sorted.sort_by_key(|c| (c.name.to_lowercase(), c.id));
        for c in &sorted {
            entries.entry(Dir::Coll(c.id)).or_default();
        }
        for c in &sorted {
            let parent = match c.parent_id {
                Some(p) if colls.contains_key(&p) => Dir::Coll(p),
                _ => Dir::Root,
            };
            let list = entries.entry(parent).or_default();
            let name = unique(list, sanitize_name(&c.name), c.id);
            list.push((name, Entry::Dir(Dir::Coll(c.id))));
        }

        let mut docs_sorted: Vec<&DocumentSummary> = docs.iter().collect();
        docs_sorted.sort_by_key(|d| d.id);
        for d in docs_sorted {
            let name = doc_file_name(d);
            let dirs: Vec<Dir> = if d.collection_ids.is_empty() {
                vec![Dir::Inbox]
            } else {
                d.collection_ids.iter().filter(|c| colls.contains_key(c)).map(|c| Dir::Coll(*c)).collect()
            };
            for dir in dirs {
                let list = entries.entry(dir).or_default();
                let n = unique(list, name.clone(), d.id);
                list.push((n, Entry::Doc(d.id)));
            }
        }
        Snapshot { colls, docs: docs.into_iter().map(|d| (d.id, d)).collect(), entries }
    }
}

/// Bei Namensgleichheit (ohne Groß-/Kleinschreibung) die ID anhängen
fn unique(list: &[(String, Entry)], name: String, id: i64) -> String {
    let taken = |n: &str| list.iter().any(|(m, _)| m.to_lowercase() == n.to_lowercase());
    if !taken(&name) {
        return name;
    }
    let mut candidate = with_suffix(&name, &id.to_string());
    let mut i = 2;
    while taken(&candidate) {
        candidate = with_suffix(&name, &format!("{id}-{i}"));
        i += 1;
    }
    candidate
}

/// Nur lokal vorhandene Datei (neu angelegt und noch nicht hochgeladen,
/// temporäre Datei oder weggebenanntes Dokument)
struct LocalFile {
    dir: Dir,
    name: String,
    path: PathBuf,
    created: SystemTime,
    /// Wurde schon etwas hineingeschrieben?
    has_content: bool,
    /// Weggebenanntes Dokument (z. B. von Word vor dem Speichern): Inhalt = Dokument
    shadow_of: Option<i64>,
    /// Das Dokument hat inzwischen eine neue Fassung bekommen
    replaced: bool,
    /// Beim Hochladen als neue Version dieses Dokuments speichern
    target_doc: Option<i64>,
}

struct Handle {
    key: Key,
    file: Arc<File>,
    path: PathBuf,
    write: bool,
    dirty: bool,
    /// Version, auf der die Änderung beruht
    base_version: Option<i64>,
    /// Arbeitskopie, beim Schließen löschen
    work_copy: bool,
}

struct State {
    snap: Arc<Snapshot>,
    loaded: Option<Instant>,
    locals: HashMap<u64, LocalFile>,
    next_local: u64,
    /// Dokumente, die in einem Ordner gerade als temporäre Datei weggebenannt sind
    hidden: HashSet<(Dir, i64)>,
    inodes: HashMap<Key, Ino>,
    keys: HashMap<Ino, Key>,
    next_ino: Ino,
    handles: HashMap<u64, Handle>,
    next_fh: u64,
}

impl State {
    fn ino(&mut self, key: Key) -> Ino {
        if let Some(i) = self.inodes.get(&key) {
            return *i;
        }
        let i = self.next_ino;
        self.next_ino += 1;
        self.inodes.insert(key.clone(), i);
        self.keys.insert(i, key);
        i
    }

    fn key(&self, ino: Ino) -> Result<Key> {
        self.keys.get(&ino).cloned().ok_or(Error::NotFound)
    }

    fn dir(&self, ino: Ino) -> Result<Dir> {
        match self.key(ino)? {
            Key::Dir(d) => Ok(d),
            _ => Err(Error::NotDir),
        }
    }

    fn dir_exists(&self, d: Dir) -> bool {
        match d {
            Dir::Root | Dir::Inbox => true,
            Dir::Coll(c) => self.snap.colls.contains_key(&c),
        }
    }
}

/// Fund beim Auflösen eines Namens
#[derive(Clone, Copy, Debug)]
enum Found {
    Dir(Dir),
    Doc(i64),
    Local(u64),
}

pub struct Vfs {
    client: Client,
    cache: PathBuf,
    case_insensitive: bool,
    started: SystemTime,
    state: Mutex<State>,
    refreshing: Mutex<()>,
    downloading: Mutex<()>,
    last_error: Mutex<Option<String>>,
}

impl Vfs {
    pub fn new(client: Client, cache: PathBuf, case_insensitive: bool) -> std::io::Result<Self> {
        for sub in ["docs", "work", "local"] {
            let p = cache.join(sub);
            if sub != "docs" {
                let _ = std::fs::remove_dir_all(&p);
            }
            std::fs::create_dir_all(&p)?;
        }
        let mut st = State {
            snap: Arc::new(Snapshot::default()),
            loaded: None,
            locals: HashMap::new(),
            next_local: 1,
            hidden: HashSet::new(),
            inodes: HashMap::new(),
            keys: HashMap::new(),
            next_ino: ROOT_INO,
            handles: HashMap::new(),
            next_fh: 1,
        };
        let root = st.ino(Key::Dir(Dir::Root));
        debug_assert_eq!(root, ROOT_INO);
        Ok(Vfs {
            client,
            cache,
            case_insensitive,
            started: SystemTime::now(),
            state: Mutex::new(st),
            refreshing: Mutex::new(()),
            downloading: Mutex::new(()),
            last_error: Mutex::new(None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Letzter Fehler beim Hochladen/Ändern (für die Statusanzeige)
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.clone()
    }

    fn report(&self, msg: String) {
        log::warn!("Büro-Laufwerk: {msg}");
        if let Ok(mut e) = self.last_error.lock() {
            *e = Some(msg);
        }
    }

    fn same(&self, a: &str, b: &str) -> bool {
        if self.case_insensitive {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    }

    // -----------------------------------------------------------------------
    // Stand vom Server
    // -----------------------------------------------------------------------

    /// Stand neu laden, wenn er älter als ein paar Sekunden ist (`force`: immer).
    pub fn refresh(&self, force: bool) -> Result<()> {
        let fresh = |st: &State| st.loaded.is_some_and(|t| t.elapsed() < REFRESH_AFTER);
        if !force && fresh(&self.lock()) {
            return Ok(());
        }
        let _r = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
        if !force && fresh(&self.lock()) {
            return Ok(());
        }
        let loaded = self.client.collections().and_then(|c| Ok((c, self.client.all_documents()?)));
        match loaded {
            Ok((colls, docs)) => {
                let snap = Snapshot::build(colls, docs);
                let mut st = self.lock();
                st.snap = Arc::new(snap);
                st.loaded = Some(Instant::now());
                Ok(())
            }
            Err(e) => {
                let mut st = self.lock();
                if st.loaded.is_some() {
                    // Alten Stand weiter zeigen, später erneut versuchen
                    st.loaded = Some(Instant::now());
                    log::warn!("Büro-Laufwerk: Aktualisieren fehlgeschlagen: {e}");
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    fn find(&self, st: &State, dir: Dir, name: &str) -> Option<Found> {
        if let Some((id, _)) = st.locals.iter().find(|(_, l)| l.dir == dir && self.same(&l.name, name)) {
            return Some(Found::Local(*id));
        }
        let list = st.snap.entries.get(&dir)?;
        list.iter().find(|(n, e)| self.same(n, name) && !matches!(e, Entry::Doc(d) if st.hidden.contains(&(dir, *d))))
            .map(|(_, e)| match e {
                Entry::Dir(d) => Found::Dir(*d),
                Entry::Doc(id) => Found::Doc(*id),
            })
    }

    fn name_of_doc(&self, st: &State, dir: Dir, id: i64) -> Option<String> {
        st.snap.entries.get(&dir)?.iter().find(|(_, e)| *e == Entry::Doc(id)).map(|(n, _)| n.clone())
    }

    fn found_key(dir: Dir, f: Found) -> Key {
        match f {
            Found::Dir(d) => Key::Dir(d),
            Found::Doc(id) => Key::Doc(dir, id),
            Found::Local(l) => Key::Local(l),
        }
    }

    fn dir_attr(&self, ino: Ino) -> Attr {
        Attr { ino, kind: Kind::Dir, size: 0, mtime: self.started, ctime: self.started, crtime: self.started }
    }

    fn attr(&self, st: &State, ino: Ino, key: &Key) -> Result<Attr> {
        // Offene, geänderte Arbeitskopie hat Vorrang
        let open_size = st
            .handles
            .values()
            .find(|h| &h.key == key && h.write)
            .and_then(|h| h.file.metadata().ok())
            .map(|m| m.len());
        match key {
            Key::Dir(d) => {
                if st.dir_exists(*d) {
                    Ok(self.dir_attr(ino))
                } else {
                    Err(Error::NotFound)
                }
            }
            Key::Doc(_, id) => {
                let d = st.snap.docs.get(id).ok_or(Error::NotFound)?;
                Ok(Attr {
                    ino,
                    kind: Kind::File,
                    size: open_size.unwrap_or(d.size.max(0) as u64),
                    mtime: ts(d.updated_at),
                    ctime: ts(d.updated_at),
                    crtime: ts(d.created_at),
                })
            }
            Key::Local(l) => {
                let lf = st.locals.get(l).ok_or(Error::NotFound)?;
                let meta = std::fs::metadata(&lf.path).ok();
                let shadow = lf.shadow_of.and_then(|id| st.snap.docs.get(&id));
                let size = open_size
                    .or(meta.as_ref().map(|m| m.len()))
                    .or(shadow.map(|d| d.size.max(0) as u64))
                    .unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .or(shadow.map(|d| ts(d.updated_at)))
                    .unwrap_or(lf.created);
                Ok(Attr { ino, kind: Kind::File, size, mtime, ctime: mtime, crtime: lf.created })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Lesen der Struktur
    // -----------------------------------------------------------------------

    pub fn getattr(&self, ino: Ino) -> Result<Attr> {
        let _ = self.refresh(false);
        let st = self.lock();
        let key = st.key(ino)?;
        self.attr(&st, ino, &key)
    }

    pub fn lookup(&self, parent: Ino, name: &str) -> Result<Attr> {
        self.refresh(false)?;
        let mut st = self.lock();
        let dir = st.dir(parent)?;
        let f = self.find(&st, dir, name).ok_or(Error::NotFound)?;
        let key = Self::found_key(dir, f);
        let ino = st.ino(key.clone());
        self.attr(&st, ino, &key)
    }

    /// Pfad (mit `/` oder `\` getrennt) auflösen
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn resolve(&self, path: &str) -> Result<Attr> {
        let mut attr = self.getattr(ROOT_INO)?;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if attr.kind != Kind::Dir {
                return Err(Error::NotDir);
            }
            attr = self.lookup(attr.ino, part)?;
        }
        Ok(attr)
    }

    pub fn readdir(&self, ino: Ino) -> Result<Vec<(String, Attr)>> {
        self.refresh(false)?;
        let mut st = self.lock();
        let dir = st.dir(ino)?;
        if !st.dir_exists(dir) {
            return Err(Error::NotFound);
        }
        let snap = st.snap.clone();
        let mut out = Vec::new();
        for (name, e) in snap.entries.get(&dir).into_iter().flatten() {
            let key = match e {
                Entry::Dir(d) => Key::Dir(*d),
                Entry::Doc(id) => {
                    if st.hidden.contains(&(dir, *id)) {
                        continue;
                    }
                    Key::Doc(dir, *id)
                }
            };
            let i = st.ino(key.clone());
            if let Ok(a) = self.attr(&st, i, &key) {
                out.push((name.clone(), a));
            }
        }
        let locals: Vec<(u64, String)> =
            st.locals.iter().filter(|(_, l)| l.dir == dir).map(|(id, l)| (*id, l.name.clone())).collect();
        for (id, name) in locals {
            let key = Key::Local(id);
            let i = st.ino(key.clone());
            if let Ok(a) = self.attr(&st, i, &key) {
                out.push((name, a));
            }
        }
        Ok(out)
    }

    /// Übergeordneter Ordner (für `..`)
    pub fn parent(&self, ino: Ino) -> Ino {
        let mut st = self.lock();
        let parent = match st.keys.get(&ino) {
            Some(Key::Dir(Dir::Coll(c))) => match st.snap.colls.get(c).and_then(|c| c.parent_id) {
                Some(p) if st.snap.colls.contains_key(&p) => Some(Dir::Coll(p)),
                _ => None,
            },
            _ => None,
        };
        match parent {
            Some(d) => st.ino(Key::Dir(d)),
            None => ROOT_INO,
        }
    }

    // -----------------------------------------------------------------------
    // Dateiinhalt
    // -----------------------------------------------------------------------

    /// Aktuelle Fassung eines Dokuments im Zwischenspeicher bereitstellen
    fn cached(&self, id: i64, version: i64) -> Result<PathBuf> {
        let path = self.cache.join("docs").join(format!("{id}-v{version}"));
        if path.exists() {
            return Ok(path);
        }
        let _d = self.downloading.lock().unwrap_or_else(|e| e.into_inner());
        if path.exists() {
            return Ok(path);
        }
        // Ältere Fassungen desselben Dokuments wegräumen
        if let Ok(rd) = std::fs::read_dir(self.cache.join("docs")) {
            let prefix = format!("{id}-v");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.client.download(id, &path)?;
        Ok(path)
    }

    fn add_handle(&self, st: &mut State, h: Handle) -> u64 {
        let fh = st.next_fh;
        st.next_fh += 1;
        st.handles.insert(fh, h);
        fh
    }

    fn work_path(&self, st: &mut State) -> PathBuf {
        let n = st.next_fh;
        self.cache.join("work").join(format!("{n}-{}", st.handles.len()))
    }

    pub fn open(&self, ino: Ino, write: bool, truncate: bool) -> Result<u64> {
        let key = self.lock().key(ino)?;
        match key {
            Key::Dir(_) => Err(Error::IsDir),
            Key::Doc(_, id) => {
                let version = self.lock().snap.docs.get(&id).map(|d| d.version).ok_or(Error::NotFound)?;
                let cached = if truncate { None } else { Some(self.cached(id, version)?) };
                let mut st = self.lock();
                if !write {
                    let path = cached.ok_or(Error::Invalid)?;
                    let file = Arc::new(File::open(&path)?);
                    let h = Handle { key, file, path, write, dirty: false, base_version: Some(version), work_copy: false };
                    return Ok(self.add_handle(&mut st, h));
                }
                let work = self.work_path(&mut st);
                drop(st);
                match &cached {
                    Some(c) => {
                        std::fs::copy(c, &work)?;
                    }
                    None => {
                        File::create(&work)?;
                    }
                }
                let file = Arc::new(OpenOptions::new().read(true).write(true).open(&work)?);
                let mut st = self.lock();
                let h = Handle {
                    key,
                    file,
                    path: work,
                    write,
                    dirty: truncate,
                    base_version: Some(version),
                    work_copy: true,
                };
                Ok(self.add_handle(&mut st, h))
            }
            Key::Local(l) => {
                let (path, shadow) = {
                    let st = self.lock();
                    let lf = st.locals.get(&l).ok_or(Error::NotFound)?;
                    (lf.path.clone(), lf.shadow_of.and_then(|id| st.snap.docs.get(&id).map(|d| (id, d.version))))
                };
                if !path.exists() {
                    match shadow {
                        Some((id, v)) if !truncate => {
                            let c = self.cached(id, v)?;
                            std::fs::copy(c, &path)?;
                        }
                        _ => {
                            File::create(&path)?;
                        }
                    }
                }
                let file = OpenOptions::new().read(true).write(true).open(&path)?;
                if truncate {
                    file.set_len(0)?;
                }
                let mut st = self.lock();
                let h = Handle {
                    key,
                    file: Arc::new(file),
                    path,
                    write,
                    dirty: truncate,
                    base_version: None,
                    work_copy: false,
                };
                Ok(self.add_handle(&mut st, h))
            }
        }
    }

    pub fn create(&self, parent: Ino, name: &str) -> Result<(Attr, u64)> {
        self.refresh(false)?;
        let mut st = self.lock();
        let dir = st.dir(parent)?;
        if !st.dir_exists(dir) {
            return Err(Error::NotFound);
        }
        if self.find(&st, dir, name).is_some() {
            return Err(Error::Exists);
        }
        if name != sanitize_name(name) && !is_temp(name) {
            return Err(Error::Invalid);
        }
        // Wurde das Original eben weggebenannt (vim, Word), wird das hier seine neue Fassung
        let target_doc = self.shadowed_with_name(&st, dir, name);
        let id = st.next_local;
        st.next_local += 1;
        let path = self.cache.join("local").join(id.to_string());
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
        st.locals.insert(
            id,
            LocalFile {
                dir,
                name: name.to_string(),
                path: path.clone(),
                created: SystemTime::now(),
                has_content: false,
                shadow_of: None,
                replaced: false,
                target_doc,
            },
        );
        let key = Key::Local(id);
        let ino = st.ino(key.clone());
        let h = Handle { key: key.clone(), file: Arc::new(file), path, write: true, dirty: false, base_version: None, work_copy: false };
        let fh = self.add_handle(&mut st, h);
        let attr = self.attr(&st, ino, &key)?;
        Ok((attr, fh))
    }

    /// Ausgeblendetes Dokument, dessen eigentlicher Name `name` ist
    fn shadowed_with_name(&self, st: &State, dir: Dir, name: &str) -> Option<i64> {
        st.hidden
            .iter()
            .filter(|(d, _)| *d == dir)
            .find(|(_, id)| self.name_of_doc(st, dir, *id).is_some_and(|n| self.same(&n, name)))
            .map(|(_, id)| *id)
    }

    fn handle_file(&self, fh: u64) -> Result<Arc<File>> {
        self.lock().handles.get(&fh).map(|h| h.file.clone()).ok_or(Error::Invalid)
    }

    pub fn read(&self, fh: u64, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let file = self.handle_file(fh)?;
        let mut done = 0;
        while done < buf.len() {
            let n = read_at(&file, &mut buf[done..], offset + done as u64)?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }

    pub fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<usize> {
        let file = {
            let mut st = self.lock();
            let h = st.handles.get_mut(&fh).ok_or(Error::Invalid)?;
            if !h.write {
                return Err(Error::Perm);
            }
            h.dirty = true;
            h.file.clone()
        };
        let mut done = 0;
        while done < data.len() {
            done += write_at(&file, &data[done..], offset + done as u64)?;
        }
        Ok(done)
    }

    /// Größe ändern (über offenes Handle oder direkt)
    pub fn set_size(&self, ino: Ino, fh: Option<u64>, size: u64) -> Result<Attr> {
        let own = fh.filter(|f| self.lock().handles.get(f).is_some_and(|h| h.write));
        let fh_used = match own {
            Some(f) => f,
            None => self.open(ino, true, size == 0)?,
        };
        let res = (|| {
            let file = {
                let mut st = self.lock();
                let h = st.handles.get_mut(&fh_used).ok_or(Error::Invalid)?;
                h.dirty = true;
                h.file.clone()
            };
            file.set_len(size)?;
            Ok::<_, Error>(())
        })();
        if own.is_none() {
            self.release(fh_used)?;
        }
        res?;
        self.getattr(ino)
    }

    pub fn handle_attr(&self, fh: u64) -> Result<Attr> {
        let st = self.lock();
        let key = st.handles.get(&fh).map(|h| h.key.clone()).ok_or(Error::Invalid)?;
        let ino = *st.inodes.get(&key).ok_or(Error::NotFound)?;
        self.attr(&st, ino, &key)
    }

    /// Änderungen hochladen
    pub fn flush(&self, fh: u64) -> Result<()> {
        let (key, path, base) = {
            let st = self.lock();
            let Some(h) = st.handles.get(&fh) else { return Ok(()) };
            if !h.dirty {
                return Ok(());
            }
            (h.key.clone(), h.path.clone(), h.base_version)
        };
        match key {
            Key::Doc(dir, id) => {
                let doc = self.save_version(dir, id, &path, base)?;
                let mut st = self.lock();
                if let Some(h) = st.handles.get_mut(&fh) {
                    h.dirty = false;
                    h.base_version = Some(doc.version);
                    if let Key::Doc(d, _) = h.key {
                        h.key = Key::Doc(d, doc.id);
                    }
                }
                Ok(())
            }
            Key::Local(l) => {
                let temp = {
                    let mut st = self.lock();
                    let lf = st.locals.get_mut(&l).ok_or(Error::NotFound)?;
                    lf.has_content = true;
                    is_temp(&lf.name)
                };
                if temp {
                    if let Some(h) = self.lock().handles.get_mut(&fh) {
                        h.dirty = false;
                    }
                    return Ok(());
                }
                let (dir, doc) = self.commit_local(l, Some(&path))?;
                let mut st = self.lock();
                if let Some(h) = st.handles.get_mut(&fh) {
                    h.dirty = false;
                    h.key = Key::Doc(dir, doc.id);
                    h.base_version = Some(doc.version);
                    h.work_copy = true;
                }
                Ok(())
            }
            Key::Dir(_) => Ok(()),
        }
    }

    pub fn release(&self, fh: u64) -> Result<()> {
        let res = self.flush(fh);
        let h = self.lock().handles.remove(&fh);
        if let Some(h) = h {
            if let Err(e) = &res {
                if h.dirty {
                    self.keep_unsaved(&h, e);
                }
            }
            drop(h.file);
            if h.work_copy {
                let _ = std::fs::remove_file(&h.path);
            }
        }
        res
    }

    /// Handle schließen, ohne Änderungen hochzuladen (Datei wird gelöscht)
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn discard(&self, fh: u64) {
        let h = self.lock().handles.remove(&fh);
        if let Some(h) = h {
            drop(h.file);
            if h.work_copy {
                let _ = std::fs::remove_file(&h.path);
            }
        }
    }

    /// Darf das Objekt gelöscht werden? (Windows fragt vorher)
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn can_delete(&self, ino: Ino) -> Result<()> {
        let st = self.lock();
        match st.key(ino)? {
            Key::Dir(Dir::Coll(c)) => {
                let has_entries = st.snap.entries.get(&Dir::Coll(c)).is_some_and(|e| !e.is_empty());
                if has_entries || st.locals.values().any(|l| l.dir == Dir::Coll(c)) {
                    Err(Error::NotEmpty)
                } else {
                    Ok(())
                }
            }
            Key::Dir(_) => Err(Error::Perm),
            Key::Doc(Dir::Root, _) => Err(Error::Perm),
            _ => Ok(()),
        }
    }

    /// Nicht hochladbare Änderungen nicht verwerfen, sondern beiseitelegen
    fn keep_unsaved(&self, h: &Handle, e: &Error) {
        let name = {
            let st = self.lock();
            match &h.key {
                Key::Doc(dir, id) => self.name_of_doc(&st, *dir, *id),
                Key::Local(l) => st.locals.get(l).map(|l| l.name.clone()),
                Key::Dir(_) => None,
            }
        }
        .unwrap_or_else(|| "Datei".into());
        let dir = self.cache.parent().unwrap_or(&self.cache).join("Nicht hochgeladen");
        let _ = std::fs::create_dir_all(&dir);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dest = dir.join(format!("{stamp}-{name}"));
        let saved = std::fs::copy(&h.path, &dest).is_ok();
        self.report(format!(
            "„{name}“ konnte nicht hochgeladen werden ({e:?}).{}",
            if saved { format!(" Kopie: {}", dest.display()) } else { String::new() }
        ));
    }

    /// Datei als neue Version speichern; bei Konflikt als eigenes Dokument daneben
    fn save_version(&self, dir: Dir, id: i64, path: &Path, base: Option<i64>) -> Result<DocumentSummary> {
        let doc = self.lock().snap.docs.get(&id).cloned().ok_or(Error::NotFound)?;
        let res = match self.client.new_version(id, path, &doc.original_filename, base) {
            Err(e) if e.status == Some(409) => {
                let title = format!("{} (Konflikt)", doc.title);
                self.report(format!("„{}“ wurde inzwischen anderswo geändert – gespeichert als „{title}“", doc.title));
                self.client.upload(path, &doc.original_filename, &title, &dir.collection().into_iter().collect::<Vec<_>>())
            }
            r => r,
        };
        let res = res.map_err(Error::from);
        let _ = self.refresh(true);
        res
    }

    /// Lokale Datei hochladen und zum Dokument machen (gleiche Inode)
    fn commit_local(&self, l: u64, content: Option<&Path>) -> Result<(Dir, DocumentSummary)> {
        let (dir, name, path, target) = {
            let st = self.lock();
            let lf = st.locals.get(&l).ok_or(Error::NotFound)?;
            (lf.dir, lf.name.clone(), lf.path.clone(), lf.target_doc)
        };
        let path = content.map(Path::to_path_buf).unwrap_or(path);
        let doc = match target {
            Some(t) => self.save_version(dir, t, &path, None)?,
            None => {
                let colls: Vec<i64> = dir.collection().into_iter().collect();
                let d = self.client.upload(&path, &name, stem(&name), &colls)?;
                let _ = self.refresh(true);
                d
            }
        };
        let doc_dir = if dir == Dir::Root { Dir::Inbox } else { dir };
        let mut st = self.lock();
        st.locals.remove(&l);
        if let Some(t) = target {
            st.hidden.remove(&(dir, t));
            for lf in st.locals.values_mut() {
                if lf.dir == dir && lf.shadow_of == Some(t) {
                    lf.replaced = true;
                }
            }
        }
        if let Some(ino) = st.inodes.remove(&Key::Local(l)) {
            let key = Key::Doc(doc_dir, doc.id);
            st.inodes.insert(key.clone(), ino);
            st.keys.insert(ino, key);
        }
        Ok((doc_dir, doc))
    }

    // -----------------------------------------------------------------------
    // Struktur ändern
    // -----------------------------------------------------------------------

    pub fn unlink(&self, parent: Ino, name: &str) -> Result<()> {
        self.refresh(false)?;
        let (dir, found) = {
            let st = self.lock();
            let dir = st.dir(parent)?;
            (dir, self.find(&st, dir, name).ok_or(Error::NotFound)?)
        };
        match found {
            Found::Dir(_) => Err(Error::IsDir),
            Found::Local(l) => {
                let lf = {
                    let mut st = self.lock();
                    let lf = st.locals.remove(&l).ok_or(Error::NotFound)?;
                    if let Some(id) = lf.shadow_of {
                        st.hidden.remove(&(dir, id));
                    }
                    lf
                };
                let _ = std::fs::remove_file(&lf.path);
                match lf.shadow_of {
                    // Weggebenanntes Original ohne Ersatz gelöscht → wirklich entfernen
                    Some(id) if !lf.replaced => self.remove_doc(dir, id),
                    _ => Ok(()),
                }
            }
            Found::Doc(id) => self.remove_doc(dir, id),
        }
    }

    /// Dokument aus dem Ordner entfernen: aus der Sammlung nehmen bzw. im Eingang löschen
    fn remove_doc(&self, dir: Dir, id: i64) -> Result<()> {
        let doc = self.lock().snap.docs.get(&id).cloned().ok_or(Error::NotFound)?;
        match dir {
            Dir::Coll(c) => {
                let ids: Vec<i64> = doc.collection_ids.iter().copied().filter(|x| *x != c).collect();
                self.client.update_document(id, &DocumentUpdate { collection_ids: Some(ids), ..Default::default() })?;
            }
            Dir::Inbox => self.client.delete_document(id)?,
            Dir::Root => return Err(Error::Perm),
        }
        self.refresh(true)
    }

    pub fn mkdir(&self, parent: Ino, name: &str) -> Result<Attr> {
        self.refresh(false)?;
        let dir = {
            let st = self.lock();
            let dir = st.dir(parent)?;
            if self.find(&st, dir, name).is_some() {
                return Err(Error::Exists);
            }
            dir
        };
        if is_temp(name) || dir == Dir::Inbox {
            return Err(Error::Perm);
        }
        if name != sanitize_name(name) {
            return Err(Error::Invalid);
        }
        let c = self.client.create_collection(name, dir.collection())?;
        self.refresh(true)?;
        let mut st = self.lock();
        let key = Key::Dir(Dir::Coll(c.id));
        let ino = st.ino(key.clone());
        self.attr(&st, ino, &key)
    }

    pub fn rmdir(&self, parent: Ino, name: &str) -> Result<()> {
        self.refresh(false)?;
        let c = {
            let st = self.lock();
            let dir = st.dir(parent)?;
            match self.find(&st, dir, name).ok_or(Error::NotFound)? {
                Found::Dir(Dir::Coll(c)) => {
                    let has_entries = st.snap.entries.get(&Dir::Coll(c)).is_some_and(|e| !e.is_empty());
                    let has_locals = st.locals.values().any(|l| l.dir == Dir::Coll(c));
                    if has_entries || has_locals {
                        return Err(Error::NotEmpty);
                    }
                    c
                }
                Found::Dir(_) => return Err(Error::Perm),
                _ => return Err(Error::NotDir),
            }
        };
        self.client.delete_collection(c)?;
        self.refresh(true)
    }

    pub fn rename(&self, parent: Ino, name: &str, newparent: Ino, newname: &str, replace: bool) -> Result<()> {
        self.refresh(false)?;
        let (dir, newdir, src, dst) = {
            let st = self.lock();
            let dir = st.dir(parent)?;
            let newdir = st.dir(newparent)?;
            if !st.dir_exists(newdir) {
                return Err(Error::NotFound);
            }
            let src = self.find(&st, dir, name).ok_or(Error::NotFound)?;
            let dst = self.find(&st, newdir, newname);
            (dir, newdir, src, dst)
        };
        if dir == newdir && name == newname {
            return Ok(());
        }
        // Nur Groß-/Kleinschreibung geändert: Ziel ist die Quelle selbst
        let dst = match (src, dst) {
            (Found::Doc(a), Some(Found::Doc(b))) if a == b && dir == newdir => None,
            (Found::Dir(a), Some(Found::Dir(b))) if a == b => None,
            (Found::Local(a), Some(Found::Local(b))) if a == b => None,
            _ => dst,
        };
        if dst.is_some() && !replace {
            return Err(Error::Exists);
        }
        if newname != sanitize_name(newname) && !is_temp(newname) {
            return Err(Error::Invalid);
        }

        match src {
            Found::Dir(d) => self.rename_dir(d, newdir, newname, dst),
            Found::Doc(id) => self.rename_doc(dir, id, newdir, newname, dst),
            Found::Local(l) => self.rename_local(l, newdir, newname, dst),
        }
    }

    fn rename_dir(&self, d: Dir, newdir: Dir, newname: &str, dst: Option<Found>) -> Result<()> {
        let Dir::Coll(c) = d else { return Err(Error::Perm) };
        if dst.is_some() {
            return Err(Error::NotEmpty);
        }
        if newdir == Dir::Inbox || is_temp(newname) {
            return Err(Error::Perm);
        }
        let current = self.lock().snap.colls.get(&c).cloned().ok_or(Error::NotFound)?;
        let new_parent = newdir.collection();
        let input = CollectionInput {
            name: (sanitize_name(&current.name) != newname).then(|| newname.to_string()),
            parent_id: (current.parent_id != new_parent).then_some(new_parent.unwrap_or(0)),
            color: None,
            ai_locked: None,
        };
        self.client.update_collection(c, &input)?;
        self.refresh(true)
    }

    fn rename_doc(&self, dir: Dir, id: i64, newdir: Dir, newname: &str, dst: Option<Found>) -> Result<()> {
        if is_temp(newname) {
            if newdir != dir {
                return Err(Error::Perm);
            }
            // Original wird vor dem Speichern weggebenannt: nur lokal merken
            let mut st = self.lock();
            if let Some(Found::Local(l)) = dst {
                if let Some(old) = st.locals.remove(&l) {
                    let _ = std::fs::remove_file(old.path);
                }
            }
            let l = st.next_local;
            st.next_local += 1;
            let path = self.cache.join("local").join(l.to_string());
            st.locals.insert(
                l,
                LocalFile {
                    dir,
                    name: newname.to_string(),
                    path,
                    created: SystemTime::now(),
                    has_content: true,
                    shadow_of: Some(id),
                    replaced: false,
                    target_doc: None,
                },
            );
            st.hidden.insert((dir, id));
            // Offene Handles und Inode wandern mit
            if let Some(ino) = st.inodes.remove(&Key::Doc(dir, id)) {
                st.inodes.insert(Key::Local(l), ino);
                st.keys.insert(ino, Key::Local(l));
            }
            return Ok(());
        }
        match dst {
            Some(Found::Doc(_)) | Some(Found::Dir(_)) => return Err(Error::Perm),
            Some(Found::Local(l)) => self.drop_local(l),
            None => {}
        }
        if newdir == Dir::Root {
            return Err(Error::Perm);
        }
        let (doc, cur_name) = {
            let st = self.lock();
            (st.snap.docs.get(&id).cloned().ok_or(Error::NotFound)?, self.name_of_doc(&st, dir, id))
        };
        let mut u = DocumentUpdate::default();
        if cur_name.as_deref() != Some(newname) {
            u.title = Some(title_from_name(newname, &doc.original_filename));
        }
        if newdir != dir {
            let mut ids: Vec<i64> = doc.collection_ids.iter().copied().filter(|c| Some(*c) != dir.collection()).collect();
            if let Some(c) = newdir.collection() {
                if !ids.contains(&c) {
                    ids.push(c);
                }
            }
            u.collection_ids = Some(ids);
        }
        self.client.update_document(id, &u)?;
        self.refresh(true)?;
        let mut st = self.lock();
        if let Some(ino) = st.inodes.remove(&Key::Doc(dir, id)) {
            st.inodes.insert(Key::Doc(newdir, id), ino);
            st.keys.insert(ino, Key::Doc(newdir, id));
        }
        Ok(())
    }

    fn drop_local(&self, l: u64) {
        let mut st = self.lock();
        if let Some(old) = st.locals.remove(&l) {
            if let Some(id) = old.shadow_of {
                st.hidden.remove(&(old.dir, id));
            }
            let _ = std::fs::remove_file(old.path);
        }
    }

    fn rename_local(&self, l: u64, newdir: Dir, newname: &str, dst: Option<Found>) -> Result<()> {
        let shadow = self.lock().locals.get(&l).ok_or(Error::NotFound)?.shadow_of;
        match dst {
            Some(Found::Dir(_)) => return Err(Error::IsDir),
            Some(Found::Doc(target)) => {
                // Gespeicherte Datei ersetzt ein Dokument (LibreOffice, gedit, …)
                if shadow.is_some() {
                    return Err(Error::Perm);
                }
                {
                    let mut st = self.lock();
                    let lf = st.locals.get_mut(&l).ok_or(Error::NotFound)?;
                    lf.dir = newdir;
                    lf.name = newname.to_string();
                    lf.target_doc = Some(target);
                }
                // Inode des Ziels bleibt das Dokument, die lokale Inode verschwindet
                let ino_target = self.lock().inodes.get(&Key::Doc(newdir, target)).copied();
                self.commit_local(l, None)?;
                if let Some(i) = ino_target {
                    let mut st = self.lock();
                    st.inodes.insert(Key::Doc(newdir, target), i);
                    st.keys.insert(i, Key::Doc(newdir, target));
                }
                return Ok(());
            }
            Some(Found::Local(other)) => self.drop_local(other),
            None => {}
        }

        if let Some(id) = shadow {
            if !is_temp(newname) {
                // Weggebenanntes Original kommt zurück
                let dir = {
                    let mut st = self.lock();
                    let lf = st.locals.remove(&l).ok_or(Error::NotFound)?;
                    let _ = std::fs::remove_file(&lf.path);
                    st.hidden.remove(&(lf.dir, id));
                    if let Some(ino) = st.inodes.remove(&Key::Local(l)) {
                        st.inodes.insert(Key::Doc(lf.dir, id), ino);
                        st.keys.insert(ino, Key::Doc(lf.dir, id));
                    }
                    lf.dir
                };
                let name = self.name_of_doc(&self.lock(), dir, id);
                if name.as_deref() == Some(newname) && dir == newdir {
                    return Ok(());
                }
                return self.rename_doc(dir, id, newdir, newname, None);
            }
        }

        let commit = {
            let mut st = self.lock();
            let target = if is_temp(newname) { None } else { self.shadowed_with_name(&st, newdir, newname) };
            let busy = st.handles.values().any(|h| h.key == Key::Local(l) && h.dirty);
            let lf = st.locals.get_mut(&l).ok_or(Error::NotFound)?;
            lf.dir = newdir;
            lf.name = newname.to_string();
            if target.is_some() {
                lf.target_doc = target;
            }
            !is_temp(newname) && lf.has_content && !busy && shadow.is_none()
        };
        if commit {
            self.commit_local(l, None)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}

#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, off)
}

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}

#[cfg(windows)]
fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: i64, title: &str, file: &str, colls: &[i64]) -> DocumentSummary {
        DocumentSummary {
            id,
            title: title.into(),
            original_filename: file.into(),
            mime: String::new(),
            kind: "other".into(),
            size: 1,
            created_at: 0,
            updated_at: 0,
            document_date: None,
            version: 1,
            extract_status: "done".into(),
            page_count: None,
            has_thumbnail: false,
            collection_ids: colls.to_vec(),
            tag_ids: vec![],
            snippet: None,
            score: None,
        }
    }

    fn coll(id: i64, parent: Option<i64>, name: &str) -> Collection {
        Collection { id, parent_id: parent, name: name.into(), color: None, document_count: 0, ai_locked: false }
    }

    #[test]
    fn names() {
        assert_eq!(doc_file_name(&doc(1, "Rechnung 03/2026", "scan.pdf", &[])), "Rechnung 03_2026.pdf");
        assert_eq!(doc_file_name(&doc(1, "brief.pdf", "brief.pdf", &[])), "brief.pdf");
        assert_eq!(doc_file_name(&doc(1, "Notizen", "notes", &[])), "Notizen");
        assert_eq!(title_from_name("Neu.PDF", "x.pdf"), "Neu");
        assert_eq!(title_from_name("Neu.txt", "x.pdf"), "Neu.txt");
        assert_eq!(sanitize_name("a:b. "), "a_b");
        assert!(is_temp("~$Brief.docx") && is_temp(".~lock.a.odt#") && is_temp("lu123.tmp") && !is_temp("a.docx"));
    }

    #[test]
    fn tree() {
        let s = Snapshot::build(
            vec![coll(1, None, "Finanzen"), coll(2, Some(1), "Rechnungen"), coll(3, None, "Eingang")],
            vec![doc(10, "A", "a.pdf", &[1, 2]), doc(11, "A", "b.pdf", &[2]), doc(12, "Neu", "n.txt", &[])],
        );
        let names = |d: Dir| s.entries[&d].iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(Dir::Root), ["Eingang", "Eingang (3)", "Finanzen"]);
        assert_eq!(names(Dir::Coll(1)), ["Rechnungen", "A.pdf"]);
        assert_eq!(names(Dir::Coll(2)), ["A.pdf", "A (11).pdf"]);
        assert_eq!(names(Dir::Inbox), ["Neu.txt"]);
    }
}
