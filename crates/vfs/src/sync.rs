// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Abgleich zwischen einem echten lokalen Ordner und Büro (Windows-Sync-Ordner
//! wie bei OneDrive/Nextcloud).
//!
//! Anders als beim Laufwerk liegen die Dateien wirklich auf der Platte (als
//! Platzhalter, die erst beim Öffnen geladen werden). Deshalb arbeitet der
//! Abgleich nicht mit einzelnen Dateioperationen, sondern vergleicht in jedem
//! Durchlauf drei Stände:
//!
//! - **lokal**: was gerade im Ordner liegt; jeder abgeglichene Eintrag trägt
//!   seine Büro-Kennung ([`Meta`]) samt Name und Ort beim letzten Abgleich
//! - **Server**: Sammlungen und Dokumente in Büro
//! - **Index**: welche Einträge beim letzten Durchlauf lokal vorhanden waren
//!   (um lokal Gelöschtes von neu auf dem Server Angelegtem zu unterscheiden)
//!
//! Ein Durchlauf überträgt erst lokale Änderungen (neue Dateien und Ordner,
//! geänderter Inhalt, Umbenennen, Verschieben, Löschen) und holt dann den
//! Stand des Servers (neue, geänderte, umbenannte und gelöschte Einträge).
//! Die Abbildung ist dieselbe wie beim Laufwerk (siehe `core`).
//!
//! Die Logik ist plattformunabhängig ([`LocalFs`], [`Remote`]) und wird mit
//! Attrappen getestet; die Windows-Anbindung steckt in `cloud`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::{Collection, CollectionInput, DocumentSummary, DocumentUpdate};

use crate::client::{ApiError, ApiResult, Client};
use crate::core::{is_temp, sanitize_name, stem, title_from_name, Dir, Entry, Snapshot, INBOX_NAME};

/// Pfad relativ zum Sync-Ordner, als Liste der Namen
pub type RelPath = Vec<String>;

/// Büro-Kennung eines lokalen Eintrags (am Platzhalter gespeichert)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Meta {
    /// Ordner „Eingang“
    #[serde(rename = "inbox")]
    Inbox,
    /// Sammlung; Name und übergeordnete Sammlung beim letzten Abgleich
    #[serde(rename = "coll")]
    Coll { id: i64, name: String, parent: Option<i64> },
    /// Dokument in einem Ordner (`dir`: 0 = Eingang, sonst Sammlung)
    #[serde(rename = "doc")]
    Doc { id: i64, dir: i64, version: i64, name: String },
}

impl Meta {
    pub fn to_blob(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_blob(b: &[u8]) -> Option<Meta> {
        serde_json::from_slice(b).ok()
    }

    pub fn ident(&self) -> Ident {
        match self {
            Meta::Inbox => Ident::Inbox,
            Meta::Coll { id, .. } => Ident::Coll(*id),
            Meta::Doc { id, dir, .. } => Ident::Doc(*id, *dir),
        }
    }
}

/// Was ein Eintrag ist, ohne Name/Version (für den Index)
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Ident {
    Inbox,
    Coll(i64),
    Doc(i64, i64),
}

fn dir_key(d: Dir) -> i64 {
    match d {
        Dir::Coll(c) => c,
        _ => 0,
    }
}

fn dir_of_key(k: i64) -> Dir {
    if k > 0 {
        Dir::Coll(k)
    } else {
        Dir::Inbox
    }
}

/// Ein Eintrag im lokalen Ordner
#[derive(Clone, Debug)]
pub struct LocalEntry {
    pub path: RelPath,
    pub is_dir: bool,
    /// `None`: noch nie abgeglichen (neu angelegt) oder kein Platzhalter
    pub meta: Option<Meta>,
    /// Datei: Inhalt seit dem letzten Abgleich geändert (oder neu)
    pub dirty: bool,
}

impl LocalEntry {
    fn name(&self) -> &str {
        self.path.last().map(String::as_str).unwrap_or("")
    }
}

/// Zugriff auf den lokalen Ordner
pub trait LocalFs {
    /// Alle Einträge (rekursiv). Ein Fehler bricht den Durchlauf ab – aus einem
    /// unvollständigen Stand dürfen keine Löschungen abgeleitet werden.
    fn scan(&self) -> std::io::Result<Vec<LocalEntry>>;
    /// Ordner als abgeglichenen Platzhalter anlegen
    fn create_dir(&self, path: &[String], meta: &Meta) -> std::io::Result<()>;
    /// Datei als (noch nicht geladenen) Platzhalter anlegen
    fn create_file(&self, path: &[String], meta: &Meta, size: u64, mtime: i64) -> std::io::Result<()>;
    /// Neue Fassung vom Server: Größe/Zeit übernehmen, alten Inhalt verwerfen
    fn replace_file(&self, path: &[String], meta: &Meta, size: u64, mtime: i64) -> std::io::Result<()>;
    /// Inhalt für den Upload festhalten: Kopie und Kennung des Stands
    fn snapshot(&self, path: &[String]) -> std::io::Result<(PathBuf, u64)>;
    /// Kennung speichern (macht neue Einträge zu Platzhaltern) und – wenn die
    /// Datei seit `stamp` unverändert ist – als abgeglichen markieren.
    /// `false`: inzwischen wieder geändert (bleibt zum Hochladen markiert).
    fn mark_synced(&self, path: &[String], meta: &Meta, stamp: Option<u64>) -> std::io::Result<bool>;
    fn rename(&self, from: &[String], to: &[String]) -> std::io::Result<()>;
    /// Datei bzw. leeren Ordner entfernen
    fn remove(&self, path: &[String], is_dir: bool) -> std::io::Result<()>;
    /// Kopie aus [`LocalFs::snapshot`] wegräumen
    fn discard_snapshot(&self, file: &Path) {
        let _ = std::fs::remove_file(file);
    }
}

/// Zugriff auf den Büro-Server
pub trait Remote {
    fn collections(&self) -> ApiResult<Vec<Collection>>;
    fn all_documents(&self) -> ApiResult<Vec<DocumentSummary>>;
    fn upload(&self, file: &Path, filename: &str, title: &str, colls: &[i64]) -> ApiResult<DocumentSummary>;
    fn new_version(&self, id: i64, file: &Path, filename: &str, basis: Option<i64>) -> ApiResult<DocumentSummary>;
    fn update_document(&self, id: i64, u: &DocumentUpdate) -> ApiResult<DocumentSummary>;
    fn delete_document(&self, id: i64) -> ApiResult<()>;
    fn create_collection(&self, name: &str, parent: Option<i64>) -> ApiResult<Collection>;
    fn update_collection(&self, id: i64, input: &CollectionInput) -> ApiResult<Collection>;
    fn delete_collection(&self, id: i64) -> ApiResult<()>;
}

impl Remote for Client {
    fn collections(&self) -> ApiResult<Vec<Collection>> {
        Client::collections(self)
    }
    fn all_documents(&self) -> ApiResult<Vec<DocumentSummary>> {
        Client::all_documents(self)
    }
    fn upload(&self, file: &Path, filename: &str, title: &str, colls: &[i64]) -> ApiResult<DocumentSummary> {
        Client::upload(self, file, filename, title, colls)
    }
    fn new_version(&self, id: i64, file: &Path, filename: &str, basis: Option<i64>) -> ApiResult<DocumentSummary> {
        Client::new_version(self, id, file, filename, basis)
    }
    fn update_document(&self, id: i64, u: &DocumentUpdate) -> ApiResult<DocumentSummary> {
        Client::update_document(self, id, u)
    }
    fn delete_document(&self, id: i64) -> ApiResult<()> {
        Client::delete_document(self, id)
    }
    fn create_collection(&self, name: &str, parent: Option<i64>) -> ApiResult<Collection> {
        Client::create_collection(self, name, parent)
    }
    fn update_collection(&self, id: i64, input: &CollectionInput) -> ApiResult<Collection> {
        Client::update_collection(self, id, input)
    }
    fn delete_collection(&self, id: i64) -> ApiResult<()> {
        Client::delete_collection(self, id)
    }
}

/// Ergebnis eines Durchlaufs
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Pass {
    /// Am Server wurde etwas geändert
    pub remote_changed: bool,
    /// Im lokalen Ordner wurde etwas geändert
    pub local_changed: bool,
    /// Es steht noch etwas aus (Löschen wird bestätigt, Speichern läuft):
    /// bald noch einmal abgleichen
    pub pending: bool,
    /// Probleme dieses Durchlaufs (für die Statusanzeige)
    pub errors: Vec<String>,
}

/// Fehlt ein Eintrag so viele Durchläufe hintereinander, gilt er als gelöscht
const CONFIRM_DELETE: u32 = 2;
/// So viele Durchläufe auf das Ende eines Speichervorgangs warten
const MAX_WAIT: u32 = 15;

/// Index: was beim letzten Durchlauf lokal vorhanden war
#[derive(Default, Serialize, Deserialize)]
struct IndexFile {
    entries: BTreeSet<Ident>,
}

pub struct Engine<R: Remote, L: LocalFs> {
    pub remote: R,
    pub local: L,
    state_file: Option<PathBuf>,
    index: BTreeSet<Ident>,
    /// Lokal fehlende Einträge: in wie vielen Durchläufen schon
    missing: HashMap<Ident, u32>,
    /// Neue Dateien, die auf das Ende eines Speichervorgangs warten
    waiting: HashMap<RelPath, u32>,
}

fn lower(p: &[String]) -> Vec<String> {
    p.iter().map(|s| s.to_lowercase()).collect()
}

fn join(base: &[String], name: &str) -> RelPath {
    let mut p = base.to_vec();
    p.push(name.to_string());
    p
}

fn show(p: &[String]) -> String {
    p.join("\\")
}

enum PullStep {
    Done(bool),
    /// Ein Ordner wurde verschoben: neu einlesen und weitermachen
    Rescan,
}

impl<R: Remote, L: LocalFs> Engine<R, L> {
    /// `state_file`: hier wird der Index gespeichert (`None`: nur im Speicher)
    pub fn new(remote: R, local: L, state_file: Option<PathBuf>) -> Self {
        let index = state_file
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<IndexFile>(&b).ok())
            .map(|i| i.entries)
            .unwrap_or_default();
        Engine { remote, local, state_file, index, missing: HashMap::new(), waiting: HashMap::new() }
    }

    fn load(&self) -> Result<Snapshot, String> {
        let colls = self.remote.collections().map_err(|e| e.to_string())?;
        let docs = self.remote.all_documents().map_err(|e| e.to_string())?;
        Ok(Snapshot::build(colls, docs))
    }

    fn scan(&self) -> Result<Vec<LocalEntry>, String> {
        let mut v = self.local.scan().map_err(|e| format!("Ordner lesen: {e}"))?;
        v.sort_by(|a, b| a.path.len().cmp(&b.path.len()).then_with(|| a.path.cmp(&b.path)));
        Ok(v)
    }

    /// Ein vollständiger Abgleich
    pub fn reconcile(&mut self) -> Result<Pass, String> {
        let mut pass = Pass::default();
        let mut snap = self.load()?;
        let mut entries = self.scan()?;
        self.push(&snap, &entries, &mut pass);
        if pass.remote_changed {
            snap = self.load()?;
            entries = self.scan()?;
        }
        let mut done = false;
        for _ in 0..8 {
            match self.pull(&snap, &entries, &mut pass) {
                PullStep::Done(changed) => {
                    pass.local_changed |= changed;
                    done = true;
                    break;
                }
                PullStep::Rescan => {
                    pass.local_changed = true;
                    entries = self.scan()?;
                }
            }
        }
        if !done {
            pass.pending = true;
        }
        if pass.local_changed {
            entries = self.scan()?;
        }
        // Noch nicht bestätigte Löschungen bleiben im Index, damit sie beim
        // nächsten Mal wieder als fehlend erkannt werden
        let mut index: BTreeSet<Ident> = entries.iter().filter_map(|e| e.meta.as_ref().map(Meta::ident)).collect();
        index.extend(self.missing.keys().cloned());
        self.index = index;
        self.save_index();
        pass.pending |= !self.missing.is_empty() || !self.waiting.is_empty();
        Ok(pass)
    }

    fn save_index(&self) {
        if let Some(p) = &self.state_file {
            let f = IndexFile { entries: self.index.clone() };
            if let Ok(b) = serde_json::to_vec(&f) {
                let tmp = p.with_extension("tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, p);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Lokal → Server
    // -----------------------------------------------------------------------

    fn push(&mut self, snap: &Snapshot, entries: &[LocalEntry], pass: &mut Pass) {
        let mut dirs: HashMap<Vec<String>, Dir> = HashMap::new();
        dirs.insert(Vec::new(), Dir::Root);
        let mut by_ident: HashMap<Ident, Vec<&LocalEntry>> = HashMap::new();
        for e in entries {
            if let Some(m) = &e.meta {
                by_ident.entry(m.ident()).or_default().push(e);
            }
        }

        // Ordner (von oben nach unten)
        for e in entries.iter().filter(|e| e.is_dir) {
            let name = e.name().to_string();
            let Some(&pdir) = dirs.get(&lower(&e.path[..e.path.len() - 1])) else { continue };
            if is_temp(&name) {
                continue;
            }
            if pdir == Dir::Root && name.eq_ignore_ascii_case(INBOX_NAME) {
                dirs.insert(lower(&e.path), Dir::Inbox);
                if e.meta != Some(Meta::Inbox) {
                    self.mark(&e.path, &Meta::Inbox, None, pass);
                }
                continue;
            }
            if pdir == Dir::Inbox {
                pass.errors.push(format!("„{}“: Im Eingang sind keine Unterordner möglich", show(&e.path)));
                continue;
            }
            let parent = pdir.collection();
            match &e.meta {
                Some(Meta::Coll { id, name: mname, parent: mparent }) if snap.colls.contains_key(id) => {
                    dirs.insert(lower(&e.path), Dir::Coll(*id));
                    if &name != mname || parent != *mparent {
                        let input = CollectionInput {
                            name: (&name != mname).then(|| name.clone()),
                            parent_id: (parent != *mparent).then_some(parent.unwrap_or(0)),
                            color: None,
                            ai_locked: None,
                        };
                        match self.remote.update_collection(*id, &input) {
                            Ok(_) => {
                                pass.remote_changed = true;
                                self.mark(&e.path, &Meta::Coll { id: *id, name: name.clone(), parent }, None, pass);
                            }
                            Err(err) => pass.errors.push(format!("Ordner „{}“: {err}", show(&e.path))),
                        }
                    }
                }
                // Auf dem Server gelöscht: lokal wegräumen (beim Holen), außer es
                // liegen dort neue oder geänderte Dateien – dann neu anlegen
                Some(Meta::Coll { .. }) if !Self::has_changes_below(entries, &e.path) => {}
                _ => {
                    let res = self.remote.create_collection(&name, parent).or_else(|err| {
                        // Gibt es schon (z. B. gleichzeitig am Server angelegt)
                        snap.colls
                            .values()
                            .find(|c| c.parent_id == parent && sanitize_name(&c.name).eq_ignore_ascii_case(&name))
                            .cloned()
                            .ok_or(err)
                    });
                    match res {
                        Ok(c) => {
                            dirs.insert(lower(&e.path), Dir::Coll(c.id));
                            pass.remote_changed = true;
                            self.mark(&e.path, &Meta::Coll { id: c.id, name: name.clone(), parent }, None, pass);
                        }
                        Err(err) => pass.errors.push(format!("Ordner „{}“ anlegen: {err}", show(&e.path))),
                    }
                }
            }
        }

        // Dateien
        let mut adopted: HashSet<Ident> = HashSet::new();
        let mut seen_waiting: HashSet<RelPath> = HashSet::new();
        for e in entries.iter().filter(|e| !e.is_dir) {
            let name = e.name().to_string();
            if is_temp(&name) {
                continue;
            }
            let Some(&pdir) = dirs.get(&lower(&e.path[..e.path.len() - 1])) else { continue };
            match &e.meta {
                Some(Meta::Doc { id, dir, version, name: mname }) => {
                    self.push_doc(snap, e, pdir, *id, dir_of_key(*dir), *version, mname, pass);
                }
                _ => {
                    // Speichern über eine temporäre Datei (Word, LibreOffice, …):
                    // neue Datei unter dem Namen eines Dokuments, dessen Platzhalter fehlt
                    match self.replacement(snap, pdir, &name, &by_ident) {
                        Replacement::Wait => {
                            seen_waiting.insert(e.path.clone());
                            let n = self.waiting.entry(e.path.clone()).or_insert(0);
                            *n += 1;
                            if *n <= MAX_WAIT {
                                continue;
                            }
                            // Zu lange gewartet: als neue Fassung übernehmen
                            if let Some(d) = self.doc_named(snap, pdir, &name) {
                                adopted.insert(Ident::Doc(d, dir_key(pdir)));
                                self.save_as_version(snap, e, pdir, d, pass);
                            }
                        }
                        Replacement::Doc(d) => {
                            adopted.insert(Ident::Doc(d, dir_key(pdir)));
                            self.save_as_version(snap, e, pdir, d, pass);
                        }
                        Replacement::None => self.upload_new(e, pdir, pass),
                    }
                }
            }
        }
        self.waiting.retain(|p, _| seen_waiting.contains(p));

        self.push_deletions(snap, entries, &adopted, pass);
    }

    /// Liegen unter `path` neue oder geänderte Dateien?
    fn has_changes_below(entries: &[LocalEntry], path: &[String]) -> bool {
        entries
            .iter()
            .any(|e| e.path.len() > path.len() && e.path.starts_with(path) && !is_temp(e.name()) && (e.dirty || e.meta.is_none()))
    }

    fn doc_named(&self, snap: &Snapshot, dir: Dir, name: &str) -> Option<i64> {
        snap.entries.get(&dir)?.iter().find_map(|(n, e)| match e {
            Entry::Doc(d) if n.to_lowercase() == name.to_lowercase() => Some(*d),
            _ => None,
        })
    }

    fn replacement(&self, snap: &Snapshot, dir: Dir, name: &str, by_ident: &HashMap<Ident, Vec<&LocalEntry>>) -> Replacement {
        let Some(d) = self.doc_named(snap, dir, name) else { return Replacement::None };
        let ident = Ident::Doc(d, dir_key(dir));
        // Nie lokal gewesen: gleichnamige neue Datei, kein Ersatz
        if !self.index.contains(&ident) {
            return Replacement::None;
        }
        match by_ident.get(&ident) {
            None => Replacement::Doc(d),
            // Original gerade weggebenannt (z. B. „~WRL0002.tmp“): Speichern läuft noch
            Some(list) if list.iter().all(|e| is_temp(e.name())) => Replacement::Wait,
            Some(_) => Replacement::None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_doc(&mut self, snap: &Snapshot, e: &LocalEntry, pdir: Dir, id: i64, mdir: Dir, version: i64, mname: &str, pass: &mut Pass) {
        let name = e.name().to_string();
        let Some(doc) = snap.docs.get(&id) else {
            // Auf dem Server gelöscht; lokale Änderungen als neues Dokument retten
            if e.dirty {
                self.upload_new(e, if pdir == Dir::Root { Dir::Inbox } else { pdir }, pass);
            }
            return;
        };
        if pdir == Dir::Root {
            pass.errors.push(format!("„{name}“: Dateien bitte in einen Ordner legen (z. B. „{INBOX_NAME}“)"));
            return;
        }
        let moved = pdir != mdir;
        let renamed = name != mname;
        if !e.dirty && !moved && !renamed {
            return;
        }
        let mut meta = Meta::Doc { id, dir: dir_key(mdir), version, name: mname.to_string() };
        let mut stamp = None;
        if e.dirty {
            let (file, st) = match self.local.snapshot(&e.path) {
                Ok(x) => x,
                Err(err) => {
                    // Z. B. noch zum Schreiben geöffnet: später erneut
                    pass.pending = true;
                    log::info!("Büro-Sync: {} noch nicht lesbar: {err}", show(&e.path));
                    return;
                }
            };
            stamp = Some(st);
            let res = self.remote.new_version(id, &file, &doc.original_filename, Some(version));
            match res {
                Ok(d) => {
                    pass.remote_changed = true;
                    if let Meta::Doc { version, .. } = &mut meta {
                        *version = d.version;
                    }
                }
                Err(err) if err.status == Some(409) => {
                    // Inzwischen anderswo geändert: eigene Fassung als neues Dokument daneben
                    let title = format!("{} (Konflikt)", title_from_name(&name, &doc.original_filename));
                    let res = self.remote.upload(&file, &name, &title, &pdir.collection().into_iter().collect::<Vec<_>>());
                    self.local.discard_snapshot(&file);
                    match res {
                        Ok(d) => {
                            pass.remote_changed = true;
                            pass.errors.push(format!(
                                "„{}“ wurde inzwischen anderswo geändert – deine Fassung liegt daneben als „{}“",
                                doc.title, d.title
                            ));
                            // Diese Datei wird zum neuen Dokument; das Original kommt neu
                            let m = Meta::Doc { id: d.id, dir: dir_key(pdir), version: d.version, name };
                            self.mark(&e.path, &m, stamp, pass);
                        }
                        Err(err) => pass.errors.push(format!("„{name}“ hochladen: {err}")),
                    }
                    return;
                }
                Err(err) => {
                    self.local.discard_snapshot(&file);
                    pass.errors.push(format!("„{name}“ hochladen: {err}"));
                    return;
                }
            }
            self.local.discard_snapshot(&file);
        }
        if moved || renamed {
            let mut u = DocumentUpdate::default();
            if renamed {
                u.title = Some(title_from_name(&name, &doc.original_filename));
            }
            if moved {
                let mut ids: Vec<i64> = doc.collection_ids.iter().copied().filter(|c| Some(*c) != mdir.collection()).collect();
                if let Some(c) = pdir.collection() {
                    if !ids.contains(&c) {
                        ids.push(c);
                    }
                }
                u.collection_ids = Some(ids);
            }
            match self.remote.update_document(id, &u) {
                Ok(_) => {
                    pass.remote_changed = true;
                    if let Meta::Doc { dir, name: n, .. } = &mut meta {
                        *dir = dir_key(pdir);
                        *n = name.clone();
                    }
                }
                Err(err) => pass.errors.push(format!("„{name}“ umbenennen/verschieben: {err}")),
            }
        }
        self.mark(&e.path, &meta, stamp, pass);
    }

    /// Neue lokale Datei als neue Fassung eines vorhandenen Dokuments speichern
    fn save_as_version(&mut self, snap: &Snapshot, e: &LocalEntry, pdir: Dir, id: i64, pass: &mut Pass) {
        let Some(doc) = snap.docs.get(&id) else { return };
        let (file, stamp) = match self.local.snapshot(&e.path) {
            Ok(x) => x,
            Err(_) => {
                pass.pending = true;
                return;
            }
        };
        let res = self.remote.new_version(id, &file, &doc.original_filename, None);
        self.local.discard_snapshot(&file);
        match res {
            Ok(d) => {
                pass.remote_changed = true;
                let m = Meta::Doc { id, dir: dir_key(pdir), version: d.version, name: e.name().to_string() };
                self.mark(&e.path, &m, Some(stamp), pass);
            }
            Err(err) => pass.errors.push(format!("„{}“ speichern: {err}", e.name())),
        }
    }

    fn upload_new(&mut self, e: &LocalEntry, pdir: Dir, pass: &mut Pass) {
        let mut path = e.path.clone();
        let mut dir = pdir;
        let name = e.name().to_string();
        if name != sanitize_name(&name) {
            pass.errors.push(format!("„{}“: Name enthält ungültige Zeichen", show(&path)));
            return;
        }
        if dir == Dir::Root {
            // Direkt im Sync-Ordner: in den Eingang legen
            let target = vec![INBOX_NAME.to_string(), name.clone()];
            if let Err(err) = self.local.rename(&path, &target) {
                pass.errors.push(format!("„{name}“ in den Eingang verschieben: {err}"));
                return;
            }
            pass.local_changed = true;
            path = target;
            dir = Dir::Inbox;
        }
        let (file, stamp) = match self.local.snapshot(&path) {
            Ok(x) => x,
            Err(_) => {
                pass.pending = true;
                return;
            }
        };
        let colls: Vec<i64> = dir.collection().into_iter().collect();
        let res = self.remote.upload(&file, &name, stem(&name), &colls);
        self.local.discard_snapshot(&file);
        match res {
            Ok(d) => {
                pass.remote_changed = true;
                let m = Meta::Doc { id: d.id, dir: dir_key(dir), version: d.version, name };
                self.mark(&path, &m, Some(stamp), pass);
            }
            Err(err) => pass.errors.push(format!("„{name}“ hochladen: {err}")),
        }
    }

    fn mark(&self, path: &[String], meta: &Meta, stamp: Option<u64>, pass: &mut Pass) {
        match self.local.mark_synced(path, meta, stamp) {
            Ok(true) => {}
            // Während des Hochladens wieder geändert: nächster Durchlauf lädt erneut
            Ok(false) => pass.pending = true,
            Err(err) => pass.errors.push(format!("„{}“: {err}", show(path))),
        }
    }

    /// Lokal gelöschte Einträge am Server entfernen
    fn push_deletions(&mut self, snap: &Snapshot, entries: &[LocalEntry], adopted: &HashSet<Ident>, pass: &mut Pass) {
        let present: HashSet<Ident> = entries.iter().filter_map(|e| e.meta.as_ref().map(Meta::ident)).collect();
        let inbox_present = present.contains(&Ident::Inbox)
            || entries.iter().any(|e| e.is_dir && e.path.len() == 1 && e.name().eq_ignore_ascii_case(INBOX_NAME));
        let gone: Vec<Ident> = self
            .index
            .iter()
            .filter(|i| !present.contains(i) && !adopted.contains(i) && **i != Ident::Inbox)
            .filter(|i| match i {
                Ident::Coll(c) => snap.colls.contains_key(c),
                Ident::Doc(d, k) => snap
                    .entries
                    .get(&dir_of_key(*k))
                    .is_some_and(|l| l.iter().any(|(_, e)| *e == Entry::Doc(*d))),
                Ident::Inbox => false,
            })
            .cloned()
            .collect();
        self.missing.retain(|i, _| gone.contains(i));
        if gone.is_empty() {
            return;
        }
        // Schutz: fehlt fast alles (Ordner verschoben, Platte nicht da, …),
        // wird nichts gelöscht
        if !inbox_present || (gone.len() > 20 && gone.len() * 2 > self.index.len()) {
            pass.errors.push(format!(
                "{} Einträge fehlen im Sync-Ordner – zur Sicherheit wird nichts am Server gelöscht",
                gone.len()
            ));
            self.missing.clear();
            return;
        }
        let mut confirmed = Vec::new();
        for i in gone {
            let n = self.missing.entry(i.clone()).or_insert(0);
            *n += 1;
            if *n >= CONFIRM_DELETE {
                confirmed.push(i);
            }
        }
        // Erst Dokumente, dann Sammlungen (Unterordner zuerst)
        confirmed.sort_by_key(|i| match i {
            Ident::Doc(..) => 0,
            _ => 1,
        });
        for i in confirmed {
            let res = match &i {
                Ident::Doc(d, k) => match dir_of_key(*k) {
                    Dir::Coll(c) => match snap.docs.get(d) {
                        Some(doc) => {
                            let ids: Vec<i64> = doc.collection_ids.iter().copied().filter(|x| *x != c).collect();
                            self.remote
                                .update_document(*d, &DocumentUpdate { collection_ids: Some(ids), ..Default::default() })
                                .map(|_| ())
                        }
                        None => Ok(()),
                    },
                    _ => self.remote.delete_document(*d),
                },
                Ident::Coll(c) => self.remote.delete_collection(*c),
                Ident::Inbox => Ok(()),
            };
            match res {
                Ok(()) | Err(ApiError { status: Some(404), .. }) => {
                    pass.remote_changed = true;
                    self.missing.remove(&i);
                }
                Err(err) => pass.errors.push(format!("Löschen am Server: {err}")),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Server → lokal
    // -----------------------------------------------------------------------

    /// Erwartete Pfade aller Ordner
    fn dir_paths(snap: &Snapshot) -> Vec<(Dir, RelPath)> {
        let mut out = Vec::new();
        let mut queue = vec![(Dir::Root, Vec::<String>::new())];
        let mut seen = HashSet::new();
        while let Some((d, p)) = queue.pop() {
            if !seen.insert(d) {
                continue;
            }
            for (name, e) in snap.entries.get(&d).into_iter().flatten() {
                if let Entry::Dir(child) = e {
                    queue.push((*child, join(&p, name)));
                }
            }
            out.push((d, p));
        }
        out.sort_by(|a, b| a.1.len().cmp(&b.1.len()).then_with(|| a.1.cmp(&b.1)));
        out
    }

    fn pull(&mut self, snap: &Snapshot, entries: &[LocalEntry], pass: &mut Pass) -> PullStep {
        let mut changed = false;
        let mut by_ident: HashMap<Ident, Vec<&LocalEntry>> = HashMap::new();
        let mut by_path: HashMap<Vec<String>, &LocalEntry> = HashMap::new();
        for e in entries {
            if let Some(m) = &e.meta {
                by_ident.entry(m.ident()).or_default().push(e);
            }
            by_path.insert(lower(&e.path), e);
        }
        let dirs = Self::dir_paths(snap);
        let dir_path: HashMap<Dir, RelPath> = dirs.iter().cloned().collect();
        let mut expected: HashSet<Ident> = HashSet::new();

        // Ordner
        for (d, path) in &dirs {
            let (ident, meta) = match d {
                Dir::Root => continue,
                Dir::Inbox => (Ident::Inbox, Meta::Inbox),
                Dir::Coll(c) => {
                    let parent = snap.colls.get(c).and_then(|x| x.parent_id).filter(|p| snap.colls.contains_key(p));
                    (Ident::Coll(*c), Meta::Coll { id: *c, name: path.last().cloned().unwrap_or_default(), parent })
                }
            };
            expected.insert(ident.clone());
            match by_ident.get(&ident).and_then(|l| l.first()) {
                Some(e) if &e.path != path => {
                    // Am Server umbenannt/verschoben
                    if by_path.get(&lower(path)).is_some_and(|o| o.path != e.path && !o.path.starts_with(&e.path)) {
                        pass.errors.push(format!("„{}“ ist lokal schon belegt", show(path)));
                        continue;
                    }
                    match self.local.rename(&e.path, path) {
                        Ok(()) => {
                            self.mark(path, &meta, None, pass);
                            return PullStep::Rescan;
                        }
                        Err(err) => pass.errors.push(format!("„{}“ umbenennen: {err}", show(&e.path))),
                    }
                }
                Some(e) => {
                    if e.meta.as_ref() != Some(&meta) {
                        self.mark(path, &meta, None, pass);
                    }
                }
                None => {
                    if ident != Ident::Inbox && self.index.contains(&ident) {
                        continue; // lokal gelöscht, wird gerade bestätigt
                    }
                    match by_path.get(&lower(path)) {
                        Some(o) if o.is_dir && o.meta.is_none() => self.mark(path, &meta, None, pass),
                        Some(_) => pass.errors.push(format!("„{}“ ist lokal schon belegt", show(path))),
                        None => match self.local.create_dir(path, &meta) {
                            Ok(()) => changed = true,
                            Err(err) => pass.errors.push(format!("Ordner „{}“ anlegen: {err}", show(path))),
                        },
                    }
                }
            }
        }

        // Dateien
        for (d, list) in &snap.entries {
            let Some(dpath) = dir_path.get(d) else { continue };
            if *d == Dir::Root {
                continue;
            }
            for (name, e) in list {
                let Entry::Doc(id) = e else { continue };
                let Some(doc) = snap.docs.get(id) else { continue };
                let path = join(dpath, name);
                let ident = Ident::Doc(*id, dir_key(*d));
                expected.insert(ident.clone());
                let meta = Meta::Doc { id: *id, dir: dir_key(*d), version: doc.version, name: name.clone() };
                let size = doc.size.max(0) as u64;
                let locals = by_ident.get(&ident).cloned().unwrap_or_default();
                // Doppelt (z. B. Kopie im selben Ordner): eine behalten
                let keep = locals
                    .iter()
                    .find(|l| l.path == path)
                    .or_else(|| locals.iter().find(|l| !is_temp(l.name())))
                    .or(locals.first())
                    .copied();
                for other in locals.iter().filter(|l| Some(l.path.as_slice()) != keep.map(|k| k.path.as_slice())) {
                    if !other.dirty && !is_temp(other.name()) && self.local.remove(&other.path, false).is_ok() {
                        changed = true;
                    }
                }
                match keep {
                    Some(l) => {
                        if is_temp(l.name()) {
                            continue; // wird gerade gespeichert
                        }
                        let mut cur = l.path.clone();
                        if l.path != path {
                            if by_path.get(&lower(&path)).is_some_and(|o| o.path != l.path) {
                                pass.errors.push(format!("„{}“ ist lokal schon belegt", show(&path)));
                                continue;
                            }
                            match self.local.rename(&l.path, &path) {
                                Ok(()) => {
                                    changed = true;
                                    cur = path.clone();
                                }
                                Err(err) => {
                                    pass.errors.push(format!("„{}“ umbenennen: {err}", show(&l.path)));
                                    continue;
                                }
                            }
                        }
                        let local_version = match &l.meta {
                            Some(Meta::Doc { version, .. }) => *version,
                            _ => 0,
                        };
                        if l.dirty {
                            // Lokale Änderung wird beim nächsten Durchlauf hochgeladen
                            pass.pending = true;
                        } else if local_version != doc.version {
                            match self.local.replace_file(&cur, &meta, size, doc.updated_at) {
                                Ok(()) => changed = true,
                                Err(err) => pass.errors.push(format!("„{}“ aktualisieren: {err}", show(&cur))),
                            }
                        } else if l.meta.as_ref() != Some(&meta) {
                            self.mark(&cur, &meta, None, pass);
                        }
                    }
                    None => {
                        if self.index.contains(&ident) {
                            continue; // lokal gelöscht, wird gerade bestätigt
                        }
                        if by_path.contains_key(&lower(&path)) {
                            continue; // belegt (neue lokale Datei gleichen Namens)
                        }
                        match self.local.create_file(&path, &meta, size, doc.updated_at) {
                            Ok(()) => changed = true,
                            Err(err) => pass.errors.push(format!("„{}“ anlegen: {err}", show(&path))),
                        }
                    }
                }
            }
        }

        // Am Server Entferntes lokal wegräumen (tiefste Einträge zuerst)
        let mut stale: Vec<&LocalEntry> = entries
            .iter()
            .filter(|e| e.meta.as_ref().is_some_and(|m| !expected.contains(&m.ident())))
            .collect();
        stale.sort_by_key(|e| std::cmp::Reverse(e.path.len()));
        for e in stale {
            if !e.is_dir && (e.dirty || is_temp(e.name())) {
                continue;
            }
            if self.local.remove(&e.path, e.is_dir).is_ok() {
                changed = true;
            }
        }
        PullStep::Done(changed)
    }
}

enum Replacement {
    None,
    Wait,
    Doc(i64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    // ---- Attrappe Server ----

    #[derive(Default)]
    struct FakeServer {
        colls: BTreeMap<i64, Collection>,
        docs: BTreeMap<i64, (DocumentSummary, Vec<u8>)>,
        next: i64,
        log: Vec<String>,
    }

    #[derive(Default)]
    struct FakeRemote(RefCell<FakeServer>);

    fn summary(id: i64, title: &str, filename: &str, colls: Vec<i64>, version: i64, size: i64) -> DocumentSummary {
        DocumentSummary {
            id,
            title: title.into(),
            original_filename: filename.into(),
            mime: "application/octet-stream".into(),
            kind: "other".into(),
            size,
            created_at: 1000,
            updated_at: 1000 + version,
            document_date: None,
            version,
            extract_status: "done".into(),
            page_count: None,
            has_thumbnail: false,
            collection_ids: colls,
            tag_ids: vec![],
            parent_id: None,
            parent_title: None,
            child_count: 0,
            email_from: None,
            snippet: None,
            score: None,
        }
    }

    impl FakeRemote {
        fn coll(&self, name: &str, parent: Option<i64>) -> i64 {
            let mut s = self.0.borrow_mut();
            s.next += 1;
            let id = s.next;
            s.colls.insert(id, Collection { id, parent_id: parent, name: name.into(), color: None, document_count: 0, ai_locked: false });
            id
        }
        fn doc(&self, title: &str, filename: &str, colls: Vec<i64>, content: &str) -> i64 {
            let mut s = self.0.borrow_mut();
            s.next += 1;
            let id = s.next;
            s.docs.insert(id, (summary(id, title, filename, colls, 1, content.len() as i64), content.as_bytes().to_vec()));
            id
        }
        fn get(&self, id: i64) -> Option<(DocumentSummary, String)> {
            self.0.borrow().docs.get(&id).map(|(d, c)| (d.clone(), String::from_utf8_lossy(c).into()))
        }
        fn log(&self) -> Vec<String> {
            self.0.borrow().log.clone()
        }
    }

    fn api_err(status: u16) -> ApiError {
        ApiError { status: Some(status), message: format!("HTTP {status}") }
    }

    impl Remote for FakeRemote {
        fn collections(&self) -> ApiResult<Vec<Collection>> {
            Ok(self.0.borrow().colls.values().cloned().collect())
        }
        fn all_documents(&self) -> ApiResult<Vec<DocumentSummary>> {
            Ok(self.0.borrow().docs.values().map(|d| d.0.clone()).collect())
        }
        fn upload(&self, file: &Path, filename: &str, title: &str, colls: &[i64]) -> ApiResult<DocumentSummary> {
            let content = std::fs::read(file).unwrap();
            let mut s = self.0.borrow_mut();
            s.next += 1;
            let id = s.next;
            let d = summary(id, title, filename, colls.to_vec(), 1, content.len() as i64);
            s.docs.insert(id, (d.clone(), content));
            s.log.push(format!("upload {filename}"));
            Ok(d)
        }
        fn new_version(&self, id: i64, file: &Path, _filename: &str, basis: Option<i64>) -> ApiResult<DocumentSummary> {
            let content = std::fs::read(file).unwrap();
            let mut s = self.0.borrow_mut();
            let (d, c) = s.docs.get_mut(&id).ok_or(api_err(404))?;
            if basis.is_some_and(|b| b != d.version) {
                return Err(api_err(409));
            }
            d.version += 1;
            d.updated_at += 1;
            d.size = content.len() as i64;
            *c = content;
            let d = d.clone();
            s.log.push(format!("version {id}"));
            Ok(d)
        }
        fn update_document(&self, id: i64, u: &DocumentUpdate) -> ApiResult<DocumentSummary> {
            let mut s = self.0.borrow_mut();
            let (d, _) = s.docs.get_mut(&id).ok_or(api_err(404))?;
            if let Some(t) = &u.title {
                d.title = t.clone();
            }
            if let Some(c) = &u.collection_ids {
                d.collection_ids = c.clone();
            }
            let d = d.clone();
            s.log.push(format!("update {id}"));
            Ok(d)
        }
        fn delete_document(&self, id: i64) -> ApiResult<()> {
            let mut s = self.0.borrow_mut();
            s.docs.remove(&id).ok_or(api_err(404))?;
            s.log.push(format!("delete {id}"));
            Ok(())
        }
        fn create_collection(&self, name: &str, parent: Option<i64>) -> ApiResult<Collection> {
            if self.0.borrow().colls.values().any(|c| c.parent_id == parent && c.name == name) {
                return Err(api_err(409));
            }
            let id = self.coll(name, parent);
            self.0.borrow_mut().log.push(format!("mkcoll {name}"));
            Ok(self.0.borrow().colls[&id].clone())
        }
        fn update_collection(&self, id: i64, input: &CollectionInput) -> ApiResult<Collection> {
            let mut s = self.0.borrow_mut();
            let c = s.colls.get_mut(&id).ok_or(api_err(404))?;
            if let Some(n) = &input.name {
                c.name = n.clone();
            }
            if let Some(p) = input.parent_id {
                c.parent_id = (p > 0).then_some(p);
            }
            let c = c.clone();
            s.log.push(format!("updcoll {id}"));
            Ok(c)
        }
        fn delete_collection(&self, id: i64) -> ApiResult<()> {
            let mut s = self.0.borrow_mut();
            s.colls.remove(&id).ok_or(api_err(404))?;
            for (d, _) in s.docs.values_mut() {
                d.collection_ids.retain(|c| *c != id);
            }
            s.log.push(format!("rmcoll {id}"));
            Ok(())
        }
    }

    // ---- Attrappe lokaler Ordner ----

    #[derive(Clone, Debug)]
    struct Node {
        path: RelPath,
        is_dir: bool,
        meta: Option<Meta>,
        /// `None`: Platzhalter ohne geladenen Inhalt
        content: Option<String>,
        dirty: bool,
        stamp: u64,
    }

    struct FakeLocal {
        nodes: RefCell<BTreeMap<Vec<String>, Node>>,
        tmp: PathBuf,
        counter: RefCell<u64>,
    }

    impl FakeLocal {
        fn new() -> Self {
            let tmp = std::env::temp_dir().join(format!("buero-sync-test-{}", rand_id()));
            std::fs::create_dir_all(&tmp).unwrap();
            FakeLocal { nodes: RefCell::new(BTreeMap::new()), tmp, counter: RefCell::new(0) }
        }
        fn p(s: &str) -> RelPath {
            s.split('/').map(String::from).collect()
        }
        fn tick(&self) -> u64 {
            *self.counter.borrow_mut() += 1;
            *self.counter.borrow()
        }
        /// Benutzer legt Datei an / schreibt hinein
        fn write(&self, path: &str, content: &str) {
            let p = Self::p(path);
            let st = self.tick();
            let mut n = self.nodes.borrow_mut();
            let node = n.entry(lower(&p)).or_insert(Node { path: p.clone(), is_dir: false, meta: None, content: None, dirty: true, stamp: 0 });
            node.content = Some(content.into());
            node.dirty = true;
            node.stamp = st;
        }
        fn mkdir(&self, path: &str) {
            let p = Self::p(path);
            self.nodes.borrow_mut().insert(lower(&p), Node { path: p, is_dir: true, meta: None, content: None, dirty: false, stamp: 0 });
        }
        fn user_rename(&self, from: &str, to: &str) {
            self.rename(&Self::p(from), &Self::p(to)).unwrap();
        }
        fn user_delete(&self, path: &str) {
            let p = lower(&Self::p(path));
            self.nodes.borrow_mut().retain(|k, _| !k.starts_with(&p));
        }
        fn has(&self, path: &str) -> bool {
            self.nodes.borrow().get(&lower(&Self::p(path))).is_some_and(|n| n.path == Self::p(path))
        }
        fn node(&self, path: &str) -> Node {
            self.nodes.borrow().get(&lower(&Self::p(path))).cloned().unwrap()
        }
        fn paths(&self) -> Vec<String> {
            self.nodes.borrow().values().map(|n| n.path.join("/")).collect()
        }
    }

    fn rand_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::process::id() as u64 * 1000 + N.fetch_add(1, Ordering::Relaxed)
    }

    fn io_err(msg: &str) -> std::io::Error {
        std::io::Error::other(msg.to_string())
    }

    impl LocalFs for FakeLocal {
        fn scan(&self) -> std::io::Result<Vec<LocalEntry>> {
            Ok(self
                .nodes
                .borrow()
                .values()
                .map(|n| LocalEntry { path: n.path.clone(), is_dir: n.is_dir, meta: n.meta.clone(), dirty: n.dirty })
                .collect())
        }
        fn create_dir(&self, path: &[String], meta: &Meta) -> std::io::Result<()> {
            let mut n = self.nodes.borrow_mut();
            if n.contains_key(&lower(path)) {
                return Err(io_err("exists"));
            }
            n.insert(lower(path), Node { path: path.to_vec(), is_dir: true, meta: Some(meta.clone()), content: None, dirty: false, stamp: 0 });
            Ok(())
        }
        fn create_file(&self, path: &[String], meta: &Meta, _size: u64, _mtime: i64) -> std::io::Result<()> {
            let mut n = self.nodes.borrow_mut();
            if n.contains_key(&lower(path)) {
                return Err(io_err("exists"));
            }
            n.insert(lower(path), Node { path: path.to_vec(), is_dir: false, meta: Some(meta.clone()), content: None, dirty: false, stamp: 0 });
            Ok(())
        }
        fn replace_file(&self, path: &[String], meta: &Meta, _size: u64, _mtime: i64) -> std::io::Result<()> {
            let mut n = self.nodes.borrow_mut();
            let node = n.get_mut(&lower(path)).ok_or(io_err("missing"))?;
            node.meta = Some(meta.clone());
            node.content = None;
            node.dirty = false;
            Ok(())
        }
        fn snapshot(&self, path: &[String]) -> std::io::Result<(PathBuf, u64)> {
            let n = self.nodes.borrow();
            let node = n.get(&lower(path)).ok_or(io_err("missing"))?;
            let file = self.tmp.join(format!("snap-{}", rand_id()));
            std::fs::write(&file, node.content.clone().unwrap_or_default())?;
            Ok((file, node.stamp))
        }
        fn mark_synced(&self, path: &[String], meta: &Meta, stamp: Option<u64>) -> std::io::Result<bool> {
            let mut n = self.nodes.borrow_mut();
            let node = n.get_mut(&lower(path)).ok_or(io_err("missing"))?;
            node.meta = Some(meta.clone());
            if stamp.is_none() || stamp == Some(node.stamp) {
                node.dirty = false;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        fn rename(&self, from: &[String], to: &[String]) -> std::io::Result<()> {
            let mut n = self.nodes.borrow_mut();
            let lf = lower(from);
            let lt = lower(to);
            if lf != lt && n.contains_key(&lt) {
                return Err(io_err("target exists"));
            }
            let moved: Vec<(Vec<String>, Node)> = n.iter().filter(|(k, _)| k.starts_with(&lf)).map(|(k, v)| (k.clone(), v.clone())).collect();
            if moved.is_empty() {
                return Err(io_err("missing"));
            }
            for (k, mut v) in moved {
                n.remove(&k);
                let mut np = to.to_vec();
                np.extend(v.path[from.len()..].iter().cloned());
                v.path = np;
                n.insert(lower(&v.path), v);
            }
            Ok(())
        }
        fn remove(&self, path: &[String], is_dir: bool) -> std::io::Result<()> {
            let mut n = self.nodes.borrow_mut();
            let lp = lower(path);
            if is_dir && n.keys().any(|k| k.len() > lp.len() && k.starts_with(&lp)) {
                return Err(io_err("not empty"));
            }
            n.remove(&lp).map(|_| ()).ok_or(io_err("missing"))
        }
    }

    fn setup() -> (Engine<FakeRemote, FakeLocal>, i64, i64) {
        let r = FakeRemote::default();
        let steuer = r.coll("Steuer", None);
        let doc = r.doc("Bescheid 2024", "bescheid.pdf", vec![steuer], "v1");
        r.doc("Zettel", "zettel.txt", vec![], "inbox");
        let mut e = Engine::new(r, FakeLocal::new(), None);
        let p = e.reconcile().unwrap();
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        (e, steuer, doc)
    }

    #[test]
    fn initial_pull_creates_placeholders() {
        let (e, steuer, doc) = setup();
        let paths = e.local.paths();
        assert!(e.local.has("Eingang"), "{paths:?}");
        assert!(e.local.has("Eingang/Zettel.txt"), "{paths:?}");
        assert!(e.local.has("Steuer"));
        assert!(e.local.has("Steuer/Bescheid 2024.pdf"));
        assert_eq!(e.local.node("Steuer").meta, Some(Meta::Coll { id: steuer, name: "Steuer".into(), parent: None }));
        assert_eq!(
            e.local.node("Steuer/Bescheid 2024.pdf").meta,
            Some(Meta::Doc { id: doc, dir: steuer, version: 1, name: "Bescheid 2024.pdf".into() })
        );
        assert!(e.remote.log().is_empty(), "{:?}", e.remote.log());
    }

    #[test]
    fn new_local_file_and_edit() {
        let (mut e, steuer, _) = setup();
        e.local.write("Steuer/Beleg.txt", "hallo");
        e.reconcile().unwrap();
        let Some(Meta::Doc { id, .. }) = e.local.node("Steuer/Beleg.txt").meta else { panic!() };
        let (d, c) = e.remote.get(id).unwrap();
        assert_eq!((d.title.as_str(), d.collection_ids.clone(), c.as_str()), ("Beleg", vec![steuer], "hallo"));
        assert!(!e.local.node("Steuer/Beleg.txt").dirty);

        // Ändern → neue Version, kein neues Dokument
        e.local.write("Steuer/Beleg.txt", "neu");
        e.reconcile().unwrap();
        let (d, c) = e.remote.get(id).unwrap();
        assert_eq!((d.version, c.as_str()), (2, "neu"));
        assert_eq!(e.remote.log().iter().filter(|l| l.starts_with("upload")).count(), 1);
        // Ruhiger Durchlauf ändert nichts
        let before = e.remote.log().len();
        let p = e.reconcile().unwrap();
        assert_eq!(e.remote.log().len(), before);
        assert!(!p.local_changed && !p.remote_changed);
    }

    #[test]
    fn file_in_root_goes_to_inbox() {
        let (mut e, _, _) = setup();
        e.local.write("Notiz.txt", "x");
        e.reconcile().unwrap();
        assert!(!e.local.has("Notiz.txt"));
        let Some(Meta::Doc { id, dir: 0, .. }) = e.local.node("Eingang/Notiz.txt").meta else { panic!() };
        assert!(e.remote.get(id).unwrap().0.collection_ids.is_empty());
    }

    #[test]
    fn rename_and_move() {
        let (mut e, steuer, doc) = setup();
        let privat = e.remote.coll("Privat", None);
        e.reconcile().unwrap();
        e.local.user_rename("Steuer/Bescheid 2024.pdf", "Steuer/Bescheid ESt 2024.pdf");
        e.reconcile().unwrap();
        assert_eq!(e.remote.get(doc).unwrap().0.title, "Bescheid ESt 2024");
        assert!(e.local.has("Steuer/Bescheid ESt 2024.pdf"));

        e.local.user_rename("Steuer/Bescheid ESt 2024.pdf", "Privat/Bescheid ESt 2024.pdf");
        e.reconcile().unwrap();
        assert_eq!(e.remote.get(doc).unwrap().0.collection_ids, vec![privat]);
        assert!(e.local.has("Privat/Bescheid ESt 2024.pdf"));
        assert!(!e.local.has("Steuer/Bescheid ESt 2024.pdf"));
        let _ = steuer;
    }

    #[test]
    fn delete_needs_confirmation() {
        let (mut e, steuer, doc) = setup();
        e.local.user_delete("Steuer/Bescheid 2024.pdf");
        let p = e.reconcile().unwrap();
        assert!(p.pending);
        // Noch nicht gelöscht, und auch nicht neu angelegt
        assert_eq!(e.remote.get(doc).unwrap().0.collection_ids, vec![steuer]);
        assert!(!e.local.has("Steuer/Bescheid 2024.pdf"));
        e.reconcile().unwrap();
        // Aus der Sammlung genommen (Dokument bleibt, landet im Eingang)
        assert!(e.remote.get(doc).unwrap().0.collection_ids.is_empty());
        e.reconcile().unwrap();
        assert!(e.local.has("Eingang/Bescheid 2024.pdf"), "{:?}", e.local.paths());

        // Im Eingang löschen = Dokument löschen
        e.local.user_delete("Eingang/Bescheid 2024.pdf");
        e.reconcile().unwrap();
        e.reconcile().unwrap();
        assert!(e.remote.get(doc).is_none());
    }

    #[test]
    fn mass_deletion_is_blocked() {
        let r = FakeRemote::default();
        let c = r.coll("Viel", None);
        for i in 0..40 {
            r.doc(&format!("Dok {i}"), "a.txt", vec![c], "x");
        }
        let mut e = Engine::new(r, FakeLocal::new(), None);
        e.reconcile().unwrap();
        e.local.user_delete("Viel");
        e.local.user_delete("Eingang");
        let p = e.reconcile().unwrap();
        assert!(!p.errors.is_empty());
        // Nichts am Server gelöscht; lokal wird alles wiederhergestellt
        for _ in 0..3 {
            e.reconcile().unwrap();
        }
        assert_eq!(e.remote.0.borrow().docs.len(), 40);
        assert!(e.remote.0.borrow().colls.contains_key(&c));
        assert!(e.local.has("Viel/Dok 7.txt"), "{:?}", e.local.paths());
        assert!(e.local.has("Eingang"));
    }

    #[test]
    fn office_save_pattern_makes_new_version() {
        let (mut e, _, doc) = setup();
        // Word: neue Fassung in Temp-Datei, Original wegbenennen, Temp umbenennen, Original löschen
        e.local.write("Steuer/~WRL0001.tmp", "v2");
        e.local.user_rename("Steuer/Bescheid 2024.pdf", "Steuer/~WRL0002.tmp");
        e.local.user_rename("Steuer/~WRL0001.tmp", "Steuer/Bescheid 2024.pdf");
        // Durchlauf mitten im Speichern: warten
        let p = e.reconcile().unwrap();
        assert!(p.pending);
        assert!(e.remote.log().is_empty(), "{:?}", e.remote.log());
        e.local.user_delete("Steuer/~WRL0002.tmp");
        e.reconcile().unwrap();
        e.reconcile().unwrap();
        let (d, c) = e.remote.get(doc).unwrap();
        assert_eq!((d.version, c.as_str()), (2, "v2"));
        assert!(e.remote.log().iter().all(|l| !l.starts_with("upload") && !l.starts_with("update")), "{:?}", e.remote.log());
        assert_eq!(e.local.node("Steuer/Bescheid 2024.pdf").meta.unwrap().ident(), Ident::Doc(doc, e.local.node("Steuer").meta.unwrap().ident_coll()));
    }

    #[test]
    fn libreoffice_save_pattern() {
        let (mut e, _, doc) = setup();
        // LibreOffice: Temp-Datei schreiben, Original löschen, Temp umbenennen
        e.local.write("Steuer/lu1234.tmp", "v2");
        e.local.user_delete("Steuer/Bescheid 2024.pdf");
        e.local.user_rename("Steuer/lu1234.tmp", "Steuer/Bescheid 2024.pdf");
        e.reconcile().unwrap();
        e.reconcile().unwrap();
        let (d, c) = e.remote.get(doc).unwrap();
        assert_eq!((d.version, c.as_str(), d.collection_ids.len()), (2, "v2", 1));
        assert!(e.remote.log().iter().all(|l| !l.starts_with("upload")), "{:?}", e.remote.log());
    }

    #[test]
    fn document_in_two_collections() {
        let r = FakeRemote::default();
        let a = r.coll("A", None);
        let b = r.coll("B", None);
        let d = r.doc("Vertrag", "v.pdf", vec![a, b], "x");
        let mut e = Engine::new(r, FakeLocal::new(), None);
        e.reconcile().unwrap();
        assert!(e.local.has("A/Vertrag.pdf") && e.local.has("B/Vertrag.pdf"));
        e.local.user_delete("B/Vertrag.pdf");
        e.reconcile().unwrap();
        e.reconcile().unwrap();
        assert_eq!(e.remote.get(d).unwrap().0.collection_ids, vec![a]);
        assert!(e.local.has("A/Vertrag.pdf"));
        assert!(!e.local.has("B/Vertrag.pdf"));
    }

    impl Meta {
        fn ident_coll(&self) -> i64 {
            match self {
                Meta::Coll { id, .. } => *id,
                _ => 0,
            }
        }
    }

    #[test]
    fn server_changes_arrive() {
        let (mut e, steuer, doc) = setup();
        // Neue Version am Server: Platzhalter wird erneuert
        e.local.nodes.borrow_mut().get_mut(&lower(&FakeLocal::p("steuer/bescheid 2024.pdf"))).unwrap().content = Some("v1".into());
        let f = std::env::temp_dir().join(format!("v2-{}", rand_id()));
        std::fs::write(&f, "v2").unwrap();
        e.remote.new_version(doc, &f, "bescheid.pdf", None).unwrap();
        e.reconcile().unwrap();
        let n = e.local.node("Steuer/Bescheid 2024.pdf");
        assert!(n.content.is_none());
        assert!(matches!(n.meta, Some(Meta::Doc { version: 2, .. })));

        // Umbenannt am Server
        e.remote.update_document(doc, &DocumentUpdate { title: Some("Neu".into()), ..Default::default() }).unwrap();
        e.reconcile().unwrap();
        assert!(e.local.has("Steuer/Neu.pdf"), "{:?}", e.local.paths());
        assert!(!e.local.has("Steuer/Bescheid 2024.pdf"));

        // Sammlung umbenannt
        e.remote.update_collection(steuer, &CollectionInput { name: Some("Steuern".into()), ..Default::default() }).unwrap();
        e.reconcile().unwrap();
        assert!(e.local.has("Steuern/Neu.pdf"), "{:?}", e.local.paths());

        // Gelöscht am Server
        e.remote.delete_document(doc).unwrap();
        e.reconcile().unwrap();
        assert!(!e.local.has("Steuern/Neu.pdf"));
        // Nichts davon wurde zurück zum Server übertragen
        assert!(e.remote.log().iter().all(|l| !l.starts_with("upload")), "{:?}", e.remote.log());
    }

    #[test]
    fn folders() {
        let (mut e, steuer, _) = setup();
        e.local.mkdir("Steuer/2025");
        e.local.write("Steuer/2025/Quittung.txt", "q");
        e.reconcile().unwrap();
        let Some(Meta::Coll { id: y, parent: Some(p), .. }) = e.local.node("Steuer/2025").meta else { panic!() };
        assert_eq!(p, steuer);
        let Some(Meta::Doc { id: q, .. }) = e.local.node("Steuer/2025/Quittung.txt").meta else { panic!() };
        assert_eq!(e.remote.get(q).unwrap().0.collection_ids, vec![y]);

        e.local.user_rename("Steuer/2025", "Steuer/Jahr 2025");
        e.reconcile().unwrap();
        assert_eq!(e.remote.0.borrow().colls[&y].name, "Jahr 2025");
        assert!(e.local.has("Steuer/Jahr 2025/Quittung.txt"));

        // Ordner löschen: Dokument aus der Sammlung, Sammlung weg
        e.local.user_delete("Steuer/Jahr 2025");
        e.reconcile().unwrap();
        e.reconcile().unwrap();
        assert!(!e.remote.0.borrow().colls.contains_key(&y));
        assert!(e.remote.get(q).is_some());
    }

    #[test]
    fn conflict_keeps_both() {
        let (mut e, _, doc) = setup();
        e.local.write("Steuer/Bescheid 2024.pdf", "lokal");
        // Gleichzeitig am Server geändert
        let f = std::env::temp_dir().join(format!("srv-{}", rand_id()));
        std::fs::write(&f, "server").unwrap();
        e.remote.new_version(doc, &f, "bescheid.pdf", None).unwrap();
        let p = e.reconcile().unwrap();
        assert!(p.errors.iter().any(|m| m.contains("Konflikt")), "{:?}", p.errors);
        e.reconcile().unwrap();
        assert_eq!(e.remote.get(doc).unwrap().1, "server");
        let paths = e.local.paths();
        assert!(paths.iter().any(|p| p.contains("Konflikt")), "{paths:?}");
        assert!(e.local.has("Steuer/Bescheid 2024.pdf"), "{paths:?}");
    }

    #[test]
    fn index_survives_restart() {
        let dir = std::env::temp_dir().join(format!("buero-sync-idx-{}", rand_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("index.json");
        let r = FakeRemote::default();
        let c = r.coll("A", None);
        let d = r.doc("x", "x.txt", vec![c], "1");
        let mut e = Engine::new(r, FakeLocal::new(), Some(state.clone()));
        e.reconcile().unwrap();
        let Engine { remote, local, .. } = e;
        // Neustart; inzwischen lokal gelöscht
        local.user_delete("A/x.txt");
        let mut e = Engine::new(remote, local, Some(state));
        e.reconcile().unwrap();
        assert!(!e.local.has("A/x.txt"), "darf nicht neu angelegt werden");
        e.reconcile().unwrap();
        assert!(e.remote.get(d).unwrap().0.collection_ids.is_empty());
    }
}
