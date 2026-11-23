// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! E-Mails (`.eml`): Text und Kopfdaten auslesen, Konversationen über
//! `Message-ID`, `In-Reply-To` und `References` bilden, Anhänge als eigene
//! Dokumente ablegen (verknüpft über `email_attachments`).

use std::collections::HashSet;

use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders};

use crate::state::AppState;

/// Kleine eingebettete Bilder (Logos in Signaturen) nicht als Anhang ablegen
const INLINE_IMAGE_MIN: usize = 100 * 1024;
/// Höchstens so viele Anhänge pro Mail
const MAX_ATTACHMENTS: usize = 50;

pub struct Parsed {
    pub message_id: Option<String>,
    /// In-Reply-To und References (ohne spitze Klammern)
    pub refs: Vec<String>,
    pub subject: String,
    pub sender: String,
    pub recipients: String,
    pub cc: String,
    pub sent_at: Option<i64>,
    pub body: String,
    pub attachments: Vec<Attachment>,
}

pub struct Attachment {
    pub name: String,
    pub data: Vec<u8>,
}

fn ids(v: &HeaderValue) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn clean_id(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!s.is_empty() && s.len() < 1000).then(|| s.to_string())
}

fn addresses(a: Option<&Address>) -> String {
    let Some(a) = a else { return String::new() };
    a.iter()
        .map(|x| match (x.name(), x.address()) {
            (Some(n), Some(ad)) if !n.trim().is_empty() && n.trim() != ad => format!("{} <{ad}>", n.trim()),
            (_, Some(ad)) => ad.to_string(),
            (Some(n), None) => n.to_string(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Nur die Kopfzeilen auswerten (für den IMAP-Abruf ohne den ganzen Inhalt)
pub fn header_ids(raw: &[u8]) -> (Option<String>, Vec<String>) {
    match MessageParser::default().parse_headers(raw) {
        Some(m) => (m.message_id().and_then(clean_id), refs_of(&m)),
        None => (None, Vec::new()),
    }
}

fn refs_of(m: &Message) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for r in ids(m.in_reply_to()).into_iter().chain(ids(m.references())) {
        if let Some(r) = clean_id(&r) {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
    }
    refs
}

pub fn parse(raw: &[u8]) -> Option<Parsed> {
    let m = MessageParser::default().parse(raw)?;
    let body = m
        .body_text(0)
        .map(|t| t.to_string())
        .or_else(|| m.body_html(0).map(|h| crate::extract::strip_html(&h)))
        .unwrap_or_default();
    let mut attachments = Vec::new();
    for part in m.attachments().take(MAX_ATTACHMENTS) {
        let data = part.contents();
        if data.is_empty() {
            continue;
        }
        let ctype = part.content_type().map(|c| c.ctype().to_ascii_lowercase()).unwrap_or_default();
        let inline_image = ctype == "image" && (part.content_id().is_some() || part.content_disposition().is_some_and(|d| d.is_inline()));
        if inline_image && data.len() < INLINE_IMAGE_MIN {
            continue;
        }
        let name = part
            .attachment_name()
            .map(str::to_string)
            .or_else(|| {
                // Weitergeleitete Mail ohne Dateinamen
                part.message().map(|inner| format!("{}.eml", inner.subject().unwrap_or("Weitergeleitete Mail")))
            })
            .unwrap_or_else(|| {
                let sub = part.content_type().and_then(|c| c.subtype()).unwrap_or("bin");
                format!("Anhang.{sub}")
            });
        let data = match part.message() {
            Some(inner) => inner.raw_message().to_vec(),
            None => data.to_vec(),
        };
        attachments.push(Attachment { name, data });
    }
    Some(Parsed {
        message_id: m.message_id().and_then(clean_id),
        refs: refs_of(&m),
        subject: m.subject().unwrap_or("").trim().to_string(),
        sender: addresses(m.from()),
        recipients: addresses(m.to()),
        cc: addresses(m.cc()),
        sent_at: m.date().map(|d| d.to_timestamp()),
        body,
        attachments,
    })
}

/// Durchsuchbarer Text einer Mail
pub fn text(p: &Parsed) -> String {
    let mut s = String::new();
    let mut line = |k: &str, v: &str| {
        if !v.is_empty() {
            s.push_str(&format!("{k}: {v}\n"));
        }
    };
    line("Von", &p.sender);
    line("An", &p.recipients);
    line("Cc", &p.cc);
    if let Some(t) = p.sent_at {
        line("Datum", &crate::util::fmt_date(t));
    }
    line("Betreff", &p.subject);
    let names: Vec<&str> = p.attachments.iter().map(|a| a.name.as_str()).collect();
    line("Anhänge", &names.join(", "));
    s.push('\n');
    s.push_str(p.body.trim());
    s
}

/// Sind diese Message-IDs schon in Büro?
pub async fn known_ids(st: &AppState) -> anyhow::Result<HashSet<String>> {
    let v: Vec<String> = sqlx::query_scalar("SELECT message_id FROM emails WHERE message_id IS NOT NULL")
        .fetch_all(&st.pool)
        .await?;
    Ok(v.into_iter().collect())
}

/// Mail-Dokument erfassen: Kopfdaten speichern, in eine Konversation
/// einordnen, Anhänge als Dokumente anlegen; Titel und Datum ergänzen.
/// Mehrfacher Aufruf ist harmlos (nur beim ersten Mal wirksam).
pub async fn register(st: &AppState, doc_id: i64) -> anyhow::Result<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT seq FROM emails WHERE document_id = ?")
        .bind(doc_id)
        .fetch_optional(&st.pool)
        .await?;
    if exists.is_some() {
        return Ok(());
    }
    let Some(doc) = crate::db::get_doc(&st.pool, doc_id).await? else { return Ok(()) };
    let raw = tokio::fs::read(st.storage.file_path(&doc.sha256)).await?;
    let Some(p) = parse(&raw) else { return Ok(()) };

    // Konversation: alle Konversationen, mit denen diese Mail verbunden ist, zusammenführen
    let mut threads: Vec<String> = Vec::new();
    for r in &p.refs {
        let t: Vec<String> = sqlx::query_scalar("SELECT thread_id FROM emails WHERE message_id = ?")
            .bind(r)
            .fetch_all(&st.pool)
            .await?;
        threads.extend(t);
    }
    if let Some(mid) = &p.message_id {
        // Antworten, die schon vor dieser Mail in Büro waren
        let t: Vec<String> = sqlx::query_scalar("SELECT thread_id FROM emails WHERE instr(refs, ?) > 0")
            .bind(format!("<{mid}>"))
            .fetch_all(&st.pool)
            .await?;
        threads.extend(t);
    }
    threads.sort();
    threads.dedup();
    let thread = threads
        .first()
        .cloned()
        .or_else(|| p.message_id.clone())
        .unwrap_or_else(|| format!("doc:{doc_id}"));
    let refs = p.refs.iter().map(|r| format!("<{r}>")).collect::<Vec<_>>().join(" ");

    let mut tx = st.pool.begin().await?;
    sqlx::query(
        "INSERT OR IGNORE INTO emails (document_id, message_id, refs, thread_id, sender, recipients, cc, subject, sent_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(doc_id)
    .bind(&p.message_id)
    .bind(&refs)
    .bind(&thread)
    .bind(&p.sender)
    .bind(&p.recipients)
    .bind(&p.cc)
    .bind(&p.subject)
    .bind(p.sent_at)
    .execute(&mut *tx)
    .await?;
    for t in threads.iter().skip(1) {
        sqlx::query("UPDATE emails SET thread_id = ? WHERE thread_id = ?").bind(&thread).bind(t).execute(&mut *tx).await?;
    }
    // Titel aus dem Betreff, wenn er noch dem Dateinamen entspricht; Datum ergänzen
    if !p.subject.is_empty() && doc.title == crate::util::title_from_filename(&doc.original_filename) {
        sqlx::query("UPDATE documents SET title = ? WHERE id = ?").bind(&p.subject).bind(doc_id).execute(&mut *tx).await?;
    }
    if let Some(t) = p.sent_at {
        sqlx::query("UPDATE documents SET document_date = ? WHERE id = ? AND IFNULL(document_date, '') = ''")
            .bind(crate::util::fmt_date(t))
            .bind(doc_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    // Anhänge: in dieselben Sammlungen wie die Mail
    let mut attached_all: Vec<i64> = Vec::new();
    if !p.attachments.is_empty() {
        let collections: Vec<i64> =
            sqlx::query_scalar("SELECT collection_id FROM document_collections WHERE document_id = ?")
                .bind(doc_id)
                .fetch_all(&st.pool)
                .await?;
        let date = p.sent_at.map(crate::util::fmt_date);
        let mut uploads = Vec::new();
        for a in p.attachments {
            let filename = crate::api::documents::sanitize_filename(&a.name);
            let mime = crate::api::documents::guess_mime(&filename, None);
            let stream = futures_util::stream::once(async move {
                Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(a.data))
            });
            match st.storage.stage(Box::pin(stream), st.config.max_upload_bytes).await {
                Ok(staged) => uploads.push(crate::api::documents::Upload { staged, filename, mime }),
                Err(e) => tracing::warn!("Anhang {filename} von Dokument {doc_id} übersprungen: {e}"),
            }
        }
        if !uploads.is_empty() {
            let form = crate::api::documents::UploadForm {
                uploads,
                collections,
                date,
                link_duplicates: true,
                ..Default::default()
            };
            let results = crate::api::documents::store_uploads(st, form).await.map_err(|e| anyhow::anyhow!(e.message))?;
            let mut attached = Vec::new();
            for r in results {
                let aid = r.document.id;
                if aid == doc_id {
                    continue;
                }
                sqlx::query("INSERT OR IGNORE INTO email_attachments (email_id, document_id) VALUES (?, ?)")
                    .bind(doc_id)
                    .bind(aid)
                    .execute(&st.pool)
                    .await?;
                // Neue Anhänge werden Unterdokumente der Mail; schon vorhandene
                // Dokumente (gleicher Inhalt) bekommen nur einen Verweis
                let linked = if r.duplicate || r.document.parent_id.is_some() {
                    crate::api::links::relate(st, doc_id, aid).await
                } else {
                    crate::api::links::set_parent(st, aid, doc_id).await
                };
                if let Err(e) = linked {
                    tracing::warn!("Anhang {aid} nicht mit Mail {doc_id} verknüpft: {}", e.message);
                }
                attached.push(aid);
            }
            attached_all = attached;
        }
    }
    let mut touched = attached_all;
    touched.push(doc_id);
    st.reindex(&touched).await?;
    Ok(())
}

/// E-Mail-Angaben für die Detailansicht
pub async fn info(st: &AppState, doc_id: i64) -> anyhow::Result<shared::EmailInfo> {
    let mut out = shared::EmailInfo::default();
    let row: Option<(String, String, String, String, Option<i64>, String)> = sqlx::query_as(
        "SELECT subject, sender, recipients, cc, sent_at, thread_id FROM emails WHERE document_id = ?",
    )
    .bind(doc_id)
    .fetch_optional(&st.pool)
    .await?;
    if let Some((subject, sender, recipients, cc, sent_at, thread)) = row {
        out.email = Some(shared::EmailHeader { subject, sender, recipients, cc, sent_at });
        out.thread = refs(
            st,
            "SELECT document_id, subject, sender, sent_at FROM emails WHERE thread_id = ? \
             ORDER BY IFNULL(sent_at, 0), seq",
            &thread,
        )
        .await?;
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT document_id FROM email_attachments WHERE email_id = ? ORDER BY document_id")
                .bind(doc_id)
                .fetch_all(&st.pool)
                .await?;
        out.attachments = crate::db::summaries(&st.pool, &ids).await?;
    }
    out.attached_to = refs(
        st,
        "SELECT e.document_id, e.subject, e.sender, e.sent_at FROM email_attachments a \
         JOIN emails e ON e.document_id = a.email_id WHERE a.document_id = ? ORDER BY IFNULL(e.sent_at, 0)",
        &doc_id.to_string(),
    )
    .await?;
    Ok(out)
}

async fn refs(st: &AppState, sql: &str, arg: &str) -> anyhow::Result<Vec<shared::EmailRef>> {
    let rows: Vec<(i64, String, String, Option<i64>)> =
        sqlx::query_as(sql).bind(arg).fetch_all(&st.pool).await?;
    Ok(rows
        .into_iter()
        .map(|(id, subject, sender, sent_at)| shared::EmailRef { id, subject, sender, sent_at })
        .collect())
}
