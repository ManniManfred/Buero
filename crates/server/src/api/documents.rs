// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use axum::body::Body;
use axum::extract::{Multipart, Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use shared::{
    BulkUpdate, DocumentDetail, DocumentSummary, DocumentUpdate, ListResponse, SearchParams,
    UploadResult,
};

use crate::db;
use crate::error::{AppError, AppResult};
use crate::search::Query_;
use crate::state::AppState;
use crate::storage::StagedFile;
use crate::util::{self, now, parse_ids};

pub async fn list(
    State(st): State<AppState>,
    Query(p): Query<SearchParams>,
) -> AppResult<Json<ListResponse>> {
    let started = std::time::Instant::now();
    let q = Query_ {
        text: p.q.chars().take(500).collect(),
        collections: parse_ids(&p.collections),
        direct: p.direct,
        hide_children: p.hide_children,
        tags: parse_ids(&p.tags),
        kind: p.kind.clone(),
        unfiled: p.unfiled,
        exclude_collections: Vec::new(),
        sort: p.sort.clone(),
        offset: p.offset,
        limit: if p.limit == 0 { 50 } else { p.limit },
    };
    let st2 = st.clone();
    let hits = tokio::task::spawn_blocking(move || st2.search.search(&q)).await??;
    let ids: Vec<i64> = hits.ids.iter().map(|(id, _)| *id).collect();
    let mut items = db::summaries(&st.pool, &ids).await?;

    if !p.q.trim().is_empty() && !items.is_empty() {
        let texts = db::contents(&st.pool, &ids).await?;
        let st2 = st.clone();
        let qtext = p.q.clone();
        let (snips, hits) =
            tokio::task::spawn_blocking(move || (st2.search.snippets(&hits, &qtext, &texts), hits)).await?;
        for it in items.iter_mut() {
            it.snippet = snips.iter().find(|(id, _)| *id == it.id).map(|(_, s)| s.clone());
            it.score = hits.ids.iter().find(|(id, _)| *id == it.id).and_then(|(_, s)| *s);
        }
        return Ok(Json(ListResponse {
            total: hits.total as u64,
            items,
            took_ms: started.elapsed().as_secs_f64() * 1000.0,
            fuzzy: hits.fuzzy,
        }));
    }
    Ok(Json(ListResponse {
        total: hits.total as u64,
        items,
        took_ms: started.elapsed().as_secs_f64() * 1000.0,
        fuzzy: hits.fuzzy,
    }))
}

pub async fn get(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<DocumentDetail>> {
    let row = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    let summary = db::summary(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    let content_length: i64 = sqlx::query_scalar("SELECT length(content) FROM documents WHERE id = ?")
        .bind(id)
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(DocumentDetail {
        summary,
        description: row.description,
        extract_error: row.extract_error,
        content_length,
        versions: db::versions(&st.pool, id).await?,
    }))
}

pub(crate) fn guess_mime(filename: &str, declared: Option<&str>) -> String {
    // `.mpeg` ist meist MP3-Audio; als video/mpeg spielt es kein Browser ab
    if util::extension_of(filename).eq_ignore_ascii_case("mpeg") {
        return "audio/mpeg".to_string();
    }
    match mime_guess::from_path(filename).first() {
        Some(m) => m.essence_str().to_string(),
        None => declared
            .filter(|m| !m.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string(),
    }
}

pub(crate) struct Upload {
    pub staged: StagedFile,
    pub filename: String,
    pub mime: String,
}

#[derive(Default)]
pub(crate) struct UploadForm {
    pub uploads: Vec<Upload>,
    pub collections: Vec<i64>,
    pub tags: Vec<i64>,
    /// Tags per Name (nur für den KI-Zugang, der sie bei Bedarf anlegt)
    pub tag_names: Vec<String>,
    pub title: Option<String>,
    pub date: Option<String>,
    /// Bereits vorhandene Dokumente (gleicher Inhalt) trotzdem den Sammlungen
    /// und Tags zuordnen. Für Uploads über Freigabelinks abgeschaltet, damit
    /// Fremde keine bestehenden Dokumente in die freigegebene Sammlung holen.
    pub link_duplicates: bool,
}

/// Multipart-Formular lesen: beliebig viele Felder `file`, optional
/// `collection_ids`, `tag_ids` (kommagetrennt), `tags` (Namen, kommagetrennt),
/// `title` und `document_date`.
/// `max_bytes`: Höchstgröße pro Datei.
pub(crate) async fn read_upload_form(st: &AppState, mp: &mut Multipart, max_bytes: usize) -> AppResult<UploadForm> {
    let mut f = UploadForm { link_duplicates: true, ..Default::default() };
    let res: AppResult<()> = async {
        while let Some(field) = mp.next_field().await? {
            let name = field.name().unwrap_or("").to_string();
            match name.as_str() {
                "file" | "files" | "file[]" => {
                    let filename = field
                        .file_name()
                        .map(sanitize_filename)
                        .unwrap_or_else(|| "upload".into());
                    let mime = guess_mime(&filename, field.content_type());
                    let staged = st
                        .storage
                        .stage(Box::pin(field), max_bytes)
                        .await
                        .map_err(|e| AppError::bad_request(format!("{filename}: {e}")))?;
                    f.uploads.push(Upload { staged, filename, mime });
                }
                "collection_ids" => f.collections = parse_ids(&field.text().await?),
                "tag_ids" => f.tags = parse_ids(&field.text().await?),
                "tags" => {
                    f.tag_names.extend(field.text().await?.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()))
                }
                "title" => f.title = Some(field.text().await?).filter(|t| !t.trim().is_empty()),
                "document_date" => {
                    f.date = Some(field.text().await?).filter(|d| util::valid_date(d))
                }
                _ => {}
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = res {
        for u in &f.uploads {
            st.storage.discard(&u.staged).await;
        }
        return Err(e);
    }
    if f.uploads.is_empty() {
        return Err(AppError::bad_request("Keine Datei übermittelt"));
    }
    Ok(f)
}

pub async fn upload(State(st): State<AppState>, mut mp: Multipart) -> AppResult<Json<Vec<UploadResult>>> {
    let form = read_upload_form(&st, &mut mp, st.config.max_upload_bytes).await?;
    Ok(Json(store_uploads(&st, form).await?))
}

/// Gestagte Dateien als Dokumente anlegen (Duplikate werden erkannt).
pub(crate) async fn store_uploads(st: &AppState, form: UploadForm) -> AppResult<Vec<UploadResult>> {
    let UploadForm { uploads, collections, tags, title, date, link_duplicates, .. } = form;
    let single = uploads.len() == 1;
    let mut results = Vec::with_capacity(uploads.len());
    let mut touched = Vec::new();
    for u in uploads {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT document_id FROM document_versions WHERE sha256 = ? ORDER BY id LIMIT 1",
        )
        .bind(&u.staged.sha256)
        .fetch_optional(&st.pool)
        .await?;

        let (id, duplicate) = if let Some(id) = existing {
            st.storage.discard(&u.staged).await;
            (id, true)
        } else {
            st.storage.commit(&u.staged).await?;
            let ts = now();
            let ext = util::extension_of(&u.filename);
            let doc_title = if single { title.clone() } else { None }
                .unwrap_or_else(|| util::title_from_filename(&u.filename));
            let mut tx = st.pool.begin().await?;
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO documents (title, original_filename, mime, extension, size, sha256, version, \
                 created_at, updated_at, document_date) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?) RETURNING id",
            )
            .bind(&doc_title)
            .bind(&u.filename)
            .bind(&u.mime)
            .bind(&ext)
            .bind(u.staged.size)
            .bind(&u.staged.sha256)
            .bind(ts)
            .bind(ts)
            .bind(&date)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO document_versions (document_id, version, original_filename, size, sha256, created_at) \
                 VALUES (?, 1, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(&u.filename)
            .bind(u.staged.size)
            .bind(&u.staged.sha256)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            (id, false)
        };
        if duplicate && !link_duplicates {
            results.push((id, duplicate));
            continue;
        }
        // Zuordnungen auch bei Duplikaten ergänzen
        for c in &collections {
            sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) SELECT ?, id FROM collections WHERE id = ?")
                .bind(id).bind(c).execute(&st.pool).await?;
        }
        for t in &tags {
            sqlx::query("INSERT OR IGNORE INTO document_tags (document_id, tag_id) SELECT ?, id FROM tags WHERE id = ?")
                .bind(id).bind(t).execute(&st.pool).await?;
        }
        touched.push(id);
        if !duplicate {
            let _ = st.jobs.send(id);
        }
        results.push((id, duplicate));
    }
    st.reindex(&touched).await?;

    let all: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
    let summaries = db::summaries(&st.pool, &all).await?;
    Ok(results
        .into_iter()
        .filter_map(|(id, duplicate)| {
            summaries
                .iter()
                .find(|s| s.id == id)
                .map(|s| UploadResult { document: s.clone(), duplicate })
        })
        .collect())
}

pub(crate) fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() { "upload".into() } else { cleaned.chars().take(255).collect() }
}

#[derive(Deserialize)]
pub struct VersionParams {
    /// Version, auf der die hochgeladene Datei beruht: Gibt es inzwischen eine
    /// neuere, wird abgelehnt (409), statt fremde Änderungen zu überdecken.
    basis: Option<i64>,
}

pub async fn new_version(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(p): Query<VersionParams>,
    mut mp: Multipart,
) -> AppResult<Json<DocumentSummary>> {
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    if let Some(basis) = p.basis {
        if basis != doc.version {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                format!("Inzwischen gibt es eine neuere Version ({}, bearbeitet wurde {basis})", doc.version),
            ));
        }
    }
    let mut upload = None;
    while let Some(field) = mp.next_field().await? {
        if field.name() == Some("file") {
            let filename = field.file_name().map(sanitize_filename).unwrap_or_else(|| doc.original_filename.clone());
            let mime = guess_mime(&filename, field.content_type());
            let staged = st
                .storage
                .stage(Box::pin(field), st.config.max_upload_bytes)
                .await
                .map_err(|e| AppError::bad_request(e.to_string()))?;
            upload = Some(Upload { staged, filename, mime });
            break;
        }
    }
    let u = upload.ok_or_else(|| AppError::bad_request("Keine Datei übermittelt"))?;
    if add_version(&st, &doc, u).await? {
        // Eine evtl. offene Euro-Office-Sitzung bezieht sich auf die alte Fassung
        sqlx::query("UPDATE documents SET office_key = NULL WHERE id = ?").bind(id).execute(&st.pool).await?;
    }
    db::summary(&st.pool, id).await?.map(Json).ok_or_else(AppError::not_found)
}

/// Gestagte Datei als neue Version eines Dokuments speichern. Gleicher Inhalt
/// wie die aktuelle Version wird verworfen (Rückgabe `false`).
pub(crate) async fn add_version(st: &AppState, doc: &db::DocRow, u: Upload) -> AppResult<bool> {
    let id = doc.id;
    if u.staged.sha256 == doc.sha256 {
        st.storage.discard(&u.staged).await;
        return Ok(false);
    }
    st.storage.commit(&u.staged).await?;
    let ts = now();
    let version = doc.version + 1;
    let mut tx = st.pool.begin().await?;
    sqlx::query(
        "INSERT INTO document_versions (document_id, version, original_filename, size, sha256, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(version)
    .bind(&u.filename)
    .bind(u.staged.size)
    .bind(&u.staged.sha256)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE documents SET original_filename = ?, mime = ?, extension = ?, size = ?, sha256 = ?, version = ?, \
         updated_at = ?, extract_status = 'pending', extract_error = NULL, has_thumbnail = 0 WHERE id = ?",
    )
    .bind(&u.filename)
    .bind(&u.mime)
    .bind(util::extension_of(&u.filename))
    .bind(u.staged.size)
    .bind(&u.staged.sha256)
    .bind(version)
    .bind(ts)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let _ = tokio::fs::remove_file(st.storage.thumb_path(id)).await;
    let _ = st.jobs.send(id);
    Ok(true)
}

pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(u): Json<DocumentUpdate>,
) -> AppResult<Json<DocumentSummary>> {
    db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    // Für Unterdokumente: Sammlungen vorher, um nur die Änderung zu übertragen
    let old_collections: Vec<i64> = sqlx::query_scalar("SELECT collection_id FROM document_collections WHERE document_id = ?")
        .bind(id)
        .fetch_all(&st.pool)
        .await?;
    let children = if u.with_children && u.collection_ids.is_some() {
        db::descendants_of(&st.pool, &[id]).await?
    } else {
        Vec::new()
    };
    let mut tx = st.pool.begin().await?;
    if let Some(t) = u.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        sqlx::query("UPDATE documents SET title = ? WHERE id = ?").bind(t).bind(id).execute(&mut *tx).await?;
    }
    if let Some(d) = &u.description {
        sqlx::query("UPDATE documents SET description = ? WHERE id = ?").bind(d).bind(id).execute(&mut *tx).await?;
    }
    if let Some(d) = &u.document_date {
        let v = if d.trim().is_empty() {
            None
        } else if util::valid_date(d) {
            Some(d.trim().to_string())
        } else {
            return Err(AppError::bad_request("Ungültiges Datum (erwartet JJJJ-MM-TT)"));
        };
        sqlx::query("UPDATE documents SET document_date = ? WHERE id = ?").bind(v).bind(id).execute(&mut *tx).await?;
    }
    if let Some(cs) = &u.collection_ids {
        sqlx::query("DELETE FROM document_collections WHERE document_id = ?").bind(id).execute(&mut *tx).await?;
        for c in cs {
            sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) SELECT ?, id FROM collections WHERE id = ?")
                .bind(id).bind(c).execute(&mut *tx).await?;
        }
        for child in &children {
            for c in cs.iter().filter(|c| !old_collections.contains(c)) {
                sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) SELECT ?, id FROM collections WHERE id = ?")
                    .bind(child).bind(c).execute(&mut *tx).await?;
            }
            for c in old_collections.iter().filter(|c| !cs.contains(c)) {
                sqlx::query("DELETE FROM document_collections WHERE document_id = ? AND collection_id = ?")
                    .bind(child).bind(c).execute(&mut *tx).await?;
            }
        }
    }
    if let Some(ts) = &u.tag_ids {
        sqlx::query("DELETE FROM document_tags WHERE document_id = ?").bind(id).execute(&mut *tx).await?;
        for t in ts {
            sqlx::query("INSERT OR IGNORE INTO document_tags (document_id, tag_id) SELECT ?, id FROM tags WHERE id = ?")
                .bind(id).bind(t).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE documents SET updated_at = ? WHERE id = ?").bind(now()).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    let mut touched = children;
    touched.push(id);
    st.reindex(&touched).await?;
    db::summary(&st.pool, id).await?.map(Json).ok_or_else(AppError::not_found)
}

async fn delete_docs(st: &AppState, ids: &[i64]) -> AppResult<()> {
    // Unterdokumente, die bleiben, werden eigenständig (parent_id = NULL) – neu indexieren
    let mut orphans: Vec<i64> = Vec::new();
    for chunk in ids.chunks(500) {
        let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT id FROM documents WHERE parent_id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        qb.push(")");
        orphans.extend(qb.build_query_scalar::<i64>().fetch_all(&st.pool).await?);
    }
    orphans.retain(|o| !ids.contains(o));
    for &id in ids {
        let shas: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT sha256 FROM document_versions WHERE document_id = ?")
                .bind(id)
                .fetch_all(&st.pool)
                .await?;
        sqlx::query("DELETE FROM documents WHERE id = ?").bind(id).execute(&st.pool).await?;
        for sha in shas {
            if !db::sha_in_use(&st.pool, &sha).await? {
                st.storage.remove(&sha).await;
            }
        }
        let _ = tokio::fs::remove_file(st.storage.thumb_path(id)).await;
        st.search.delete(id);
    }
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || st2.search.commit()).await??;
    st.reindex(&orphans).await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct DeleteParams {
    #[serde(default)]
    with_children: bool,
}

pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>, Query(p): Query<DeleteParams>) -> AppResult<StatusCode> {
    let mut ids = vec![id];
    if p.with_children {
        ids.extend(db::descendants_of(&st.pool, &[id]).await?);
    }
    delete_docs(&st, &ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn bulk(State(st): State<AppState>, Json(mut b): Json<BulkUpdate>) -> AppResult<StatusCode> {
    let children = if b.with_children { db::descendants_of(&st.pool, &b.ids).await? } else { Vec::new() };
    if b.delete {
        b.ids.extend(children);
        delete_docs(&st, &b.ids).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let mut tx = st.pool.begin().await?;
    // Sammlungen auch für Unterdokumente, Tags nur für die gewählten Dokumente
    for &id in b.ids.iter().chain(children.iter()) {
        for c in &b.add_collections {
            sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) SELECT d.id, c.id FROM documents d, collections c WHERE d.id = ? AND c.id = ?")
                .bind(id).bind(c).execute(&mut *tx).await?;
        }
        for c in &b.remove_collections {
            sqlx::query("DELETE FROM document_collections WHERE document_id = ? AND collection_id = ?")
                .bind(id).bind(c).execute(&mut *tx).await?;
        }
        if !b.ids.contains(&id) {
            continue;
        }
        for t in &b.add_tags {
            sqlx::query("INSERT OR IGNORE INTO document_tags (document_id, tag_id) SELECT d.id, t.id FROM documents d, tags t WHERE d.id = ? AND t.id = ?")
                .bind(id).bind(t).execute(&mut *tx).await?;
        }
        for t in &b.remove_tags {
            sqlx::query("DELETE FROM document_tags WHERE document_id = ? AND tag_id = ?")
                .bind(id).bind(t).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    b.ids.extend(children);
    st.reindex(&b.ids).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reprocess(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let n = sqlx::query("UPDATE documents SET extract_status = 'pending', extract_error = NULL WHERE id = ?")
        .bind(id)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    let _ = st.jobs.send(id);
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct FileParams {
    #[serde(default)]
    download: bool,
    version: Option<i64>,
}

fn content_disposition(inline: bool, filename: &str) -> HeaderValue {
    let ascii: String = filename
        .chars()
        .map(|c| if c.is_ascii() && c != '"' && !c.is_control() { c } else { '_' })
        .collect();
    let encoded = percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC);
    let v = format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        ascii,
        encoded
    );
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

pub async fn file(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(p): Query<FileParams>,
    req: Request,
) -> AppResult<Response> {
    let doc = db::get_doc(&st.pool, id).await?.ok_or_else(AppError::not_found)?;
    serve_document(&st, &doc, p.version, p.download, req).await
}

/// Liefert eine Dokumentdatei (optional eine ältere Version) aus.
pub(crate) async fn serve_document(
    st: &AppState,
    doc: &db::DocRow,
    version: Option<i64>,
    download: bool,
    req: Request,
) -> AppResult<Response> {
    let (sha, filename) = match version {
        Some(v) if v != doc.version => {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT sha256, original_filename FROM document_versions WHERE document_id = ? AND version = ?",
            )
            .bind(doc.id)
            .bind(v)
            .fetch_optional(&st.pool)
            .await?;
            row.ok_or_else(AppError::not_found)?
        }
        _ => (doc.sha256.clone(), doc.original_filename.clone()),
    };
    let mime = guess_mime(&filename, Some(&doc.mime));
    let mut res = serve(st.storage.file_path(&sha), &mime, req).await?;
    res.headers_mut()
        .insert(header::CONTENT_DISPOSITION, content_disposition(!download, &filename));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    res.headers_mut()
        .insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    // Aktive Inhalte (HTML, SVG, XML) nicht im Kontext der App ausführen.
    // (Nicht für PDF: Chrome zeigt PDFs in Sandbox-Kontexten nicht an.)
    if mime.contains("html") || mime.contains("svg") || mime.contains("xml") || mime.contains("javascript") {
        res.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'; img-src * data:; style-src 'unsafe-inline'"),
        );
    }
    Ok(res)
}

pub async fn thumbnail(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    req: Request,
) -> AppResult<Response> {
    let path = st.storage.thumb_path(id);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(AppError::not_found());
    }
    let mut res = serve(path, "image/jpeg", req).await?;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=600"));
    Ok(res)
}

pub(crate) async fn serve(path: std::path::PathBuf, mime: &str, req: Request) -> AppResult<Response> {
    let mime = mime.parse().unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM);
    let res = match ServeFile::new_with_mime(path, &mime).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    if res.status() == StatusCode::NOT_FOUND {
        return Err(AppError::not_found());
    }
    Ok(res.map(Body::new))
}

pub async fn text(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Response> {
    let content: Option<String> = sqlx::query_scalar("SELECT content FROM documents WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.pool)
        .await?;
    let content = content.ok_or_else(AppError::not_found)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], content).into_response())
}
