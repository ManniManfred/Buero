// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Schutzmaßnahmen:
//!
//! * Bremse für Fehlversuche (Passwort-Raten) pro IP-Adresse
//! * Sitzungs-Epoche: Passwortwechsel oder „Überall abmelden“ macht alle
//!   Anmeldungen und Datei-Links ungültig
//! * kurzlebige, signierte Datei-Links statt des Sitzungs-Tokens in URLs
//! * HTTP-Sicherheits-Header (CSP, Einbetten nur von der eigenen Seite, …)
//! * Passwort-Hashes mit Argon2

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Mutex;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use shared::FileKey;

use crate::error::AppError;
use crate::sign;
use crate::state::AppState;
use crate::util::now;

// ---------------------------------------------------------------------------
// Passwort-Hashes

/// Argon2id-Hash (PHC-Format, enthält Salz und Parameter)
pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .unwrap_or_default()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// Wie [`verify_password`], aber ohne den Async-Thread zu blockieren
pub async fn verify_password_async(pw: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&pw, &hash)).await.unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Client-Adresse

/// Adresse des Aufrufers. Kommt die Verbindung aus einem internen Netz (Reverse
/// Proxy wie CapRover/nginx), gilt der letzte Eintrag in `X-Forwarded-For` –
/// den hat der Proxy selbst angehängt, er lässt sich nicht fälschen.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let ip = unmap(peer.ip());
    if !is_internal(ip) {
        return ip;
    }
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .fold(None, |_, ip| Some(ip));
    let real = || headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    forwarded.or_else(real).map(unmap).unwrap_or(ip)
}

fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_private() || v.is_link_local(),
        IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// IPv6: ganzes /64-Netz zusammenfassen (sonst wechselt man einfach die Adresse)
fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => {
            let s = v.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

// ---------------------------------------------------------------------------
// Bremse für Fehlversuche

/// Kostenlose Fehlversuche, danach wächst die Sperre: 30 s, 1 min, 2 min, … bis 1 h
const FREE_FAILS: u32 = 5;
const MAX_BLOCK: i64 = 3600;
/// Nach einem Tag ohne Fehlversuch wird der Zähler zurückgesetzt
const FORGET: i64 = 86400;

#[derive(Default)]
struct Entry {
    fails: u32,
    last_fail: i64,
    blocked_until: i64,
    /// Es läuft gerade eine Prüfung von dieser Adresse (keine parallelen Versuche)
    busy: bool,
}

pub struct Limiter {
    what: &'static str,
    map: Mutex<HashMap<IpAddr, Entry>>,
}

impl Limiter {
    pub fn new(what: &'static str) -> Self {
        Limiter { what, map: Mutex::new(HashMap::new()) }
    }

    /// Einen Versuch beginnen – abgelehnt, solange die Adresse gesperrt ist
    /// oder von ihr schon ein Versuch läuft.
    pub fn begin(&self, ip: IpAddr) -> Result<Attempt<'_>, AppError> {
        let ip = bucket(ip);
        let t = now();
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, e| e.busy || t - e.last_fail < FORGET);
        }
        let e = m.entry(ip).or_default();
        if e.fails > 0 && t - e.last_fail > FORGET {
            e.fails = 0;
        }
        if e.blocked_until > t {
            return Err(too_many(e.blocked_until - t));
        }
        if e.busy {
            return Err(AppError::new(StatusCode::TOO_MANY_REQUESTS, "Bitte einen Moment warten und erneut versuchen"));
        }
        e.busy = true;
        Ok(Attempt { lim: self, ip })
    }
}

fn too_many(secs: i64) -> AppError {
    let msg = if secs < 90 {
        format!("Zu viele Fehlversuche – bitte in {secs} Sekunden erneut versuchen")
    } else {
        format!("Zu viele Fehlversuche – bitte in {} Minuten erneut versuchen", (secs + 59) / 60)
    };
    AppError::new(StatusCode::TOO_MANY_REQUESTS, msg)
}

pub struct Attempt<'a> {
    lim: &'a Limiter,
    ip: IpAddr,
}

impl Attempt<'_> {
    pub fn success(self) {
        if let Some(e) = self.lim.map.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&self.ip) {
            e.fails = 0;
            e.blocked_until = 0;
        }
    }

    /// Fehlversuch zählen; ab dem sechsten wird die Adresse gesperrt.
    pub async fn failed(self) {
        let t = now();
        let fails = {
            let mut m = self.lim.map.lock().unwrap_or_else(|e| e.into_inner());
            let e = m.entry(self.ip).or_default();
            e.fails += 1;
            e.last_fail = t;
            if e.fails >= FREE_FAILS {
                let step = (e.fails - FREE_FAILS).min(7);
                e.blocked_until = t + (30i64 << step).min(MAX_BLOCK);
            }
            e.fails
        };
        if fails >= FREE_FAILS {
            tracing::warn!("{}: {fails} Fehlversuche von {} – vorübergehend gesperrt", self.lim.what, self.ip);
        }
        // Jeder Fehlversuch dauert etwas (die Prüfung gilt bis dahin als laufend)
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if let Some(e) = self.lim.map.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&self.ip) {
            e.busy = false;
        }
    }
}

/// Zählt Uploads pro Freigabe und Stunde
#[derive(Default)]
pub struct UploadCounter(Mutex<HashMap<i64, (i64, u32)>>);

impl UploadCounter {
    /// Bisher in dieser Stunde hochgeladene Dateien
    pub fn used(&self, share: i64) -> u32 {
        let t = now();
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(&share) {
            Some((start, n)) if t - start < 3600 => *n,
            _ => 0,
        }
    }

    pub fn add(&self, share: i64, n: u32) {
        let t = now();
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (start, _)| t - *start < 3600);
        let e = m.entry(share).or_insert((t, 0));
        e.1 += n;
    }
}

// ---------------------------------------------------------------------------
// Sitzungs-Epoche

const AUTH_FILE: &str = "auth.json";

#[derive(Serialize, Deserialize, Default)]
struct AuthFile {
    /// Argon2-Hash des Passworts, mit dem die Sitzungen angelegt wurden
    #[serde(default)]
    password_hash: String,
    /// Zufallswert, der in jeden Datei-Link eingeht
    #[serde(default)]
    epoch: String,
}

fn random_hex() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

async fn write_auth(data_dir: &Path, f: &AuthFile) -> anyhow::Result<()> {
    let path = data_dir.join(AUTH_FILE);
    let tmp = data_dir.join(format!("{AUTH_FILE}.tmp"));
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(f)?).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
    }
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Beim Start: Wurde `BUERO_PASSWORD` geändert, alle Sitzungen beenden.
/// Liefert die aktuelle Epoche.
pub async fn init_auth(data_dir: &Path, password: Option<&str>, pool: &sqlx::SqlitePool) -> anyhow::Result<String> {
    let mut f: AuthFile = tokio::fs::read(data_dir.join(AUTH_FILE))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut dirty = false;
    if f.epoch.is_empty() {
        f.epoch = random_hex();
        dirty = true;
    }
    if let Some(pw) = password {
        let changed = !f.password_hash.is_empty() && !verify_password_async(pw.to_string(), f.password_hash.clone()).await;
        if changed {
            let n = sqlx::query("DELETE FROM sessions").execute(pool).await?.rows_affected();
            f.epoch = random_hex();
            tracing::warn!("BUERO_PASSWORD wurde geändert – {n} Anmeldung(en) beendet");
        }
        if changed || f.password_hash.is_empty() {
            let pw = pw.to_string();
            f.password_hash = tokio::task::spawn_blocking(move || hash_password(&pw)).await?;
            dirty = true;
        }
    }
    if dirty {
        write_auth(data_dir, &f).await?;
    }
    Ok(f.epoch)
}

/// Alle Anmeldungen und Datei-Links ungültig machen („Überall abmelden“)
pub async fn logout_everywhere(st: &AppState) -> Result<u64, AppError> {
    let n = sqlx::query("DELETE FROM sessions").execute(&st.pool).await?.rows_affected();
    let epoch = random_hex();
    let mut f: AuthFile = tokio::fs::read(st.config.data_dir.join(AUTH_FILE))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    f.epoch = epoch.clone();
    write_auth(&st.config.data_dir, &f).await.map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *st.epoch.write().unwrap_or_else(|e| e.into_inner()) = epoch;
    tracing::info!("Überall abgemeldet ({n} Anmeldung(en) beendet)");
    Ok(n)
}

fn epoch(st: &AppState) -> String {
    st.epoch.read().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---------------------------------------------------------------------------
// Signierte Links

/// Datei-Schlüssel gelten in Zeitfenstern von 6 Stunden (6–12 h ab Ausgabe),
/// damit Vorschaubilder im Browser-Cache bleiben.
const FILE_KEY_WINDOW: i64 = 6 * 3600;

pub fn file_key(st: &AppState) -> FileKey {
    if st.config.password.is_none() {
        return FileKey::default();
    }
    let exp = (now() / FILE_KEY_WINDOW + 2) * FILE_KEY_WINDOW;
    let sig = sign::sign(&st.secret, &format!("files:{}:{exp}", epoch(st)));
    FileKey { key: format!("{exp}.{sig}"), expires_at: exp }
}

fn check_file_key(st: &AppState, key: &str) -> bool {
    let Some((exp, sig)) = key.split_once('.') else { return false };
    let Ok(exp) = exp.parse::<i64>() else { return false };
    let t = now();
    exp > t && exp <= t + 2 * FILE_KEY_WINDOW + 60 && sign::verify(&st.secret, &format!("files:{}:{exp}", epoch(st)), sig)
}

/// Pfad samt Signatur, `secs` Sekunden gültig (nur für GET)
pub fn signed_path(st: &AppState, path: &str, secs: i64) -> String {
    let exp = now() + secs;
    let sig = sign::sign(&st.secret, &format!("get:{}:{path}:{exp}", epoch(st)));
    format!("{path}?exp={exp}&sig={sig}")
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v))
}

/// Darf diese Anfrage ohne Sitzungs-Token durch? (Datei-Schlüssel `fk` für
/// Dateien/Vorschaubilder/Text oder ein signierter Link)
pub fn signed_access(st: &AppState, req: &Request) -> bool {
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return false;
    }
    let path = req.uri().path();
    let query = req.uri().query().unwrap_or("");
    let file_path = path.starts_with("/api/documents/")
        && (path.ends_with("/file") || path.ends_with("/thumbnail") || path.ends_with("/text"));
    if file_path && query_param(query, "fk").is_some_and(|k| check_file_key(st, k)) {
        return true;
    }
    match (query_param(query, "exp").and_then(|e| e.parse::<i64>().ok()), query_param(query, "sig")) {
        (Some(exp), Some(sig)) => exp >= now() && sign::verify(&st.secret, &format!("get:{}:{path}:{exp}", epoch(st)), sig),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// HTTP-Header

/// Herkunft der Oberfläche im Desktop-Client (Tauri: macOS/Linux, Windows)
pub const DESKTOP_ORIGINS: [&str; 3] = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];

/// `frame-ancestors` für Dokumentdateien: außer der Webseite selbst darf auch
/// der Desktop-Client sie einbetten (Vorschau), dessen Oberfläche von einem
/// eigenen Ursprung kommt. `X-Frame-Options` kennt keine solche Liste und
/// entfällt dann (siehe [`security_headers`]).
pub fn file_frame_ancestors() -> String {
    format!("frame-ancestors 'self' {}", DESKTOP_ORIGINS.join(" "))
}

/// Content-Security-Policy für die Oberfläche. Inline-Skripte (Startskript von
/// trunk) werden per Hash erlaubt, Euro-Office per Herkunft.
pub fn build_csp(web_dir: &Path, office_url: Option<&str>) -> HeaderValue {
    let mut scripts = vec!["'self'".to_string(), "'wasm-unsafe-eval'".to_string()];
    if let Ok(html) = std::fs::read_to_string(web_dir.join("index.html")) {
        let mut rest = html.as_str();
        while let Some(start) = rest.find("<script") {
            let after = &rest[start..];
            let Some(tag_end) = after.find('>') else { break };
            let tag = &after[..tag_end];
            let body_start = &after[tag_end + 1..];
            let Some(close) = body_start.find("</script>") else { break };
            if !tag.contains("src=") {
                let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&body_start.as_bytes()[..close]));
                scripts.push(format!("'sha256-{hash}'"));
            }
            rest = &body_start[close..];
        }
    }
    let office = office_url.map(origin).unwrap_or_default();
    if !office.is_empty() {
        scripts.push(office.clone());
    }
    let csp = format!(
        "default-src 'self'; script-src {}; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
         media-src 'self' blob: mediastream:; connect-src 'self' blob: {office}; frame-src 'self' {office}; \
         worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'",
        scripts.join(" ")
    );
    HeaderValue::from_str(csp.split_whitespace().collect::<Vec<_>>().join(" ").as_str())
        .unwrap_or_else(|_| HeaderValue::from_static("frame-ancestors 'self'"))
}

/// `https://host:port/pfad` → `https://host:port`
fn origin(url: &str) -> String {
    let Some(i) = url.find("://") else { return String::new() };
    let rest = &url[i + 3..];
    let end = rest.find('/').unwrap_or(rest.len());
    format!("{}{}", &url[..i + 3], &rest[..end])
}

pub async fn security_headers(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let https = st.config.public_url.as_deref().is_some_and(|u| u.starts_with("https://"))
        || req.headers().get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.entry(header::X_CONTENT_TYPE_OPTIONS).or_insert(HeaderValue::from_static("nosniff"));
    // Legt die Antwort selbst fest, wer sie einbetten darf, gilt das statt
    // X-Frame-Options (das sonst Vorrang hätte und keine Ausnahmen kennt)
    let own_ancestors = h
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("frame-ancestors"));
    if !own_ancestors {
        h.entry(header::X_FRAME_OPTIONS).or_insert(HeaderValue::from_static("SAMEORIGIN"));
    }
    h.entry(header::REFERRER_POLICY).or_insert(HeaderValue::from_static("same-origin"));
    if https {
        h.entry(header::STRICT_TRANSPORT_SECURITY).or_insert(HeaderValue::from_static("max-age=31536000"));
    }
    let html = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    // Tupfer Designer (eigene Anwendung) und Dateien der API bringen ihre eigenen Regeln mit
    if html && !path.starts_with("/api/") && !path.starts_with("/tupfer") && !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(header::CONTENT_SECURITY_POLICY, st.csp.clone());
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password("geheim");
        assert!(h.starts_with("$argon2"));
        assert!(verify_password("geheim", &h));
        assert!(!verify_password("falsch", &h));
        assert!(!verify_password("geheim", "kaputt"));
    }

    #[test]
    fn ip_behind_proxy() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 1.2.3.4".parse().unwrap());
        // vom Proxy: letzter Eintrag
        assert_eq!(client_ip("10.0.1.5:80".parse().unwrap(), &h), "1.2.3.4".parse::<IpAddr>().unwrap());
        // direkt aus dem Internet: Header wird ignoriert
        assert_eq!(client_ip("9.9.9.9:80".parse().unwrap(), &h), "9.9.9.9".parse::<IpAddr>().unwrap());
        assert_eq!(bucket("2001:db8:1:2:3:4:5:6".parse().unwrap()), "2001:db8:1:2::".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn limiter_blocks() {
        let l = Limiter::new("Test");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        {
            let a = l.begin(ip).unwrap();
            // parallel: abgelehnt
            assert!(l.begin(ip).is_err());
            a.success();
        }
        for _ in 0..FREE_FAILS {
            let a = l.begin(ip).unwrap();
            a.failed().await;
        }
        assert_eq!(l.begin(ip).err().map(|e| e.status), Some(StatusCode::TOO_MANY_REQUESTS));
        // andere Adresse unberührt
        assert!(l.begin("5.6.7.8".parse().unwrap()).is_ok());
    }

    #[test]
    fn csp_origin() {
        assert_eq!(origin("https://office.example.com/x/y"), "https://office.example.com");
        assert_eq!(origin("http://h:8080"), "http://h:8080");
    }
}
