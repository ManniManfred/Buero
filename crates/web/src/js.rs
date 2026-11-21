// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Kleine JavaScript-Helfer: Zwischenablage, Euro-Office-Editor und Tupfer Designer.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function copy_text(t) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    return navigator.clipboard.writeText(t).then(() => true, () => false);
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = t; ta.style.position = "fixed"; ta.style.opacity = "0";
    document.body.appendChild(ta); ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return Promise.resolve(ok);
  } catch (e) { return Promise.resolve(false); }
}

let editor = null;

export function office_open(scriptUrl, elementId, configJson, onEvent) {
  const start = () => {
    try {
      const cfg = JSON.parse(configJson);
      cfg.events = {
        onAppReady: () => onEvent("ready", ""),
        onDocumentStateChange: (e) => onEvent("state", e && e.data ? "dirty" : "clean"),
        onRequestClose: () => onEvent("close", ""),
        onError: (e) => onEvent("error", String((e && e.data && (e.data.errorDescription || e.data.errorCode)) || "unbekannter Fehler").replace(/<br\s*\/?>/gi, " ")),
      };
      office_close();
      editor = new window.DocsAPI.DocEditor(elementId, cfg);
    } catch (err) { onEvent("error", String(err)); }
  };
  if (window.DocsAPI && window.__bueroOfficeScript === scriptUrl) { start(); return; }
  const s = document.createElement("script");
  s.src = scriptUrl;
  s.onload = () => { window.__bueroOfficeScript = scriptUrl; start(); };
  s.onerror = () => onEvent("error", "Euro-Office ist nicht erreichbar (" + scriptUrl + ")");
  document.head.appendChild(s);
}

let tupferListener = null;

// Meldungen des Tupfer Designers im iframe (gespeichert, Status der Änderungen)
export function tupfer_listen(frameId, onEvent) {
  tupfer_unlisten();
  tupferListener = (e) => {
    const f = document.getElementById(frameId);
    const d = e.data || {};
    if (!f || e.source !== f.contentWindow || d.quelle !== "tupfer") return;
    onEvent(String(d.art || ""), String(d.text || ""), d.geaendert === true);
  };
  window.addEventListener("message", tupferListener);
}

export function tupfer_unlisten() {
  if (tupferListener) {
    window.removeEventListener("message", tupferListener);
    tupferListener = null;
  }
}

// Fragt den Designer, ob es ungespeicherte Änderungen gibt (Antwort über tupfer_listen, art "status")
export function tupfer_ask(frameId) {
  const f = document.getElementById(frameId);
  if (!f || !f.contentWindow) return false;
  f.contentWindow.postMessage({ quelle: "buero", art: "geaendert?" }, "*");
  return true;
}

export function office_close() {
  if (editor) {
    try { editor.destroyEditor(); } catch (e) {}
    editor = null;
  }
}
"#)]
extern "C" {
    fn copy_text(t: &str) -> js_sys::Promise;
    pub fn office_open(script_url: &str, element_id: &str, config_json: &str, on_event: &js_sys::Function);
    pub fn office_close();
    pub fn tupfer_listen(frame_id: &str, on_event: &js_sys::Function);
    pub fn tupfer_unlisten();
    pub fn tupfer_ask(frame_id: &str) -> bool;
}

/// Text in die Zwischenablage kopieren; `true` bei Erfolg.
pub async fn copy(text: &str) -> bool {
    wasm_bindgen_futures::JsFuture::from(copy_text(text))
        .await
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or(false)
}
