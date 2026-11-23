// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Aktivitäten und Fehler eines Sync-Ordners: für das Aktivitätsfenster (die
//! letzten Einträge im Speicher) und als Protokolldatei auf der Platte.
//!
//! Fehler, die bei jedem Durchlauf wieder auftreten, werden nur einmal
//! protokolliert – beim ersten Auftreten und wenn sie behoben sind.

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::sync::{Act, ActKind, Pass};

/// So viele Einträge bleiben im Speicher
const KEEP: usize = 500;
/// Ab dieser Größe wird die Protokolldatei zu `….1` und neu begonnen
const MAX_FILE: u64 = 2 * 1024 * 1024;
/// Mehr Aktivitäten in einem Durchlauf werden zusammengefasst (z. B. beim
/// ersten Abgleich mit Tausenden Dateien)
const MAX_DETAIL: usize = 50;

#[derive(Clone, Debug, Serialize)]
pub struct ActivityEntry {
    /// Unix-Zeit in Sekunden
    pub time: i64,
    pub kind: ActKind,
    pub path: String,
    pub text: String,
}

/// Zustand eines Sync-Ordners für die Anzeige
#[derive(Clone, Debug, Default, Serialize)]
pub struct SyncState {
    /// Gerade wird abgeglichen
    pub running: bool,
    /// Ende des letzten erfolgreichen Abgleichs (Unix-Zeit)
    pub last_sync: Option<i64>,
    /// Es steht noch etwas aus (wird gleich erneut abgeglichen)
    pub pending: bool,
    /// Aktuelle Probleme
    pub errors: Vec<String>,
    /// Letzte Aktivitäten, neueste zuerst
    pub entries: Vec<ActivityEntry>,
}

pub struct ActivityLog {
    /// Name des Ordners in der Protokolldatei
    name: String,
    file: Option<PathBuf>,
    entries: VecDeque<ActivityEntry>,
    running: bool,
    last_sync: Option<i64>,
    pending: bool,
    errors: Vec<String>,
}

pub type SharedLog = Arc<Mutex<ActivityLog>>;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `2026-11-23 21:07:29` (UTC) ohne zusätzliche Abhängigkeit
fn utc(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // Kalenderdatum aus Tagen seit 1970 (H. Hinnant, „civil_from_days“)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn label(k: ActKind) -> &'static str {
    match k {
        ActKind::Upload => "Hochgeladen ",
        ActKind::Download => "Vom Server  ",
        ActKind::Change => "Geändert    ",
        ActKind::Remove => "Entfernt    ",
        ActKind::Error => "FEHLER      ",
    }
}

impl ActivityLog {
    pub fn new(name: impl Into<String>, file: Option<PathBuf>) -> SharedLog {
        Arc::new(Mutex::new(ActivityLog {
            name: name.into(),
            file,
            entries: VecDeque::new(),
            running: false,
            last_sync: None,
            pending: false,
            errors: Vec::new(),
        }))
    }

    pub fn add(&mut self, kind: ActKind, path: impl Into<String>, text: impl Into<String>) {
        let e = ActivityEntry { time: now(), kind, path: path.into(), text: text.into() };
        self.write(&e);
        if self.entries.len() >= KEEP {
            self.entries.pop_back();
        }
        self.entries.push_front(e);
    }

    fn write(&self, e: &ActivityEntry) {
        let Some(file) = &self.file else { return };
        if std::fs::metadata(file).is_ok_and(|m| m.len() > MAX_FILE) {
            let _ = std::fs::rename(file, file.with_extension("log.1"));
        }
        let what = if e.path.is_empty() { e.text.clone() } else { format!("{}: {}", e.path, e.text) };
        let line = format!("{} UTC  {} [{}] {what}\r\n", utc(e.time), label(e.kind), self.name);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Ein Abgleich beginnt
    pub fn started(&mut self) {
        self.running = true;
    }

    /// Ein Abgleich ist fertig
    pub fn finished(&mut self, res: &Result<Pass, String>) {
        self.running = false;
        match res {
            Ok(p) => {
                self.record(&p.activity);
                self.set_errors(p.errors.clone());
                self.last_sync = Some(now());
                self.pending = p.pending;
            }
            Err(e) => self.set_errors(vec![e.clone()]),
        }
    }

    fn record(&mut self, acts: &[Act]) {
        if acts.len() <= MAX_DETAIL {
            for a in acts {
                self.add(a.kind, a.path.clone(), a.text.clone());
            }
            return;
        }
        // Zusammenfassen: je Art und Text eine Zeile mit Anzahl
        let mut groups: Vec<((ActKind, &str), usize)> = Vec::new();
        let mut pos: HashMap<(ActKind, &str), usize> = HashMap::new();
        for a in acts {
            let key = (a.kind, a.text.as_str());
            match pos.get(&key) {
                Some(&i) => groups[i].1 += 1,
                None => {
                    pos.insert(key, groups.len());
                    groups.push((key, 1));
                }
            }
        }
        for ((kind, text), n) in groups {
            let t = if n == 1 { text.to_string() } else { format!("{text} ({n} Einträge)") };
            self.add(kind, "", t);
        }
    }

    /// Neue Probleme protokollieren, behobene vermerken
    fn set_errors(&mut self, errors: Vec<String>) {
        for e in &errors {
            if !self.errors.contains(e) {
                self.add(ActKind::Error, "", e.clone());
            }
        }
        if !self.errors.is_empty() && errors.is_empty() {
            self.write(&ActivityEntry { time: now(), kind: ActKind::Change, path: String::new(), text: "Probleme behoben".into() });
        }
        self.errors = errors;
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn state(&self) -> SyncState {
        SyncState {
            running: self.running,
            last_sync: self.last_sync,
            pending: self.pending,
            errors: self.errors.clone(),
            entries: self.entries.iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(acts: Vec<Act>, errors: Vec<&str>) -> Result<Pass, String> {
        Ok(Pass { activity: acts, errors: errors.into_iter().map(String::from).collect(), ..Default::default() })
    }

    fn act(kind: ActKind, path: &str, text: &str) -> Act {
        Act { kind, path: path.into(), text: text.into() }
    }

    #[test]
    fn utc_format() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(1_795_461_249), "2026-11-23 19:14:09");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
    }

    #[test]
    fn errors_only_once() {
        let log = ActivityLog::new("Test", None);
        let mut l = log.lock().unwrap();
        l.finished(&pass(vec![], vec!["kaputt"]));
        l.finished(&pass(vec![], vec!["kaputt"]));
        l.finished(&pass(vec![], vec!["kaputt", "auch kaputt"]));
        let s = l.state();
        assert_eq!(s.errors.len(), 2);
        let logged: Vec<&str> = s.entries.iter().filter(|e| e.kind == ActKind::Error).map(|e| e.text.as_str()).collect();
        assert_eq!(logged, ["auch kaputt", "kaputt"]);
        l.finished(&pass(vec![], vec![]));
        assert!(l.state().errors.is_empty());
        assert!(l.state().last_sync.is_some());
    }

    #[test]
    fn many_activities_are_summarised() {
        let log = ActivityLog::new("Test", None);
        let mut l = log.lock().unwrap();
        let mut acts: Vec<Act> = (0..120).map(|i| act(ActKind::Download, &format!("a\\{i}.pdf"), "Neu vom Server")).collect();
        acts.push(act(ActKind::Upload, "b.pdf", "Hochgeladen"));
        l.finished(&pass(acts, vec![]));
        let s = l.state();
        assert_eq!(s.entries.len(), 2);
        assert!(s.entries.iter().any(|e| e.text == "Neu vom Server (120 Einträge)"));
        assert!(s.entries.iter().any(|e| e.text == "Hochgeladen"));
    }

    #[test]
    fn keeps_newest() {
        let log = ActivityLog::new("Test", None);
        let mut l = log.lock().unwrap();
        for i in 0..(KEEP + 10) {
            l.add(ActKind::Upload, format!("{i}"), "Hochgeladen");
        }
        let s = l.state();
        assert_eq!(s.entries.len(), KEEP);
        assert_eq!(s.entries[0].path, format!("{}", KEEP + 9));
    }

    #[test]
    fn writes_file() {
        let dir = std::env::temp_dir().join(format!("buero-activity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sync.log");
        let log = ActivityLog::new("Steuer", Some(file.clone()));
        log.lock().unwrap().finished(&pass(vec![act(ActKind::Upload, "x.pdf", "Hochgeladen")], vec!["kaputt"]));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("[Steuer] x.pdf: Hochgeladen"));
        assert!(text.contains("FEHLER"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
