// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! PDF-Vorschau für Office-Dateien (Word, Excel, PowerPoint, OpenDocument …)
//! über den Umwandlungsdienst von Euro-Office (`ConvertService.ashx`).
//!
//! Der Document Server holt die Datei über denselben signierten Link wie beim
//! Bearbeiten ab und liefert ein PDF, das Büro je Dateiinhalt zwischenspeichert
//! (`previews/<sha256>.pdf`). Daraus entsteht auch das Vorschaubild.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use axum::http::header;
use futures_util::TryStreamExt;
use serde_json::{json, Value};

use crate::db::DocRow;
use crate::state::AppState;

/// Längste Wartezeit auf eine Umwandlung
const CONVERT_TIMEOUT: Duration = Duration::from_secs(180);

/// Immer nur eine Umwandlung gleichzeitig – schont den Document Server, etwa
/// beim Übernehmen vieler Dateien
static CONVERT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Kann für dieses Dokument eine PDF-Vorschau erzeugt werden?
pub fn available(st: &AppState, doc: &DocRow) -> bool {
    st.config.office_url.is_some() && shared::office_previewable(&doc.extension)
}

/// Liefert die PDF-Vorschau des Dokuments und erzeugt sie bei Bedarf.
/// `base`: Adresse, unter der der Document Server Büro erreicht.
/// `task`: Eintrag der Aufgabenliste, der den Schritt anzeigt (sonst ein eigener).
pub async fn ensure(st: &AppState, doc: &DocRow, base: &str, task: Option<&crate::tasks::Task>) -> Result<PathBuf> {
    let path = st.storage.preview_path(&doc.sha256);
    if exists(&path).await {
        return Ok(path);
    }
    // Eigener Eintrag, sofern nicht schon die Verarbeitung des Dokuments dort steht
    let own = (task.is_none() && !crate::tasks::has_document(doc.id))
        .then(|| crate::tasks::start("preview", &doc.title, "", Some(doc.id)));
    let task = task.or(own.as_ref());
    let phase = |p: &str| {
        if let Some(t) = task {
            t.phase(p);
        }
    };
    phase("Wartet auf Euro-Office");
    let _guard = CONVERT_LOCK.lock().await;
    // Inzwischen von einer anderen Anfrage erzeugt?
    if exists(&path).await {
        return Ok(path);
    }
    phase("PDF-Vorschau über Euro-Office");
    let started = Instant::now();
    let url = convert(st, doc, base).await?;
    download(st, &url, &path).await?;
    tracing::info!("PDF-Vorschau für Dokument {} erzeugt in {:.1?}", doc.id, started.elapsed());
    Ok(path)
}

/// Vorschaubild aus der PDF-Vorschau erzeugen (nur, solange das Dokument
/// noch denselben Inhalt hat)
pub async fn make_thumbnail(st: &AppState, doc: &DocRow, pdf: &std::path::Path) -> Result<bool> {
    let thumb = st.storage.thumb_path(doc.id);
    if !crate::extract::thumbnail(pdf, "pdf", &thumb, &st.storage.tmp_dir()).await? {
        return Ok(false);
    }
    let r = sqlx::query("UPDATE documents SET has_thumbnail = 1 WHERE id = ? AND sha256 = ?")
        .bind(doc.id)
        .bind(&doc.sha256)
        .execute(&st.pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

async fn exists(path: &std::path::Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// Lässt den Document Server umwandeln und liefert die Adresse des PDFs.
async fn convert(st: &AppState, doc: &DocRow, base: &str) -> Result<String> {
    let office = st.config.office_url.as_deref().context("Euro-Office ist nicht eingerichtet")?;
    let ext = doc.extension.to_ascii_lowercase();
    let mut body = json!({
        "async": true,
        "filetype": ext,
        // Gleicher Inhalt → gleicher Schlüssel (der Document Server merkt sich Ergebnisse)
        "key": format!("pv-{}", &doc.sha256[..40.min(doc.sha256.len())]),
        "outputtype": "pdf",
        "title": doc.original_filename,
        "url": crate::api::office::file_link(st, base, doc.id, doc.version),
    });
    if shared::office_document_type(&ext) == Some("cell") {
        // Breite Tabellen auf Seitenbreite verkleinern statt auf viele Seiten zu verteilen
        body["spreadsheetLayout"] = json!({ "fitToWidth": 1, "fitToHeight": 0 });
    }
    if let Some(secret) = &st.config.office_jwt_secret {
        body["token"] = Value::String(crate::sign::jwt_encode(secret, &body));
    }

    let endpoint = format!("{office}/ConvertService.ashx");
    let deadline = Instant::now() + CONVERT_TIMEOUT;
    loop {
        let res: Value = st
            .http
            .post(&endpoint)
            .header(header::ACCEPT, "application/json")
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .context("Euro-Office nicht erreichbar")?
            .json()
            .await
            .context("Unerwartete Antwort von Euro-Office")?;
        if let Some(code) = res.get("error").and_then(Value::as_i64) {
            bail!("Euro-Office: {} (Fehler {code})", error_text(code));
        }
        if res.get("endConvert").and_then(Value::as_bool) == Some(true) {
            return res
                .get("fileUrl")
                .and_then(Value::as_str)
                .map(str::to_string)
                .context("Euro-Office lieferte keine Datei-Adresse");
        }
        if Instant::now() > deadline {
            bail!("Euro-Office: Umwandlung dauert zu lange");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn download(st: &AppState, url: &str, dest: &std::path::Path) -> Result<()> {
    let resp = st
        .http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("Download der Vorschau von Euro-Office")?;
    let stream = resp.bytes_stream().map_err(|e| e.to_string());
    let staged = st.storage.stage(Box::pin(stream), st.config.max_upload_bytes).await?;
    let mut head = [0u8; 5];
    let is_pdf = {
        use tokio::io::AsyncReadExt;
        let mut f = tokio::fs::File::open(&staged.tmp_path).await?;
        f.read_exact(&mut head).await.is_ok() && &head == b"%PDF-"
    };
    if !is_pdf {
        st.storage.discard(&staged).await;
        bail!("Euro-Office lieferte kein PDF");
    }
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    if let Err(e) = tokio::fs::rename(&staged.tmp_path, dest).await {
        st.storage.discard(&staged).await;
        return Err(e.into());
    }
    Ok(())
}

/// Fehlercodes des Umwandlungsdienstes
fn error_text(code: i64) -> &'static str {
    match code {
        -2 => "Zeitüberschreitung bei der Umwandlung",
        -3 => "Umwandlung fehlgeschlagen",
        -4 => "Datei konnte nicht von Büro geladen werden (BUERO_PUBLIC_URL prüfen)",
        -5 => "Datei ist passwortgeschützt",
        -6 => "Datenbankfehler im Document Server",
        -7 => "Ungültige Anfrage",
        -8 => "Ungültiges Token (BUERO_OFFICE_JWT_SECRET prüfen)",
        -10 => "Datei ist zu groß",
        _ => "unbekannter Fehler",
    }
}

/// Nach so vielen Fehlschlägen in Folge gilt Euro-Office als gestört: das
/// Nachholen endet, ohne die Dokumente als nicht umwandelbar zu vermerken.
const BACKFILL_MAX_FAILS: usize = 3;

/// Nach dem Start: Office-Dateien ohne Vorschaubild nacheinander umwandeln
/// (z. B. aus der Zeit vor der Office-Vorschau). Dateien, die Euro-Office
/// nicht umwandeln kann, werden vermerkt und erst mit einer neuen Version
/// wieder versucht.
pub fn spawn_backfill(st: AppState) {
    if st.config.office_url.is_none() {
        return;
    }
    tokio::spawn(async move {
        let wait = |secs| {
            let st = st.clone();
            async move {
                tokio::select! {
                    _ = st.restart.cancelled() => false,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => true,
                }
            }
        };
        // Erst die normale Verarbeitung nach dem Start durchlassen
        if !wait(60).await {
            return;
        }
        if let Err(e) = backfill(&st, wait).await {
            tracing::warn!("Office-Vorschauen nachholen: {e:#}");
        }
    });
}

async fn backfill<W, F>(st: &AppState, wait: W) -> Result<()>
where
    W: Fn(u64) -> F,
    F: std::future::Future<Output = bool>,
{
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, extension FROM documents WHERE has_thumbnail = 0 AND extract_status = 'done' \
         AND IFNULL(preview_failed_sha, '') <> sha256 ORDER BY updated_at DESC",
    )
    .fetch_all(&st.pool)
    .await?;
    let ids: Vec<i64> = rows.into_iter().filter(|(_, ext)| shared::office_previewable(ext)).map(|(id, _)| id).collect();
    if ids.is_empty() {
        return Ok(());
    }
    // Adresse, unter der Euro-Office Büro erreicht: ohne BUERO_PUBLIC_URL
    // erst nach der ersten angemeldeten Anfrage bekannt
    let base = loop {
        if let Some(b) = st.background_base() {
            break b;
        }
        if !wait(30).await {
            return Ok(());
        }
    };
    tracing::info!("Office-Vorschauen nachholen: {} Dokumente", ids.len());
    let task = crate::tasks::start("preview", "Office-Vorschauen nachholen", "", None);
    let (mut made, mut failed) = (0usize, Vec::new());
    for (i, id) in ids.iter().enumerate() {
        if st.restart.is_cancelled() {
            return Ok(());
        }
        let Some(doc) = crate::db::get_doc(&st.pool, *id).await? else { continue };
        // Inzwischen anders erledigt (angesehen, neue Version in Arbeit)?
        if doc.has_thumbnail || doc.extract_status != "done" {
            continue;
        }
        task.retitle(format!("Office-Vorschau nachholen ({} von {}): {}", i + 1, ids.len(), doc.title), Some(doc.id));
        let res = match ensure(st, &doc, &base, Some(&task)).await {
            Ok(pdf) => make_thumbnail(st, &doc, &pdf).await,
            Err(e) => Err(e),
        };
        match res {
            // Vorschau da, aber kein Bild daraus: liegt an der Datei
            Ok(false) => mark_failed(st, &mut vec![(doc.id, doc.sha256.clone())]).await,
            Ok(true) => {
                made += 1;
                // Fehlschläge vor einem Erfolg lagen an den Dateien
                mark_failed(st, &mut failed).await;
            }
            Err(e) => {
                tracing::warn!("Keine PDF-Vorschau für {}: {e:#}", doc.id);
                failed.push((doc.id, doc.sha256.clone()));
                if failed.len() >= BACKFILL_MAX_FAILS {
                    tracing::warn!("Office-Vorschauen nachholen abgebrochen: Euro-Office antwortet nicht wie erwartet");
                    return Ok(());
                }
            }
        }
        // Euro-Office zwischendurch für die Bearbeitung frei lassen
        if !wait(1).await {
            return Ok(());
        }
    }
    mark_failed(st, &mut failed).await;
    tracing::info!("Office-Vorschauen nachgeholt: {made} von {}", ids.len());
    Ok(())
}

async fn mark_failed(st: &AppState, failed: &mut Vec<(i64, String)>) {
    for (id, sha) in failed.drain(..) {
        let _ = sqlx::query("UPDATE documents SET preview_failed_sha = ? WHERE id = ? AND sha256 = ?")
            .bind(&sha)
            .bind(id)
            .bind(&sha)
            .execute(&st.pool)
            .await;
    }
}
