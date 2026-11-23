// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Leere Dateien für „Neu“: Word, Excel, PowerPoint, Text, Markdown.
//!
//! Word und Excel werden als kleinste gültige Office-Open-XML-Pakete erzeugt
//! (Calibri 11, A4, Deutsch). PowerPoint braucht Master, Layouts und Design –
//! dafür liegt eine leere Präsentation (16:9) als Vorlage bei.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const PPTX: &[u8] = include_bytes!("../templates/leer.pptx");

/// Art eines neuen Dokuments: (Kennung, Endung, Bezeichnung)
pub const NEW_TYPES: &[(&str, &str, &str)] = &[
    ("word", "docx", "Word-Dokument"),
    ("spreadsheet", "xlsx", "Excel-Tabelle"),
    ("presentation", "pptx", "PowerPoint-Präsentation"),
    ("text", "txt", "Textdatei"),
    ("markdown", "md", "Markdown"),
];

/// Endung und Inhalt der leeren Datei
pub fn blank(kind: &str, title: &str) -> Option<(&'static str, Vec<u8>)> {
    Some(match kind {
        "word" => ("docx", docx()),
        "spreadsheet" => ("xlsx", xlsx()),
        "presentation" => ("pptx", PPTX.to_vec()),
        "text" => ("txt", Vec::new()),
        "markdown" => ("md", format!("# {}\n\n", title.trim()).into_bytes()),
        _ => return None,
    })
}

fn package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in files {
        // Schreiben in einen Speicherpuffer schlägt nicht fehl
        let _ = zip.start_file(*name, opts);
        let _ = zip.write_all(content.as_bytes());
    }
    zip.finish().map(|c| c.into_inner()).unwrap_or_default()
}

const XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const REL_DOC: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const REL_STYLES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";

fn docx() -> Vec<u8> {
    let types = format!(
        r#"{XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#
    );
    let rels = format!(
        r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_DOC}" Target="word/document.xml"/></Relationships>"#
    );
    let doc_rels = format!(
        r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_STYLES}" Target="styles.xml"/></Relationships>"#
    );
    let styles = format!(
        r#"{XML}<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="de-DE" w:eastAsia="en-US" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Standard"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="berschrift1"><w:name w:val="heading 1"/><w:basedOn w:val="Standard"/><w:next w:val="Standard"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="berschrift2"><w:name w:val="heading 2"/><w:basedOn w:val="Standard"/><w:next w:val="Standard"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="200" w:after="80"/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/><w:sz w:val="26"/><w:szCs w:val="26"/></w:rPr></w:style></w:styles>"#
    );
    let document = format!(
        r#"{XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/><w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1417" w:right="1417" w:bottom="1134" w:left="1417" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    package(&[
        ("[Content_Types].xml", &types),
        ("_rels/.rels", &rels),
        ("word/_rels/document.xml.rels", &doc_rels),
        ("word/styles.xml", &styles),
        ("word/document.xml", &document),
    ])
}

fn xlsx() -> Vec<u8> {
    let types = format!(
        r#"{XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#
    );
    let rels = format!(
        r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL_DOC}" Target="xl/workbook.xml"/></Relationships>"#
    );
    let wb_rels = format!(
        r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="{REL_STYLES}" Target="styles.xml"/></Relationships>"#
    );
    let workbook = format!(
        r#"{XML}<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Tabelle1" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let sheet = format!(
        r#"{XML}<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#
    );
    let styles = format!(
        r#"{XML}<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/><family val="2"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs><cellStyles count="1"><cellStyle name="Standard" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#
    );
    package(&[
        ("[Content_Types].xml", &types),
        ("_rels/.rels", &rels),
        ("xl/_rels/workbook.xml.rels", &wb_rels),
        ("xl/workbook.xml", &workbook),
        ("xl/worksheets/sheet1.xml", &sheet),
        ("xl/styles.xml", &styles),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_packages_are_zips() {
        for (kind, ext, _) in NEW_TYPES {
            let (e, bytes) = blank(kind, "Test").unwrap();
            assert_eq!(&e, ext);
            if ["docx", "xlsx", "pptx"].contains(ext) {
                let z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
                assert!(z.file_names().any(|n| n == "[Content_Types].xml"), "{kind}");
            }
        }
        assert_eq!(blank("markdown", " Notizen ").unwrap().1, b"# Notizen\n\n");
        assert!(blank("exe", "x").is_none());
    }
}
