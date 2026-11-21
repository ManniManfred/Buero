// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Datensicherung und Wiederherstellung.
//!
//! Ein Backup ist ein tar-Archiv mit `manifest.json`, einer konsistenten Kopie
//! der Datenbank (`buero.db`, per `VACUUM INTO`), `secret.key`, den
//! Originaldateien (`files/`) und Vorschaubildern (`thumbs/`). Der Suchindex
//! wird nicht gesichert, sondern nach der Wiederherstellung neu aufgebaut.
//!
//! Eine Wiederherstellung wird erst geprüft und nach `restore/` entpackt; der
//! Server startet dann im selben Prozess neu und tauscht die Daten beim Start
//! aus. Der vorherige Stand bleibt in `vor-wiederherstellung/` liegen.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::TryStreamExt;
use shared::RestoreResult;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::mpsc;
use tokio_util::io::{StreamReader, SyncIoBridge};

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util::now;

const FORMAT: &str = "buero-backup";
const FORMAT_VERSION: u64 = 1;
const RESTORE_DIR: &str = "restore";
const RESTORE_TMP: &str = "restore.tmp";
pub const PREVIOUS_DIR: &str = "vor-wiederherstellung";
/// Teile des Datenverzeichnisses, die gesichert und beim Wiederherstellen ersetzt werden
const PARTS: [&str; 4] = ["buero.db", "secret.key", "files", "thumbs"];
const CHUNK: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Backup

/// Gibt den geschriebenen Archivstrom in Blöcken an den Antwortkörper weiter.
struct ChanWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Download abgebrochen"))
    }
}

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

/// Datum `JJJJ-MM-TT` (UTC) aus Unix-Sekunden.
fn date_string(ts: i64) -> String {
    let z = ts.div_euclid(86400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub async fn backup(State(st): State<AppState>) -> AppResult<Response> {
    let data = st.config.data_dir.clone();
    let snapshot = st.storage.tmp_dir().join(format!("backup-{:016x}.db", rand::random::<u64>()));
    sqlx::query("VACUUM INTO ?")
        .bind(snapshot.to_string_lossy().into_owned())
        .execute(&st.pool)
        .await?;
    let documents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM documents").fetch_one(&st.pool).await?;
    let created_at = now();
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "created_at": created_at,
        "documents": documents,
    }))
    .map_err(anyhow::Error::from)?;

    let (tx, mut rx) = mpsc::channel::<io::Result<Bytes>>(8);
    tokio::task::spawn_blocking(move || {
        let mut w = ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(CHUNK) };
        let res = write_archive(&mut w, &data, &snapshot, &manifest, created_at);
        let _ = std::fs::remove_file(&snapshot);
        if let Err(e) = res {
            tracing::warn!("Backup abgebrochen: {e:#}");
            // Fehler im Strom bricht den Download ab, statt ein unvollständiges Archiv zu liefern
            let _ = tx.blocking_send(Err(io::Error::other(e.to_string())));
        } else {
            tracing::info!("Backup erstellt ({documents} Dokumente)");
        }
    });
    let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));

    let name = format!("buero-backup-{}.tar", date_string(created_at));
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn write_archive(w: &mut ChanWriter, data: &Path, snapshot: &Path, manifest: &[u8], mtime: i64) -> anyhow::Result<()> {
    let mut tar = tar::Builder::new(w);
    tar.follow_symlinks(false);
    let mut h = tar::Header::new_gnu();
    h.set_size(manifest.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(mtime as u64);
    tar.append_data(&mut h, "manifest.json", manifest)?;
    tar.append_path_with_name(snapshot, "buero.db")?;
    for part in &PARTS[1..] {
        let p = data.join(part);
        if p.is_dir() {
            tar.append_dir_all(part, &p)?;
        } else if p.is_file() {
            tar.append_path_with_name(&p, part)?;
        }
    }
    tar.into_inner()?.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Wiederherstellung

pub async fn restore(State(st): State<AppState>, body: Body) -> AppResult<Json<RestoreResult>> {
    if st.restoring.swap(true, Ordering::SeqCst) {
        return Err(AppError::new(StatusCode::CONFLICT, "Eine Wiederherstellung läuft bereits"));
    }
    let tmp = st.config.data_dir.join(RESTORE_TMP);
    let res = prepare(&st, body, &tmp).await;
    match res {
        Ok(r) => {
            let restart = st.restart.clone();
            tokio::spawn(async move {
                // Antwort zuerst ausliefern
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                restart.cancel();
            });
            Ok(Json(r))
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            st.restoring.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

async fn prepare(st: &AppState, body: Body, tmp: &Path) -> AppResult<RestoreResult> {
    let _ = tokio::fs::remove_dir_all(tmp).await;
    tokio::fs::create_dir_all(tmp).await?;

    let reader = SyncIoBridge::new(StreamReader::new(body.into_data_stream().map_err(io::Error::other)));
    let dest = tmp.to_path_buf();
    tokio::task::spawn_blocking(move || unpack(reader, &dest))
        .await?
        .map_err(|e| AppError::bad_request(format!("Backup nicht lesbar: {e:#}")))?;

    let manifest: serde_json::Value = tokio::fs::read(tmp.join("manifest.json"))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|m: &serde_json::Value| m["format"] == FORMAT)
        .ok_or_else(|| AppError::bad_request("Das ist kein Büro-Backup (manifest.json fehlt)"))?;
    if manifest["format_version"].as_u64().unwrap_or(0) > FORMAT_VERSION {
        return Err(AppError::bad_request("Das Backup stammt von einer neueren Büro-Version"));
    }
    if !tokio::fs::try_exists(tmp.join("buero.db")).await.unwrap_or(false) {
        return Err(AppError::bad_request("Das Backup enthält keine Datenbank"));
    }

    let (documents, missing_files) = check_database(st, tmp).await?;

    let staged = st.config.data_dir.join(RESTORE_DIR);
    let _ = tokio::fs::remove_dir_all(&staged).await;
    tokio::fs::rename(tmp, &staged).await?;
    tracing::info!("Backup geprüft ({documents} Dokumente) – Server startet zur Wiederherstellung neu");
    Ok(RestoreResult { documents, missing_files, created_at: manifest["created_at"].as_i64() })
}

/// Entpackt nur bekannte Bestandteile (normale Dateien und Ordner) ins Zielverzeichnis.
fn unpack<R: Read>(reader: R, dest: &Path) -> anyhow::Result<()> {
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(false);
    ar.set_preserve_mtime(true);
    ar.set_overwrite(true);
    for entry in ar.entries()? {
        let mut e = entry?;
        let path = e.path()?.into_owned();
        let top = path.components().next().and_then(|c| c.as_os_str().to_str()).unwrap_or("");
        let known = top == "manifest.json" || PARTS.contains(&top);
        let kind = e.header().entry_type();
        if !known || !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        if !e.unpack_in(dest)? {
            anyhow::bail!("Ungültiger Pfad im Archiv: {}", path.display());
        }
    }
    Ok(())
}

/// Prüft die Datenbank des Backups, übernimmt die aktuellen Anmeldungen und
/// zählt Dokumente und fehlende Dateien.
async fn check_database(st: &AppState, dir: &Path) -> AppResult<(i64, i64)> {
    let bad = |m: &str| AppError::bad_request(m.to_string());
    let url = format!("sqlite://{}", dir.join("buero.db").display());
    let opts = SqliteConnectOptions::from_str(&url).map_err(anyhow::Error::from)?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|_| bad("Die Datenbank im Backup ist nicht lesbar"))?;

    let res: AppResult<(i64, i64)> = async {
        let check: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&pool)
            .await
            .map_err(|_| bad("Die Datenbank im Backup ist beschädigt"))?;
        if check != "ok" {
            return Err(bad("Die Datenbank im Backup ist beschädigt"));
        }
        let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(&pool)
            .await
            .map_err(|_| bad("Die Datenbank im Backup stammt nicht von Büro"))?;
        let known: HashSet<i64> = sqlx::migrate!("./migrations").iter().map(|m| m.version).collect();
        if applied.iter().any(|v| !known.contains(v)) {
            return Err(bad("Das Backup stammt von einer neueren Büro-Version – bitte Büro zuerst aktualisieren"));
        }
        let documents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM documents").fetch_one(&pool).await?;

        // Wer gerade angemeldet ist, bleibt es nach der Wiederherstellung
        let sessions: Vec<(String, i64, i64)> =
            sqlx::query_as("SELECT token_hash, created_at, expires_at FROM sessions WHERE expires_at > ?")
                .bind(now())
                .fetch_all(&st.pool)
                .await?;
        for (hash, created, expires) in sessions {
            sqlx::query("INSERT OR IGNORE INTO sessions (token_hash, created_at, expires_at) VALUES (?, ?, ?)")
                .bind(hash)
                .bind(created)
                .bind(expires)
                .execute(&pool)
                .await?;
        }

        let shas: Vec<String> =
            sqlx::query_scalar("SELECT sha256 FROM documents UNION SELECT sha256 FROM document_versions")
                .fetch_all(&pool)
                .await?;
        let mut missing = 0;
        for sha in shas.iter().filter(|s| s.len() > 2) {
            if !tokio::fs::try_exists(dir.join("files").join(&sha[..2]).join(sha)).await.unwrap_or(false) {
                missing += 1;
            }
        }
        Ok((documents, missing))
    }
    .await;
    pool.close().await;
    res
}

/// Beim Start: eine vorbereitete Wiederherstellung übernehmen. Der bisherige
/// Stand wandert nach `vor-wiederherstellung/`, der Suchindex wird verworfen.
pub async fn apply_pending(data: &Path) -> anyhow::Result<bool> {
    let staged = data.join(RESTORE_DIR);
    if !tokio::fs::try_exists(staged.join("buero.db")).await.unwrap_or(false) {
        return Ok(false);
    }
    let previous = data.join(PREVIOUS_DIR);
    let _ = tokio::fs::remove_dir_all(&previous).await;
    tokio::fs::create_dir_all(&previous).await?;
    // Datenbank, Dateien und Vorschaubilder gehören zusammen; den Schlüssel nur
    // ersetzen, wenn das Backup einen enthält.
    let has_key = tokio::fs::try_exists(staged.join("secret.key")).await.unwrap_or(false);
    for name in ["buero.db", "buero.db-wal", "buero.db-shm", "files", "thumbs", "secret.key"] {
        if name == "secret.key" && !has_key {
            continue;
        }
        let p = data.join(name);
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            tokio::fs::rename(&p, previous.join(name)).await?;
        }
    }
    for name in PARTS {
        let p = staged.join(name);
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            tokio::fs::rename(&p, data.join(name)).await?;
        }
    }
    let _ = tokio::fs::remove_dir_all(data.join("index")).await;
    tokio::fs::remove_dir_all(&staged).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::date_string(0), "1970-01-01");
        assert_eq!(super::date_string(1_795_000_000), "2026-11-18");
        assert_eq!(super::date_string(951_782_400), "2000-02-29");
    }
}
