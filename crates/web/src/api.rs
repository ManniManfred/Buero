// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! HTTP-Client für die Server-API (identisch für Web und Desktop).

use gloo_net::http::{Request, RequestBuilder, Response};
use leptos::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;

const KEY_SERVER: &str = "buero.server";
const KEY_TOKEN: &str = "buero.token";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn load(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?.filter(|v| !v.is_empty())
}

pub fn save(key: &str, value: Option<&str>) {
    if let Some(s) = storage() {
        match value {
            Some(v) => {
                let _ = s.set_item(key, v);
            }
            None => {
                let _ = s.remove_item(key);
            }
        }
    }
}

/// Läuft die Oberfläche im Desktop-Client (Tauri)?
pub fn is_desktop() -> bool {
    let Some(w) = web_sys::window() else { return false };
    let loc = w.location();
    let proto = loc.protocol().unwrap_or_default();
    let host = loc.hostname().unwrap_or_default();
    proto == "tauri:" || host == "tauri.localhost"
        || js_sys::Reflect::has(&w, &"__TAURI_INTERNALS__".into()).unwrap_or(false)
}

#[derive(Clone, Copy)]
pub struct Api {
    /// Basis-URL des Servers, leer = gleicher Ursprung (Web)
    pub base: RwSignal<String>,
    pub token: RwSignal<Option<String>>,
    /// Kurzlebiger Schlüssel für Datei-Links (nie das Sitzungs-Token in URLs)
    pub file_key: RwSignal<String>,
    /// Wird bei 401 gesetzt, damit die Anmeldung erscheint
    pub unauthorized: RwSignal<bool>,
}

impl Api {
    pub fn new() -> Self {
        let base = if is_desktop() { load(KEY_SERVER).unwrap_or_default() } else { String::new() };
        Api {
            base: RwSignal::new(base),
            token: RwSignal::new(load(KEY_TOKEN)),
            file_key: RwSignal::new(String::new()),
            unauthorized: RwSignal::new(false),
        }
    }

    pub fn set_server(&self, url: &str) {
        let url = url.trim().trim_end_matches('/').to_string();
        save(KEY_SERVER, Some(&url));
        self.base.set(url);
    }

    pub fn set_token(&self, token: Option<String>) {
        save(KEY_TOKEN, token.as_deref());
        if token.is_none() {
            self.file_key.set(String::new());
        }
        self.token.set(token);
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.get_untracked(), path)
    }

    /// URL für Dateien, die direkt vom Browser geladen werden (img, iframe, Download).
    /// Enthält den Datei-Schlüssel – gilt nur zum Lesen von Dateien und nur einige Stunden.
    pub fn file_url(&self, path: &str) -> String {
        let mut u = self.url(path);
        let k = self.file_key.get_untracked();
        if !k.is_empty() {
            u.push(if u.contains('?') { '&' } else { '?' });
            u.push_str("fk=");
            u.push_str(&k);
        }
        u
    }

    /// Datei-Schlüssel holen bzw. erneuern
    pub async fn load_file_key(&self) -> Result<(), String> {
        let k = self.get::<shared::FileKey>("/api/auth/file-key").await?;
        self.file_key.set(k.key);
        Ok(())
    }

    /// Wenige Minuten gültiger Link (z. B. Backup-Download), absolute URL
    pub async fn signed_link(&self, path: &str) -> Result<String, String> {
        let l = self.send::<_, shared::SignedLink>("POST", path, &()).await?;
        Ok(self.url(&l.path))
    }

    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        match self.token.get_untracked() {
            Some(t) => rb.header("Authorization", &format!("Bearer {t}")),
            None => rb,
        }
    }

    async fn check(&self, resp: Result<Response, gloo_net::Error>) -> Result<Response, String> {
        let resp = resp.map_err(|e| format!("Server nicht erreichbar: {e}"))?;
        if resp.status() == 401 {
            self.unauthorized.set(true);
        }
        if !resp.ok() {
            let status = resp.status();
            let msg = match resp.json::<shared::ApiError>().await {
                Ok(e) => e.error,
                Err(_) => format!("Fehler {status}"),
            };
            return Err(msg);
        }
        Ok(resp)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let r = self.auth(Request::get(&self.url(path))).send().await;
        self.check(r).await?.json().await.map_err(|e| e.to_string())
    }

    pub async fn get_text(&self, path: &str) -> Result<String, String> {
        let r = self.auth(Request::get(&self.url(path))).send().await;
        self.check(r).await?.text().await.map_err(|e| e.to_string())
    }

    fn builder(&self, method: &str, path: &str) -> RequestBuilder {
        let url = self.url(path);
        let rb = match method {
            "POST" => Request::post(&url),
            "PATCH" => Request::patch(&url),
            "PUT" => Request::put(&url),
            "DELETE" => Request::delete(&url),
            _ => Request::get(&url),
        };
        self.auth(rb)
    }

    pub async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        let req = self.builder(method, path).json(body).map_err(|e| e.to_string())?;
        let r = req.send().await;
        self.check(r).await?.json().await.map_err(|e| e.to_string())
    }

    /// Anfrage ohne Antwortinhalt
    pub async fn send_empty<B: Serialize>(&self, method: &str, path: &str, body: Option<&B>) -> Result<(), String> {
        let r = match body {
            Some(b) => self.builder(method, path).json(b).map_err(|e| e.to_string())?.send().await,
            None => self.builder(method, path).send().await,
        };
        self.check(r).await.map(|_| ())
    }

    /// Sendet eine Datei unverändert als Anfragekörper (z. B. ein Backup).
    pub async fn post_file<T: DeserializeOwned>(&self, path: &str, file: &web_sys::File) -> Result<T, String> {
        let req = self
            .builder("POST", path)
            .header("Content-Type", "application/octet-stream")
            .body(file.clone())
            .map_err(|e| e.to_string())?;
        let r = req.send().await;
        self.check(r).await?.json().await.map_err(|e| e.to_string())
    }

    pub async fn upload<T: DeserializeOwned>(&self, path: &str, form: web_sys::FormData) -> Result<T, String> {
        let req = self.builder("POST", path).body(form).map_err(|e| e.to_string())?;
        let r = req.send().await;
        self.check(r).await?.json().await.map_err(|e| e.to_string())
    }
}

/// Öffnet eine URL extern (Desktop: Standardprogramm/Browser; Web: neuer Tab).
pub fn open_external(url: &str) {
    let Some(w) = web_sys::window() else { return };
    if is_desktop() {
        // Tauri-Opener-Plugin über die globale API (withGlobalTauri)
        let tauri = js_sys::Reflect::get(&w, &"__TAURI__".into()).ok();
        let opener = tauri.and_then(|t| js_sys::Reflect::get(&t, &"opener".into()).ok());
        let f = opener
            .as_ref()
            .and_then(|o| js_sys::Reflect::get(o, &"openUrl".into()).ok())
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
        if let (Some(o), Some(f)) = (opener, f) {
            let _ = f.call1(&o, &url.into());
            return;
        }
    }
    let _ = w.open_with_url_and_target(url, "_blank");
}

use wasm_bindgen::JsCast;

/// Fensterbefehl des Desktop-Clients (`minimize`, `toggleMaximize`, `close`).
pub fn window_action(method: &str) {
    let Some(w) = web_sys::window() else { return };
    let get = |o: &wasm_bindgen::JsValue, k: &str| js_sys::Reflect::get(o, &k.into()).ok();
    let Some(win_mod) = get(&w, "__TAURI__").and_then(|t| get(&t, "window")) else { return };
    let Some(current) = get(&win_mod, "getCurrentWindow")
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call0(&win_mod).ok())
    else {
        return;
    };
    if let Some(f) = get(&current, method).and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
        let _ = f.call0(&current);
    }
}

/// Befehl des Desktop-Clients (Tauri) aufrufen.
pub async fn invoke<T: DeserializeOwned>(cmd: &str, args: serde_json::Value) -> Result<T, String> {
    let js_err = |e: wasm_bindgen::JsValue| e.as_string().unwrap_or_else(|| format!("{e:?}"));
    let w = web_sys::window().ok_or("Kein Fenster")?;
    let core = js_sys::Reflect::get(&w, &"__TAURI__".into())
        .and_then(|t| js_sys::Reflect::get(&t, &"core".into()))
        .map_err(js_err)?;
    let f = js_sys::Reflect::get(&core, &"invoke".into())
        .map_err(js_err)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| "Nur im Desktop-Client verfügbar".to_string())?;
    let args = js_sys::JSON::parse(&args.to_string()).map_err(js_err)?;
    let promise = f.call2(&core, &cmd.into(), &args).map_err(js_err)?.dyn_into::<js_sys::Promise>().map_err(js_err)?;
    let v = wasm_bindgen_futures::JsFuture::from(promise).await.map_err(js_err)?;
    let text = if v.is_undefined() {
        "null".to_string()
    } else {
        js_sys::JSON::stringify(&v).map_err(js_err)?.as_string().unwrap_or_else(|| "null".into())
    };
    serde_json::from_str(&text).map_err(|e| e.to_string())
}
