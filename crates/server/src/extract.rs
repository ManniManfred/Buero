// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Textextraktion für die Volltextsuche.
//!
//! * PDF: `pdftotext` (poppler); bei Scans ohne Textebene OCR mit `tesseract`
//! * Bilder: OCR mit `tesseract`
//! * Office (docx, pptx, odt, odp): XML aus dem ZIP-Container
//! * Tabellen (xlsx, xls, ods): `calamine`
//! * Text, Markdown, HTML, CSV, …: direkt

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use tokio::process::Command;

/// Maximale Textmenge pro Dokument (Zeichen), damit der Index schlank bleibt.
pub const MAX_TEXT: usize = 4 * 1024 * 1024;

pub struct Extracted {
    pub text: String,
    pub page_count: Option<i64>,
}

pub struct ExtractOptions {
    pub ocr: bool,
    pub ocr_lang: String,
    pub tmp_dir: PathBuf,
    /// Höchstens so viele PDF-Seiten per OCR erkennen
    pub ocr_max_pages: u32,
}

/// Zeitlimits für die externen Programme (eine präparierte Datei darf die
/// Verarbeitung nicht endlos blockieren)
const T_INFO: u64 = 30;
const T_TEXT: u64 = 180;
const T_RENDER: u64 = 900;
const T_THUMB: u64 = 60;
const T_OCR_PAGE: u64 = 180;

/// Grenzen für ZIP-Container (Office-Dateien), Schutz vor „Zip-Bomben“
const ZIP_MAX_ENTRIES: usize = 20_000;
const ZIP_MAX_ENTRY: u64 = 256 * 1024 * 1024;
const ZIP_MAX_TOTAL: u64 = 1024 * 1024 * 1024;

pub async fn extract(path: &Path, ext: &str, opts: &ExtractOptions) -> Result<Extracted> {
    let mut out = match ext {
        "pdf" => extract_pdf(path, opts).await?,
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => {
            if opts.ocr {
                Extracted { text: ocr_image(path, &opts.ocr_lang).await?, page_count: Some(1) }
            } else {
                Extracted { text: String::new(), page_count: Some(1) }
            }
        }
        "docx" | "docm" | "dotx" => blocking_zip(path, |z| zip_xml(z, &["word/document.xml"], "word/")).await?,
        "pptx" => blocking_zip(path, |z| zip_xml(z, &[], "ppt/slides/slide")).await?,
        "odt" | "odp" | "ott" => blocking_zip(path, |z| zip_xml(z, &["content.xml"], "")).await?,
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => {
            let p = path.to_path_buf();
            let zipped = ext != "xls";
            tokio::task::spawn_blocking(move || {
                if zipped {
                    let file = std::io::BufReader::new(std::fs::File::open(&p)?);
                    let mut z = zip::ZipArchive::new(file).context("Ungültige Tabellendatei")?;
                    check_zip(&mut z)?;
                }
                spreadsheet(&p)
            })
            .await??
        }
        "html" | "htm" => Extracted { text: strip_html(&read_text(path).await?), page_count: None },
        "rtf" => Extracted { text: strip_rtf(&read_text(path).await?), page_count: None },
        "eml" => {
            let raw = tokio::fs::read(path).await?;
            let text = crate::email::parse(&raw).map(|p| crate::email::text(&p)).unwrap_or_default();
            Extracted { text, page_count: None }
        }
        _ => {
            if is_probably_text(path).await {
                Extracted { text: read_text(path).await?, page_count: None }
            } else {
                Extracted { text: String::new(), page_count: None }
            }
        }
    };
    out.text = normalize(&out.text);
    Ok(out)
}

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT));
    let mut blank_lines = 0;
    for line in s.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        out.push_str(l);
        out.push('\n');
        if out.len() >= MAX_TEXT {
            break;
        }
    }
    if out.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out.trim().to_string()
}

async fn read_text(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let bytes = if bytes.len() > MAX_TEXT { &bytes[..MAX_TEXT] } else { &bytes[..] };
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // Fallback: Latin-1 (häufig bei alten Windows-Textdateien)
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    })
}

async fn is_probably_text(path: &Path) -> bool {
    let Ok(mut f) = tokio::fs::File::open(path).await else { return false };
    let mut buf = vec![0u8; 8192];
    use tokio::io::AsyncReadExt;
    let Ok(n) = f.read(&mut buf).await else { return false };
    let buf = &buf[..n];
    !buf.contains(&0) && (std::str::from_utf8(buf).is_ok() || n < 8192)
}

/// Startet ein Programm und bricht es nach `secs` Sekunden ab.
async fn run(cmd: &mut Command, secs: u64) -> Result<Vec<u8>> {
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let fut = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    // Bei Zeitüberschreitung wird der Prozess durch `kill_on_drop` beendet
    let out = tokio::time::timeout(std::time::Duration::from_secs(secs), fut)
        .await
        .map_err(|_| anyhow::anyhow!("{program} nach {secs} s abgebrochen"))?
        .with_context(|| format!("Programm konnte nicht gestartet werden: {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{:?} fehlgeschlagen: {}",
            cmd.as_std().get_program(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

async fn pdf_page_count(path: &Path) -> Option<i64> {
    let out = run(Command::new("pdfinfo").arg(path), T_INFO).await.ok()?;
    String::from_utf8_lossy(&out)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:").and_then(|v| v.trim().parse().ok()))
}

async fn extract_pdf(path: &Path, opts: &ExtractOptions) -> Result<Extracted> {
    let pages = pdf_page_count(path).await;
    let text = run(Command::new("pdftotext").args(["-layout", "-enc", "UTF-8"]).arg(path).arg("-"), T_TEXT)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();

    // Wenig Text pro Seite => vermutlich ein Scan ohne Textebene
    let chars = text.chars().filter(|c| c.is_alphanumeric()).count();
    let per_page = chars / pages.unwrap_or(1).max(1) as usize;
    if opts.ocr && per_page < 30 {
        match ocr_pdf(path, opts).await {
            Ok(ocr) if ocr.trim().len() > text.trim().len() => {
                return Ok(Extracted { text: ocr, page_count: pages });
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("OCR fehlgeschlagen für {}: {e:#}", path.display()),
        }
    }
    Ok(Extracted { text, page_count: pages })
}

async fn ocr_pdf(path: &Path, opts: &ExtractOptions) -> Result<String> {
    let dir = opts.tmp_dir.join(format!("ocr-{:016x}", rand::random::<u64>()));
    tokio::fs::create_dir_all(&dir).await?;
    let result = async {
        let last = opts.ocr_max_pages.max(1).to_string();
        run(
            Command::new("pdftoppm")
                .args(["-r", "300", "-gray", "-png", "-l", &last])
                .arg(path)
                .arg(dir.join("p")),
            T_RENDER,
        )
        .await?;
        let mut pages = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await?;
        while let Some(e) = rd.next_entry().await? {
            pages.push(e.path());
        }
        pages.sort();
        // Seiten parallel erkennen (tesseract nutzt pro Prozess wenige Kerne gut aus)
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(num_cpus::get().clamp(1, 4)));
        let mut handles = Vec::new();
        for p in pages {
            let sem = sem.clone();
            let lang = opts.ocr_lang.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await;
                ocr_image(&p, &lang).await
            }));
        }
        let mut text = String::new();
        for h in handles {
            text.push_str(&h.await??);
            text.push_str("\n\n");
        }
        Ok::<_, anyhow::Error>(text)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn ocr_image(path: &Path, lang: &str) -> Result<String> {
    let out = run(Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .args(["-l", lang])
        .env("OMP_THREAD_LIMIT", "1"), T_OCR_PAGE)
    .await?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

type Zip = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

async fn blocking_zip<F>(path: &Path, f: F) -> Result<Extracted>
where
    F: FnOnce(&mut Zip) -> Result<String> + Send + 'static,
{
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::io::BufReader::new(std::fs::File::open(&p)?);
        let mut z = zip::ZipArchive::new(file).context("Ungültige Office-Datei")?;
        check_zip(&mut z)?;
        Ok(Extracted { text: f(&mut z)?, page_count: None })
    })
    .await?
}

/// Lehnt ZIP-Container ab, die entpackt unverhältnismäßig groß würden.
fn check_zip<R: std::io::Read + std::io::Seek>(z: &mut zip::ZipArchive<R>) -> Result<()> {
    if z.len() > ZIP_MAX_ENTRIES {
        bail!("Datei enthält zu viele Einträge ({})", z.len());
    }
    let mut total = 0u64;
    for i in 0..z.len() {
        let e = z.by_index_raw(i)?;
        if e.size() > ZIP_MAX_ENTRY {
            bail!("Eintrag „{}“ ist entpackt zu groß", e.name());
        }
        total += e.size();
    }
    if total > ZIP_MAX_TOTAL {
        bail!("Datei ist entpackt zu groß");
    }
    Ok(())
}

/// Liest XML-Dateien aus einem ZIP-Container. `exact`: exakte Pfade,
/// `prefix`: zusätzlich alle `.xml`-Einträge mit diesem Präfix (bei leerem Präfix keine).
fn zip_xml(z: &mut Zip, exact: &[&str], prefix: &str) -> Result<String> {
    let mut names: Vec<String> = exact.iter().map(|s| s.to_string()).collect();
    if !prefix.is_empty() {
        let mut extra: Vec<String> = z
            .file_names()
            .filter(|n| n.starts_with(prefix) && n.ends_with(".xml") && !names.contains(&n.to_string()))
            .filter(|n| {
                // In Word nur Kopf-/Fußzeilen und Fußnoten zusätzlich
                !prefix.starts_with("word/")
                    || ["header", "footer", "footnotes", "endnotes"].iter().any(|k| n.contains(k))
            })
            .map(|s| s.to_string())
            .collect();
        // Folien in natürlicher Reihenfolge (slide2 vor slide10)
        extra.sort_by_key(|n| {
            let digits: String = n.chars().filter(|c| c.is_ascii_digit()).collect();
            (digits.parse::<u64>().unwrap_or(0), n.clone())
        });
        names.extend(extra);
    }
    let mut text = String::new();
    for name in names {
        let Ok(entry) = z.by_name(&name) else { continue };
        // Angegebene Größen können lügen: nie mehr als die Grenze lesen
        let mut xml = String::new();
        entry.take(ZIP_MAX_ENTRY).read_to_string(&mut xml)?;
        xml_text(&xml, &mut text);
        text.push('\n');
        if text.len() > MAX_TEXT {
            break;
        }
    }
    Ok(text)
}

fn xml_text(xml: &str, out: &mut String) {
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.unescape() {
                    out.push_str(&s);
                }
            }
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"tab" => out.push('\t'),
                b"br" | b"cr" | b"line-break" => out.push('\n'),
                b"s" => out.push(' '),
                _ => {}
            },
            Ok(Event::End(e)) => {
                if matches!(e.local_name().as_ref(), b"p" | b"h" | b"tr" | b"li") {
                    out.push('\n');
                } else if matches!(e.local_name().as_ref(), b"tc" | b"table-cell") {
                    out.push('\t');
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn spreadsheet(path: &Path) -> Result<Extracted> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut wb = open_workbook_auto(path).context("Tabelle konnte nicht gelesen werden")?;
    let mut text = String::new();
    let sheets = wb.sheet_names().to_owned();
    for name in &sheets {
        let Ok(range) = wb.worksheet_range(name) else { continue };
        text.push_str(name);
        text.push('\n');
        for row in range.rows() {
            let mut first = true;
            for cell in row {
                if !first {
                    text.push('\t');
                }
                first = false;
                match cell {
                    Data::Empty => {}
                    Data::String(s) => text.push_str(s),
                    other => text.push_str(&other.to_string()),
                }
            }
            text.push('\n');
            if text.len() > MAX_TEXT {
                break;
            }
        }
        text.push('\n');
    }
    Ok(Extracted { text, page_count: Some(sheets.len() as i64) })
}

pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if let Some(end) = skip_until {
            if let Some(pos) = lower[i..].find(end) {
                i += pos + end.len();
            } else {
                break;
            }
            skip_until = None;
            continue;
        }
        let c = bytes[i];
        if c == b'<' {
            if lower[i..].starts_with("<script") {
                skip_until = Some("</script>");
                continue;
            }
            if lower[i..].starts_with("<style") {
                skip_until = Some("</style>");
                continue;
            }
            if lower[i..].starts_with("<br") || lower[i..].starts_with("<p") || lower[i..].starts_with("</p")
                || lower[i..].starts_with("<div") || lower[i..].starts_with("<li") || lower[i..].starts_with("<tr")
            {
                out.push('\n');
            }
            in_tag = true;
        } else if c == b'>' && in_tag {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            // UTF-8-sicher: ganze Zeichen kopieren
            let ch_len = utf8_len(c);
            if let Some(s) = html.get(i..i + ch_len) {
                out.push_str(s);
            }
            i += ch_len;
            continue;
        }
        i += 1;
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&auml;", "ä")
        .replace("&ouml;", "ö")
        .replace("&uuml;", "ü")
        .replace("&Auml;", "Ä")
        .replace("&Ouml;", "Ö")
        .replace("&Uuml;", "Ü")
        .replace("&szlig;", "ß")
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Sehr einfache RTF-Bereinigung: Steuerwörter und Gruppen entfernen.
pub fn strip_rtf(rtf: &str) -> String {
    let mut out = String::new();
    let mut chars = rtf.chars().peekable();
    let mut depth_skip: Vec<bool> = vec![false];
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                let parent = *depth_skip.last().unwrap_or(&false);
                depth_skip.push(parent);
            }
            '}' => {
                depth_skip.pop();
                if depth_skip.is_empty() {
                    depth_skip.push(false);
                }
            }
            '\\' => {
                let mut word = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.is_empty() {
                    match chars.next() {
                        Some('\'') => {
                            let h: String = chars.by_ref().take(2).collect();
                            if let Ok(b) = u8::from_str_radix(&h, 16) {
                                if !depth_skip.last().unwrap_or(&false) {
                                    out.push(b as char); // cp1252 ~ latin1
                                }
                            }
                        }
                        Some('*') => {
                            if let Some(l) = depth_skip.last_mut() {
                                *l = true;
                            }
                        }
                        Some(o) if !depth_skip.last().unwrap_or(&false) => out.push(o),
                        _ => {}
                    }
                    continue;
                }
                // numerischer Parameter
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_digit() || n == '-' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                match word.as_str() {
                    "par" | "line" | "row" => out.push('\n'),
                    "tab" | "cell" => out.push('\t'),
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header" | "footer" => {
                        if let Some(l) = depth_skip.last_mut() {
                            *l = true;
                        }
                    }
                    _ => {}
                }
            }
            '\r' | '\n' => {}
            _ => {
                if !depth_skip.last().unwrap_or(&false) {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Vorschaubild erzeugen (JPEG, max. 480 px Breite).
pub async fn thumbnail(path: &Path, ext: &str, dest: &Path, tmp_dir: &Path) -> Result<bool> {
    match ext {
        "pdf" => {
            let prefix = tmp_dir.join(format!("th-{:016x}", rand::random::<u64>()));
            run(Command::new("pdftoppm")
                .args(["-f", "1", "-l", "1", "-singlefile", "-scale-to", "480", "-jpeg", "-jpegopt", "quality=80"])
                .arg(path)
                .arg(&prefix), T_THUMB)
            .await?;
            let produced = prefix.with_extension("jpg");
            tokio::fs::rename(&produced, dest).await?;
            Ok(true)
        }
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => {
            let src = path.to_path_buf();
            let dst = dest.to_path_buf();
            tokio::task::spawn_blocking(move || -> Result<bool> {
                let img = image::ImageReader::open(&src)?.with_guessed_format()?.decode()?;
                let th = img.thumbnail(480, 640).to_rgb8();
                let mut f = std::io::BufWriter::new(std::fs::File::create(&dst)?);
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 80).encode_image(&th)?;
                Ok(true)
            })
            .await?
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html() {
        let t = strip_html("<html><style>x{}</style><p>Grüße &amp; mehr</p><script>alert(1)</script>ende");
        assert!(t.contains("Grüße & mehr"));
        assert!(!t.contains("alert"));
        assert!(t.contains("ende"));
    }

    #[test]
    fn rtf() {
        let t = strip_rtf(r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Hallo\par Welt \'e4}");
        assert!(t.contains("Hallo"));
        assert!(t.contains("Welt ä"));
        assert!(!t.contains("Arial"));
    }

    #[test]
    fn xml() {
        let mut s = String::new();
        xml_text("<w:document><w:p><w:r><w:t>Hallo</w:t></w:r><w:r><w:tab/><w:t>Welt</w:t></w:r></w:p></w:document>", &mut s);
        assert_eq!(s, "Hallo\tWelt\n");
    }
}
