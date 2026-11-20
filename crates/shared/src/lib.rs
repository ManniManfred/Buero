//! Gemeinsame Datentypen für Server, Web- und Desktop-Client.

use serde::{Deserialize, Serialize};

/// Grobe Dokumentart, abgeleitet aus Dateiendung / MIME-Typ.
pub const KINDS: &[(&str, &str)] = &[
    ("pdf", "PDF"),
    ("image", "Bilder / Scans"),
    ("word", "Textdokumente"),
    ("spreadsheet", "Tabellen"),
    ("presentation", "Präsentationen"),
    ("text", "Text / Markdown"),
    ("email", "E-Mails"),
    ("other", "Sonstige"),
];

pub fn kind_for(extension: &str, mime: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" | "heic" => "image",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "word",
        "xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv" | "numbers" => "spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "presentation",
        "md" | "markdown" | "txt" | "text" | "log" | "json" | "xml" | "html" | "htm" | "yaml"
        | "yml" | "toml" | "ini" | "rs" | "py" | "js" | "ts" | "sh" => "text",
        "eml" | "msg" => "email",
        _ => {
            if mime.starts_with("image/") {
                "image"
            } else if mime.starts_with("text/") {
                "text"
            } else {
                "other"
            }
        }
    }
}

pub fn kind_label(kind: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .unwrap_or("Sonstige")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentSummary {
    pub id: i64,
    pub title: String,
    pub original_filename: String,
    pub mime: String,
    pub kind: String,
    pub size: i64,
    /// Unix-Zeit in Sekunden
    pub created_at: i64,
    pub updated_at: i64,
    /// Fachliches Datum des Dokuments (z. B. Rechnungsdatum), `YYYY-MM-DD`
    pub document_date: Option<String>,
    pub version: i64,
    /// pending | processing | done | failed
    pub extract_status: String,
    pub page_count: Option<i64>,
    pub has_thumbnail: bool,
    pub collection_ids: Vec<i64>,
    pub tag_ids: Vec<i64>,
    /// HTML-Snippet mit `<b>`-Hervorhebungen (nur bei Suche)
    pub snippet: Option<String>,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub version: i64,
    pub original_filename: String,
    pub size: i64,
    pub sha256: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentDetail {
    #[serde(flatten)]
    pub summary: DocumentSummary,
    pub description: String,
    pub extract_error: Option<String>,
    pub content_length: i64,
    pub versions: Vec<VersionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListResponse {
    pub total: u64,
    pub items: Vec<DocumentSummary>,
    pub took_ms: f64,
    /// true, wenn die exakte Suche nichts fand und unscharf gesucht wurde
    pub fuzzy: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    /// Kommagetrennte Sammlungs-IDs (UND-verknüpft, inkl. Untersammlungen)
    #[serde(default)]
    pub collections: String,
    /// Kommagetrennte Tag-IDs (UND-verknüpft)
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub kind: String,
    /// Nur Dokumente ohne Sammlung ("Eingang")
    #[serde(default)]
    pub unfiled: bool,
    /// relevance | newest | oldest | date_desc | date_asc
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub document_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionInput {
    pub name: Option<String>,
    /// `Some(None)` wird als `0` übertragen: 0 = Wurzel
    pub parent_id: Option<i64>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub color: Option<String>,
    pub document_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagInput {
    pub name: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    /// Leerer String löscht das Datum
    pub document_date: Option<String>,
    pub collection_ids: Option<Vec<i64>>,
    pub tag_ids: Option<Vec<i64>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkUpdate {
    pub ids: Vec<i64>,
    #[serde(default)]
    pub add_collections: Vec<i64>,
    #[serde(default)]
    pub remove_collections: Vec<i64>,
    #[serde(default)]
    pub add_tags: Vec<i64>,
    #[serde(default)]
    pub remove_tags: Vec<i64>,
    #[serde(default)]
    pub delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResult {
    pub document: DocumentSummary,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatus {
    pub auth_required: bool,
    pub authenticated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    pub documents: i64,
    pub total_size: i64,
    pub pending: i64,
    pub failed: i64,
    pub unfiled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
