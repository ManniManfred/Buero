// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashMap;

use shared::{Collection, CollectionInput, UploadResult};
use wasm_bindgen::JsCast;

use crate::icons::Icon;
use crate::state::{ctx, Ctx, UploadItem};

pub fn pick_files(list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(list) = list else { return Vec::new() };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// Lädt Dateien hoch (max. 3 gleichzeitig). Ist eine Sammlung ausgewählt,
/// landen die Dokumente direkt darin.
pub fn start_uploads(c: Ctx, files: Vec<web_sys::File>) {
    let target = c.collection.get_untracked();
    start_uploads_to(c, files.into_iter().map(|f| (f.name(), f, target)).collect());
}

/// Lädt Dateien hoch, jede in ihre Sammlung: (angezeigter Name, Datei, Sammlung)
fn start_uploads_to(c: Ctx, files: Vec<(String, web_sys::File, Option<i64>)>) {
    if files.is_empty() {
        return;
    }
    let base_key = c.uploads.with_untracked(|u| u.iter().map(|x| x.key).max().unwrap_or(0)) + 1;
    c.uploads.update(|u| {
        u.retain(|x| x.status == "uploading" || x.status == "waiting");
        for (i, (name, _, _)) in files.iter().enumerate() {
            let key = base_key + i as u32;
            u.push(UploadItem { key, name: name.clone(), status: "waiting".into(), message: String::new(), doc_id: None });
        }
    });
    let jobs: Vec<(u32, web_sys::File, Option<i64>)> =
        files.into_iter().enumerate().map(|(i, (_, f, t))| (base_key + i as u32, f, t)).collect();
    let queue = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::from(jobs)));
    for _ in 0..3 {
        let queue = queue.clone();
        spawn_local(async move {
            loop {
                let next = queue.borrow_mut().pop_front();
                let Some((key, file, target)) = next else { break };
                set_status(c, key, "uploading", "", None);
                let form = web_sys::FormData::new().unwrap();
                if let Some(t) = target {
                    let _ = form.append_with_str("collection_ids", &t.to_string());
                }
                let _ = form.append_with_blob_and_filename("file", &file, &file.name());
                match c.api.upload::<Vec<UploadResult>>("/api/documents", form).await {
                    Ok(r) => {
                        let first = r.into_iter().next();
                        let dup = first.as_ref().map(|x| x.duplicate).unwrap_or(false);
                        set_status(
                            c,
                            key,
                            if dup { "duplicate" } else { "done" },
                            if dup { "bereits vorhanden" } else { "" },
                            first.map(|x| x.document.id),
                        );
                    }
                    Err(e) => set_status(c, key, "error", &e, None),
                }
            }
            c.load_meta();
            c.reload();
        });
    }
}

/// Abgelegter Eintrag: Pfad (Ordner/…/Name) und Datei (`None`: Ordner)
type DropEntry = (Vec<String>, Option<web_sys::File>);

fn parse_entries(v: &wasm_bindgen::JsValue) -> Vec<DropEntry> {
    let Some(arr) = v.dyn_ref::<js_sys::Array>() else { return Vec::new() };
    arr.iter()
        .filter_map(|o| {
            let path = js_sys::Reflect::get(&o, &"path".into()).ok()?;
            let path: Vec<String> = path.dyn_ref::<js_sys::Array>()?.iter().filter_map(|p| p.as_string()).collect();
            let file = js_sys::Reflect::get(&o, &"file".into()).ok().and_then(|f| f.dyn_into::<web_sys::File>().ok());
            (!path.is_empty()).then_some((path, file))
        })
        .collect()
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Ordner abgelegt: Ordner werden Sammlungen (in der gewählten Sammlung bzw.
/// ganz oben), Unterordner Untersammlungen; vorhandene gleichnamige Sammlungen
/// werden weiterverwendet. Danach die Dateien in ihre Sammlung hochladen.
async fn upload_folders(c: Ctx, entries: Vec<DropEntry>) {
    let base = c.collection.get_untracked();
    let mut known: Vec<Collection> = c.collections.get_untracked();
    let mut dirs: Vec<&Vec<String>> = entries.iter().filter(|(_, f)| f.is_none()).map(|(p, _)| p).collect();
    dirs.sort_by_key(|p| p.len());
    let mut target: HashMap<Vec<String>, i64> = HashMap::new();
    let mut created = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for dir in dirs {
        let name = dir.last().cloned().unwrap_or_default();
        let parent = if dir.len() == 1 {
            base
        } else {
            match target.get(&dir[..dir.len() - 1]) {
                Some(p) => Some(*p),
                None => continue, // übergeordneter Ordner fehlgeschlagen
            }
        };
        let find = |list: &[Collection]| list.iter().find(|x| x.parent_id == parent && same_name(&x.name, &name)).map(|x| x.id);
        let id = match find(&known) {
            Some(id) => Some(id),
            None => {
                let input = CollectionInput { name: Some(name.trim().to_string()), parent_id: Some(parent.unwrap_or(0)), color: None, ai_locked: None };
                match c.api.send::<_, Collection>("POST", "/api/collections", &input).await {
                    Ok(col) => {
                        created += 1;
                        let id = col.id;
                        known.push(col);
                        Some(id)
                    }
                    Err(e) => {
                        // Evtl. inzwischen angelegt: Liste neu holen
                        if let Ok(v) = c.api.get::<Vec<Collection>>("/api/collections").await {
                            known = v;
                        }
                        let id = find(&known);
                        if id.is_none() {
                            failed.push(format!("{name}: {e}"));
                        }
                        id
                    }
                }
            }
        };
        if let Some(id) = id {
            target.insert(dir.clone(), id);
        }
    }
    if created > 0 {
        c.notify(format!("{created} Sammlung(en) angelegt"), false);
    }
    if !failed.is_empty() {
        c.notify(format!("Sammlung anlegen: {}", failed.join(", ")), true);
    }
    c.load_meta();
    let files: Vec<(String, web_sys::File, Option<i64>)> = entries
        .into_iter()
        .filter_map(|(path, file)| {
            let file = file?;
            let coll = if path.len() == 1 { base } else { Some(*target.get(&path[..path.len() - 1])?) };
            Some((path.join("/"), file, coll))
        })
        .collect();
    start_uploads_to(c, files);
}

fn set_status(c: Ctx, key: u32, status: &str, msg: &str, doc: Option<i64>) {
    c.uploads.update(|u| {
        if let Some(x) = u.iter_mut().find(|x| x.key == key) {
            x.status = status.into();
            x.message = msg.into();
            x.doc_id = doc;
        }
    });
}

#[component]
pub fn UploadPanel() -> impl IntoView {
    let c = ctx();
    view! {
        <Show when=move || c.uploads.with(|u| !u.is_empty())>
            <div class="uploads card">
                <div class="uploads-head">
                    <strong>{move || {
                        let (done, all) = c.uploads.with(|u| (u.iter().filter(|x| x.status != "waiting" && x.status != "uploading").count(), u.len()));
                        format!("Upload {done}/{all}")
                    }}</strong>
                    <button class="icon" on:click=move |_| c.uploads.update(|u| u.retain(|x| x.status == "uploading" || x.status == "waiting"))><Icon name="x" /></button>
                </div>
                <ul>
                    <For each=move || c.uploads.get() key=|u| (u.key, u.status.clone()) let:u>
                        <li class=format!("up-{}", u.status) on:click=move |_| if let Some(id) = u.doc_id { c.selected.set(Some(id)) }>
                            <span class="ellipsis grow">{u.name.clone()}</span>
                            <span class="small">{match u.status.as_str() {
                                "waiting" => "wartet".into_any(),
                                "uploading" => "lädt …".into_any(),
                                "done" => view! { <Icon name="check" class="ok" /> }.into_any(),
                                _ => u.message.clone().into_any(),
                            }}</span>
                        </li>
                    </For>
                </ul>
            </div>
        </Show>
    }
}

/// Dateien irgendwo ins Fenster ziehen
#[component]
pub fn DropOverlay() -> impl IntoView {
    let c = ctx();
    let active = RwSignal::new(false);
    let has_files = |ev: &web_sys::DragEvent| {
        ev.data_transfer().map(|d| d.types().includes(&"Files".into(), 0)).unwrap_or(false)
    };
    let _ = window_event_listener(leptos::ev::dragover, move |ev| {
        if has_files(&ev) {
            ev.prevent_default();
            active.set(true);
        }
    });
    let _ = window_event_listener(leptos::ev::dragleave, move |ev| {
        // Fenster verlassen
        if ev.client_x() <= 0 || ev.client_y() <= 0 || ev.related_target().is_none() {
            active.set(false);
        }
    });
    let _ = window_event_listener(leptos::ev::drop, move |ev| {
        active.set(false);
        if has_files(&ev) {
            ev.prevent_default();
            let Some(dt) = ev.data_transfer() else { return };
            // Ordner? Die Einträge gibt es nur jetzt, während des Ereignisses
            let entries = crate::js::drop_entries(&dt);
            let files = pick_files(dt.files());
            spawn_local(async move {
                match wasm_bindgen_futures::JsFuture::from(entries).await {
                    Ok(v) if !v.is_null() => upload_folders(c, parse_entries(&v)).await,
                    _ => start_uploads(c, files),
                }
            });
        }
    });
    view! {
        <Show when=move || active.get()>
            <div class="drop-overlay">
                <div>
                    "Dateien oder Ordner hier ablegen"
                    <div class="small">{move || match c.collection.get() {
                        Some(id) => format!("→ in Sammlung „{}“", c.collection_name(id)),
                        None => "→ in den Eingang".to_string(),
                    }}</div>
                    <div class="small">"Ordner werden zu Sammlungen"</div>
                </div>
            </div>
        </Show>
    }
}
