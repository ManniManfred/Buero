// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Browser-Verlauf für die Navigation innerhalb der Oberfläche: Die Ansicht
//! steht in der Adresse (`#/sammlung/12/dokument/345?q=…`), jeder Wechsel legt
//! einen Verlaufseintrag an, „Zurück“ / „Vor“ stellt die Ansicht wieder her.
//! Lesezeichen und Neuladen landen an derselben Stelle.
//!
//! Aufbau der Adresse (Teile in dieser Reihenfolge, alle optional):
//! `#/uebersicht` · `/eingang` · `/sammlung/<id>` · `/dokument/<id>` ·
//! `/text/<id>` · `/office/<id>` · `/tupfer/<id>` · `/einstellungen` · `/scannen`,
//! dahinter `?q=…&tags=1,2&art=pdf&sort=…&unter=1`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::state::Ctx;

#[derive(Clone, Debug, Default, PartialEq)]
struct Route {
    dashboard: bool,
    // Liste (in der Übersicht nicht Teil der Adresse)
    unfiled: bool,
    collection: Option<i64>,
    include_sub: bool,
    query: String,
    tags: Vec<i64>,
    kind: String,
    sort: String,
    // Ebenen darüber
    doc: Option<i64>,
    text: Option<i64>,
    office: Option<i64>,
    tupfer: Option<i64>,
    settings: bool,
    scan: bool,
}

impl Route {
    /// Aktuelle Ansicht (liest die Signale reaktiv)
    fn current(c: &Ctx) -> Self {
        let dashboard = c.dashboard.get();
        let mut r = Route {
            dashboard,
            doc: c.selected.get(),
            text: c.text_doc.get(),
            office: c.office_doc.get(),
            tupfer: c.tupfer_doc.get(),
            settings: c.show_settings.get(),
            scan: c.scan.get(),
            ..Default::default()
        };
        if !dashboard {
            r.unfiled = c.unfiled.get();
            r.collection = c.collection.get();
            r.include_sub = r.collection.is_some() && c.include_sub.get();
            r.query = c.query.get().trim().to_string();
            r.tags = c.tag_filter.get();
            r.kind = c.kind.get();
            r.sort = c.sort.get();
        }
        r
    }

    /// Ohne die Ebenen (Detailansicht, Editoren, Einstellungen, Scanner)
    fn base(&self) -> Self {
        Route { doc: None, text: None, office: None, tupfer: None, settings: false, scan: false, ..self.clone() }
    }

    /// `self` entsteht aus `old` nur durch Schließen von Ebenen
    fn closes_layers_of(&self, old: &Route) -> bool {
        fn sub<T: PartialEq>(new: &Option<T>, old: &Option<T>) -> bool {
            new.is_none() || new == old
        }
        self != old
            && self.base() == old.base()
            && sub(&self.doc, &old.doc)
            && sub(&self.text, &old.text)
            && sub(&self.office, &old.office)
            && sub(&self.tupfer, &old.tupfer)
            && (!self.settings || old.settings)
            && (!self.scan || old.scan)
    }

    /// Schließt eine Ebene, die vor dem Schließen nachfragen kann
    fn closes_guarded_layer_of(&self, old: &Route) -> bool {
        (old.text.is_some() && self.text != old.text)
            || (old.office.is_some() && self.office != old.office)
            || (old.tupfer.is_some() && self.tupfer != old.tupfer)
            || (old.scan && !self.scan)
    }

    fn to_hash(&self) -> String {
        let mut h = String::from("#/");
        let mut seg = |s: String| {
            if h.len() > 2 {
                h.push('/');
            }
            h.push_str(&s);
        };
        if self.dashboard {
            seg("uebersicht".into());
        }
        if self.unfiled {
            seg("eingang".into());
        }
        let ids = [
            ("sammlung", self.collection),
            ("dokument", self.doc),
            ("text", self.text),
            ("office", self.office),
            ("tupfer", self.tupfer),
        ];
        for (name, id) in ids {
            if let Some(id) = id {
                seg(format!("{name}/{id}"));
            }
        }
        if self.settings {
            seg("einstellungen".into());
        }
        if self.scan {
            seg("scannen".into());
        }

        let enc = |s: &str| String::from(js_sys::encode_uri_component(s));
        let mut q = Vec::new();
        if !self.query.is_empty() {
            q.push(format!("q={}", enc(&self.query)));
        }
        if !self.tags.is_empty() {
            q.push(format!("tags={}", self.tags.iter().map(i64::to_string).collect::<Vec<_>>().join(",")));
        }
        if !self.kind.is_empty() {
            q.push(format!("art={}", enc(&self.kind)));
        }
        if !self.sort.is_empty() {
            q.push(format!("sort={}", enc(&self.sort)));
        }
        if self.include_sub {
            q.push("unter=1".into());
        }
        if !q.is_empty() {
            h.push('?');
            h.push_str(&q.join("&"));
        }
        h
    }

    fn parse(hash: &str) -> Self {
        let hash = hash.trim_start_matches('#');
        let (path, query) = hash.split_once('?').unwrap_or((hash, ""));
        let mut r = Route::default();
        let mut parts = path.split('/').filter(|s| !s.is_empty());
        while let Some(p) = parts.next() {
            let mut id = || parts.next().and_then(|s| s.parse::<i64>().ok());
            match p {
                "uebersicht" => r.dashboard = true,
                "eingang" => r.unfiled = true,
                "einstellungen" => r.settings = true,
                "scannen" => r.scan = true,
                "sammlung" => r.collection = id(),
                "dokument" => r.doc = id(),
                "text" => r.text = id(),
                "office" => r.office = id(),
                "tupfer" => r.tupfer = id(),
                _ => {}
            }
        }
        let dec = |s: &str| {
            js_sys::decode_uri_component(&s.replace('+', " ")).map(String::from).unwrap_or_else(|_| s.to_string())
        };
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k {
                "q" => r.query = dec(v).trim().to_string(),
                "tags" => r.tags = v.split(',').filter_map(|t| t.parse().ok()).collect(),
                "art" => r.kind = dec(v),
                "sort" => r.sort = dec(v),
                "unter" => r.include_sub = v == "1",
                _ => {}
            }
        }
        if r.dashboard {
            // Die Übersicht hat keine Liste: Filter in der Adresse gelten nicht
            r = Route { dashboard: true, ..r.base_layers_only() };
        }
        if r.collection.is_none() {
            r.include_sub = false;
        }
        r
    }

    /// Nur die Ebenen übernehmen (für die Übersicht)
    fn base_layers_only(&self) -> Self {
        Route {
            doc: self.doc,
            text: self.text,
            office: self.office,
            tupfer: self.tupfer,
            settings: self.settings,
            scan: self.scan,
            ..Default::default()
        }
    }

    /// Ansicht in die Signale übernehmen; nur Geändertes wird gesetzt, damit
    /// nicht unnötig neu gesucht wird.
    fn apply(&self, c: &Ctx) {
        fn set<T: PartialEq + Clone + Send + Sync + 'static>(s: RwSignal<T>, v: &T) {
            if s.with_untracked(|cur| cur != v) {
                s.set(v.clone());
            }
        }
        if !self.dashboard {
            set(c.unfiled, &self.unfiled);
            set(c.collection, &self.collection);
            if self.collection.is_some() {
                set(c.include_sub, &self.include_sub);
            }
            if c.query.with_untracked(|q| q.trim() != self.query) {
                c.query.set(self.query.clone());
            }
            set(c.tag_filter, &self.tags);
            set(c.kind, &self.kind);
            set(c.sort, &self.sort);
        }
        set(c.dashboard, &self.dashboard);
        set(c.selected, &self.doc);
        set(c.text_doc, &self.text);
        set(c.office_doc, &self.office);
        set(c.tupfer_doc, &self.tupfer);
        set(c.show_settings, &self.settings);
        set(c.scan, &self.scan);
    }
}

fn current_hash() -> String {
    window().location().hash().unwrap_or_default()
}

/// Adresse des vorigen Eintrags, im Verlaufseintrag gespeichert (übersteht Neuladen)
fn prev_hash() -> Option<String> {
    let state = window().history().ok()?.state().ok()?;
    js_sys::Reflect::get(&state, &"prev".into()).ok()?.as_string()
}

fn state_with_prev(prev: &str) -> JsValue {
    let o = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&o, &"prev".into(), &prev.into());
    o.into()
}

fn push(hash: &str) {
    let prev = current_hash();
    if let Ok(h) = window().history() {
        let _ = h.push_state_with_url(&state_with_prev(&prev), "", Some(hash));
    }
}

fn replace(hash: &str) {
    let prev = prev_hash().unwrap_or_default();
    if let Ok(h) = window().history() {
        let _ = h.replace_state_with_url(&state_with_prev(&prev), "", Some(hash));
    }
}

/// Ebenen mit Rückfrage (ungespeicherte Änderungen, gescannte Seiten)
type Guard = Rc<dyn Fn()>;

thread_local! {
    static GUARDS: RefCell<Vec<(u32, Guard)>> = const { RefCell::new(Vec::new()) };
    static NEXT_GUARD: Cell<u32> = const { Cell::new(0) };
    /// Erwartete Adresse nach `history.back()`
    static PENDING_BACK: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Für die Lebenszeit der aufrufenden Komponente: „Zurück“ schließt die Ebene
/// nicht direkt, sondern ruft `close` (das ggf. nachfragt und dann schließt).
pub fn guard_back(close: impl Fn() + 'static) {
    let id = NEXT_GUARD.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1));
        id
    });
    GUARDS.with(|g| g.borrow_mut().push((id, Rc::new(close))));
    on_cleanup(move || GUARDS.with(|g| g.borrow_mut().retain(|(i, _)| *i != id)));
}

fn top_guard() -> Option<Guard> {
    GUARDS.with(|g| g.borrow().last().map(|(_, f)| f.clone()))
}

/// In der Hauptansicht einmal aufrufen, bevor die Suche startet
pub fn init(c: Ctx) {
    // Ansicht aus der Adresse (Lesezeichen, Neuladen)
    let start = Route::parse(&current_hash());
    start.apply(&c);
    let route = Memo::new(move |_| Route::current(&c));
    replace(&route.get_untracked().to_hash());

    // Ansicht geändert -> Verlaufseintrag
    Effect::new(move |_| {
        let new = route.get();
        let hash = new.to_hash();
        if PENDING_BACK.with(|p| p.borrow().as_deref() == Some(hash.as_str())) {
            return;
        }
        let cur = current_hash();
        if hash == cur {
            return;
        }
        let old = Route::parse(&cur);
        if new.closes_layers_of(&old) && prev_hash().as_deref() == Some(hash.as_str()) {
            // Ebene geschlossen und vorher war genau diese Ansicht: zurückgehen,
            // statt einen neuen Eintrag anzulegen ("Zurück" öffnet sie nicht wieder)
            PENDING_BACK.with(|p| *p.borrow_mut() = Some(hash));
            if let Ok(h) = window().history() {
                let _ = h.back();
            }
        } else if new.with_query("") == old.with_query("") && !old.query.is_empty() {
            // Weitertippen in der Suche: kein Eintrag pro Buchstabe
            replace(&hash);
        } else {
            push(&hash);
        }
    });

    // Zurück / Vor / Adresse von Hand geändert
    let _ = window_event_listener(leptos::ev::hashchange, move |_| {
        let hash = current_hash();
        let expected = PENDING_BACK.with(|p| p.borrow_mut().take());
        let target = Route::parse(&hash);
        let now = route.get_untracked();
        c.sidebar_open.set(false);
        c.share_target.set(None);
        if expected.as_deref() == Some(hash.as_str()) || target == now {
            return;
        }
        if target.closes_guarded_layer_of(&now) {
            if let Some(close) = top_guard() {
                // Ebene bleibt erst einmal offen; nach dem Schließen geht es
                // über history.back() wieder hierher zurück.
                let open = now.to_hash();
                if let Ok(h) = window().history() {
                    let _ = h.push_state_with_url(&state_with_prev(&hash), "", Some(&open));
                }
                close();
                return;
            }
        }
        target.apply(&c);
    });
}

impl Route {
    fn with_query(&self, q: &str) -> Self {
        Route { query: q.to_string(), ..self.clone() }
    }
}
