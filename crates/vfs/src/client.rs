// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Blockierender HTTP-Client für die Büro-API (nur das, was das Laufwerk braucht).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use shared::{
    ChunkedUpload, ChunkedUploadStatus, Collection, CollectionInput, DocumentSummary, DocumentUpdate, ListResponse,
    UploadResult,
};
use ureq::unversioned::multipart::{Form, Part};

/// Dateien über dieser Größe werden in Stücken dieser Größe hochgeladen
/// (bleibt unter üblichen Proxy-Grenzen wie 100 MB)
const CHUNK_SIZE: u64 = 64 * 1024 * 1024;

/// Fehler einer API-Anfrage
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP-Status, `None` = Server nicht erreichbar
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "{} (HTTP {s})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

fn net(e: impl std::fmt::Display) -> ApiError {
    ApiError { status: None, message: format!("Server nicht erreichbar: {e}") }
}

/// Leser, der die gelesenen Bytes mitzählt
struct Counting<'a, R> {
    inner: R,
    count: &'a AtomicU64,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

type Resp = ureq::http::Response<ureq::Body>;

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let builder = ureq::Agent::config_builder();
        // Unter Windows ist nur das Feature "native-tls" aktiv (SChannel + Windows-Zertifikatsspeicher);
        // ureq wählt standardmäßig Rustls und bricht dann bei https ab.
        #[cfg(windows)]
        let builder = builder.tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        );
        let agent = builder
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .user_agent("Buero-Desktop")
            .build()
            .new_agent();
        Client { base: base.trim().trim_end_matches('/').to_string(), token: token.filter(|t| !t.is_empty()), agent }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn auth<B>(&self, rb: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => rb.header("Authorization", &format!("Bearer {t}")),
            None => rb,
        }
    }

    /// Prüft den Status und liefert bei Fehlern die Meldung des Servers.
    fn check(r: Result<Resp, ureq::Error>) -> ApiResult<Resp> {
        let mut r = r.map_err(net)?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(r);
        }
        let message = r
            .body_mut()
            .read_json::<shared::ApiError>()
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("Fehler {status}"));
        Err(ApiError { status: Some(status), message })
    }

    fn json<T: DeserializeOwned>(r: Result<Resp, ureq::Error>) -> ApiResult<T> {
        let mut r = Self::check(r)?;
        r.body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_json()
            .map_err(|e| ApiError { status: None, message: format!("Ungültige Antwort: {e}") })
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        Self::json(self.auth(self.agent.get(&self.url(path))).call())
    }

    /// Prüft Adresse und Anmeldung.
    pub fn ping(&self) -> ApiResult<()> {
        self.get::<shared::Stats>("/api/stats").map(|_| ())
    }

    pub fn collections(&self) -> ApiResult<Vec<Collection>> {
        self.get("/api/collections")
    }

    /// Alle Dokumente (seitenweise geladen)
    pub fn all_documents(&self) -> ApiResult<Vec<DocumentSummary>> {
        // Der Server liefert höchstens 500 je Seite (auch wenn mehr verlangt
        // wird). Deshalb nicht an der Seitengröße erkennen, ob Schluss ist,
        // sondern an `total`.
        const PAGE: usize = 500;
        let mut out: Vec<DocumentSummary> = Vec::new();
        loop {
            let r: ListResponse =
                self.get(&format!("/api/documents?sort=oldest&limit={PAGE}&offset={}", out.len()))?;
            let n = r.items.len();
            out.extend(r.items);
            if out.len() as u64 >= r.total {
                return Ok(out);
            }
            if n == 0 {
                // Unvollständig (z. B. währenddessen gelöscht): lieber diesmal
                // nicht abgleichen, als lokal Dateien als „am Server entfernt“
                // wegzuräumen
                return Err(ApiError {
                    status: None,
                    message: format!("Dokumentliste unvollständig ({} von {})", out.len(), r.total),
                });
            }
        }
    }

    /// Lädt die aktuelle Fassung eines Dokuments nach `dest` (über eine
    /// temporäre Datei, damit nie eine halbe Datei liegen bleibt).
    pub fn download(&self, id: i64, dest: &Path) -> ApiResult<u64> {
        self.download_progress(id, dest, &AtomicU64::new(0))
    }

    /// Wie [`Client::download`]; `received` zählt die bisher empfangenen Bytes
    /// mit (wer wartet, sieht daran, ob es noch vorangeht).
    pub fn download_progress(&self, id: i64, dest: &Path, received: &AtomicU64) -> ApiResult<u64> {
        static PART: AtomicU64 = AtomicU64::new(0);
        let req = self
            .agent
            .get(&self.url(&format!("/api/documents/{id}/file")))
            .config()
            // Hängt die Verbindung, nicht ewig warten
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(3600)))
            .build();
        let mut r = Self::check(self.auth(req).call())?;
        // Eigener Name je Vorgang: ein abgebrochener, noch hängender Vorgang
        // kommt einem neuen nicht in die Quere
        let part = PART.fetch_add(1, Ordering::Relaxed);
        let tmp = dest.with_extension(format!("part{}-{part}", std::process::id()));
        let io = |e: std::io::Error| ApiError { status: None, message: format!("Lokale Datei: {e}") };
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        let mut reader = Counting { inner: r.body_mut().with_config().limit(u64::MAX).reader(), count: received };
        let n = std::io::copy(&mut reader, &mut f).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            net(e)
        })?;
        f.flush().map_err(io)?;
        drop(f);
        std::fs::rename(&tmp, dest).map_err(io)?;
        Ok(n)
    }

    /// Neues Dokument hochladen
    pub fn upload(
        &self,
        file: &Path,
        filename: &str,
        title: &str,
        collection_ids: &[i64],
    ) -> ApiResult<DocumentSummary> {
        let ids = collection_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let mtime = mtime_of(file);
        let mut fields = vec![("title", title.to_string())];
        if !ids.is_empty() {
            fields.push(("collection_ids", ids));
        }
        if let Some(t) = mtime {
            fields.push(("file_mtime", t));
        }
        let res: Vec<UploadResult> = self.send_file(file, filename, "/api/documents", &fields)?;
        res.into_iter()
            .next()
            .map(|u| u.document)
            .ok_or_else(|| ApiError { status: None, message: "Leere Antwort beim Hochladen".into() })
    }

    /// Datei mit Formularfeldern an `path` schicken. Große Dateien gehen in
    /// Teilstücken voraus (`/api/uploads`), damit ein Proxy mit Größengrenze
    /// je Anfrage (z. B. NGINX `client_max_body_size`) sie durchlässt; das
    /// Formular nennt dann nur noch die Upload-Kennung. Ältere Server ohne
    /// diese Schnittstelle bekommen die Datei wie bisher am Stück.
    fn send_file<T: DeserializeOwned>(
        &self,
        file: &Path,
        filename: &str,
        path: &str,
        fields: &[(&str, String)],
    ) -> ApiResult<T> {
        let form = || fields.iter().fold(Form::new(), |f, (k, v)| f.text(k, v));
        let len = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        if len > CHUNK_SIZE {
            match self.upload_chunked(file, len) {
                Ok(id) => {
                    let form = form().part("upload_id", Part::text(&id).file_name(filename));
                    let res = Self::json(self.auth(self.agent.post(&self.url(path))).send(form));
                    if res.is_err() {
                        self.discard_chunked(&id);
                    }
                    return res;
                }
                // Server kennt keine Teil-Uploads: am Stück versuchen
                Err(e) if matches!(e.status, Some(404 | 405)) => {}
                Err(e) => return Err(e),
            }
        }
        let mut reader = open(file)?;
        let form = form().part("file", Part::reader(&mut reader).file_name(filename));
        Self::json(self.auth(self.agent.post(&self.url(path))).send(form))
    }

    /// Lädt `file` in Stücken hoch und liefert die Upload-Kennung. Ein
    /// abgebrochenes Stück wird ein paar Mal wiederholt.
    fn upload_chunked(&self, file: &Path, len: u64) -> ApiResult<String> {
        use std::io::{Seek, SeekFrom};
        let start: ChunkedUpload = Self::json(self.auth(self.agent.post(&self.url("/api/uploads"))).send_empty())?;
        let id = start.id;
        let io = |e: std::io::Error| ApiError { status: None, message: format!("Lokale Datei: {e}") };
        let res = (|| {
            let mut f = open(file)?;
            let mut offset = 0u64;
            while offset < len {
                let n = CHUNK_SIZE.min(len - offset);
                let mut tries = 0;
                loop {
                    f.seek(SeekFrom::Start(offset)).map_err(io)?;
                    let req = self
                        .agent
                        .put(&self.url(&format!("/api/uploads/{id}?offset={offset}")))
                        .header("Content-Type", "application/octet-stream")
                        .header("Content-Length", &n.to_string());
                    let r = self.auth(req).send(ureq::SendBody::from_reader(&mut (&mut f).take(n)));
                    match Self::json::<ChunkedUploadStatus>(r) {
                        Ok(s) if s.size == offset + n => break,
                        Ok(s) => {
                            return Err(ApiError {
                                status: None,
                                message: format!("Upload: Server hat {} statt {} Bytes", s.size, offset + n),
                            })
                        }
                        // Netzwerk oder Serverfehler: nochmal (der Server hat das halbe Stück verworfen)
                        Err(e) if tries < 3 && e.status.is_none_or(|s| s >= 500) => {
                            tries += 1;
                            std::thread::sleep(Duration::from_secs(2 * tries));
                        }
                        Err(e) => return Err(e),
                    }
                }
                offset += n;
            }
            Ok(())
        })();
        match res {
            Ok(()) => Ok(id),
            Err(e) => {
                self.discard_chunked(&id);
                Err(e)
            }
        }
    }

    fn discard_chunked(&self, id: &str) {
        let _ = self.auth(self.agent.delete(&self.url(&format!("/api/uploads/{id}")))).call();
    }

    /// Neue Version hochladen. `basis`: Version, auf der die Änderung beruht
    /// (Server lehnt mit 409 ab, wenn es inzwischen eine neuere gibt).
    pub fn new_version(&self, id: i64, file: &Path, filename: &str, basis: Option<i64>) -> ApiResult<DocumentSummary> {
        let mtime = mtime_of(file);
        let mut path = format!("/api/documents/{id}/versions");
        if let Some(b) = basis {
            path.push_str(&format!("?basis={b}"));
        }
        let fields: Vec<_> = mtime.into_iter().map(|t| ("file_mtime", t)).collect();
        self.send_file(file, filename, &path, &fields)
    }

    pub fn update_document(&self, id: i64, u: &DocumentUpdate) -> ApiResult<DocumentSummary> {
        Self::json(self.auth(self.agent.patch(&self.url(&format!("/api/documents/{id}")))).send_json(u))
    }

    pub fn delete_document(&self, id: i64) -> ApiResult<()> {
        Self::check(self.auth(self.agent.delete(&self.url(&format!("/api/documents/{id}")))).call()).map(|_| ())
    }

    pub fn create_collection(&self, name: &str, parent: Option<i64>) -> ApiResult<Collection> {
        let input = CollectionInput { name: Some(name.to_string()), parent_id: parent, color: None, ai_locked: None };
        Self::json(self.auth(self.agent.post(&self.url("/api/collections"))).send_json(&input))
    }

    pub fn update_collection(&self, id: i64, input: &CollectionInput) -> ApiResult<Collection> {
        Self::json(self.auth(self.agent.patch(&self.url(&format!("/api/collections/{id}")))).send_json(input))
    }

    pub fn delete_collection(&self, id: i64) -> ApiResult<()> {
        Self::check(self.auth(self.agent.delete(&self.url(&format!("/api/collections/{id}")))).call()).map(|_| ())
    }
}

/// Änderungsdatum einer Datei in Unix-Sekunden (für den Server als Text)
fn mtime_of(file: &Path) -> Option<String> {
    let t = std::fs::metadata(file).ok()?.modified().ok()?;
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    (secs > 0).then(|| secs.to_string())
}

fn open(file: &Path) -> ApiResult<std::fs::File> {
    std::fs::File::open(file).map_err(|e| ApiError { status: None, message: format!("Lokale Datei: {e}") })
}
