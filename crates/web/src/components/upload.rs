use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::UploadResult;

use crate::state::{ctx, Ctx, UploadItem};

pub fn pick_files(list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(list) = list else { return Vec::new() };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// Lädt Dateien hoch (max. 3 gleichzeitig). Ist eine Sammlung ausgewählt,
/// landen die Dokumente direkt darin.
pub fn start_uploads(c: Ctx, files: Vec<web_sys::File>) {
    if files.is_empty() {
        return;
    }
    let target = c.collection.get_untracked();
    let base_key = c.uploads.with_untracked(|u| u.iter().map(|x| x.key).max().unwrap_or(0)) + 1;
    let jobs: Vec<(u32, web_sys::File)> =
        files.into_iter().enumerate().map(|(i, f)| (base_key + i as u32, f)).collect();
    c.uploads.update(|u| {
        u.retain(|x| x.status == "uploading" || x.status == "waiting");
        for (key, f) in &jobs {
            u.push(UploadItem { key: *key, name: f.name(), status: "waiting".into(), message: String::new(), doc_id: None });
        }
    });
    let queue = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::from(jobs)));
    for _ in 0..3 {
        let queue = queue.clone();
        spawn_local(async move {
            loop {
                let next = queue.borrow_mut().pop_front();
                let Some((key, file)) = next else { break };
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
                    <button class="icon" on:click=move |_| c.uploads.update(|u| u.retain(|x| x.status == "uploading" || x.status == "waiting"))>"✕"</button>
                </div>
                <ul>
                    <For each=move || c.uploads.get() key=|u| (u.key, u.status.clone()) let:u>
                        <li class=format!("up-{}", u.status) on:click=move |_| if let Some(id) = u.doc_id { c.selected.set(Some(id)) }>
                            <span class="ellipsis grow">{u.name.clone()}</span>
                            <span class="small">{match u.status.as_str() {
                                "waiting" => "wartet".to_string(),
                                "uploading" => "lädt …".to_string(),
                                "done" => "✓".to_string(),
                                _ => u.message.clone(),
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
            let files = pick_files(ev.data_transfer().and_then(|d| d.files()));
            start_uploads(c, files);
        }
    });
    view! {
        <Show when=move || active.get()>
            <div class="drop-overlay">
                <div>
                    "Dateien hier ablegen"
                    <div class="small">{move || match c.collection.get() {
                        Some(id) => format!("→ in Sammlung „{}“", c.collection_name(id)),
                        None => "→ in den Eingang".to_string(),
                    }}</div>
                </div>
            </div>
        </Show>
    }
}
