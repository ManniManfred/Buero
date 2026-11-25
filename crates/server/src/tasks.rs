// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Übersicht der laufenden Hintergrundaufgaben (nur im Speicher).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use shared::BackgroundTask;

type Live = Box<dyn Fn() -> Option<String> + Send>;

struct Entry {
    task: BackgroundTask,
    /// Liefert den aktuellen Schritt beim Abfragen (z. B. Mail-Fortschritt)
    live: Option<Live>,
}

static TASKS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Hält eine Aufgabe in der Liste, bis er fallen gelassen wird.
pub struct Task(u64);

/// Aufgabe eintragen; sie verschwindet, wenn der Rückgabewert endet.
pub fn start(kind: &str, title: impl Into<String>, phase: &str, document_id: Option<i64>) -> Task {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    TASKS.lock().unwrap().push(Entry {
        task: BackgroundTask {
            id,
            kind: kind.into(),
            title: title.into(),
            phase: phase.into(),
            started_at: crate::util::now(),
            document_id,
        },
        live: None,
    });
    Task(id)
}

impl Task {
    /// Aktuellen Schritt anzeigen
    pub fn phase(&self, phase: impl Into<String>) {
        if let Some(e) = TASKS.lock().unwrap().iter_mut().find(|e| e.task.id == self.0) {
            e.task.phase = phase.into();
        }
    }

    /// Titel und zugehöriges Dokument ändern (Aufgaben über mehrere Dokumente)
    pub fn retitle(&self, title: impl Into<String>, document_id: Option<i64>) {
        if let Some(e) = TASKS.lock().unwrap().iter_mut().find(|e| e.task.id == self.0) {
            e.task.title = title.into();
            e.task.document_id = document_id;
        }
    }

    /// Schritt bei jeder Abfrage neu ermitteln (`None`: letzter Stand bleibt)
    pub fn live(self, f: impl Fn() -> Option<String> + Send + 'static) -> Self {
        if let Some(e) = TASKS.lock().unwrap().iter_mut().find(|e| e.task.id == self.0) {
            e.live = Some(Box::new(f));
        }
        self
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        TASKS.lock().unwrap().retain(|e| e.task.id != self.0);
    }
}

/// Steht schon eine Aufgabe zu diesem Dokument in der Liste?
pub fn has_document(id: i64) -> bool {
    TASKS.lock().unwrap().iter().any(|e| e.task.document_id == Some(id))
}

/// Alle laufenden Aufgaben, älteste zuerst
pub fn list() -> Vec<BackgroundTask> {
    let mut all = TASKS.lock().unwrap();
    all.iter_mut()
        .map(|e| {
            if let Some(p) = e.live.as_ref().and_then(|f| f()) {
                e.task.phase = p;
            }
            e.task.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle() {
        let t = start("index", "Suchindex", "Aufbauen", None);
        t.phase("Schreiben");
        let id = t.0;
        assert!(list().iter().any(|x| x.id == id && x.phase == "Schreiben"));
        let t = t.live(|| Some("live".into()));
        assert!(list().iter().any(|x| x.id == id && x.phase == "live"));
        drop(t);
        assert!(!list().iter().any(|x| x.id == id));
    }
}
