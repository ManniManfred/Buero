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
    /// Wird bei 401 gesetzt, damit die Anmeldung erscheint
    pub unauthorized: RwSignal<bool>,
}

impl Api {
    pub fn new() -> Self {
        let base = if is_desktop() { load(KEY_SERVER).unwrap_or_default() } else { String::new() };
        Api {
            base: RwSignal::new(base),
            token: RwSignal::new(load(KEY_TOKEN)),
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
        self.token.set(token);
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.get_untracked(), path)
    }

    /// URL für Dateien, die direkt vom Browser geladen werden (img, iframe, Download).
    pub fn file_url(&self, path: &str) -> String {
        let mut u = self.url(path);
        if let Some(t) = self.token.get_untracked() {
            u.push(if u.contains('?') { '&' } else { '?' });
            u.push_str("token=");
            u.push_str(&t);
        }
        u
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
