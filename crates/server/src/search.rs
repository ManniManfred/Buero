// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Volltextindex auf Basis von Tantivy.
//!
//! Der Index enthält nur, was für Suche, Filter und Sortierung gebraucht wird;
//! die eigentlichen Daten (inkl. Text für Snippets) liegen in SQLite.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, QueryParser, TermQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED, STORED,
    STRING,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RemoveLongFilter, Stemmer,
    TextAnalyzer, Token, TokenStream, Tokenizer,
};
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term};

const SCHEMA_VERSION: &str = "8";
const TOKENIZER: &str = "de_stem";

#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    title: Field,
    filename: Field,
    description: Field,
    content: Field,
    tags_text: Field,
    collections_text: Field,
    collection: Field,
    direct_collection: Field,
    child: Field,
    parent_collection: Field,
    parent_direct_collection: Field,
    tag: Field,
    kind: Field,
    unfiled: Field,
    created: Field,
    date: Field,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    f: Fields,
}

/// Alles, was über ein Dokument in den Index wandert.
pub struct IndexDoc {
    pub id: i64,
    pub title: String,
    pub filename: String,
    pub description: String,
    pub content: String,
    pub tag_names: Vec<String>,
    pub collection_names: Vec<String>,
    /// Sammlungen inkl. aller Vorfahren
    pub collection_ids: Vec<i64>,
    /// Nur die Sammlungen, in denen das Dokument selbst liegt
    pub direct_collection_ids: Vec<i64>,
    /// Unterdokument (z. B. Mail-Anhang)
    pub is_child: bool,
    /// Unterdokument: Sammlungen des übergeordneten Dokuments (inkl. Vorfahren)
    pub parent_collection_ids: Vec<i64>,
    /// Unterdokument: Sammlungen, in denen das übergeordnete Dokument selbst liegt
    pub parent_direct_collection_ids: Vec<i64>,
    pub tag_ids: Vec<i64>,
    pub kind: String,
    pub created: i64,
    pub date: i64,
}

pub struct Query_ {
    pub text: String,
    pub collections: Vec<i64>,
    /// `collections` nur direkt, ohne Dokumente aus Untersammlungen
    pub direct: bool,
    /// Unterdokumente ausblenden (sie erscheinen unter ihrem Dokument). Mit
    /// Sammlungsfilter nur die, deren Dokument dort auch erscheint – liegt ein
    /// Unterdokument in einer anderen Sammlung, steht es dort für sich.
    pub hide_children: bool,
    pub tags: Vec<i64>,
    pub kind: String,
    pub unfiled: bool,
    /// Dokumente aus diesen Sammlungen (samt Untersammlungen) ausschließen
    pub exclude_collections: Vec<i64>,
    pub sort: String,
    pub offset: usize,
    pub limit: usize,
}

pub struct Hits {
    pub total: usize,
    pub ids: Vec<(i64, Option<f32>)>,
    pub fuzzy: bool,
    query: Option<Box<dyn Query>>,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = |stored: bool| {
        let o = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        if stored { o.set_stored() } else { o }
    };
    let f = Fields {
        id: b.add_u64_field("id", INDEXED | STORED | FAST),
        title: b.add_text_field("title", text(false)),
        filename: b.add_text_field("filename", text(false)),
        description: b.add_text_field("description", text(false)),
        content: b.add_text_field("content", text(false)),
        tags_text: b.add_text_field("tags", text(false)),
        collections_text: b.add_text_field("collections", text(false)),
        collection: b.add_u64_field("collection_id", INDEXED),
        direct_collection: b.add_u64_field("direct_collection_id", INDEXED),
        child: b.add_u64_field("child", INDEXED),
        parent_collection: b.add_u64_field("parent_collection_id", INDEXED),
        parent_direct_collection: b.add_u64_field("parent_direct_collection_id", INDEXED),
        tag: b.add_u64_field("tag_id", INDEXED),
        kind: b.add_text_field("kind", STRING),
        unfiled: b.add_u64_field("unfiled", INDEXED),
        created: b.add_i64_field("created", INDEXED | FAST),
        date: b.add_i64_field("date", INDEXED | FAST),
    };
    (b.build(), f)
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(WordTokenizer::default())
        .filter(RemoveLongFilter::limit(48))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .filter(Stemmer::new(Language::German))
        .build()
}

impl SearchIndex {
    /// Öffnet den Index. Rückgabe `true` im zweiten Feld: Index wurde neu angelegt
    /// und muss aus der Datenbank befüllt werden.
    pub fn open(dir: &Path) -> Result<(Self, bool)> {
        let (schema, f) = schema();
        let version_file = dir.join("schema_version");
        let current = std::fs::read_to_string(&version_file).unwrap_or_default();
        let mut fresh = false;
        if current.trim() != SCHEMA_VERSION {
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
            fresh = true;
        }
        std::fs::create_dir_all(dir)?;
        let index = if fresh {
            let idx = Index::create_in_dir(dir, schema)?;
            std::fs::write(&version_file, SCHEMA_VERSION)?;
            idx
        } else {
            Index::open_in_dir(dir)?
        };
        index.tokenizers().register(TOKENIZER, analyzer());
        let writer = index.writer(64 * 1024 * 1024)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok((SearchIndex { index, reader, writer: Mutex::new(writer), f }, fresh))
    }

    fn build_doc(&self, d: &IndexDoc) -> TantivyDocument {
        let f = &self.f;
        let mut doc = TantivyDocument::default();
        doc.add_u64(f.id, d.id as u64);
        doc.add_text(f.title, &d.title);
        doc.add_text(f.filename, &d.filename);
        doc.add_text(f.description, &d.description);
        doc.add_text(f.content, &d.content);
        for t in &d.tag_names {
            doc.add_text(f.tags_text, t);
        }
        for c in &d.collection_names {
            doc.add_text(f.collections_text, c);
        }
        for c in &d.collection_ids {
            doc.add_u64(f.collection, *c as u64);
        }
        for c in &d.direct_collection_ids {
            doc.add_u64(f.direct_collection, *c as u64);
        }
        for t in &d.tag_ids {
            doc.add_u64(f.tag, *t as u64);
        }
        doc.add_u64(f.child, d.is_child as u64);
        for c in &d.parent_collection_ids {
            doc.add_u64(f.parent_collection, *c as u64);
        }
        for c in &d.parent_direct_collection_ids {
            doc.add_u64(f.parent_direct_collection, *c as u64);
        }
        doc.add_text(f.kind, &d.kind);
        doc.add_u64(f.unfiled, d.collection_ids.is_empty() as u64);
        doc.add_i64(f.created, d.created);
        doc.add_i64(f.date, d.date);
        doc
    }

    pub fn upsert(&self, docs: &[IndexDoc]) -> Result<()> {
        let w = self.writer.lock().unwrap();
        for d in docs {
            w.delete_term(Term::from_field_u64(self.f.id, d.id as u64));
            w.add_document(self.build_doc(d))?;
        }
        Ok(())
    }

    pub fn delete(&self, id: i64) {
        let w = self.writer.lock().unwrap();
        w.delete_term(Term::from_field_u64(self.f.id, id as u64));
    }

    pub fn clear(&self) -> Result<()> {
        self.writer.lock().unwrap().delete_all_documents()?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.writer.lock().unwrap().commit()?;
        self.reader.reload()?;
        Ok(())
    }

    fn analyze(&self, text: &str) -> Vec<String> {
        let mut a = analyzer();
        let mut stream = a.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            // Zusammengeschriebene Wörter über ihre Teile suchen: findet
            // dann auch getrennt geschriebene
            if stream.token().position_length <= 1 {
                out.push(stream.token().text.clone());
            }
        }
        out
    }

    fn text_fields(&self) -> [(Field, f32); 6] {
        let f = &self.f;
        [
            (f.title, 4.0),
            (f.tags_text, 3.0),
            (f.filename, 2.0),
            (f.collections_text, 2.0),
            (f.description, 1.5),
            (f.content, 1.0),
        ]
    }

    /// Einfache Eingaben (nur Wörter) werden selbst in eine Abfrage übersetzt:
    /// alle Wörter müssen vorkommen (in beliebigen Feldern), das letzte Wort
    /// wird zusätzlich als Präfix gesucht (Suche während der Eingabe),
    /// optional unscharf (Tippfehler).
    fn simple_query(&self, text: &str, fuzzy: bool) -> Option<Box<dyn Query>> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let prefix_last = !text.ends_with(char::is_whitespace);
        let mut musts: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (wi, w) in words.iter().enumerate() {
            let is_last = wi + 1 == words.len();
            for tok in self.analyze(w) {
                let mut shoulds: Vec<(Occur, Box<dyn Query>)> = Vec::new();
                for (field, boost) in self.text_fields() {
                    let term = Term::from_field_text(field, &tok);
                    shoulds.push((
                        Occur::Should,
                        Box::new(BoostQuery::new(
                            Box::new(TermQuery::new(term.clone(), IndexRecordOption::WithFreqs)),
                            boost,
                        )),
                    ));
                    if is_last && prefix_last && tok.chars().count() >= 2 {
                        shoulds.push((
                            Occur::Should,
                            Box::new(BoostQuery::new(
                                Box::new(FuzzyTermQuery::new_prefix(term.clone(), 0, false)),
                                boost * 0.5,
                            )),
                        ));
                    }
                    if fuzzy {
                        let len = tok.chars().count();
                        let dist = if len >= 8 { 2 } else if len >= 4 { 1 } else { 0 };
                        if dist > 0 {
                            shoulds.push((
                                Occur::Should,
                                Box::new(BoostQuery::new(
                                    Box::new(FuzzyTermQuery::new(term, dist, true)),
                                    boost * 0.3,
                                )),
                            ));
                        }
                    }
                }
                musts.push((Occur::Must, Box::new(BooleanQuery::new(shoulds))));
            }
        }
        if musts.is_empty() {
            None
        } else {
            Some(Box::new(BooleanQuery::new(musts)))
        }
    }

    fn is_advanced(text: &str) -> bool {
        text.contains(['"', '(', ')', ':', '^', '*', '~', '[', ']'])
            || text.split_whitespace().any(|w| {
                w.starts_with('-') || w.starts_with('+') || w == "AND" || w == "OR" || w == "NOT"
            })
    }

    fn text_query(&self, text: &str, fuzzy: bool) -> Option<Box<dyn Query>> {
        if text.trim().is_empty() {
            return None;
        }
        if Self::is_advanced(text) {
            let fields: Vec<Field> = self.text_fields().iter().map(|(f, _)| *f).collect();
            let mut qp = QueryParser::for_index(&self.index, fields);
            qp.set_conjunction_by_default();
            for (f, b) in self.text_fields() {
                qp.set_field_boost(f, b);
                if fuzzy {
                    qp.set_field_fuzzy(f, false, 1, true);
                }
            }
            let (q, _errors) = qp.parse_query_lenient(text);
            Some(q)
        } else {
            self.simple_query(text, fuzzy)
        }
    }

    pub fn search(&self, q: &Query_) -> Result<Hits> {
        let first = self.search_inner(q, false)?;
        // Nichts gefunden? Dann tippfehlertolerant noch einmal versuchen.
        if first.total == 0 && !q.text.trim().is_empty() {
            let second = self.search_inner(q, true)?;
            if second.total > 0 {
                return Ok(second);
            }
        }
        Ok(first)
    }

    fn search_inner(&self, q: &Query_, fuzzy: bool) -> Result<Hits> {
        let f = &self.f;
        let text_q = self.text_query(&q.text, fuzzy);
        let has_text = text_q.is_some();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if let Some(tq) = &text_q {
            clauses.push((Occur::Must, tq.box_clone()));
        }
        let coll_field = if q.direct { f.direct_collection } else { f.collection };
        for c in &q.collections {
            clauses.push((Occur::Must, Box::new(TermQuery::new(
                Term::from_field_u64(coll_field, *c as u64),
                IndexRecordOption::Basic,
            ))));
        }
        for t in &q.tags {
            clauses.push((Occur::Must, Box::new(TermQuery::new(
                Term::from_field_u64(f.tag, *t as u64),
                IndexRecordOption::Basic,
            ))));
        }
        if q.hide_children {
            // Ausblenden, wenn das übergeordnete Dokument hier ebenfalls erscheint
            let parent_field = if q.direct { f.parent_direct_collection } else { f.parent_collection };
            let mut hidden: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, Box::new(TermQuery::new(
                Term::from_field_u64(f.child, 1),
                IndexRecordOption::Basic,
            )))];
            for c in &q.collections {
                hidden.push((Occur::Must, Box::new(TermQuery::new(
                    Term::from_field_u64(parent_field, *c as u64),
                    IndexRecordOption::Basic,
                ))));
            }
            clauses.push((Occur::MustNot, Box::new(BooleanQuery::new(hidden))));
        }
        for c in &q.exclude_collections {
            clauses.push((Occur::MustNot, Box::new(TermQuery::new(
                Term::from_field_u64(f.collection, *c as u64),
                IndexRecordOption::Basic,
            ))));
        }
        if !q.kind.is_empty() {
            clauses.push((Occur::Must, Box::new(TermQuery::new(
                Term::from_field_text(f.kind, &q.kind),
                IndexRecordOption::Basic,
            ))));
        }
        if q.unfiled {
            clauses.push((Occur::Must, Box::new(TermQuery::new(
                Term::from_field_u64(f.unfiled, 1),
                IndexRecordOption::Basic,
            ))));
        }
        let query: Box<dyn Query> = if clauses.is_empty() {
            Box::new(AllQuery)
        } else {
            if !has_text {
                clauses.push((Occur::Must, Box::new(AllQuery)));
            }
            Box::new(BooleanQuery::new(clauses))
        };

        let searcher = self.reader.searcher();
        let limit = q.limit.clamp(1, 500);
        let top = TopDocs::with_limit(limit).and_offset(q.offset);
        let sort = if q.sort.is_empty() {
            if has_text { "relevance" } else { "newest" }
        } else {
            q.sort.as_str()
        };
        let (total, addrs): (usize, Vec<(Option<f32>, DocAddress)>) = match sort {
            "relevance" if has_text => {
                let (n, docs) = searcher.search(&query, &(Count, top))?;
                (n, docs.into_iter().map(|(s, a)| (Some(s), a)).collect())
            }
            other => {
                let (field, order) = match other {
                    "oldest" => ("created", Order::Asc),
                    "date_desc" => ("date", Order::Desc),
                    "date_asc" => ("date", Order::Asc),
                    _ => ("created", Order::Desc),
                };
                let (n, docs) =
                    searcher.search(&query, &(Count, top.order_by_fast_field::<i64>(field, order)))?;
                (n, docs.into_iter().map(|(_, a)| (None, a)).collect())
            }
        };
        let mut ids = Vec::with_capacity(addrs.len());
        for (score, addr) in addrs {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(id) = doc.get_first(f.id).and_then(|v| v.as_u64()) {
                ids.push((id as i64, score));
            }
        }
        Ok(Hits { total, ids, fuzzy, query: text_q })
    }

    /// Erzeugt HTML-Snippets mit hervorgehobenen Treffern.
    pub fn snippets(&self, hits: &Hits, query_text: &str, texts: &[(i64, String)]) -> Vec<(i64, String)> {
        let Some(q) = &hits.query else { return Vec::new() };
        let searcher = self.reader.searcher();
        let mut gen = SnippetGenerator::create(&searcher, q.as_ref(), self.f.content).ok();
        if let Some(g) = gen.as_mut() {
            g.set_max_num_chars(220);
        }
        // Für Präfix-/unscharfe Treffer liefert Tantivy keine Hervorhebung:
        // dann selbst nach Wortanfängen suchen.
        let stems: Vec<String> = query_text
            .split_whitespace()
            .filter(|w| !w.starts_with('-'))
            .map(|w| {
                let f = fold(w.trim_matches(|c: char| !c.is_alphanumeric()));
                let n = if hits.fuzzy { 4 } else { 64 };
                f.chars().take(n).collect::<String>()
            })
            .filter(|w| w.chars().count() >= 2)
            .collect();
        texts
            .iter()
            .filter_map(|(id, text)| {
                let html = gen.as_ref().map(|g| g.snippet(text)).filter(|s| !s.highlighted().is_empty()).map(|s| s.to_html());
                html.or_else(|| fallback_snippet(text, &stems)).map(|h| (*id, h))
            })
            .collect()
    }
}

/// Zerlegt Text in Wörter (wie `SimpleTokenizer`) und zusätzlich
/// zusammengeschriebene Wörter in ihre Teile: `VersiegelteFlaechen` →
/// `versiegelte`, `flaechen` (plus das ganze Wort an derselben Position,
/// mit `position_length` = Anzahl der Teile), ebenso `Rechnung2024` und
/// `PDFDatei`. Umschreibungen ae/oe/ue werden wie ä/ö/ü behandelt.
#[derive(Clone, Default)]
pub struct WordTokenizer {
    tokens: Vec<Token>,
}

pub struct WordTokenStream<'a> {
    tokens: &'a mut Vec<Token>,
    next: usize,
}

impl Tokenizer for WordTokenizer {
    type TokenStream<'a> = WordTokenStream<'a>;
    fn token_stream<'a>(&'a mut self, text: &'a str) -> WordTokenStream<'a> {
        self.tokens.clear();
        let mut pos = 0;
        let push = |tokens: &mut Vec<Token>, from: usize, to: usize, position: usize, len: usize| {
            tokens.push(Token {
                offset_from: from,
                offset_to: to,
                position,
                text: digraphs(&text[from..to].to_lowercase()),
                position_length: len,
            });
        };
        for (from, word) in text.split_word_bound_indices() {
            if !word.starts_with(char::is_alphanumeric) {
                continue;
            }
            let parts = word_parts(word);
            if parts.len() > 1 {
                push(&mut self.tokens, from, from + word.len(), pos, parts.len());
                for (a, b) in &parts {
                    push(&mut self.tokens, from + a, from + b, pos, 1);
                    pos += 1;
                }
            } else {
                push(&mut self.tokens, from, from + word.len(), pos, 1);
                pos += 1;
            }
        }
        WordTokenStream { tokens: &mut self.tokens, next: 0 }
    }
}

impl TokenStream for WordTokenStream<'_> {
    fn advance(&mut self) -> bool {
        self.next += 1;
        self.next <= self.tokens.len()
    }
    fn token(&self) -> &Token {
        &self.tokens[self.next - 1]
    }
    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.next - 1]
    }
}

/// Teile eines Wortes als Byte-Bereiche: Grenzen bei klein→Groß,
/// GROSS→Groß+klein (`PDFDatei`) und zwischen Buchstaben und Ziffern
fn word_parts(word: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        let (p, c) = (chars[i - 1].1, chars[i].1);
        let next_lower = chars.get(i + 1).is_some_and(|n| n.1.is_lowercase());
        let cut = (p.is_lowercase() && c.is_uppercase())
            || (p.is_uppercase() && c.is_uppercase() && next_lower)
            || (p.is_alphabetic() && c.is_numeric())
            || (p.is_numeric() && c.is_alphabetic());
        if cut {
            parts.push((start, chars[i].0));
            start = chars[i].0;
        }
    }
    parts.push((start, word.len()));
    parts
}

/// ae/oe/ue → a/o/u (nicht „que“), passend zu ä/ö/ü nach der ASCII-Faltung
fn digraphs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c == 'e' && matches!(prev, Some('a' | 'o')) || c == 'e' && prev == Some('u') && !out.ends_with("qu") {
            prev = None;
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    out
}

fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' => out.push('a'),
            'ö' => out.push('o'),
            'ü' => out.push('u'),
            'ß' => out.push_str("ss"),
            'é' | 'è' | 'ê' => out.push('e'),
            'á' | 'à' | 'â' => out.push('a'),
            _ => out.push(c),
        }
    }
    digraphs(&out)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Sucht das erste Wort, das mit einem der Suchbegriffe beginnt, und baut
/// einen Ausschnitt drumherum (alle passenden Wörter werden hervorgehoben).
fn fallback_snippet(text: &str, stems: &[String]) -> Option<String> {
    if stems.is_empty() {
        return None;
    }
    let matches = |w: &str| {
        let f = fold(w);
        stems.iter().any(|s| f.starts_with(s.as_str()))
    };
    let mut first = None;
    for (i, w) in text.split_word_bound_indices() {
        if matches(w) {
            first = Some(i);
            break;
        }
    }
    let first = first?;
    let mut start = first.saturating_sub(80);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (first + 160).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let window = &text[start..end];
    let mut html = String::new();
    if start > 0 {
        html.push('…');
    }
    for (_, w) in window.split_word_bound_indices() {
        if matches(w) {
            html.push_str("<b>");
            html.push_str(&escape(w));
            html.push_str("</b>");
        } else {
            html.push_str(&escape(w));
        }
    }
    if end < text.len() {
        html.push('…');
    }
    Some(html)
}

/// Minimaler Wortzerleger (Buchstaben/Ziffern vs. Rest), ohne Zusatz-Crate.
trait WordBounds {
    fn split_word_bound_indices(&self) -> Vec<(usize, &str)>;
}

impl WordBounds for str {
    fn split_word_bound_indices(&self) -> Vec<(usize, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut prev: Option<bool> = None;
        for (i, c) in self.char_indices() {
            let alnum = c.is_alphanumeric();
            if let Some(p) = prev {
                if p != alnum {
                    out.push((start, &self[start..i]));
                    start = i;
                }
            }
            prev = Some(alnum);
        }
        if start < self.len() {
            out.push((start, &self[start..]));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback() {
        let s = fallback_snippet("Sehr geehrte Versicherung, hallo", &["vers".into()]).unwrap();
        assert_eq!(s, "Sehr geehrte <b>Versicherung</b>, hallo");
        assert!(fallback_snippet("abc", &["xyz".into()]).is_none());
    }

    #[test]
    fn parts() {
        let p = |w: &str| word_parts(w).into_iter().map(|(a, b)| w[a..b].to_string()).collect::<Vec<_>>();
        assert_eq!(p("VersiegelteFlaechen"), ["Versiegelte", "Flaechen"]);
        assert_eq!(p("PDFDatei"), ["PDF", "Datei"]);
        assert_eq!(p("Rechnung2024"), ["Rechnung", "2024"]);
        assert_eq!(p("Haus"), ["Haus"]);
        assert_eq!(digraphs("flaechen"), "flachen");
        assert_eq!(digraphs("quelle"), "quelle");
        assert_eq!(fold("Flächen"), fold("Flaechen"));
    }

    fn doc(id: i64, title: &str) -> IndexDoc {
        IndexDoc {
            id,
            title: title.into(),
            filename: String::new(),
            description: String::new(),
            content: String::new(),
            tag_names: vec![],
            collection_names: vec![],
            collection_ids: vec![],
            direct_collection_ids: vec![],
            is_child: false,
            parent_collection_ids: vec![],
            parent_direct_collection_ids: vec![],
            tag_ids: vec![],
            kind: "pdf".into(),
            created: 0,
            date: 0,
        }
    }

    #[test]
    fn children_in_other_collections() {
        let dir = std::env::temp_dir().join(format!("buero-search-{}", rand::random::<u64>()));
        let (idx, _) = SearchIndex::open(&dir).unwrap();
        // Mail (1) in Sammlung 10; Anhang (2) in 10 und zusätzlich in 20;
        // Anhang (3) nur in 10; Sammlung 21 liegt unter 20
        let mail = IndexDoc { collection_ids: vec![10], direct_collection_ids: vec![10], ..doc(1, "Mail") };
        let child = |id, colls: Vec<i64>| IndexDoc {
            collection_ids: colls.clone(),
            direct_collection_ids: colls,
            is_child: true,
            parent_collection_ids: vec![10],
            parent_direct_collection_ids: vec![10],
            ..doc(id, "Anhang")
        };
        let other = IndexDoc { collection_ids: vec![21, 20], direct_collection_ids: vec![21], ..doc(4, "Mail2") };
        let grandchild = IndexDoc {
            collection_ids: vec![21, 20],
            direct_collection_ids: vec![21],
            is_child: true,
            parent_collection_ids: vec![21, 20],
            parent_direct_collection_ids: vec![21],
            ..doc(5, "Anhang2")
        };
        idx.upsert(&[mail, child(2, vec![10, 20]), child(3, vec![10]), other, grandchild]).unwrap();
        idx.commit().unwrap();
        let find = |colls: Vec<i64>, direct: bool| {
            let mut ids: Vec<i64> = idx
                .search(&Query_ {
                    text: String::new(),
                    collections: colls,
                    direct,
                    hide_children: true,
                    tags: vec![],
                    kind: String::new(),
                    unfiled: false,
                    exclude_collections: vec![],
                    sort: String::new(),
                    offset: 0,
                    limit: 10,
                })
                .unwrap()
                .ids
                .into_iter()
                .map(|x| x.0)
                .collect();
            ids.sort();
            ids
        };
        // Alle Dokumente: Unterdokumente nur unter ihrem Dokument
        assert_eq!(find(vec![], false), [1, 4]);
        // Sammlung der Mail: Anhänge stehen unter der Mail
        assert_eq!(find(vec![10], false), [1]);
        // Andere Sammlung: der Anhang steht für sich
        assert_eq!(find(vec![20], true), [2]);
        // Mit Untersammlungen: Mail2 erscheint, ihr Anhang darunter
        assert_eq!(find(vec![20], false), [2, 4]);
        // Nur direkt in 20: Mail2 liegt in 21 – nicht sichtbar, Anhang2 auch nicht (liegt in 21)
        assert_eq!(find(vec![21], true), [4]);
    }

    #[test]
    fn compounds_and_umlauts() {
        let dir = std::env::temp_dir().join(format!("buero-search-{}", rand::random::<u64>()));
        let (idx, _) = SearchIndex::open(&dir).unwrap();
        idx.upsert(&[doc(1, "VersiegelteFlaechen"), doc(2, "Versiegelte Flächen 2024"), doc(3, "Gartenhaus")]).unwrap();
        idx.commit().unwrap();
        let find = |t: &str| {
            let mut ids: Vec<i64> = idx
                .search(&Query_ {
                    text: t.into(),
                    collections: vec![],
                    direct: false,
                    hide_children: false,
                    tags: vec![],
                    kind: String::new(),
                    unfiled: false,
                    exclude_collections: vec![],
                    sort: String::new(),
                    offset: 0,
                    limit: 10,
                })
                .unwrap()
                .ids
                .into_iter()
                .map(|x| x.0)
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(find("versiegelte flaechen"), [1, 2]);
        assert_eq!(find("versiegelte flächen"), [1, 2]);
        assert_eq!(find("VersiegelteFlächen"), [1, 2]);
        assert_eq!(find("versiegelteflaechen"), [1]);
        assert_eq!(find("\"versiegelte flaechen\""), [1, 2]);
        assert_eq!(find("flaech"), [1, 2]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
