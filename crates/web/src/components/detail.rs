use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{Collection, DocumentDetail, DocumentSummary, DocumentUpdate, Tag, TagInput};

use crate::api::open_external;
use crate::state::{collection_path, ctx, Ctx};
use crate::util::{format_size, format_ts, status_label};

fn save(c: Ctx, id: i64, upd: DocumentUpdate, detail: RwSignal<Option<DocumentDetail>>) {
    spawn_local(async move {
        match c.api.send::<_, DocumentSummary>("PATCH", &format!("/api/documents/{id}"), &upd).await {
            Ok(s) => {
                detail.update(|d| {
                    if let Some(d) = d {
                        d.summary = s.clone();
                    }
                });
                c.results.update(|r| {
                    if let Some(x) = r.iter_mut().find(|x| x.id == id) {
                        let snippet = x.snippet.take();
                        *x = s;
                        x.snippet = snippet;
                    }
                });
                c.load_meta();
            }
            Err(e) => c.notify(e, true),
        }
    });
}

#[component]
pub fn DetailPanel(id: i64) -> impl IntoView {
    let c = ctx();
    let detail = RwSignal::new(None::<DocumentDetail>);
    let text = RwSignal::new(None::<String>);
    let tab = RwSignal::new("preview");

    let load = move || {
        spawn_local(async move {
            match c.api.get::<DocumentDetail>(&format!("/api/documents/{id}")).await {
                Ok(d) => {
                    if c.selected.get_untracked() == Some(id) {
                        detail.set(Some(d));
                    }
                }
                Err(e) => {
                    c.notify(e, true);
                    c.selected.set(None);
                }
            }
        });
    };
    load();

    // Solange verarbeitet wird, Detail nachladen
    let handle = set_interval_with_handle(
        move || {
            let busy = detail.with_untracked(|d| {
                d.as_ref().map(|d| matches!(d.summary.extract_status.as_str(), "pending" | "processing")).unwrap_or(false)
            });
            if busy {
                load();
                text.set(None);
            }
        },
        std::time::Duration::from_secs(2),
    )
    .ok();
    on_cleanup(move || {
        if let Some(h) = handle {
            h.clear();
        }
    });

    Effect::new(move |_| {
        if tab.get() == "text" && text.with_untracked(|t| t.is_none()) {
            spawn_local(async move {
                let t = c.api.get_text(&format!("/api/documents/{id}/text")).await.unwrap_or_else(|e| e);
                text.set(Some(t));
            });
        }
    });

    // Nur bei neuer Version / Statuswechsel neu aufbauen – nicht bei jeder Änderung
    // von Titel, Tags usw. (sonst würde z. B. die Vorschau neu geladen)
    let render_key = Memo::new(move |_| {
        detail.with(|d| {
            d.as_ref().map(|d| (d.summary.id, d.summary.version, d.summary.extract_status.clone(), d.content_length))
        })
    });

    view! {
        <aside class="detail">
            {move || match render_key.get() {
                None => view! { <div class="center muted">"Lade…"</div> }.into_any(),
                Some(_) => {
                    let d = detail.get_untracked().unwrap();
                    view! { <DetailBody d=d detail=detail tab=tab text=text reload=Callback::new(move |_| load()) /> }.into_any()
                }
            }}
        </aside>
    }
}

#[component]
fn DetailBody(
    d: DocumentDetail,
    detail: RwSignal<Option<DocumentDetail>>,
    tab: RwSignal<&'static str>,
    text: RwSignal<Option<String>>,
    reload: Callback<()>,
) -> impl IntoView {
    let c = ctx();
    let s = d.summary.clone();
    let id = s.id;
    let version_input = NodeRef::<leptos::html::Input>::new();
    let file_url = c.api.file_url(&format!("/api/documents/{id}/file?v={}", s.version));
    let download_url = c.api.file_url(&format!("/api/documents/{id}/file?download=1"));
    let kind = s.kind.clone();
    let mime = s.mime.clone();
    let previewable = kind == "pdf" || kind == "image" || mime.starts_with("text/") || mime == "application/json";

    let title = RwSignal::new(s.title.clone());
    let date = RwSignal::new(s.document_date.clone().unwrap_or_default());
    let description = RwSignal::new(d.description.clone());

    let preview = {
        let file_url = file_url.clone();
        let kind = kind.clone();
        move || -> AnyView {
            match tab.get() {
                "text" => view! {
                    <pre class="fulltext">{move || text.get().unwrap_or_else(|| "Lade…".into())}</pre>
                }.into_any(),
                _ => {
                    if kind == "image" {
                        view! { <div class="preview img"><img src=file_url.clone() alt="" /></div> }.into_any()
                    } else if previewable {
                        view! { <iframe class="preview" src=file_url.clone()></iframe> }.into_any()
                    } else {
                        view! {
                            <div class="preview none">
                                <p>"Keine Vorschau für diesen Dateityp."</p>
                                <button on:click=move |_| tab.set("text")>"Erkannten Text anzeigen"</button>
                            </div>
                        }.into_any()
                    }
                }
            }
        }
    };

    let open_url = file_url.clone();
    let status = status_label(&s.extract_status);

    view! {
        <div class="detail-head">
            <input class="title-input" prop:value=move || title.get()
                on:input=move |ev| title.set(event_target_value(&ev))
                on:change=move |_| save(c, id, DocumentUpdate { title: Some(title.get_untracked()), ..Default::default() }, detail) />
            <button class="icon" title="Schließen (Esc)" on:click=move |_| c.selected.set(None)>"✕"</button>
        </div>
        <div class="actions">
            <button on:click=move |_| open_external(&open_url)>"↗ Öffnen"</button>
            <button on:click=move |_| open_external(&download_url)>"⬇ Herunterladen"</button>
            <button title="Bearbeitete Datei als neue Version hochladen"
                on:click=move |_| { if let Some(i) = version_input.get() { i.click(); } }>"⟳ Neue Version"</button>
            <input type="file" style="display:none" node_ref=version_input on:change=move |_| {
                let Some(i) = version_input.get() else { return };
                let Some(f) = i.files().and_then(|f| f.get(0)) else { return };
                i.set_value("");
                spawn_local(async move {
                    let form = web_sys::FormData::new().unwrap();
                    let _ = form.append_with_blob_and_filename("file", &f, &f.name());
                    match c.api.upload::<DocumentSummary>(&format!("/api/documents/{id}/versions"), form).await {
                        Ok(_) => { c.notify("Neue Version gespeichert", false); text.set(None); reload.run(()); c.reload(); }
                        Err(e) => c.notify(e, true),
                    }
                });
            } />
            <button class="danger" on:click=move |_| {
                if window().confirm_with_message("Dokument inkl. aller Versionen endgültig löschen?").unwrap_or(false) {
                    spawn_local(async move {
                        match c.api.send_empty::<()>("DELETE", &format!("/api/documents/{id}"), None).await {
                            Ok(()) => { c.selected.set(None); c.load_meta(); c.reload(); }
                            Err(e) => c.notify(e, true),
                        }
                    });
                }
            }>"🗑"</button>
        </div>
        {(!status.is_empty()).then(|| view! {
            <div class="notice" class:bad=s.extract_status == "failed">
                {status}
                {d.extract_error.clone().map(|e| view! { <div class="small">{e}</div> })}
                {(s.extract_status == "failed").then(|| view! {
                    <button class="link" on:click=move |_| {
                        spawn_local(async move {
                            let _ = c.api.send_empty::<()>("POST", &format!("/api/documents/{id}/reprocess"), None).await;
                            reload.run(());
                        });
                    }>"Erneut versuchen"</button>
                })}
            </div>
        })}
        <div class="tabs">
            <button class:active=move || tab.get() == "preview" on:click=move |_| tab.set("preview")>"Vorschau"</button>
            <button class:active=move || tab.get() == "text" on:click=move |_| tab.set("text")>
                {format!("Text ({} Zeichen)", d.content_length)}
            </button>
        </div>
        <div class="preview-wrap">{preview}</div>

        <div class="fields">
            <label>"Dokumentdatum"
                <input type="date" prop:value=move || date.get()
                    on:change=move |ev| {
                        let v = event_target_value(&ev);
                        date.set(v.clone());
                        save(c, id, DocumentUpdate { document_date: Some(v), ..Default::default() }, detail);
                    } />
            </label>
            <label>"Notiz"
                <textarea rows="3" prop:value=move || description.get()
                    on:input=move |ev| description.set(event_target_value(&ev))
                    on:change=move |_| save(c, id, DocumentUpdate { description: Some(description.get_untracked()), ..Default::default() }, detail)>
                </textarea>
            </label>

            <div class="field-label">"Sammlungen"</div>
            <CollectionPicker id=id detail=detail />

            <div class="field-label">"Tags"</div>
            <TagEditor id=id detail=detail />

            <div class="field-label">"Infos"</div>
            <table class="info">
                <tr><td>"Datei"</td><td class="ellipsis">{s.original_filename.clone()}</td></tr>
                <tr><td>"Typ"</td><td>{s.mime.clone()}</td></tr>
                <tr><td>"Größe"</td><td>{format_size(s.size)}</td></tr>
                {s.page_count.map(|p| view! { <tr><td>"Seiten"</td><td>{p}</td></tr> })}
                <tr><td>"Hochgeladen"</td><td>{format_ts(s.created_at)}</td></tr>
                <tr><td>"Geändert"</td><td>{format_ts(s.updated_at)}</td></tr>
            </table>

            {(d.versions.len() > 1).then(|| view! {
                <div class="field-label">"Versionen"</div>
                <ul class="versions">
                    {d.versions.iter().map(|v| {
                        let url = c.api.file_url(&format!("/api/documents/{id}/file?download=1&version={}", v.version));
                        view! {
                            <li>
                                <a href="#" on:click=move |ev| { ev.prevent_default(); open_external(&url); }>
                                    {format!("v{}", v.version)}
                                </a>
                                " · "{format_ts(v.created_at)}" · "{format_size(v.size)}
                                <span class="muted small ellipsis">" "{v.original_filename.clone()}</span>
                            </li>
                        }
                    }).collect_view()}
                </ul>
            })}
        </div>
    }
}

/// Mehrfachzuordnung zu Sammlungen als aufklappbarer Baum mit Checkboxen
#[component]
fn CollectionPicker(id: i64, detail: RwSignal<Option<DocumentDetail>>) -> impl IntoView {
    let c = ctx();
    let filter = RwSignal::new(String::new());
    let assigned = move || detail.with(|d| d.as_ref().map(|d| d.summary.collection_ids.clone()).unwrap_or_default());
    let assigned_now = move || detail.with_untracked(|d| d.as_ref().map(|d| d.summary.collection_ids.clone()).unwrap_or_default());

    let toggle = move |cid: i64, on: bool| {
        let mut ids = assigned_now();
        if on {
            if !ids.contains(&cid) {
                ids.push(cid);
            }
        } else {
            ids.retain(|x| *x != cid);
        }
        save(c, id, DocumentUpdate { collection_ids: Some(ids), ..Default::default() }, detail);
    };

    view! {
        <div class="chips">
            {move || assigned().into_iter().map(|cid| view! {
                <span class="chip coll removable" title="Entfernen" on:click=move |_| toggle(cid, false)>
                    "🗂 "{c.collection_name(cid)}" ✕"
                </span>
            }).collect_view()}
        </div>
        <details class="picker">
            <summary>"Sammlungen zuordnen …"</summary>
            <input type="search" placeholder="Filtern oder neue Sammlung anlegen (Enter)"
                prop:value=move || filter.get()
                on:input=move |ev| filter.set(event_target_value(&ev))
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        ev.prevent_default();
                        let name = filter.get_untracked().trim().to_string();
                        if name.is_empty() { return; }
                        let exists = c.collections.with_untracked(|cs| cs.iter().find(|x| x.name.eq_ignore_ascii_case(&name)).map(|x| x.id));
                        if let Some(cid) = exists {
                            toggle(cid, true);
                        } else {
                            spawn_local(async move {
                                let body = shared::CollectionInput { name: Some(name), parent_id: None, color: None };
                                match c.api.send::<_, Collection>("POST", "/api/collections", &body).await {
                                    Ok(col) => { c.collections.update(|v| v.push(col.clone())); toggle(col.id, true); }
                                    Err(e) => c.notify(e, true),
                                }
                            });
                        }
                        filter.set(String::new());
                    }
                } />
            <div class="picker-list">
                {move || {
                    let f = filter.get().to_lowercase();
                    let current = assigned();
                    c.collections.with(|cs| {
                        let mut items: Vec<(i64, String)> = cs.iter().map(|x| (x.id, collection_path(cs, x.id))).collect();
                        items.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                        items.into_iter()
                            .filter(|(_, p)| f.is_empty() || p.to_lowercase().contains(&f))
                            .map(|(cid, path)| {
                                let on = current.contains(&cid);
                                view! {
                                    <label class="check">
                                        <input type="checkbox" prop:checked=on
                                            on:change=move |ev| toggle(cid, event_target_checked(&ev)) />
                                        {path}
                                    </label>
                                }
                            })
                            .collect_view()
                    })
                }}
            </div>
        </details>
    }
}

#[component]
fn TagEditor(id: i64, detail: RwSignal<Option<DocumentDetail>>) -> impl IntoView {
    let c = ctx();
    let input = RwSignal::new(String::new());
    let assigned = move || detail.with(|d| d.as_ref().map(|d| d.summary.tag_ids.clone()).unwrap_or_default());
    let assigned_now = move || detail.with_untracked(|d| d.as_ref().map(|d| d.summary.tag_ids.clone()).unwrap_or_default());
    let set_tags = move |ids: Vec<i64>| save(c, id, DocumentUpdate { tag_ids: Some(ids), ..Default::default() }, detail);

    let add = move || {
        let name = input.get_untracked().trim().trim_start_matches('#').to_string();
        if name.is_empty() {
            return;
        }
        input.set(String::new());
        spawn_local(async move {
            match c.api.send::<_, Tag>("POST", "/api/tags", &TagInput { name: Some(name), color: None }).await {
                Ok(t) => {
                    c.tags.update(|ts| if !ts.iter().any(|x| x.id == t.id) { ts.push(t.clone()) });
                    let mut ids = assigned_now();
                    if !ids.contains(&t.id) {
                        ids.push(t.id);
                    }
                    set_tags(ids);
                }
                Err(e) => c.notify(e, true),
            }
        });
    };

    view! {
        <div class="chips">
            {move || assigned().into_iter().map(|tid| {
                let name = c.tags.with(|ts| ts.iter().find(|t| t.id == tid).map(|t| t.name.clone()).unwrap_or_default());
                view! {
                    <span class="chip tag removable" title="Entfernen" on:click=move |_| {
                        let mut ids = assigned_now();
                        ids.retain(|x| *x != tid);
                        set_tags(ids);
                    }>"#"{name}" ✕"</span>
                }
            }).collect_view()}
            <input class="tag-input" list="tag-suggestions" placeholder="+ Tag"
                prop:value=move || input.get()
                on:input=move |ev| input.set(event_target_value(&ev))
                on:keydown=move |ev| if ev.key() == "Enter" || ev.key() == "," { ev.prevent_default(); add(); }
                on:change=move |_| add() />
            <datalist id="tag-suggestions">
                {move || c.tags.get().into_iter().map(|t| view! { <option value=t.name></option> }).collect_view()}
            </datalist>
        </div>
    }
}
