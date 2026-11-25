// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Titel und Dokumentdatum für Scans aus dem erkannten Text ableiten.
//!
//! Feste Regeln für deutsche Geschäftsbriefe: Betreffzeile, Absender (aus der
//! Rücksendezeile über der Anschrift oder einer Zeile mit Rechtsform),
//! Dokumentart (Rechnung, Bescheid, …) und Briefdatum. Angewandt nur, solange
//! der Titel noch ein automatisch vergebener Scan-Name ist.

/// Ergebnis der Auswertung; beide Teile sind optional.
#[derive(Debug, Default, PartialEq)]
pub struct Guess {
    pub title: Option<String>,
    /// `JJJJ-MM-TT`
    pub date: Option<String>,
}

/// Dateiendungen, bei denen es sich um einen Scan handeln kann.
pub fn scannable(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "pdf" | "jpg" | "jpeg" | "png" | "tif" | "tiff")
}

/// Ist der Titel ein automatisch vergebener Name? Etwa „Scan 2026-11-25 06-35“
/// (Büro), „Scan2026-11-24 230135“ (Brother), „SCN_0001“, „DOC001“, „IMG 1234“.
pub fn is_scan_name(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    // Längere Wörter zuerst, damit „document“ nicht als „doc“ + Rest gilt
    const PREFIXES: [&str; 8] = ["dokument", "document", "image", "scan", "scn", "doc", "dok", "img"];
    for p in PREFIXES {
        if let Some(rest) = t.strip_prefix(p) {
            return rest.chars().all(|c| c.is_ascii_digit() || " -_.:()".contains(c));
        }
    }
    false
}

/// Titel und Datum aus dem Text ermitteln. `today` als `JJJJ-MM-TT` begrenzt
/// das Jahr plausibler Daten.
pub fn guess(text: &str, today: &str) -> Guess {
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect();
    let max_year: i32 = today.get(..4).and_then(|y| y.parse().ok()).unwrap_or(2100) + 1;
    let date = letter_date(&lines, max_year);
    let subject = subject(&lines);
    let sender = sender(&lines);
    let kind = if subject.is_none() { kind(&lines) } else { None };
    let on = date.as_deref().map(|d| format!(" vom {}.{}.{}", &d[8..10], &d[5..7], &d[..4])).unwrap_or_default();

    let title = match (sender, subject, kind) {
        (Some(s), Some(b), _) if b.to_lowercase().contains(&s.to_lowercase()) => Some(b),
        (Some(s), Some(b), _) => Some(format!("{s} – {b}")),
        (None, Some(b), _) => Some(b),
        (Some(s), None, Some(k)) => Some(format!("{s} – {k}{on}")),
        (None, None, Some(k)) => Some(format!("{k}{on}")),
        (Some(s), None, None) if !on.is_empty() => Some(format!("{s} – Schreiben{on}")),
        (Some(s), None, None) => Some(s),
        (None, None, None) => None,
    };
    Guess { title: title.map(|t| shorten(&t, 120)), date }
}

/// Auf höchstens `max` Zeichen kürzen, möglichst an einer Wortgrenze.
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max / 2 => &cut[..i],
        _ => &cut,
    };
    format!("{}…", cut.trim_end_matches([' ', ',', ';', '-', '–']))
}

/// Text von Satzzeichen am Rand befreien.
fn tidy(s: &str) -> String {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '–' | '|' | '·' | '•'))
        .to_string()
}

// ---------------------------------------------------------------- Betreff

fn subject(lines: &[String]) -> Option<String> {
    const LABELS: [&str; 4] = ["betreff", "betrifft", "betr.", "betr:"];
    for (i, l) in lines.iter().enumerate().take(80) {
        for label in LABELS {
            let n = label.len();
            if l.len() < n || !l.is_char_boundary(n) || !l[..n].eq_ignore_ascii_case(label) {
                continue;
            }
            let rest = l[n..].trim_start_matches([':', '.', ' ']).trim();
            // Betreff in der nächsten Zeile, wenn hinter dem Wort nichts steht
            let text = if rest.is_empty() { lines.get(i + 1).map(String::as_str).unwrap_or("") } else { rest };
            let text = tidy(text.trim_end_matches('.'));
            if text.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
                return Some(shorten(&text, 100));
            }
        }
    }
    None
}

// ---------------------------------------------------------------- Absender

/// Absender aus der Rücksendezeile („Firma GmbH · Straße 1 · 12345 Ort“) oder
/// aus einer Zeile mit Rechtsform bzw. einer bekannten Art von Absender.
fn sender(lines: &[String]) -> Option<String> {
    for l in lines.iter().take(30) {
        if let Some(s) = return_address(l) {
            return Some(s);
        }
    }
    // Oben auch Behörden und Banken; im übrigen Text (Fußzeile) nur Firmen mit
    // Rechtsform, sonst zählt „Ihre Haftpflichtversicherung“ als Absender
    lines
        .iter()
        .take(15)
        .find_map(|l| organisation(l, true))
        .or_else(|| lines.iter().find_map(|l| organisation(l, false)))
}

fn segments(line: &str) -> Vec<String> {
    let parts: Vec<&str> = if line.contains(['·', '•', '|']) {
        line.split(['·', '•', '|']).collect()
    } else if line.contains(" - ") || line.contains(" – ") {
        line.split(" - ").flat_map(|p| p.split(" – ")).collect()
    } else {
        line.split(',').collect()
    };
    parts.into_iter().map(tidy).filter(|p| !p.is_empty()).collect()
}

fn has_postcode(s: &str) -> bool {
    s.split(|c: char| !c.is_ascii_digit()).any(|d| d.len() == 5)
}

fn return_address(line: &str) -> Option<String> {
    if line.chars().count() > 140 {
        return None;
    }
    let segs = segments(line);
    if segs.len() < 2 || !segs[1..].iter().any(|s| has_postcode(s)) {
        return None;
    }
    let first = &segs[0];
    let ok = first.chars().any(char::is_alphabetic)
        && !has_postcode(first)
        && first.chars().count() <= 60
        && !first.contains(':')
        // „Postfach 100“ oder „Hauptstraße 1“ ist kein Name
        && !first.split_whitespace().last().is_some_and(|w| w.chars().any(|c| c.is_ascii_digit()));
    ok.then(|| first.clone())
}

const LEGAL_FORMS: [&str; 14] =
    ["gmbh", "mbh", "ag", "kg", "kgaa", "ohg", "e.v.", "ev", "eg", "e.g.", "se", "gbr", "ug", "e.k."];
const INSTITUTIONS: [&str; 16] = [
    "sparkasse", "volksbank", "raiffeisenbank", "bank", "finanzamt", "krankenkasse", "versicherung",
    "stadtwerke", "landratsamt", "bürgeramt", "stadtverwaltung", "gemeindeverwaltung", "amtsgericht",
    "jobcenter", "rentenversicherung", "berufsgenossenschaft",
];
const FOOTER: [&str; 9] = ["sitz", "register", "geschäftsführ", "vorstand", "hrb", "hra", "ust", "iban", "steuer"];

fn organisation(line: &str, institutions: bool) -> Option<String> {
    let low = line.to_lowercase();
    if FOOTER.iter().any(|f| low.contains(f)) && segments(line).len() < 2 {
        return None;
    }
    segments(line).into_iter().find(|seg| {
        let low = seg.to_lowercase();
        let words: Vec<&str> = low.split_whitespace().collect();
        if words.is_empty() || words.len() > 8 || seg.chars().count() > 60 || seg.contains(':') {
            return false;
        }
        if FOOTER.iter().any(|f| low.contains(f)) {
            return false;
        }
        // Rechtsform hinter dem Namen („Muster GmbH“), nicht davor („AG Köln“)
        let legal = words
            .iter()
            .skip(1)
            .any(|w| LEGAL_FORMS.contains(&w.trim_end_matches([',', ';', ')'])) || w.ends_with("gmbh"));
        let inst = institutions && words.iter().any(|w| INSTITUTIONS.iter().any(|i| w.trim_matches(|c: char| !c.is_alphabetic()).ends_with(i)));
        legal || inst
    })
}

// ---------------------------------------------------------------- Dokumentart

/// Wortenden, die eine Dokumentart bezeichnen; „Stromrechnung“ und
/// „Steuerbescheid“ zählen mit.
const KINDS: [&str; 30] = [
    "rechnung", "gutschrift", "mahnung", "zahlungserinnerung", "zahlungsaufforderung", "bescheid",
    "kontoauszug", "depotauszug", "vertrag", "kündigung", "angebot", "bestätigung", "lieferschein",
    "quittung", "abrechnung", "bescheinigung", "police", "versicherungsschein", "kostenvoranschlag",
    "befund", "arztbrief", "zeugnis", "einladung", "mitteilung", "antrag", "vollmacht", "urkunde",
    "protokoll", "nachweis", "beitragsübersicht",
];
/// Wörter, die auf eine Dokumentart enden, aber keine sind.
const NOT_KINDS: [&str; 3] = ["berechnung", "verrechnung", "anrechnung"];

fn kind(lines: &[String]) -> Option<String> {
    let mut best: Option<(u8, String)> = None;
    for l in lines.iter().take(50) {
        let words: Vec<&str> = l.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).collect();
        for (i, w) in words.iter().enumerate() {
            let low = w.to_lowercase();
            if low.chars().count() > 30 || NOT_KINDS.contains(&low.as_str()) || !KINDS.iter().any(|k| low.ends_with(k)) {
                continue;
            }
            // Am Zeilenanfang („Rechnung Nr. 4711“) wiegt es mehr als im Fließtext
            let score = if i == 0 { 2 } else { 1 };
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, capitalize(w)));
            }
        }
    }
    best.map(|(_, k)| k)
}

/// „RECHNUNG“ → „Rechnung“, „rechnung“ → „Rechnung“.
fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------- Datum

/// Briefdatum: bevorzugt „Ort, (den) 12.03.2026“, dann Zeilen mit „Datum“,
/// sonst das erste plausible Datum oben im Text.
fn letter_date(lines: &[String], max_year: i32) -> Option<String> {
    const SKIP: [&str; 7] = ["geburt", "geb.", "fällig", "gültig", "zahlbar", "bis zum", "frist"];
    let mut best: Option<(u8, String)> = None;
    for (n, l) in lines.iter().enumerate().take(60) {
        let low = l.to_lowercase();
        if SKIP.iter().any(|s| low.contains(s)) {
            continue;
        }
        for (pos, date) in dates_in(l, max_year) {
            let before = l[..pos].trim_end();
            let before = before.strip_suffix("den").unwrap_or(before).trim_end();
            let score = if before.ends_with(',') && before.split_whitespace().count() <= 3 {
                3
            } else if low.contains("datum") || low.contains("den ") {
                2
            } else if n < 40 {
                1
            } else {
                0
            };
            if score > 0 && best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, date));
            }
        }
    }
    best.map(|(_, d)| d)
}

const MONTHS: [(&str, u32); 27] = [
    ("januar", 1), ("jänner", 1), ("jan", 1), ("februar", 2), ("feb", 2), ("märz", 3), ("maerz", 3),
    ("mär", 3), ("mrz", 3), ("april", 4), ("apr", 4), ("mai", 5), ("juni", 6), ("jun", 6), ("juli", 7),
    ("jul", 7), ("august", 8), ("aug", 8), ("september", 9), ("sept", 9), ("sep", 9), ("oktober", 10),
    ("okt", 10), ("november", 11), ("nov", 11), ("dezember", 12), ("dez", 12),
];

/// Alle Daten einer Zeile mit ihrer Byte-Position: `12.03.2026`, `12.3.26`,
/// `12. März 2026`, `2026-03-12`.
fn dates_in(line: &str, max_year: i32) -> Vec<(usize, String)> {
    let c: Vec<(usize, char)> = line.char_indices().collect();
    let digits = |from: usize| -> (u32, usize) {
        let mut v = 0u32;
        let mut j = from;
        while j < c.len() && c[j].1.is_ascii_digit() && j - from < 4 {
            v = v * 10 + c[j].1.to_digit(10).unwrap_or(0);
            j += 1;
        }
        (v, j - from)
    };
    let at = |j: usize, ch: char| c.get(j).is_some_and(|x| x.1 == ch);
    let skip_space = |mut j: usize| {
        if c.get(j).is_some_and(|x| x.1 == ' ') {
            j += 1;
        }
        j
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if !c[i].1.is_ascii_digit() || (i > 0 && c[i - 1].1.is_alphanumeric()) {
            i += 1;
            continue;
        }
        let (n1, l1) = digits(i);
        let mut j = i + l1;
        let mut found: Option<(i32, u32, u32)> = None;
        if l1 == 4 && at(j, '-') {
            let (m, lm) = digits(j + 1);
            if lm == 2 && at(j + 3, '-') {
                let (d, ld) = digits(j + 4);
                if ld == 2 {
                    j += 6;
                    found = Some((n1 as i32, m, d));
                }
            }
        } else if l1 <= 2 && (at(j, '.') || at(j, ' ')) {
            let k = skip_space(if at(j, '.') { j + 1 } else { j });
            if c.get(k).is_some_and(|x| x.1.is_ascii_digit()) && at(j, '.') {
                let (m, lm) = digits(k);
                if lm <= 2 && at(k + lm, '.') {
                    let y0 = skip_space(k + lm + 1);
                    let (y, ly) = digits(y0);
                    if ly == 4 || ly == 2 {
                        let y = if ly == 2 {
                            let y = y as i32;
                            if 2000 + y <= max_year { 2000 + y } else { 1900 + y }
                        } else {
                            y as i32
                        };
                        j = y0 + ly;
                        found = Some((y, m, n1));
                    }
                }
            } else if c.get(k).is_some_and(|x| x.1.is_alphabetic()) {
                let mut e = k;
                while e < c.len() && c[e].1.is_alphabetic() {
                    e += 1;
                }
                let word: String = c[k..e].iter().map(|x| x.1).collect::<String>().to_lowercase();
                if let Some((_, m)) = MONTHS.iter().find(|(name, _)| *name == word) {
                    let y0 = skip_space(if at(e, '.') { e + 1 } else { e });
                    let (y, ly) = digits(y0);
                    if ly == 4 {
                        j = y0 + ly;
                        found = Some((y as i32, *m, n1));
                    }
                }
            }
        }
        let next_is_digit = c.get(j).is_some_and(|x| x.1.is_ascii_digit());
        match found {
            Some((y, m, d)) if !next_is_digit && (1950..=max_year).contains(&y) => {
                let s = format!("{y:04}-{m:02}-{d:02}");
                // Rundreise prüft auch Monatslängen (kein 31.02.)
                let real = crate::util::date_to_ts(&s).map(crate::util::fmt_date);
                if real.as_deref() == Some(s.as_str()) {
                    out.push((c[i].0, s));
                }
                i = j;
            }
            _ => i += l1.max(1),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-11-25";

    #[test]
    fn scan_names() {
        for t in ["Scan 2026-11-25 06-35", "Scan2026-11-24 230135", "SCN_0001", "scan0003", "DOC001", "IMG 1234", "Scan", "Dokument 1"] {
            assert!(is_scan_name(t), "{t}");
        }
        for t in ["Scan Steuer 2025", "Rechnung", "Dokumentation", "Imgur", "Doctor 1"] {
            assert!(!is_scan_name(t), "{t}");
        }
    }

    #[test]
    fn invoice_with_return_address() {
        let text = "\
Stadtwerke Musterstadt GmbH · Hauptstraße 1 · 12345 Musterstadt
Herrn
Max Mustermann
Musterweg 5
54321 Beispielhausen
Kundennummer: 4711
Musterstadt, 12.03.2026
Rechnung Nr. 2026-0815
Sehr geehrter Herr Mustermann,
anbei erhalten Sie Ihre Jahresabrechnung. Zahlbar bis 31.03.2026.
";
        let g = guess(text, TODAY);
        assert_eq!(g.date.as_deref(), Some("2026-03-12"));
        assert_eq!(g.title.as_deref(), Some("Stadtwerke Musterstadt GmbH – Rechnung vom 12.03.2026"));
    }

    #[test]
    fn subject_line() {
        let text = "\
Finanzamt Beispielstadt
Postfach 100 · 11111 Beispielstadt
Frau Erika Musterfrau
Datum: 3. Februar 2026
Betreff: Einkommensteuerbescheid 2024
Steuernummer 12/345/67890
";
        let g = guess(text, TODAY);
        assert_eq!(g.date.as_deref(), Some("2026-02-03"));
        assert_eq!(g.title.as_deref(), Some("Finanzamt Beispielstadt – Einkommensteuerbescheid 2024"));
    }

    #[test]
    fn subject_on_next_line_without_sender() {
        let text = "Liebe Mitglieder,\nBetreff\nEinladung zur Jahreshauptversammlung\nam 14.04.2026 um 19 Uhr";
        let g = guess(text, TODAY);
        assert_eq!(g.title.as_deref(), Some("Einladung zur Jahreshauptversammlung"));
        assert_eq!(g.date.as_deref(), Some("2026-04-14"));
    }

    #[test]
    fn kind_only() {
        let text = "KONTOAUSZUG\nNr. 11/2026\nBuchungstag Wert Betrag\n01.11.26 Gehalt 2.500,00";
        let g = guess(text, TODAY);
        assert_eq!(g.date.as_deref(), Some("2026-11-01"));
        assert_eq!(g.title.as_deref(), Some("Kontoauszug vom 01.11.2026"));
    }

    #[test]
    fn nothing_found() {
        assert_eq!(guess("", TODAY), Guess::default());
        assert_eq!(guess("lorem ipsum dolor sit amet", TODAY), Guess::default());
    }

    #[test]
    fn ignores_due_and_birth_dates() {
        let text = "Geburtsdatum: 01.01.1980\nFällig am 30.06.2026\nRechnung";
        let g = guess(text, TODAY);
        assert_eq!(g.date, None);
        assert_eq!(g.title.as_deref(), Some("Rechnung"));
    }

    #[test]
    fn date_formats() {
        let d = |s| dates_in(s, 2027).into_iter().map(|(_, d)| d).collect::<Vec<_>>();
        assert_eq!(d("Berlin, den 5.1.2026"), ["2026-01-05"]);
        assert_eq!(d("am 12. März 2026"), ["2026-03-12"]);
        assert_eq!(d("2026-03-12"), ["2026-03-12"]);
        assert_eq!(d("12.03.26"), ["2026-03-12"]);
        assert!(d("31.02.2026").is_empty());
        assert!(d("Tel. 0123.45.6789").is_empty());
        assert!(d("Version 1.2.3").is_empty());
        assert!(d("12.03.2099").is_empty());
    }

    #[test]
    fn no_false_kind() {
        let text = "Berechnung der Zinsen\nSehr geehrte Damen und Herren";
        assert_eq!(guess(text, TODAY).title, None);
    }
}
