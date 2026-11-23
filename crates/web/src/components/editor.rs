// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Editor für Text- und Markdown-Dateien als Vollbild-Ebene.
//!
//! Speichern legt eine neue Version an. Hat inzwischen jemand anderes
//! gespeichert, wird vorher gefragt (der Server lehnt mit `basis` ab).
//! Markdown zeigt daneben (oder statt des Textes) eine Vorschau.

use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{DocumentDetail, DocumentSummary};
use wasm_bindgen::JsCast;

use crate::icons::Icon;
use crate::state::{ctx, Ctx};

/// Lässt sich das Dokument im Text-Editor öffnen?
pub fn editable(d: &DocumentSummary) -> bool {
    const MAX: i64 = 5 * 1024 * 1024;
    let ext = shared::extension_of(&d.original_filename).to_ascii_lowercase();
    d.size <= MAX
        && (d.kind == "markdown"
            || matches!(
                ext.as_str(),
                "txt" | "text" | "log" | "csv" | "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg"
                    | "sh" | "py" | "js" | "ts" | "rs" | "css" | "sql"
            ))
}

/// Links in gerendertem Markdown extern öffnen statt in der Oberfläche
fn link_click(ev: web_sys::MouseEvent) {
    let Some(t) = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else { return };
    if let Ok(Some(a)) = t.closest("a[href]") {
        let href = a.get_attribute("href").unwrap_or_default();
        // Sprungmarken (Fußnoten) normal behandeln
        if href.starts_with('#') {
            return;
        }
        ev.prevent_default();
        if href.starts_with("http://") || href.starts_with("https://") || href.starts_with("mailto:") {
            crate::api::open_external(&href);
        }
    }
}

fn is_markdown(d: &DocumentSummary) -> bool {
    d.kind == "markdown"
}

/// Dateiinhalt als Blob (UTF-8)
fn blob(text: &str, mime: &str) -> Option<web_sys::Blob> {
    let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(text));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(&format!("{mime};charset=utf-8"));
    web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).ok()
}

async fn upload(c: Ctx, d: &DocumentSummary, text: &str, basis: Option<i64>) -> Result<DocumentSummary, String> {
    let b = blob(text, &d.mime).ok_or("Speichern nicht möglich")?;
    let form = web_sys::FormData::new().map_err(|_| "Speichern nicht möglich")?;
    form.append_with_blob_and_filename("file", &b, &d.original_filename).map_err(|_| "Speichern nicht möglich")?;
    let path = match basis {
        Some(v) => format!("/api/documents/{}/versions?basis={v}", d.id),
        None => format!("/api/documents/{}/versions", d.id),
    };
    c.api.upload(&path, form).await
}

#[component]
pub fn TextEditor(id: i64) -> impl IntoView {
    let c = ctx();
    let doc = RwSignal::new(None::<DocumentSummary>);
    let content = RwSignal::new(String::new());
    let saved = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    let changed = RwSignal::new(false);
    // Markdown: edit | split | preview
    let mode = RwSignal::new("split");
    let area = NodeRef::<leptos::html::Textarea>::new();

    spawn_local(async move {
        let d = match c.api.get::<DocumentDetail>(&format!("/api/documents/{id}")).await {
            Ok(d) => d.summary,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        match c.api.get_text(&format!("/api/documents/{id}/file?v={}", d.version)).await {
            Ok(t) => {
                // Zeilenenden so lassen, wie sie sind; nur das Byte-Order-Mark weg
                let t = t.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(t);
                content.set(t.clone());
                saved.set(t);
                doc.set(Some(d));
                if let Some(a) = area.get_untracked() {
                    let _ = a.focus();
                }
            }
            Err(e) => error.set(Some(e)),
        }
    });

    let dirty = move || content.with(|a| saved.with(|b| a != b));

    let save = move || {
        if saving.get_untracked() || !dirty() {
            return;
        }
        let Some(d) = doc.get_untracked() else { return };
        let text = content.get_untracked();
        saving.set(true);
        spawn_local(async move {
            let mut res = upload(c, &d, &text, Some(d.version)).await;
            if matches!(&res, Err(e) if e.contains("neuere Version")) {
                let ok = crate::dialog::confirm(
                    "Inzwischen geändert",
                    "Jemand anderes hat das Dokument inzwischen gespeichert. Trotzdem deinen Stand als neueste Version \
                     speichern? Die andere Version bleibt erhalten.",
                )
                .ok("Trotzdem speichern")
                .ask_ok()
                .await;
                res = if ok { upload(c, &d, &text, None).await } else { Err(String::new()) };
            }
            saving.set(false);
            match res {
                Ok(s) => {
                    c.notify(format!("Gespeichert (Version {})", s.version), false);
                    doc.set(Some(s));
                    saved.set(text);
                    changed.set(true);
                    c.detail_refresh.update(|n| *n += 1);
                    c.reload();
                }
                Err(e) if e.is_empty() => {}
                Err(e) => c.notify(e, true),
            }
        });
    };

    let close_now = move || {
        c.text_doc.set(None);
        if changed.get_untracked() {
            c.detail_refresh.update(|n| *n += 1);
        }
    };
    let close = move || {
        if dirty() {
            crate::dialog::confirm_then(
                crate::dialog::confirm("Ungespeicherte Änderungen", "Die Änderungen wurden noch nicht gespeichert. Trotzdem schließen?")
                    .ok("Schließen ohne Speichern")
                    .danger(),
                close_now,
            );
        } else {
            close_now();
        }
    };

    let keydown = move |ev: web_sys::KeyboardEvent| {
        if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
            ev.prevent_default();
            save();
        } else if ev.key() == "Tab" && !ev.shift_key() && !ev.ctrl_key() && !ev.alt_key() {
            // Tab rückt ein, statt das Feld zu verlassen
            let Some(a) = area.get_untracked() else { return };
            ev.prevent_default();
            let indent = if doc.with_untracked(|d| d.as_ref().is_some_and(is_markdown)) { "    " } else { "\t" };
            let _ = a.set_range_text(indent);
            let pos = a.selection_end().ok().flatten().unwrap_or(0) + indent.encode_utf16().count() as u32;
            let _ = a.set_selection_range(pos, pos);
            content.set(a.value());
        } else if ev.key() == "Escape" {
            ev.prevent_default();
            close();
        }
    };

    let is_md = move || doc.with(|d| d.as_ref().is_some_and(is_markdown));
    let show_text = move || !is_md() || mode.get() != "preview";
    let show_preview = move || is_md() && mode.get() != "edit";

    view! {
        <div class="office-overlay text-editor">
            <div class="office-bar">
                <Icon name="file-pen" />
                <span class="grow ellipsis">
                    <b>{move || doc.with(|d| d.as_ref().map(|d| d.original_filename.clone()).unwrap_or_default())}</b>
                    {move || dirty().then(|| view! { <span class="muted">" – geändert"</span> })}
                </span>
                <Show when=is_md>
                    <div class="segmented" role="group" aria-label="Ansicht">
                        <button class:active=move || mode.get() == "edit" on:click=move |_| mode.set("edit")>"Text"</button>
                        <button class:active=move || mode.get() == "split" on:click=move |_| mode.set("split")>"Beides"</button>
                        <button class:active=move || mode.get() == "preview" on:click=move |_| mode.set("preview")>"Vorschau"</button>
                    </div>
                </Show>
                <button on:click=move |_| save() disabled=move || saving.get() || !dirty() title="Als neue Version speichern (Strg+S)">
                    <Icon name="save" />{move || if saving.get() { "Speichert …" } else { "Speichern" }}
                </button>
                <button class="primary" on:click=move |_| close()><Icon name="x" />"Schließen"</button>
            </div>
            {move || error.get().map(|e| view! { <div class="notice bad">{e}</div> })}
            <div class="editor-body" class:split=move || show_text() && show_preview()>
                <Show when=show_text>
                    <textarea node_ref=area class="editor-text" spellcheck="true"
                        prop:value=move || content.get()
                        on:input=move |ev| content.set(event_target_value(&ev))
                        on:keydown=keydown
                        disabled=move || doc.with(|d| d.is_none())></textarea>
                </Show>
                <Show when=show_preview>
                    <div class="editor-preview markdown" on:click=link_click
                        inner_html=move || crate::markdown::to_html(&content.get())></div>
                </Show>
            </div>
        </div>
    }
}

/// Markdown-Datei gerendert (Detailansicht)
#[component]
pub fn MarkdownView(id: i64, version: i64) -> impl IntoView {
    let c = ctx();
    let html = RwSignal::new(None::<String>);
    spawn_local(async move {
        let t = c
            .api
            .get_text(&format!("/api/documents/{id}/file?v={version}"))
            .await
            .map(|t| crate::markdown::to_html(&t))
            .unwrap_or_else(|e| format!("<p>{}</p>", e.replace('<', "&lt;")));
        html.set(Some(t));
    });
    view! {
        <div class="preview markdown md-view" on:click=link_click
            inner_html=move || html.get().unwrap_or_else(|| "<p class=\"muted\">Lade…</p>".into())></div>
    }
}

/// „Neu“: Art wählen, Namen abfragen, Dokument anlegen und gleich öffnen
pub fn new_document(c: Ctx) {
    use crate::dialog::{choice, menu, prompt};
    spawn_local(async move {
        let mut choices = vec![
            choice("word", "Word-Dokument", "word"),
            choice("spreadsheet", "Excel-Tabelle", "spreadsheet"),
            choice("presentation", "PowerPoint-Präsentation", "presentation"),
            choice("text", "Textdatei", "text"),
            choice("markdown", "Markdown", "markdown"),
        ];
        if c.features.get_untracked().tupfer {
            choices.push(choice("tupfer", "Tupfer-Projekt", "tupfer"));
        }
        let Some(kind) = menu("Neues Dokument", choices).ask().await.map(|a| a.value) else { return };
        if kind == "tupfer" {
            // Der Designer legt das Dokument beim ersten Speichern selbst an (im Eingang)
            c.editor_title.set("Neues Tupfer-Projekt".into());
            c.tupfer_doc.set(Some(0));
            return;
        }
        let (heading, default) = match kind.as_str() {
            "word" => ("Neues Word-Dokument", "Neues Dokument"),
            "spreadsheet" => ("Neue Excel-Tabelle", "Neue Tabelle"),
            "presentation" => ("Neue PowerPoint-Präsentation", "Neue Präsentation"),
            "markdown" => ("Neue Markdown-Datei", "Notizen"),
            _ => ("Neue Textdatei", "Notiz"),
        };
        let coll = c.collection.get_untracked();
        let place = match coll {
            Some(id) => format!("Name – wird in der Sammlung „{}“ angelegt", c.collection_name(id)),
            None => "Name – wird im Eingang angelegt (ohne Sammlung)".to_string(),
        };
        let Some(title) = prompt(heading, place).value(default).ok("Anlegen").ask_text().await else { return };
        let body = serde_json::json!({ "kind": kind, "title": title, "collection_ids": coll.into_iter().collect::<Vec<_>>() });
        match c.api.send::<_, DocumentSummary>("POST", "/api/documents/new", &body).await {
            Ok(d) => {
                c.reload();
                c.load_meta();
                c.selected.set(Some(d.id));
                c.editor_title.set(d.title.clone());
                match kind.as_str() {
                    "text" | "markdown" => c.text_doc.set(Some(d.id)),
                    _ if c.features.get_untracked().office => c.office_doc.set(Some(d.id)),
                    _ => c.notify(format!("„{}“ angelegt – zum Bearbeiten öffnen bzw. herunterladen", d.original_filename), false),
                }
            }
            Err(e) => c.notify(e, true),
        }
    });
}
