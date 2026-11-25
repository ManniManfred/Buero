// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Übersicht: Speicherplatz und Kennzahlen.

use std::collections::HashMap;
use std::path::Path;

use axum::extract::State;
use axum::Json;
use shared::{Dashboard, DocumentBrief, KindStat, MonthStat, StorageUsage};

use crate::error::AppResult;
use crate::state::AppState;

pub async fn dashboard(State(st): State<AppState>) -> AppResult<Json<Dashboard>> {
    let data_dir = st.config.data_dir.clone();
    let disk = tokio::task::spawn_blocking(move || (disk_space(&data_dir), storage_usage(&data_dir)));

    let pool = &st.pool;
    let (documents, documents_size, pending, failed): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), IFNULL(SUM(size), 0), \
         IFNULL(SUM(extract_status IN ('pending','processing')), 0), \
         IFNULL(SUM(extract_status = 'failed'), 0) FROM documents",
    )
    .fetch_one(pool)
    .await?;
    let (old_versions, old_versions_size): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), IFNULL(SUM(v.size), 0) FROM document_versions v \
         JOIN documents d ON d.id = v.document_id WHERE v.version <> d.version",
    )
    .fetch_one(pool)
    .await?;
    let count = |sql: &'static str| async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await };
    let unfiled = count(
        "SELECT COUNT(*) FROM documents d WHERE NOT EXISTS \
         (SELECT 1 FROM document_collections dc WHERE dc.document_id = d.id)",
    )
    .await?;
    let collections = count("SELECT COUNT(*) FROM collections").await?;
    let tags = count("SELECT COUNT(*) FROM tags").await?;
    let shares = count("SELECT COUNT(*) FROM shares").await?;

    // Dokumentart wird aus Endung/MIME-Typ abgeleitet, daher in Rust gruppieren
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT extension, mime, COUNT(*), IFNULL(SUM(size), 0) FROM documents GROUP BY extension, mime",
    )
    .fetch_all(pool)
    .await?;
    let mut kinds: HashMap<&'static str, (i64, i64)> = HashMap::new();
    for (ext, mime, n, size) in rows {
        let e = kinds.entry(shared::kind_for(&ext, &mime)).or_default();
        e.0 += n;
        e.1 += size;
    }
    let mut kinds: Vec<KindStat> =
        kinds.into_iter().map(|(k, (count, size))| KindStat { kind: k.to_string(), count, size }).collect();
    kinds.sort_by(|a, b| b.size.cmp(&a.size).then(b.count.cmp(&a.count)));

    // Letzte 12 Monate, auch solche ohne neue Dokumente
    let month_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT strftime('%Y-%m', created_at, 'unixepoch', 'localtime') AS m, COUNT(*), IFNULL(SUM(size), 0) \
         FROM documents WHERE created_at >= CAST(strftime('%s', date('now', 'localtime', 'start of month', '-11 months'), 'utc') AS INTEGER) \
         GROUP BY m",
    )
    .fetch_all(pool)
    .await?;
    let labels: Vec<String> = sqlx::query_scalar(
        "WITH RECURSIVE n(i) AS (SELECT 11 UNION ALL SELECT i - 1 FROM n WHERE i > 0) \
         SELECT strftime('%Y-%m', date('now', 'localtime', 'start of month', '-' || i || ' months')) FROM n",
    )
    .fetch_all(pool)
    .await?;
    let months = labels
        .into_iter()
        .map(|m| {
            let (count, size) =
                month_rows.iter().find(|r| r.0 == m).map(|r| (r.1, r.2)).unwrap_or((0, 0));
            MonthStat { month: m, count, size }
        })
        .collect();

    let largest = briefs(pool, "ORDER BY size DESC, id DESC LIMIT 5").await?;
    let recent = briefs(pool, "ORDER BY created_at DESC, id DESC LIMIT 5").await?;

    let ((disk_total, disk_free), storage) = disk.await?;
    Ok(Json(Dashboard {
        disk_total,
        disk_free,
        storage,
        documents,
        documents_size,
        old_versions,
        old_versions_size,
        collections,
        tags,
        shares,
        pending,
        failed,
        unfiled,
        kinds,
        months,
        largest,
        recent,
    }))
}

async fn briefs(pool: &sqlx::SqlitePool, order: &str) -> AppResult<Vec<DocumentBrief>> {
    let rows: Vec<(i64, String, String, String, i64, i64)> = sqlx::query_as(&format!(
        "SELECT id, title, extension, mime, size, created_at FROM documents {order}"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, title, ext, mime, size, created_at)| DocumentBrief {
            id,
            title,
            kind: shared::kind_for(&ext, &mime).to_string(),
            size,
            created_at,
        })
        .collect())
}

/// Gesamtgröße und freier Platz (für normale Benutzer verfügbar) des Datenträgers
#[cfg(unix)]
fn disk_space(path: &Path) -> (Option<u64>, Option<u64>) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return (None, None) };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: gültiger, nullterminierter Pfad und ausreichend großer Puffer
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return (None, None);
    }
    let frag = s.f_frsize as u64;
    (Some(s.f_blocks as u64 * frag), Some(s.f_bavail as u64 * frag))
}

#[cfg(not(unix))]
fn disk_space(_: &Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// Belegung des Datenordners, nach Bereichen
fn storage_usage(data_dir: &Path) -> StorageUsage {
    let mut u = StorageUsage::default();
    let Ok(rd) = std::fs::read_dir(data_dir) else { return u };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let size = dir_size(&e.path());
        let slot = match name.as_str() {
            "files" => &mut u.files,
            "thumbs" | "previews" => &mut u.thumbnails,
            "index" => &mut u.index,
            n if n.starts_with("buero.db") => &mut u.database,
            _ => &mut u.other,
        };
        *slot += size;
    }
    u
}

/// Belegter Platz auf dem Datenträger (Symlinks werden nicht verfolgt)
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        allocated(&meta)
    }
}

#[cfg(unix)]
fn allocated(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}
