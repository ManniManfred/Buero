// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Mails per IMAP abrufen (TLS, Port 993). Je Postfach beliebig viele
//! Ordner, jeweils mit Modus:
//!
//! - `all`: jede Mail des Ordners übernehmen (z. B. ein Ordner „Büro“),
//! - `replies`: nur Mails übernehmen, die zu einer Konversation gehören, die
//!   schon in Büro liegt (z. B. „Gesendet“ oder der Posteingang). Neue Mails
//!   werden an ihren Kopfzeilen erkannt; kommt eine Konversation später dazu,
//!   sucht Büro deren ältere Antworten per IMAP-SEARCH nach.
//!
//! Der Mailserver wird nur gelesen (`EXAMINE`, `BODY.PEEK`): nichts wird
//! verschoben, gelöscht oder als gelesen markiert.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::TryStreamExt;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;

use crate::state::AppState;
use crate::util::now;

type Session = async_imap::Session<TlsStream<TcpStream>>;

/// Läuft gerade ein Abruf? (je Postfach höchstens einer)
static RUNNING: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
/// Fortschritt laufender Abrufe je Postfach
static PROGRESS: std::sync::Mutex<Vec<(i64, shared::MailProgress)>> = std::sync::Mutex::new(Vec::new());

pub fn is_running(account: i64) -> bool {
    RUNNING.lock().unwrap().contains(&account)
}

pub fn progress(account: i64) -> Option<shared::MailProgress> {
    PROGRESS.lock().unwrap().iter().find(|p| p.0 == account).map(|p| p.1.clone())
}

fn set_progress(account: i64, f: impl FnOnce(&mut shared::MailProgress)) {
    let mut all = PROGRESS.lock().unwrap();
    match all.iter_mut().find(|p| p.0 == account) {
        Some(p) => f(&mut p.1),
        None => {
            let mut p = shared::MailProgress::default();
            f(&mut p);
            all.push((account, p));
        }
    }
}

/// Postfächer, deren laufender Abruf abgebrochen werden soll
static CANCEL: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
const CANCELLED: &str = "Abruf abgebrochen";

/// Laufenden Abruf abbrechen; `false`, wenn keiner läuft
pub fn cancel(account: i64) -> bool {
    if !is_running(account) {
        return false;
    }
    let mut c = CANCEL.lock().unwrap();
    if !c.contains(&account) {
        c.push(account);
    }
    set_progress(account, |p| p.phase = "Wird abgebrochen …".into());
    true
}

/// Bei Abbruch mit Fehler `CANCELLED` aussteigen
fn check_cancel(account: i64) -> Result<()> {
    if CANCEL.lock().unwrap().contains(&account) {
        bail!(CANCELLED);
    }
    Ok(())
}

struct Guard(i64);
impl Drop for Guard {
    fn drop(&mut self) {
        RUNNING.lock().unwrap().retain(|a| *a != self.0);
        PROGRESS.lock().unwrap().retain(|p| p.0 != self.0);
        CANCEL.lock().unwrap().retain(|a| *a != self.0);
    }
}

fn try_start(account: i64) -> Option<Guard> {
    let mut r = RUNNING.lock().unwrap();
    if r.contains(&account) {
        return None;
    }
    r.push(account);
    CANCEL.lock().unwrap().retain(|a| *a != account);
    Some(Guard(account))
}

pub struct Account {
    pub host: String,
    pub port: i64,
    pub username: String,
    pub password: String,
}

pub async fn load_account(st: &AppState, id: i64) -> Result<Option<Account>> {
    let row: Option<(i64, String, i64, String, String)> = sqlx::query_as(
        "SELECT id, host, port, username, password FROM mail_accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await?;
    Ok(row.map(|(_, host, port, username, password)| Account {
        host,
        port,
        username,
        password,
    }))
}

async fn connect(a: &Account) -> Result<Session> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Zusätzlich die Zertifikate des Systems (z. B. eigene Zertifizierungsstelle)
    for c in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(c);
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let host = a.host.trim().to_string();
    let port = u16::try_from(a.port).unwrap_or(993);
    let tcp = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect((host.as_str(), port)))
        .await
        .context("Zeitüberschreitung beim Verbinden")?
        .with_context(|| format!("Verbindung zu {host}:{port} fehlgeschlagen"))?;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).context("Ungültiger Servername")?;
    let tls = tokio::time::timeout(Duration::from_secs(20), connector.connect(name, tcp))
        .await
        .context("Zeitüberschreitung beim TLS-Aufbau")?
        .context("TLS-Verbindung fehlgeschlagen (Port 993 mit TLS erwartet)")?;
    let mut client = async_imap::Client::new(tls);
    let greeting = tokio::time::timeout(Duration::from_secs(20), client.read_response())
        .await
        .context("Keine Begrüßung vom Mailserver")?;
    if greeting.is_none() {
        bail!("Mailserver hat die Verbindung beendet");
    }
    match tokio::time::timeout(Duration::from_secs(30), client.login(&a.username, &a.password)).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err((e, _))) => bail!("Anmeldung fehlgeschlagen: {e}"),
        Err(_) => bail!("Zeitüberschreitung bei der Anmeldung"),
    }
}

/// Ordner auf dem Server (für die Auswahl in den Einstellungen)
pub async fn list_folders(a: &Account) -> Result<Vec<shared::ServerFolder>> {
    use async_imap::types::NameAttribute as N;
    let mut s = connect(a).await?;
    let names: Vec<_> = s.list(Some(""), Some("*")).await?.try_collect().await?;
    let mut out: Vec<shared::ServerFolder> = names
        .iter()
        .filter(|n| !n.attributes().iter().any(|a| matches!(a, N::NoSelect)))
        .map(|n| {
            let special = n
                .attributes()
                .iter()
                .find_map(|a| match a {
                    N::Sent => Some("sent"),
                    N::Drafts => Some("drafts"),
                    N::Trash => Some("trash"),
                    N::Junk => Some("junk"),
                    N::Archive => Some("archive"),
                    _ => None,
                })
                .unwrap_or(if n.name().eq_ignore_ascii_case("INBOX") { "inbox" } else { "" });
            shared::ServerFolder { name: n.name().to_string(), special: special.to_string() }
        })
        .collect();
    let _ = s.logout().await;
    out.sort_by(|a, b| {
        (a.special != "inbox", a.special.is_empty(), a.name.to_lowercase())
            .cmp(&(b.special != "inbox", b.special.is_empty(), b.name.to_lowercase()))
    });
    Ok(out)
}

struct Folder {
    id: i64,
    name: String,
    mode: String,
    uid_validity: Option<i64>,
    last_uid: i64,
    last_seq: i64,
    /// Schon übernommene Mails in die Unter-Sammlung einsortieren
    resort: bool,
}

/// IMAP-Strings in Anführungszeichen
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Ein Postfach abrufen; Rückgabe: Anzahl übernommener Mails
pub async fn run(st: &AppState, account_id: i64) -> Result<usize> {
    let Some(_guard) = try_start(account_id) else { bail!("Abruf läuft bereits") };
    let res = tokio::time::timeout(Duration::from_secs(30 * 60), run_inner(st, account_id)).await;
    let res = match res {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("Abruf nach 30 Minuten abgebrochen")),
    };
    let (err, n) = match &res {
        Ok(n) => (None, *n as i64),
        // Abgebrochen: kein Fehler, Übernommenes zählt
        Err(e) if e.chain().any(|c| c.to_string() == CANCELLED) => {
            (None, progress(account_id).map(|p| p.imported).unwrap_or(0))
        }
        Err(e) => (Some(format!("{e:#}")), 0),
    };
    sqlx::query("UPDATE mail_accounts SET last_run = ?, last_error = ?, last_imported = ? WHERE id = ?")
        .bind(now())
        .bind(&err)
        .bind(n)
        .bind(account_id)
        .execute(&st.pool)
        .await?;
    res
}

async fn run_inner(st: &AppState, account_id: i64) -> Result<usize> {
    let a = load_account(st, account_id).await?.context("Postfach nicht gefunden")?;
    let rows: Vec<(i64, String, String, Option<i64>, i64, i64, bool)> = sqlx::query_as(
        "SELECT id, name, mode, uid_validity, last_uid, last_seq, resort <> 0 FROM mail_folders \
         WHERE account_id = ? ORDER BY mode = 'replies', id",
    )
    .bind(account_id)
    .fetch_all(&st.pool)
    .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let folders: Vec<Folder> = rows
        .into_iter()
        .map(|(id, name, mode, uid_validity, last_uid, last_seq, resort)| Folder {
            id,
            name,
            mode,
            uid_validity,
            last_uid,
            last_seq,
            resort,
        })
        .collect();
    set_progress(account_id, |p| p.phase = "Verbinden …".into());
    let mut s = connect(&a).await?;
    let mut known = crate::email::known_ids(st).await?;
    let mut imported = 0;
    let count = folders.len() as i64;
    for (i, f) in folders.iter().enumerate() {
        set_progress(account_id, |p| {
            p.folder = shared::imap_folder_label(&f.name);
            p.folder_index = i as i64 + 1;
            p.folder_count = count;
            p.phase = "Ordner öffnen …".into();
            p.done = 0;
            p.total = 0;
        });
        if let Err(e) = check_cancel(account_id) {
            let _ = s.logout().await;
            return Err(e);
        }
        match folder(st, &mut s, account_id, f, &mut known).await {
            Ok(n) => imported += n,
            Err(e) => {
                let _ = s.logout().await;
                return Err(e.context(format!("Ordner „{}“", shared::imap_folder_label(&f.name))));
            }
        }
    }
    let _ = s.logout().await;
    if imported > 0 {
        tracing::info!("Mail-Abruf Postfach {account_id}: {imported} Mail(s) übernommen");
    }
    Ok(imported)
}

async fn folder(st: &AppState, s: &mut Session, account: i64, f: &Folder, known: &mut HashSet<String>) -> Result<usize> {
    let mb = s.examine(&f.name).await?;
    let validity = mb.uid_validity.map(i64::from);
    let mut last_uid = f.last_uid;
    if validity != f.uid_validity {
        // Ordner neu angelegt oder umnummeriert: von vorn (Duplikate werden erkannt)
        last_uid = 0;
    }
    let mut imported = 0;
    let ctx = ImportCtx { account, folder: f.id, count: f.mode != "replies" };

    if f.resort {
        resort(st, s, account, f).await?;
    }

    // Neue Mails seit dem letzten Abruf
    let mut uids: Vec<u32> = s
        .uid_search(format!("UID {}:*", last_uid + 1))
        .await?
        .into_iter()
        .filter(|u| i64::from(*u) > last_uid)
        .collect();
    uids.sort_unstable();
    set_progress(account, |p| {
        p.phase = if f.mode == "replies" { "Antworten suchen".into() } else { "Mails übernehmen".into() };
        p.total = uids.len() as i64;
    });
    for chunk in uids.chunks(50) {
        check_cancel(account)?;
        let set = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let wanted: Vec<u32> = if f.mode == "replies" {
            let heads: Vec<_> = s
                .uid_fetch(&set, "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES)])")
                .await?
                .try_collect()
                .await?;
            let mut w = Vec::new();
            for h in &heads {
                let (Some(uid), Some(raw)) = (h.uid, h.header()) else { continue };
                let (mid, refs) = crate::email::header_ids(raw);
                let new = mid.as_ref().is_none_or(|m| !known.contains(m));
                if new && refs.iter().any(|r| known.contains(r)) {
                    w.push(uid);
                    // Antworten auf diese Antwort im selben Durchlauf erkennen
                    if let Some(m) = mid {
                        known.insert(m);
                    }
                }
            }
            w
        } else {
            chunk.to_vec()
        };
        imported += fetch_and_import(st, s, &wanted, &ctx, known).await?;
        if f.mode == "replies" {
            // Bei „alle Mails“ zählt fetch_and_import selbst mit
            set_progress(account, |p| p.done += chunk.len() as i64);
        }
        last_uid = last_uid.max(chunk.iter().copied().map(i64::from).max().unwrap_or(0));
        save_state(st, f.id, validity, last_uid, None).await?;
    }

    // Ältere Antworten zu Konversationen, die seit dem letzten Mal neu in Büro sind
    if f.mode == "replies" {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT seq, message_id FROM emails WHERE seq > ? AND message_id IS NOT NULL ORDER BY seq",
        )
        .bind(f.last_seq)
        .fetch_all(&st.pool)
        .await?;
        let max_seq = rows.last().map(|r| r.0).unwrap_or(f.last_seq);
        set_progress(account, |p| {
            p.phase = "Ältere Antworten nachsuchen".into();
            p.done = 0;
            p.total = rows.len() as i64;
        });
        for chunk in rows.chunks(10) {
            check_cancel(account)?;
            // OR-Kette: (OR (OR a b) c) …
            let mut q = String::new();
            for (i, (_, mid)) in chunk.iter().enumerate() {
                let m = quote(&format!("<{mid}>"));
                let one = format!("OR HEADER IN-REPLY-TO {m} HEADER REFERENCES {m}");
                q = if i == 0 { one } else { format!("OR ({q}) ({one})") };
            }
            let mut hits: Vec<u32> = s.uid_search(&q).await?.into_iter().collect();
            hits.sort_unstable();
            if !hits.is_empty() {
                imported += fetch_and_import(st, s, &hits, &ctx, known).await?;
            }
            set_progress(account, |p| p.done += chunk.len() as i64);
        }
        save_state(st, f.id, validity, last_uid, Some(max_seq)).await?;
    }
    Ok(imported)
}

async fn save_state(st: &AppState, folder: i64, validity: Option<i64>, last_uid: i64, last_seq: Option<i64>) -> Result<()> {
    sqlx::query("UPDATE mail_folders SET uid_validity = ?, last_uid = ?, last_seq = IFNULL(?, last_seq) WHERE id = ?")
        .bind(validity)
        .bind(last_uid)
        .bind(last_seq)
        .bind(folder)
        .execute(&st.pool)
        .await?;
    Ok(())
}

async fn fetch_and_import(
    st: &AppState,
    s: &mut Session,
    uids: &[u32],
    ctx: &ImportCtx,
    known: &mut HashSet<String>,
) -> Result<usize> {
    let mut n = 0;
    for chunk in uids.chunks(20) {
        let set = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let msgs: Vec<_> = s.uid_fetch(&set, "(UID BODY.PEEK[])").await?.try_collect().await?;
        for m in &msgs {
            check_cancel(ctx.account)?;
            if ctx.count {
                set_progress(ctx.account, |p| p.done += 1);
            }
            let Some(raw) = m.body() else { continue };
            if import(st, raw, ctx, known).await? {
                n += 1;
                set_progress(ctx.account, |p| p.imported += 1);
            }
        }
    }
    Ok(n)
}

/// Eine Mail als `.eml`-Dokument ablegen; `false`, wenn sie schon da ist
/// Wohin übernommene Mails gehen (bei jeder Mail neu gelesen, damit eine
/// Änderung während eines laufenden Abrufs sofort gilt)
struct ImportCtx {
    account: i64,
    folder: i64,
    /// Jede geladene Mail im Fortschritt mitzählen
    count: bool,
}

async fn target_collection(st: &AppState, ctx: &ImportCtx) -> Result<Option<i64>> {
    Ok(folder_target(st, ctx.folder).await?.1)
}

/// (Sammlung des Postfachs, Ziel-Sammlung) eines Ordners; legt die
/// Unter-Sammlung des Ordners bei Bedarf an
async fn folder_target(st: &AppState, folder: i64) -> Result<(Option<i64>, Option<i64>)> {
    let row: Option<(Option<i64>, Option<i64>, bool, Option<i64>, String)> = sqlx::query_as(
        "SELECT f.collection_id, a.collection_id, f.own_collection <> 0, f.auto_collection_id, f.name \
         FROM mail_folders f JOIN mail_accounts a ON a.id = f.account_id WHERE f.id = ?",
    )
    .bind(folder)
    .fetch_optional(&st.pool)
    .await?;
    let Some((fixed, account, own, auto, name)) = row else { return Ok((None, None)) };
    let target = match (fixed, account) {
        (Some(c), _) => Some(c),
        (None, None) => None,
        (None, Some(a)) if !own => Some(a),
        (None, Some(a)) => Some(sub_collection(st, folder, a, auto, &name).await?),
    };
    Ok((account, target))
}

/// Unter-Sammlung eines Ordners in der Sammlung des Postfachs (auch nach
/// Umbenennen dieselbe; sonst per Name gefunden oder neu angelegt)
async fn sub_collection(st: &AppState, folder: i64, parent: i64, auto: Option<i64>, name: &str) -> Result<i64> {
    if let Some(id) = auto {
        let p: Option<Option<i64>> =
            sqlx::query_scalar("SELECT parent_id FROM collections WHERE id = ?").bind(id).fetch_optional(&st.pool).await?;
        if p == Some(Some(parent)) {
            return Ok(id);
        }
    }
    let label = shared::mail_folder_collection_name(name);
    sqlx::query("INSERT OR IGNORE INTO collections (parent_id, name, color, created_at) VALUES (?, ?, NULL, ?)")
        .bind(parent)
        .bind(&label)
        .bind(now())
        .execute(&st.pool)
        .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM collections WHERE parent_id = ? AND name = ?")
        .bind(parent)
        .bind(&label)
        .fetch_one(&st.pool)
        .await?;
    sqlx::query("UPDATE mail_folders SET auto_collection_id = ? WHERE id = ?").bind(id).bind(folder).execute(&st.pool).await?;
    Ok(id)
}

/// Schon übernommene Mails dieses Ordners (samt Anhängen) aus der Sammlung
/// des Postfachs in die Unter-Sammlung verschieben; liest nur Message-IDs
async fn resort(st: &AppState, s: &mut Session, account: i64, f: &Folder) -> Result<()> {
    let (Some(from), Some(to)) = folder_target(st, f.id).await? else {
        return clear_resort(st, f.id).await;
    };
    if from == to {
        return clear_resort(st, f.id).await;
    }
    let mut uids: Vec<u32> = s.uid_search("ALL").await?.into_iter().collect();
    uids.sort_unstable();
    set_progress(account, |p| {
        p.phase = "Vorhandene Mails einsortieren".into();
        p.done = 0;
        p.total = uids.len() as i64;
    });
    let mut moved = 0usize;
    for chunk in uids.chunks(200) {
        check_cancel(account)?;
        let set = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let heads: Vec<_> = s.uid_fetch(&set, "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])").await?.try_collect().await?;
        let mut docs: Vec<i64> = Vec::new();
        for h in &heads {
            let Some(raw) = h.header() else { continue };
            let (Some(mid), _) = crate::email::header_ids(raw) else { continue };
            let ids: Vec<i64> = sqlx::query_scalar(
                "SELECT d.id FROM emails e JOIN documents d ON d.id = e.document_id OR d.parent_id = e.document_id \
                 WHERE e.message_id = ? AND EXISTS (SELECT 1 FROM document_collections dc \
                 WHERE dc.document_id = d.id AND dc.collection_id = ?)",
            )
            .bind(&mid)
            .bind(from)
            .fetch_all(&st.pool)
            .await?;
            docs.extend(ids);
        }
        docs.sort_unstable();
        docs.dedup();
        if !docs.is_empty() {
            let mut tx = st.pool.begin().await?;
            for d in &docs {
                sqlx::query("DELETE FROM document_collections WHERE document_id = ? AND collection_id = ?")
                    .bind(d)
                    .bind(from)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) VALUES (?, ?)")
                    .bind(d)
                    .bind(to)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            st.reindex(&docs).await?;
            moved += docs.len();
        }
        set_progress(account, |p| p.done += chunk.len() as i64);
    }
    if moved > 0 {
        tracing::info!("Mail-Abruf Postfach {account}: {moved} Dokument(e) in Unter-Sammlung einsortiert");
    }
    clear_resort(st, f.id).await
}

async fn clear_resort(st: &AppState, folder: i64) -> Result<()> {
    sqlx::query("UPDATE mail_folders SET resort = 0 WHERE id = ?").bind(folder).execute(&st.pool).await?;
    Ok(())
}

async fn import(st: &AppState, raw: &[u8], ctx: &ImportCtx, known: &mut HashSet<String>) -> Result<bool> {
    let (mid, _) = crate::email::header_ids(raw);
    let exists: Option<i64> = match &mid {
        Some(m) => sqlx::query_scalar("SELECT document_id FROM emails WHERE message_id = ? LIMIT 1")
            .bind(m)
            .fetch_optional(&st.pool)
            .await?,
        None => None,
    };
    if exists.is_some() {
        return Ok(false);
    }
    let parsed = crate::email::parse(raw);
    let subject = parsed.as_ref().map(|p| p.subject.clone()).filter(|s| !s.is_empty());
    let date = parsed.as_ref().and_then(|p| p.sent_at).map(crate::util::fmt_date);
    let base: String = subject
        .clone()
        .unwrap_or_else(|| "E-Mail".into())
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .take(120)
        .collect();
    let filename = format!("{}.eml", base.trim());
    let data = raw.to_vec();
    let stream = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(data)) });
    let staged = st.storage.stage(Box::pin(stream), st.config.max_upload_bytes).await?;
    let collection = target_collection(st, ctx).await?;
    let form = crate::api::documents::UploadForm {
        uploads: vec![crate::api::documents::Upload { staged, filename, mime: "message/rfc822".into() }],
        collections: collection.into_iter().collect(),
        title: subject,
        date,
        link_duplicates: true,
        ..Default::default()
    };
    let res = crate::api::documents::store_uploads(st, form).await.map_err(|e| anyhow::anyhow!(e.message))?;
    let Some(r) = res.into_iter().next() else { return Ok(false) };
    // Sofort erfassen, damit Antworten im selben Abruf zugeordnet werden
    crate::email::register(st, r.document.id).await?;
    if let Some(m) = mid {
        known.insert(m);
    }
    Ok(!r.duplicate)
}

/// Hintergrundaufgabe: fällige Postfächer abrufen
pub fn spawn(st: AppState) {
    tokio::spawn(async move {
        // Nach dem Start erst kurz warten
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            let due: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM mail_accounts WHERE enabled <> 0 AND password <> '' \
                 AND IFNULL(last_run, 0) + interval_min * 60 <= ?",
            )
            .bind(now())
            .fetch_all(&st.pool)
            .await
            .unwrap_or_default();
            for id in due {
                if let Err(e) = run(&st, id).await {
                    tracing::warn!("Mail-Abruf Postfach {id}: {e:#}");
                }
            }
            tokio::select! {
                _ = st.restart.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
    });
}
