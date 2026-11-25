// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Gemeinsame Datentypen für Server, Web- und Desktop-Client.

use serde::{Deserialize, Serialize};

/// Grobe Dokumentart, abgeleitet aus Dateiendung / MIME-Typ.
pub const KINDS: &[(&str, &str)] = &[
    ("pdf", "PDF"),
    ("image", "Bilder / Scans"),
    ("word", "Textdokumente"),
    ("spreadsheet", "Tabellen"),
    ("presentation", "Präsentationen"),
    ("text", "Text"),
    ("markdown", "Markdown"),
    ("email", "E-Mails"),
    ("audio", "Audio"),
    ("video", "Videos"),
    ("other", "Sonstige"),
];

pub fn kind_for(extension: &str, mime: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" | "heic" => "image",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "word",
        "xls" | "xlsx" | "xlsm" | "xlsb" | "ods" | "csv" | "numbers" => "spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "presentation",
        "md" | "markdown" | "mdown" | "mkd" => "markdown",
        "txt" | "text" | "log" | "json" | "xml" | "html" | "htm" | "yaml"
        | "yml" | "toml" | "ini" | "rs" | "py" | "js" | "ts" | "sh" => "text",
        "eml" | "msg" => "email",
        // `.mpeg` ist in der Praxis meist MP3-Audio (Messenger, Diktiergeräte)
        "mp3" | "mpeg" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "flac" | "wma" => "audio",
        "mp4" | "m4v" | "webm" | "mov" | "mkv" | "avi" | "mpg" | "ogv" | "3gp" => "video",
        _ => {
            if mime.starts_with("image/") {
                "image"
            } else if mime.starts_with("audio/") {
                "audio"
            } else if mime.starts_with("video/") {
                "video"
            } else if mime == "text/markdown" {
                "markdown"
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
    /// Änderungsdatum der hochgeladenen Datei (Unix-Sekunden), falls bekannt;
    /// der Sync-Ordner gibt Dateien dieses Datum
    #[serde(default)]
    pub file_mtime: Option<i64>,
    pub version: i64,
    /// pending | processing | done | failed
    pub extract_status: String,
    pub page_count: Option<i64>,
    pub has_thumbnail: bool,
    pub collection_ids: Vec<i64>,
    pub tag_ids: Vec<i64>,
    /// Übergeordnetes Dokument (bei Unterdokumenten, z. B. Mail-Anhängen)
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub parent_title: Option<String>,
    /// Anzahl direkter Unterdokumente
    #[serde(default)]
    pub child_count: i64,
    /// Absender (nur bei E-Mails)
    #[serde(default)]
    pub email_from: Option<String>,
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
    /// Sammlungen nur direkt, ohne Dokumente aus Untersammlungen
    #[serde(default)]
    pub direct: bool,
    /// Unterdokumente (z. B. Mail-Anhänge) nicht einzeln aufführen
    #[serde(default)]
    pub hide_children: bool,
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
    /// Für den KI-Zugang (MCP) gesperrt – gilt auch für alle Untersammlungen
    #[serde(default)]
    pub ai_locked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionInput {
    pub name: Option<String>,
    /// `Some(None)` wird als `0` übertragen: 0 = Wurzel
    pub parent_id: Option<i64>,
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_locked: Option<bool>,
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
    /// Sammlungsänderungen auch auf alle Unterdokumente anwenden
    #[serde(default)]
    pub with_children: bool,
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
    /// Sammlungsänderungen bzw. Löschen auch für alle Unterdokumente
    #[serde(default)]
    pub with_children: bool,
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

/// Kurzlebiger Schlüssel für Datei-Links (Vorschau, Vorschaubilder, Download),
/// damit das Sitzungs-Token nie in einer URL steht.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileKey {
    /// Leer, wenn keine Anmeldung nötig ist
    pub key: String,
    pub expires_at: i64,
}

/// Signierter, wenige Minuten gültiger Link (z. B. Backup-Download)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedLink {
    /// Pfad samt Abfrage, relativ zur Server-Adresse
    pub path: String,
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

/// Übersicht (Dashboard): Speicherplatz und Kennzahlen
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Dashboard {
    /// Größe und freier Platz des Datenträgers mit dem Datenordner (`None`: unbekannt)
    pub disk_total: Option<u64>,
    pub disk_free: Option<u64>,
    /// Belegung des Datenordners auf dem Datenträger
    pub storage: StorageUsage,
    pub documents: i64,
    /// Summe der aktuellen Fassungen
    pub documents_size: i64,
    /// Ältere Fassungen (ohne die aktuellen)
    pub old_versions: i64,
    pub old_versions_size: i64,
    pub collections: i64,
    pub tags: i64,
    pub shares: i64,
    pub pending: i64,
    pub failed: i64,
    pub unfiled: i64,
    /// Nach Dokumentart, größte zuerst
    pub kinds: Vec<KindStat>,
    /// Neu hinzugekommene Dokumente der letzten 12 Monate (ältester zuerst, `YYYY-MM`)
    pub months: Vec<MonthStat>,
    pub largest: Vec<DocumentBrief>,
    pub recent: Vec<DocumentBrief>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StorageUsage {
    /// Dateien aller Fassungen (gleiche Inhalte nur einmal)
    pub files: u64,
    pub thumbnails: u64,
    pub index: u64,
    pub database: u64,
    /// Sonstiges im Datenordner (z. B. vorbereitete Wiederherstellung)
    pub other: u64,
}

impl StorageUsage {
    pub fn total(&self) -> u64 {
        self.files + self.thumbnails + self.index + self.database + self.other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KindStat {
    pub kind: String,
    pub count: i64,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonthStat {
    pub month: String,
    pub count: i64,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentBrief {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub size: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Freigaben
// ---------------------------------------------------------------------------

/// Freigabelink (Ansicht für den Besitzer)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Share {
    pub id: i64,
    pub token: String,
    pub document_id: Option<i64>,
    pub collection_id: Option<i64>,
    /// Titel des Dokuments bzw. Name der Sammlung
    pub target_name: String,
    pub has_password: bool,
    pub allow_upload: bool,
    pub expires_at: Option<i64>,
    pub note: String,
    pub created_at: i64,
    pub access_count: i64,
    pub last_access_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareInput {
    pub document_id: Option<i64>,
    pub collection_id: Option<i64>,
    /// Leer / None = ohne Passwort
    pub password: Option<String>,
    /// Unix-Zeit; None = unbegrenzt
    pub expires_at: Option<i64>,
    /// Nur bei Sammlungen: Empfänger dürfen Dateien hochladen
    #[serde(default)]
    pub allow_upload: bool,
    #[serde(default)]
    pub note: String,
}

/// Was ein Empfänger über den Link sieht
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicShare {
    pub title: String,
    pub is_collection: bool,
    pub allow_upload: bool,
    pub expires_at: Option<i64>,
    pub documents: Vec<PublicDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicDocument {
    pub id: i64,
    pub title: String,
    pub original_filename: String,
    pub kind: String,
    pub mime: String,
    pub size: i64,
    pub document_date: Option<String>,
    pub updated_at: i64,
    pub has_thumbnail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareUnlock {
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareKey {
    pub key: String,
}

// ---------------------------------------------------------------------------
// Euro-Office
// ---------------------------------------------------------------------------

/// Vom Server aktivierte Zusatzfunktionen
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Features {
    /// Euro-Office (OnlyOffice-kompatibler Document Server) ist angebunden
    pub office: bool,
    /// Der Tupfer Designer liegt bei (Bearbeiten von `.tupfer`-Projekten unter `/tupfer/`)
    #[serde(default)]
    pub tupfer: bool,
}

/// Installationsdatei des Desktop-Clients, die der Server zum Herunterladen anbietet
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesktopDownload {
    pub name: String,
    pub size: u64,
    /// Download-Adresse (GitHub-Release); fehlt sie, liegt die Datei auf dem
    /// Server unter `/desktop/{name}`
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficeConfig {
    /// Adresse von `api.js` des Document Servers
    pub script_url: String,
    /// Editor-Konfiguration als JSON (inkl. signiertem `token`)
    pub config: String,
    pub can_edit: bool,
}

/// Dateiendungen, die Euro-Office öffnen kann, mit Dokumenttyp
pub fn office_document_type(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "doc" | "docx" | "docm" | "dot" | "dotx" | "odt" | "ott" | "rtf" | "txt" => "word",
        "xls" | "xlsx" | "xlsm" | "ods" | "ots" | "csv" => "cell",
        "ppt" | "pptx" | "ppsx" | "pps" | "odp" | "otp" => "slide",
        _ => return None,
    })
}

/// Dateiendungen, für die Euro-Office eine PDF-Vorschau erzeugt (Text und CSV
/// zeigt die Oberfläche direkt an)
pub fn office_previewable(extension: &str) -> bool {
    office_document_type(extension).is_some() && !matches!(extension.to_ascii_lowercase().as_str(), "txt" | "csv")
}

/// Dateiendungen, die Euro-Office bearbeiten und im selben Format speichern kann
pub fn office_editable(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "docx" | "docm" | "dotx" | "odt" | "ott" | "rtf" | "txt" | "xlsx" | "xlsm" | "ods" | "ots" | "csv"
            | "pptx" | "ppsx" | "odp" | "otp"
    )
}

pub fn extension_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

/// Ergebnis einer vorbereiteten Wiederherstellung (der Server startet danach neu).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RestoreResult {
    pub documents: i64,
    /// Dokumentdateien, die im Backup fehlen
    pub missing_files: i64,
    /// Zeitpunkt des Backups
    pub created_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// KI-Zugang (MCP)
// ---------------------------------------------------------------------------

/// Zugangsschlüssel für den MCP-Endpunkt (der Schlüssel selbst wird nur beim Anlegen gezeigt)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    /// Darf zusätzlich Dokumente hochladen und taggen
    #[serde(default)]
    pub can_upload: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiTokenInput {
    pub name: String,
    #[serde(default)]
    pub can_upload: bool,
}

/// Berechtigung eines bestehenden Zugangsschlüssels ändern
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApiTokenUpdate {
    pub can_upload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiTokenCreated {
    #[serde(flatten)]
    pub info: ApiToken,
    pub token: String,
}

// ---------------------------------------------------------------------------
// E-Mail
// ---------------------------------------------------------------------------

/// Verweis auf eine E-Mail (z. B. in einer Konversation)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmailRef {
    pub id: i64,
    pub subject: String,
    pub sender: String,
    /// Unix-Zeit
    pub sent_at: Option<i64>,
}

/// E-Mail-Angaben zu einem Dokument: bei einer Mail Kopfdaten, Anhänge und
/// Konversation; bei einem Anhang die Mails, an denen er hängt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmailInfo {
    pub email: Option<EmailHeader>,
    #[serde(default)]
    pub attachments: Vec<DocumentSummary>,
    /// Alle Mails der Konversation in zeitlicher Reihenfolge (inkl. dieser)
    #[serde(default)]
    pub thread: Vec<EmailRef>,
    /// Mails, an denen dieses Dokument als Anhang hängt
    #[serde(default)]
    pub attached_to: Vec<EmailRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmailHeader {
    pub subject: String,
    pub sender: String,
    pub recipients: String,
    pub cc: String,
    pub sent_at: Option<i64>,
}

/// IMAP-Postfach (das Passwort wird nie ausgeliefert)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailAccount {
    pub id: i64,
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub has_password: bool,
    pub enabled: bool,
    pub interval_min: i64,
    pub collection_id: Option<i64>,
    pub last_run: Option<i64>,
    pub last_error: Option<String>,
    /// Beim letzten Abruf übernommene Mails
    pub last_imported: i64,
    pub running: bool,
    /// Fortschritt eines laufenden Abrufs
    #[serde(default)]
    pub progress: Option<MailProgress>,
    pub folders: Vec<MailFolder>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MailProgress {
    /// Lesbarer Ordnername
    pub folder: String,
    /// Ordner n von m
    pub folder_index: i64,
    pub folder_count: i64,
    /// Was gerade passiert, z. B. „Mails prüfen“
    pub phase: String,
    pub done: i64,
    pub total: i64,
    /// Bisher übernommene Mails in diesem Abruf
    pub imported: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailFolder {
    pub name: String,
    /// "all" | "replies"
    pub mode: String,
    /// Feste Sammlung für diesen Ordner; sonst die des Postfachs
    pub collection_id: Option<i64>,
    /// Ohne feste Sammlung: eigene Unter-Sammlung (Name des Ordners) in der
    /// Sammlung des Postfachs anlegen statt direkt dort ablegen
    #[serde(default = "yes")]
    pub own_collection: bool,
}

fn yes() -> bool {
    true
}

/// Name der Unter-Sammlung für einen IMAP-Ordner:
/// `INBOX` → „Posteingang“, `[Gmail]/Gesendet` → „Gesendet“, `INBOX.Rechnungen` → „Rechnungen“
pub fn mail_folder_collection_name(name: &str) -> String {
    let label = imap_folder_label(name);
    if label.eq_ignore_ascii_case("INBOX") {
        return "Posteingang".into();
    }
    let leaf = match label.rsplit_once('/') {
        Some((_, l)) => l,
        None => match label.get(..6) {
            Some(p) if p.eq_ignore_ascii_case("INBOX.") => &label[6..],
            _ => &label,
        },
    };
    let leaf = leaf.trim();
    if leaf.is_empty() { label.trim().to_string() } else { leaf.to_string() }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailAccountInput {
    pub name: Option<String>,
    pub host: Option<String>,
    pub port: Option<i64>,
    pub username: Option<String>,
    /// Leer/fehlend = unverändert
    pub password: Option<String>,
    pub enabled: Option<bool>,
    pub interval_min: Option<i64>,
    /// 0 = keine
    pub collection_id: Option<i64>,
    /// Ersetzt die Ordnerliste
    pub folders: Option<Vec<MailFolder>>,
}

/// Mails (samt Anhängen) von einer Sammlung in eine andere verschieben
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailMove {
    pub from: i64,
    /// 0 = nur aus `from` entfernen
    pub to: i64,
}

/// Verknüpfungen eines Dokuments
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DocumentLinks {
    pub parent: Option<DocumentSummary>,
    pub children: Vec<DocumentSummary>,
    /// Verweise (gegenseitig)
    pub related: Vec<DocumentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInput {
    pub target: i64,
    /// "child": Ziel wird Unterdokument; "parent": Ziel wird übergeordnet; "related": Verweis
    pub kind: String,
}

/// Ordner auf dem Mailserver
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerFolder {
    pub name: String,
    /// "sent", "inbox", "drafts", "trash", "junk", "archive" oder ""
    pub special: String,
}

/// IMAP-Ordnername (modifiziertes UTF-7, RFC 3501) lesbar machen: `B&APw-ro` → `Büro`
pub fn imap_folder_label(name: &str) -> String {
    fn b64(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b',' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = String::new();
    let mut rest = name;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let enc = &after[..end];
        if enc.is_empty() {
            out.push('&');
        } else {
            let (mut bits, mut n, mut units) = (0u32, 0u32, Vec::new());
            for c in enc.bytes() {
                let Some(v) = b64(c) else { return name.to_string() };
                bits = (bits << 6) | v;
                n += 6;
                if n >= 16 {
                    n -= 16;
                    units.push(((bits >> n) & 0xffff) as u16);
                }
            }
            out.push_str(&String::from_utf16_lossy(&units));
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn imap_utf7() {
        assert_eq!(super::imap_folder_label("B&APw-ro"), "Büro");
        assert_eq!(super::imap_folder_label("INBOX"), "INBOX");
        assert_eq!(super::imap_folder_label("A&-B"), "A&B");
        assert_eq!(super::imap_folder_label("Gel&APY-schte Elemente"), "Gelöschte Elemente");
        assert_eq!(super::imap_folder_label("&ZeVnLIqe-"), "日本語");
        assert_eq!(super::mail_folder_collection_name("INBOX"), "Posteingang");
        assert_eq!(super::mail_folder_collection_name("[Gmail]/Gesendet"), "Gesendet");
        assert_eq!(super::mail_folder_collection_name("INBOX.Rechnungen"), "Rechnungen");
        assert_eq!(super::mail_folder_collection_name("Gel&APY-schte Elemente"), "Gelöschte Elemente");
    }
}
