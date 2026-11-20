use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};

use shared::{DocumentSummary, VersionInfo};

use crate::search::IndexDoc;
use crate::util;

pub async fn connect(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    let url = format!("sqlite://{}", data_dir.join("buero.db").display());
    let opts = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .pragma("cache_size", "-64000")
        .pragma("temp_store", "memory")
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(16).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

#[derive(FromRow, Clone)]
pub struct DocRow {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub original_filename: String,
    pub mime: String,
    pub extension: String,
    pub size: i64,
    pub sha256: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub document_date: Option<String>,
    pub extract_status: String,
    pub extract_error: Option<String>,
    pub page_count: Option<i64>,
    pub has_thumbnail: bool,
}

pub const DOC_COLS: &str = "id, title, description, original_filename, mime, extension, size, sha256, \
    version, created_at, updated_at, document_date, extract_status, extract_error, page_count, has_thumbnail";

impl DocRow {
    pub fn summary(&self, collection_ids: Vec<i64>, tag_ids: Vec<i64>) -> DocumentSummary {
        DocumentSummary {
            id: self.id,
            title: self.title.clone(),
            original_filename: self.original_filename.clone(),
            mime: self.mime.clone(),
            kind: shared::kind_for(&self.extension, &self.mime).to_string(),
            size: self.size,
            created_at: self.created_at,
            updated_at: self.updated_at,
            document_date: self.document_date.clone(),
            version: self.version,
            extract_status: self.extract_status.clone(),
            page_count: self.page_count,
            has_thumbnail: self.has_thumbnail,
            collection_ids,
            tag_ids,
            snippet: None,
            score: None,
        }
    }
}

pub async fn get_doc(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<DocRow>> {
    sqlx::query_as::<_, DocRow>(&format!("SELECT {DOC_COLS} FROM documents WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

fn push_ids(qb: &mut QueryBuilder<'_, Sqlite>, ids: &[i64]) {
    qb.push("(");
    let mut sep = qb.separated(",");
    for id in ids {
        sep.push_bind(*id);
    }
    sep.push_unseparated(")");
}

/// Lädt Zuordnungen (Sammlungen, Tags) für viele Dokumente auf einmal.
async fn memberships(
    pool: &SqlitePool,
    table: &str,
    col: &str,
    ids: &[i64],
) -> sqlx::Result<HashMap<i64, Vec<i64>>> {
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    for chunk in ids.chunks(500) {
        let mut qb = QueryBuilder::<Sqlite>::new(format!(
            "SELECT document_id, {col} FROM {table} WHERE document_id IN "
        ));
        push_ids(&mut qb, chunk);
        let rows: Vec<(i64, i64)> = qb.build_query_as().fetch_all(pool).await?;
        for (d, c) in rows {
            map.entry(d).or_default().push(c);
        }
    }
    Ok(map)
}

/// Lädt Zusammenfassungen in der Reihenfolge der übergebenen IDs.
pub async fn summaries(pool: &SqlitePool, ids: &[i64]) -> sqlx::Result<Vec<DocumentSummary>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows: HashMap<i64, DocRow> = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let mut qb = QueryBuilder::<Sqlite>::new(format!("SELECT {DOC_COLS} FROM documents WHERE id IN "));
        push_ids(&mut qb, chunk);
        for r in qb.build_query_as::<DocRow>().fetch_all(pool).await? {
            rows.insert(r.id, r);
        }
    }
    let mut colls = memberships(pool, "document_collections", "collection_id", ids).await?;
    let mut tags = memberships(pool, "document_tags", "tag_id", ids).await?;
    Ok(ids
        .iter()
        .filter_map(|id| {
            rows.get(id).map(|r| {
                r.summary(colls.remove(id).unwrap_or_default(), tags.remove(id).unwrap_or_default())
            })
        })
        .collect())
}

pub async fn summary(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<DocumentSummary>> {
    Ok(summaries(pool, &[id]).await?.into_iter().next())
}

pub async fn versions(pool: &SqlitePool, id: i64) -> sqlx::Result<Vec<VersionInfo>> {
    let rows: Vec<(i64, String, i64, String, i64)> = sqlx::query_as(
        "SELECT version, original_filename, size, sha256, created_at FROM document_versions \
         WHERE document_id = ? ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(version, original_filename, size, sha256, created_at)| VersionInfo {
            version,
            original_filename,
            size,
            sha256,
            created_at,
        })
        .collect())
}

pub async fn contents(pool: &SqlitePool, ids: &[i64]) -> sqlx::Result<Vec<(i64, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT id, content FROM documents WHERE id IN ");
    push_ids(&mut qb, ids);
    qb.build_query_as().fetch_all(pool).await
}

/// Sammlungsbaum: id -> (parent, name)
pub async fn collection_tree(pool: &SqlitePool) -> sqlx::Result<HashMap<i64, (Option<i64>, String)>> {
    let rows: Vec<(i64, Option<i64>, String)> =
        sqlx::query_as("SELECT id, parent_id, name FROM collections").fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(id, p, n)| (id, (p, n))).collect())
}

/// Sammlung inkl. aller Vorfahren
pub fn with_ancestors(tree: &HashMap<i64, (Option<i64>, String)>, ids: &[i64]) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for &id in ids {
        let mut cur = Some(id);
        let mut guard = 0;
        while let Some(c) = cur {
            if out.contains(&c) || guard > 64 {
                break;
            }
            out.push(c);
            cur = tree.get(&c).and_then(|(p, _)| *p);
            guard += 1;
        }
    }
    out
}

/// IDs einer Sammlung und aller Untersammlungen
pub fn descendants(tree: &HashMap<i64, (Option<i64>, String)>, root: i64) -> Vec<i64> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let cur = out[i];
        for (id, (p, _)) in tree {
            if *p == Some(cur) && !out.contains(id) {
                out.push(*id);
            }
        }
        i += 1;
    }
    out
}

/// Baut die Indexeinträge für die angegebenen Dokumente.
pub async fn index_docs(pool: &SqlitePool, ids: &[i64]) -> sqlx::Result<Vec<IndexDoc>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let tree = collection_tree(pool).await?;
    let tag_names: HashMap<i64, String> =
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM tags")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(200) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT id, title, original_filename, description, content, extension, mime, created_at, document_date \
             FROM documents WHERE id IN ",
        );
        push_ids(&mut qb, chunk);
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, String, String, String, String, i64, Option<String>)> =
            qb.build_query_as().fetch_all(pool).await?;
        let colls = memberships(pool, "document_collections", "collection_id", chunk).await?;
        let tags = memberships(pool, "document_tags", "tag_id", chunk).await?;
        for (id, title, filename, description, content, ext, mime, created, date) in rows {
            let direct = colls.get(&id).cloned().unwrap_or_default();
            let all = with_ancestors(&tree, &direct);
            let tag_ids = tags.get(&id).cloned().unwrap_or_default();
            out.push(IndexDoc {
                id,
                title,
                filename,
                description,
                content,
                collection_names: all.iter().filter_map(|c| tree.get(c).map(|(_, n)| n.clone())).collect(),
                collection_ids: all,
                tag_names: tag_ids.iter().filter_map(|t| tag_names.get(t).cloned()).collect(),
                tag_ids,
                kind: shared::kind_for(&ext, &mime).to_string(),
                created,
                date: date.as_deref().and_then(util::date_to_ts).unwrap_or(created),
            });
        }
    }
    Ok(out)
}

pub async fn all_doc_ids(pool: &SqlitePool) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar("SELECT id FROM documents").fetch_all(pool).await
}

pub async fn docs_in_collections(pool: &SqlitePool, coll_ids: &[i64]) -> sqlx::Result<Vec<i64>> {
    if coll_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT DISTINCT document_id FROM document_collections WHERE collection_id IN ",
    );
    push_ids(&mut qb, coll_ids);
    qb.build_query_scalar().fetch_all(pool).await
}

pub async fn docs_with_tag(pool: &SqlitePool, tag: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar("SELECT document_id FROM document_tags WHERE tag_id = ?")
        .bind(tag)
        .fetch_all(pool)
        .await
}

/// Wird ein Datei-Hash noch von irgendeiner Version referenziert?
pub async fn sha_in_use(pool: &SqlitePool, sha: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM document_versions WHERE sha256 = ?")
        .bind(sha)
        .fetch_one(pool)
        .await?;
    Ok(n > 0)
}
