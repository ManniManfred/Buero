// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! MCP-Endpunkt (Model Context Protocol, Streamable HTTP) für KI-Assistenten.
//!
//! - Zugang nur mit eigenem Zugangsschlüssel (`api_tokens`), unabhängig vom
//!   Büro-Passwort; Sitzungen der Oberfläche gelten hier nicht und die
//!   Schlüssel gelten nirgends sonst.
//! - Nur lesend – außer der Schlüssel darf ausdrücklich hochladen
//!   (`can_upload`): dann zusätzlich Dokumente hochladen (`upload_document`,
//!   `POST /mcp/upload`), Tags vergeben (`add_tags`) und Dokumente in
//!   Sammlungen einordnen, die bei Bedarf angelegt werden (`add_to_collection`),
//!   sowie ein fehlendes Dokumentdatum eintragen (`set_document_dates`).
//!   Löschen oder Ändern bestehender Dokumente und Sammlungen geht nie.
//! - Dokumente aus gesperrten Sammlungen (samt Untersammlungen) werden nie
//!   ausgeliefert, auch nicht als Suchtreffer; gesperrte Sammlungen selbst
//!   tauchen nicht auf.

use std::collections::HashSet;

use axum::extract::{Multipart, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngCore;
use serde_json::{json, Value};

use shared::{ApiToken, ApiTokenCreated, ApiTokenInput, ApiTokenUpdate};

use crate::auth::{hash_token, token_from_request};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::search::Query_;
use crate::state::AppState;
use crate::util::now;

const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const TOKEN_PREFIX: &str = "bmcp_";

// ---------------------------------------------------------------------------
// Zugangsschlüssel verwalten (geschützte API der Oberfläche)
// ---------------------------------------------------------------------------

pub async fn list_tokens(State(st): State<AppState>) -> AppResult<Json<Vec<ApiToken>>> {
    let rows: Vec<(i64, String, i64, Option<i64>, bool)> =
        sqlx::query_as("SELECT id, name, created_at, last_used_at, can_upload <> 0 FROM api_tokens ORDER BY id")
            .fetch_all(&st.pool)
            .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, created_at, last_used_at, can_upload)| ApiToken { id, name, created_at, last_used_at, can_upload })
            .collect(),
    ))
}

pub async fn update_token(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(i): Json<ApiTokenUpdate>,
) -> AppResult<StatusCode> {
    let n = sqlx::query("UPDATE api_tokens SET can_upload = ? WHERE id = ?")
        .bind(i.can_upload)
        .bind(id)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_token(
    State(st): State<AppState>,
    Json(i): Json<ApiTokenInput>,
) -> AppResult<Json<ApiTokenCreated>> {
    let name: String = i.name.trim().chars().take(100).collect();
    let name = if name.is_empty() { "KI-Zugang".to_string() } else { name };
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
    let ts = now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO api_tokens (name, token_hash, created_at, can_upload) VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(&name)
    .bind(hash_token(&token))
    .bind(ts)
    .bind(i.can_upload)
    .fetch_one(&st.pool)
    .await?;
    Ok(Json(ApiTokenCreated {
        info: ApiToken { id, name, created_at: ts, last_used_at: None, can_upload: i.can_upload },
        token,
    }))
}

pub async fn delete_token(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    let n = sqlx::query("DELETE FROM api_tokens WHERE id = ?").bind(id).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Schlüssel aus dem `Authorization`-Header, nie aus `?token=` (landet sonst in Logs)
fn bearer(req: &Request) -> Option<String> {
    let has_header = req.headers().contains_key(axum::http::header::AUTHORIZATION);
    token_from_request(req).filter(|t| has_header && t.starts_with(TOKEN_PREFIX))
}

/// Was ein gültiger Schlüssel darf
#[derive(Clone, Copy)]
struct Perm {
    can_upload: bool,
}

async fn check_token(st: &AppState, token: Option<String>) -> AppResult<Perm> {
    let token = token.ok_or_else(AppError::unauthorized)?;
    let row: Option<(i64, bool)> = sqlx::query_as("SELECT id, can_upload <> 0 FROM api_tokens WHERE token_hash = ?")
        .bind(hash_token(&token))
        .fetch_optional(&st.pool)
        .await?;
    let Some((id, can_upload)) = row else {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return Err(AppError::unauthorized());
    };
    sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?").bind(now()).bind(id).execute(&st.pool).await?;
    Ok(Perm { can_upload })
}

fn unauthorized(e: AppError) -> Response {
    let mut r = e.into_response();
    r.headers_mut().insert("WWW-Authenticate", axum::http::HeaderValue::from_static("Bearer"));
    r
}

// ---------------------------------------------------------------------------
// Sperre
// ---------------------------------------------------------------------------

struct Lock {
    /// Gesperrte Sammlungen samt Untersammlungen
    all: HashSet<i64>,
    /// Nur die direkt gesperrten (der Suchindex kennt die Vorfahren jedes Dokuments)
    roots: Vec<i64>,
}

impl Lock {
    async fn load(st: &AppState) -> AppResult<Lock> {
        let roots: Vec<i64> = sqlx::query_scalar("SELECT id FROM collections WHERE ai_locked <> 0")
            .fetch_all(&st.pool)
            .await?;
        let mut all = HashSet::new();
        if !roots.is_empty() {
            let tree = db::collection_tree(&st.pool).await?;
            for r in &roots {
                all.extend(db::descendants(&tree, *r));
            }
        }
        Ok(Lock { all, roots })
    }

    fn blocks(&self, collection_ids: &[i64]) -> bool {
        collection_ids.iter().any(|c| self.all.contains(c))
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC
// ---------------------------------------------------------------------------

pub async fn get_mcp() -> StatusCode {
    // Kein Server-Push (SSE)
    StatusCode::METHOD_NOT_ALLOWED
}

pub async fn post_mcp(State(st): State<AppState>, req: Request) -> Response {
    let token = bearer(&req);
    let perm = match check_token(&st, token).await {
        Ok(p) => p,
        Err(e) => return unauthorized(e),
    };
    // Mit Upload-Recht passen Dateien (Base64) in die Anfrage
    let limit = if perm.can_upload { MAX_RPC_UPLOAD } else { 1024 * 1024 };
    let body = match axum::body::to_bytes(req.into_body(), limit).await {
        Ok(b) => b,
        Err(_) => return rpc_error(Value::Null, -32700, "Anfrage zu groß oder unlesbar").into_response(),
    };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_error(Value::Null, -32700, "Ungültiges JSON").into_response(),
    };
    if let Value::Array(batch) = msg {
        let mut out = Vec::new();
        for m in batch {
            if let Some(r) = handle(&st, m, perm).await {
                out.push(r);
            }
        }
        return if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { Json(Value::Array(out)).into_response() };
    }
    match handle(&st, msg, perm).await {
        Some(r) => Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } }))
}

/// Eine Nachricht verarbeiten; `None` für Benachrichtigungen und Antworten.
async fn handle(st: &AppState, msg: Value, perm: Perm) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?.to_string();
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method.as_str() {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = PROTOCOLS.iter().find(|v| **v == requested).copied().unwrap_or(PROTOCOLS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "buero", "title": "Büro", "version": super::desktop::server_version() },
                "instructions": instructions(perm)
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs(perm) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Ok(match call_tool(st, name, &args, perm).await {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            })
        }
        _ => Err((-32601, format!("Unbekannte Methode: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": m } }),
    })
}

fn instructions(perm: Perm) -> String {
    let mut s = String::from(
        "Zugriff auf die persönliche Dokumentenverwaltung Büro. \
         Mit search_documents suchen (Treffer enthalten kurze Ausschnitte), mit get_document \
         den erkannten Text eines Dokuments lesen. Den Volltext nur abrufen, wenn er für die \
         Frage nötig ist. Manche Sammlungen sind für diesen Zugang gesperrt und unsichtbar.",
    );
    if perm.can_upload {
        s.push_str(
            " Dieser Zugang darf außerdem Dokumente hochladen (upload_document, Datei als Base64; \
             größere Dateien per multipart an POST /mcp/upload mit demselben Schlüssel), \
             Tags vergeben (add_tags) und Dokumente in Sammlungen einordnen (add_to_collection, \
             fehlende Sammlungen werden angelegt) sowie ein fehlendes Dokumentdatum eintragen \
             (set_document_dates). Löschen oder Ändern geht nicht.",
        );
    } else {
        s.push_str(" Der Zugang ist nur lesend.");
    }
    s
}

fn tool_defs(perm: Perm) -> Value {
    let ro = json!({ "readOnlyHint": true, "openWorldHint": false });
    let mut tools = json!([
        {
            "name": "search_documents",
            "title": "Dokumente suchen",
            "description": "Volltextsuche über Titel, Dateiname, Notiz, Tags, Sammlungen und erkannten \
                Inhalt (OCR). Deutsche Wortstämme, Umlaute egal, tippfehlertolerant. Syntax: Phrasen \
                in Anführungszeichen, -wort schließt aus, OR, Felder title:, tags:, filename:, content:. \
                Leere Suche listet Dokumente (z. B. mit Filter). Liefert IDs, Metadaten und Ausschnitte.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Suchtext, darf leer sein" },
                    "collection_id": { "type": "integer", "description": "Nur diese Sammlung (inkl. Untersammlungen)" },
                    "tag_id": { "type": "integer" },
                    "kind": { "type": "string", "enum": ["pdf","image","word","spreadsheet","presentation","text","markdown","email","audio","video","other"] },
                    "unfiled": { "type": "boolean", "description": "Nur Dokumente ohne Sammlung (Eingang)" },
                    "sort": { "type": "string", "enum": ["relevance","newest","oldest","date_desc","date_asc"],
                              "description": "date_* = fachliches Dokumentdatum" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 }
                }
            },
            "annotations": ro
        },
        {
            "name": "get_document",
            "title": "Dokument lesen",
            "description": "Metadaten und erkannten Text eines Dokuments. Lange Texte abschnittsweise \
                mit offset/max_chars lesen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Startzeichen im Text" },
                    "max_chars": { "type": "integer", "minimum": 0, "maximum": 100000, "default": 20000,
                                   "description": "0 = nur Metadaten" }
                },
                "required": ["id"]
            },
            "annotations": ro
        },
        {
            "name": "list_collections",
            "title": "Sammlungen",
            "description": "Alle (nicht gesperrten) Sammlungen mit Pfad, ID und Anzahl Dokumente.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro
        },
        {
            "name": "list_tags",
            "title": "Tags",
            "description": "Alle Tags mit ID.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro
        }
    ]);
    if perm.can_upload {
        let w = json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false });
        let tags = json!({ "type": "array", "items": { "type": "string" },
                           "description": "Tag-Namen; fehlende Tags werden angelegt" });
        let arr = tools.as_array_mut().expect("Liste");
        arr.push(json!({
            "name": "upload_document",
            "title": "Dokument hochladen",
            "description": "Datei als neues Dokument hochladen (Inhalt Base64-kodiert, max. ca. 24 MB). \
                Gleiche Dateien werden erkannt und nicht doppelt gespeichert; Tags und Sammlung werden \
                dann am vorhandenen Dokument ergänzt. Texterkennung läuft danach im Hintergrund.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "description": "Dateiname mit Endung" },
                    "content_base64": { "type": "string" },
                    "title": { "type": "string", "description": "Sonst aus dem Dateinamen" },
                    "document_date": { "type": "string", "description": "JJJJ-MM-TT" },
                    "tags": tags.clone(),
                    "collection_id": { "type": "integer" }
                },
                "required": ["filename", "content_base64"]
            },
            "annotations": w.clone()
        }));
        arr.push(json!({
            "name": "add_tags",
            "title": "Tags vergeben",
            "description": "Tags zu Dokumenten hinzufügen (vorhandene Tags bleiben).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "document_ids": { "type": "array", "items": { "type": "integer" } },
                    "tags": tags
                },
                "required": ["document_ids", "tags"]
            },
            "annotations": w.clone()
        }));
        arr.push(json!({
            "name": "add_to_collection",
            "title": "In Sammlung einordnen",
            "description": "Dokumente einer Sammlung hinzufügen (bisherige Sammlungen bleiben). Die Sammlung \
                per collection_id oder als Pfad von oben, z. B. [\"Musik\", \"Noten\"]; fehlende Sammlungen \
                im Pfad werden angelegt. Ohne document_ids wird nur die Sammlung angelegt. Liefert die ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "document_ids": { "type": "array", "items": { "type": "integer" } },
                    "collection_id": { "type": "integer" },
                    "path": { "type": "array", "items": { "type": "string" },
                              "description": "Namen von der obersten Sammlung abwärts" }
                }
            },
            "annotations": w.clone()
        }));
        arr.push(json!({
            "name": "set_document_dates",
            "title": "Dokumentdatum eintragen",
            "description": "Fachliches Dokumentdatum (JJJJ-MM-TT) für Dokumente eintragen, die noch keins \
                haben. Ein vorhandenes Datum wird nie überschrieben (dann skipped).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "id": { "type": "integer" }, "date": { "type": "string" } },
                            "required": ["id", "date"]
                        }
                    }
                },
                "required": ["items"]
            },
            "annotations": w
        }));
    }
    tools
}

async fn call_tool(st: &AppState, name: &str, args: &Value, perm: Perm) -> Result<String, String> {
    let lock = Lock::load(st).await.map_err(|e| e.message)?;
    let r = match name {
        "upload_document" | "add_tags" | "add_to_collection" | "set_document_dates" if !perm.can_upload => {
            return Err("Dieser Zugangsschlüssel darf nicht hochladen".into())
        }
        "upload_document" => upload_document(st, &lock, args).await,
        "add_tags" => add_tags(st, &lock, args).await,
        "add_to_collection" => add_to_collection(st, &lock, args).await,
        "set_document_dates" => set_document_dates(st, &lock, args).await,
        "search_documents" => search(st, &lock, args).await,
        "get_document" => get_document(st, &lock, args).await,
        "list_collections" => list_collections(st, &lock).await,
        "list_tags" => list_tags(st).await,
        _ => return Err(format!("Unbekanntes Werkzeug: {name}")),
    };
    r.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
}

fn int(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

fn string(args: &Value, key: &str) -> String {
    args.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn err<E: std::fmt::Display>(e: E) -> String {
    tracing::error!("MCP: {e}");
    "Interner Fehler".to_string()
}

/// Pfade der Sammlungen („Finanzen / Rechnungen“)
fn paths(tree: &std::collections::HashMap<i64, (Option<i64>, String)>, ids: &[i64]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            let mut parts = Vec::new();
            let mut cur = Some(*id);
            while let Some(c) = cur {
                let Some((p, n)) = tree.get(&c) else { break };
                parts.push(n.clone());
                cur = *p;
                if parts.len() > 64 {
                    break;
                }
            }
            parts.reverse();
            parts.join(" / ")
        })
        .collect()
}

async fn tag_names(st: &AppState) -> Result<std::collections::HashMap<i64, String>, String> {
    Ok(sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM tags")
        .fetch_all(&st.pool)
        .await
        .map_err(err)?
        .into_iter()
        .collect())
}

/// HTML-Ausschnitt aus der Suche als Text, Treffer mit **…** markiert
fn plain_snippet(html: &str) -> String {
    let s = html.replace("<b>", "**").replace("</b>", "**");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

async fn search(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    let text: String = string(args, "query").chars().take(500).collect();
    let collection = int(args, "collection_id");
    if let Some(c) = collection {
        if lock.all.contains(&c) {
            return Err("Sammlung nicht gefunden".into());
        }
    }
    let limit = int(args, "limit").unwrap_or(10).clamp(1, 50) as usize;
    let q_offset = int(args, "offset").unwrap_or(0).clamp(0, 10_000) as usize;
    let q = Query_ {
        text: text.clone(),
        collections: collection.into_iter().collect(),
        direct: false,
        hide_children: false,
        tags: int(args, "tag_id").into_iter().collect(),
        kind: string(args, "kind"),
        unfiled: args.get("unfiled").and_then(Value::as_bool).unwrap_or(false),
        exclude_collections: lock.roots.clone(),
        sort: string(args, "sort"),
        offset: q_offset,
        limit,
    };
    let st2 = st.clone();
    let hits = tokio::task::spawn_blocking(move || st2.search.search(&q)).await.map_err(err)?.map_err(err)?;
    let ids: Vec<i64> = hits.ids.iter().map(|(id, _)| *id).collect();
    // Zweite Prüfung gegen die Datenbank (Index kann kurz hinterherhinken)
    let items: Vec<_> = db::summaries(&st.pool, &ids)
        .await
        .map_err(err)?
        .into_iter()
        .filter(|d| !lock.blocks(&d.collection_ids))
        .collect();
    let ids: Vec<i64> = items.iter().map(|d| d.id).collect();

    let (total, fuzzy) = (hits.total, hits.fuzzy);
    let mut snippets = Vec::new();
    if !text.trim().is_empty() && !ids.is_empty() {
        let texts = db::contents(&st.pool, &ids).await.map_err(err)?;
        let st2 = st.clone();
        snippets = tokio::task::spawn_blocking(move || st2.search.snippets(&hits, &text, &texts))
            .await
            .map_err(err)?;
    }

    let tree = db::collection_tree(&st.pool).await.map_err(err)?;
    let tags = tag_names(st).await?;
    let results: Vec<Value> = items
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "title": d.title,
                "filename": d.original_filename,
                "kind": d.kind,
                "document_date": d.document_date,
                "uploaded": crate::util::fmt_date(d.created_at),
                "collections": paths(&tree, &d.collection_ids),
                "tags": d.tag_ids.iter().filter_map(|t| tags.get(t)).collect::<Vec<_>>(),
                "snippet": snippets.iter().find(|(id, _)| *id == d.id).map(|(_, s)| plain_snippet(s)),
            })
        })
        .collect();
    Ok(json!({
        "total": total,
        "offset": q_offset,
        "fuzzy": fuzzy,
        "results": results,
    }))
}

async fn get_document(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    let id = int(args, "id").ok_or("Parameter id fehlt")?;
    let not_found = || format!("Dokument {id} nicht gefunden");
    let row = db::get_doc(&st.pool, id).await.map_err(err)?.ok_or_else(not_found)?;
    let summary = db::summary(&st.pool, id).await.map_err(err)?.ok_or_else(not_found)?;
    if lock.blocks(&summary.collection_ids) {
        // Nicht verraten, dass es das Dokument gibt
        return Err(not_found());
    }
    let tree = db::collection_tree(&st.pool).await.map_err(err)?;
    let tags = tag_names(st).await?;
    let content = db::contents(&st.pool, &[id]).await.map_err(err)?.into_iter().next().map(|(_, c)| c).unwrap_or_default();
    let total = content.chars().count();
    let offset = int(args, "offset").unwrap_or(0).max(0) as usize;
    let max = int(args, "max_chars").unwrap_or(20000).clamp(0, 100_000) as usize;
    let part: String = content.chars().skip(offset).take(max).collect();
    let end = (offset + part.chars().count()).min(total);
    Ok(json!({
        "id": id,
        "title": row.title,
        "filename": row.original_filename,
        "mime": row.mime,
        "kind": summary.kind,
        "size_bytes": row.size,
        "pages": row.page_count,
        "version": row.version,
        "document_date": row.document_date,
        "uploaded": crate::util::fmt_date(row.created_at),
        "updated": crate::util::fmt_date(row.updated_at),
        "note": row.description,
        "collections": paths(&tree, &summary.collection_ids),
        "tags": summary.tag_ids.iter().filter_map(|t| tags.get(t)).collect::<Vec<_>>(),
        "text_status": row.extract_status,
        "text_error": row.extract_error,
        "text_length": total,
        "text_range": [offset.min(total), end],
        "text_truncated": end < total,
        "text": part,
    }))
}

async fn list_collections(st: &AppState, lock: &Lock) -> Result<Value, String> {
    // Mitgliedschaften zählen, ohne Dokumente, die (auch) in gesperrten Sammlungen liegen –
    // sonst verriete die Anzahl, dass es sie gibt
    let members: Vec<(i64, i64)> = sqlx::query_as("SELECT document_id, collection_id FROM document_collections")
        .fetch_all(&st.pool)
        .await
        .map_err(err)?;
    let blocked: HashSet<i64> =
        members.iter().filter(|(_, c)| lock.all.contains(c)).map(|(d, _)| *d).collect();
    let mut counts: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (d, c) in &members {
        if !blocked.contains(d) {
            *counts.entry(*c).or_default() += 1;
        }
    }
    let tree = db::collection_tree(&st.pool).await.map_err(err)?;
    let mut out: Vec<(String, Value)> = tree
        .keys()
        .copied()
        .filter(|id| !lock.all.contains(id))
        .map(|id| (id, counts.get(&id).copied().unwrap_or(0)))
        .map(|(id, n)| {
            let path = paths(&tree, &[id]).remove(0);
            (path.clone(), json!({ "id": id, "path": path, "documents": n }))
        })
        .collect();
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    Ok(Value::Array(out.into_iter().map(|(_, v)| v).collect()))
}

async fn list_tags(st: &AppState) -> Result<Value, String> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM tags ORDER BY name COLLATE NOCASE")
        .fetch_all(&st.pool)
        .await
        .map_err(err)?;
    Ok(Value::Array(rows.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect()))
}

// ---------------------------------------------------------------------------
// Schreiben (nur mit `can_upload`)
// ---------------------------------------------------------------------------

/// Größte JSON-RPC-Anfrage mit Upload-Recht (Base64 braucht 4/3 der Dateigröße)
const MAX_RPC_UPLOAD: usize = 32 * 1024 * 1024;

/// Tags per Name finden oder anlegen
async fn resolve_tags(st: &AppState, names: &[String]) -> Result<Vec<i64>, String> {
    let mut ids = Vec::new();
    for n in names.iter().take(50) {
        let name: String = n.trim().chars().take(100).collect();
        if name.is_empty() {
            continue;
        }
        sqlx::query("INSERT OR IGNORE INTO tags (name) VALUES (?)").bind(&name).execute(&st.pool).await.map_err(err)?;
        let id: i64 = sqlx::query_scalar("SELECT id FROM tags WHERE name = ? COLLATE NOCASE")
            .bind(&name)
            .fetch_one(&st.pool)
            .await
            .map_err(err)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn string_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(s)) => s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// Hochgeladene (gestagte) Dateien speichern. Sammlungen müssen frei sein;
/// Duplikate von Dokumenten in gesperrten Sammlungen werden nicht angefasst
/// und nicht beschrieben.
async fn store(st: &AppState, lock: &Lock, mut form: super::documents::UploadForm) -> Result<Vec<Value>, String> {
    let discard_all = |st: AppState, ups: Vec<super::documents::Upload>| async move {
        for u in &ups {
            st.storage.discard(&u.staged).await;
        }
    };
    if form.collections.iter().any(|c| lock.all.contains(c)) {
        discard_all(st.clone(), std::mem::take(&mut form.uploads)).await;
        return Err("Sammlung nicht gefunden".into());
    }
    let names = std::mem::take(&mut form.tag_names);
    form.tags = match resolve_tags(st, &names).await {
        Ok(t) => t,
        Err(e) => {
            discard_all(st.clone(), std::mem::take(&mut form.uploads)).await;
            return Err(e);
        }
    };
    form.link_duplicates = true;

    let mut skipped = Vec::new();
    let mut keep = Vec::new();
    for u in std::mem::take(&mut form.uploads) {
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT document_id FROM document_versions WHERE sha256 = ? ORDER BY id LIMIT 1")
                .bind(&u.staged.sha256)
                .fetch_optional(&st.pool)
                .await
                .map_err(err)?;
        let blocked = match existing {
            Some(id) => db::summary(&st.pool, id).await.map_err(err)?.is_some_and(|d| lock.blocks(&d.collection_ids)),
            None => false,
        };
        if blocked {
            st.storage.discard(&u.staged).await;
            skipped.push(json!({ "filename": u.filename, "skipped": true,
                                 "reason": "Gleiche Datei liegt bereits in einer gesperrten Sammlung" }));
        } else {
            keep.push(u);
        }
    }
    form.uploads = keep;
    let mut out = Vec::new();
    if !form.uploads.is_empty() {
        let tags = tag_names(st).await?;
        for r in super::documents::store_uploads(st, form).await.map_err(|e| e.message)? {
            let d = r.document;
            out.push(json!({
                "id": d.id,
                "title": d.title,
                "filename": d.original_filename,
                "duplicate": r.duplicate,
                "tags": d.tag_ids.iter().filter_map(|t| tags.get(t)).collect::<Vec<_>>(),
            }));
        }
    }
    out.extend(skipped);
    Ok(out)
}

async fn upload_document(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    use base64::Engine;
    let filename = super::documents::sanitize_filename(&string(args, "filename"));
    let data = string(args, "content_base64");
    let data: String = data.split_once(";base64,").map(|(_, d)| d).unwrap_or(&data).chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|_| "content_base64 ist kein gültiges Base64".to_string())?;
    if bytes.is_empty() {
        return Err("Datei ist leer".into());
    }
    let stream = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(bytes)) });
    let staged = st.storage.stage(Box::pin(stream), st.config.max_upload_bytes).await.map_err(|e| e.to_string())?;
    let mime = super::documents::guess_mime(&filename, None);
    let date = Some(string(args, "document_date")).filter(|d| crate::util::valid_date(d));
    let title = Some(string(args, "title")).filter(|t| !t.trim().is_empty());
    let form = super::documents::UploadForm {
        uploads: vec![super::documents::Upload { staged, filename, mime }],
        collections: int(args, "collection_id").into_iter().collect(),
        tag_names: string_list(args, "tags"),
        title,
        date,
        ..Default::default()
    };
    let mut r = store(st, lock, form).await?;
    Ok(r.pop().unwrap_or(Value::Null))
}

async fn add_tags(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    let ids: Vec<i64> = match args.get("document_ids") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_i64()).collect(),
        _ => Vec::new(),
    };
    if ids.is_empty() {
        return Err("Parameter document_ids fehlt".into());
    }
    let names = string_list(args, "tags");
    // Nur sichtbare Dokumente; Unbekannte und Gesperrte gleich behandeln
    let docs: Vec<i64> = db::summaries(&st.pool, &ids)
        .await
        .map_err(err)?
        .into_iter()
        .filter(|d| !lock.blocks(&d.collection_ids))
        .map(|d| d.id)
        .collect();
    let missing: Vec<i64> = ids.iter().copied().filter(|i| !docs.contains(i)).collect();
    if !missing.is_empty() {
        return Err(format!("Dokument(e) nicht gefunden: {missing:?}"));
    }
    let tag_ids = resolve_tags(st, &names).await?;
    if tag_ids.is_empty() {
        return Err("Parameter tags fehlt".into());
    }
    for d in &docs {
        for t in &tag_ids {
            sqlx::query("INSERT OR IGNORE INTO document_tags (document_id, tag_id) VALUES (?, ?)")
                .bind(d)
                .bind(t)
                .execute(&st.pool)
                .await
                .map_err(err)?;
        }
    }
    st.reindex(&docs).await.map_err(err)?;
    Ok(json!({ "documents": docs.len(), "tags": names }))
}

/// Sammlung über ihren Pfad finden oder (samt fehlender Elternsammlungen) anlegen
async fn collection_by_path(st: &AppState, lock: &Lock, path: &[String]) -> Result<i64, String> {
    let mut parent: Option<i64> = None;
    for seg in path {
        let name: String = seg.trim().chars().take(200).collect();
        if name.is_empty() {
            return Err("Leerer Name im Pfad".into());
        }
        sqlx::query(
            "INSERT OR IGNORE INTO collections (parent_id, name, created_at) VALUES (?, ?, ?)",
        )
        .bind(parent)
        .bind(&name)
        .bind(now())
        .execute(&st.pool)
        .await
        .map_err(err)?;
        let id: i64 = sqlx::query_scalar("SELECT id FROM collections WHERE IFNULL(parent_id, 0) = ? AND name = ?")
            .bind(parent.unwrap_or(0))
            .bind(&name)
            .fetch_one(&st.pool)
            .await
            .map_err(err)?;
        if lock.all.contains(&id) {
            return Err("Sammlung nicht gefunden".into());
        }
        parent = Some(id);
    }
    parent.ok_or_else(|| "Parameter collection_id oder path fehlt".to_string())
}

async fn add_to_collection(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    let collection = match int(args, "collection_id") {
        Some(c) => {
            let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM collections WHERE id = ?")
                .bind(c)
                .fetch_optional(&st.pool)
                .await
                .map_err(err)?;
            if exists.is_none() || lock.all.contains(&c) {
                return Err("Sammlung nicht gefunden".into());
            }
            c
        }
        None => {
            let path: Vec<String> = match args.get("path") {
                Some(Value::String(s)) => s.split('/').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
                _ => string_list(args, "path"),
            };
            if path.len() > 32 {
                return Err("Pfad zu tief".into());
            }
            collection_by_path(st, lock, &path).await?
        }
    };
    let ids: Vec<i64> = match args.get("document_ids") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_i64()).collect(),
        _ => Vec::new(),
    };
    let docs: Vec<i64> = db::summaries(&st.pool, &ids)
        .await
        .map_err(err)?
        .into_iter()
        .filter(|d| !lock.blocks(&d.collection_ids))
        .map(|d| d.id)
        .collect();
    let missing: Vec<i64> = ids.iter().copied().filter(|i| !docs.contains(i)).collect();
    if !missing.is_empty() {
        return Err(format!("Dokument(e) nicht gefunden: {missing:?}"));
    }
    for d in &docs {
        sqlx::query("INSERT OR IGNORE INTO document_collections (document_id, collection_id) VALUES (?, ?)")
            .bind(d)
            .bind(collection)
            .execute(&st.pool)
            .await
            .map_err(err)?;
    }
    st.reindex(&docs).await.map_err(err)?;
    let tree = db::collection_tree(&st.pool).await.map_err(err)?;
    Ok(json!({ "collection_id": collection, "path": paths(&tree, &[collection]).remove(0), "documents": docs.len() }))
}

async fn set_document_dates(st: &AppState, lock: &Lock, args: &Value) -> Result<Value, String> {
    let items: Vec<(i64, String)> = match args.get("items") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| Some((int(v, "id")?, string(v, "date").trim().to_string())))
            .collect(),
        _ => Vec::new(),
    };
    if items.is_empty() {
        return Err("Parameter items fehlt".into());
    }
    if items.len() > 1000 {
        return Err("Höchstens 1000 Einträge auf einmal".into());
    }
    if let Some((id, d)) = items.iter().find(|(_, d)| !crate::util::valid_date(d)) {
        return Err(format!("Ungültiges Datum für Dokument {id}: {d} (erwartet JJJJ-MM-TT)"));
    }
    let ids: Vec<i64> = items.iter().map(|(id, _)| *id).collect();
    let visible: HashSet<i64> = db::summaries(&st.pool, &ids)
        .await
        .map_err(err)?
        .into_iter()
        .filter(|d| !lock.blocks(&d.collection_ids))
        .map(|d| d.id)
        .collect();
    let missing: Vec<i64> = ids.iter().copied().filter(|i| !visible.contains(i)).collect();
    if !missing.is_empty() {
        return Err(format!("Dokument(e) nicht gefunden: {missing:?}"));
    }
    let (mut set, mut skipped) = (Vec::new(), Vec::new());
    for (id, date) in &items {
        let n = sqlx::query("UPDATE documents SET document_date = ? WHERE id = ? AND IFNULL(document_date, '') = ''")
            .bind(date)
            .bind(id)
            .execute(&st.pool)
            .await
            .map_err(err)?
            .rows_affected();
        if n > 0 { set.push(*id) } else { skipped.push(*id) }
    }
    st.reindex(&set).await.map_err(err)?;
    Ok(json!({ "set": set.len(), "skipped": skipped }))
}

/// `POST /mcp/upload`: Dateien per multipart hochladen (Felder wie bei
/// `/api/documents`: `file` (mehrfach), `tags` als Namen, `collection_ids`,
/// `title`, `document_date`). Nur mit Upload-Recht.
pub async fn upload_multipart(State(st): State<AppState>, req: Request) -> Response {
    match check_token(&st, bearer(&req)).await {
        Ok(p) if p.can_upload => {}
        Ok(_) => return AppError::new(StatusCode::FORBIDDEN, "Dieser Zugangsschlüssel darf nicht hochladen").into_response(),
        Err(e) => return unauthorized(e),
    }
    let mut mp = match <Multipart as axum::extract::FromRequest<AppState>>::from_request(req, &st).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let form = match super::documents::read_upload_form(&st, &mut mp, st.config.max_upload_bytes).await {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let lock = match Lock::load(&st).await {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    match store(&st, &lock, form).await {
        Ok(v) => Json(Value::Array(v)).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}
