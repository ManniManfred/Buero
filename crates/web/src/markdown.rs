// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Markdown → HTML für Vorschau und Detailansicht.
//!
//! Eingebettetes HTML wird als Text gezeigt und gefährliche Link-Ziele
//! (`javascript:` usw.) entfernt: Das Ergebnis landet per `inner_html` in der
//! Oberfläche und darf keinen Code ausführen.

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let lower: String = url.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect::<String>().to_lowercase();
    if ["javascript:", "vbscript:", "data:", "file:"].iter().any(|p| lower.starts_with(p)) {
        CowStr::Borrowed("#")
    } else {
        url
    }
}

pub fn to_html(text: &str) -> String {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES;
    let events = Parser::new_ext(text, opts).map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Link { link_type, dest_url: safe_url(dest_url), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Image { link_type, dest_url: safe_url(dest_url), title, id })
        }
        e => e,
    });
    let mut out = String::with_capacity(text.len() * 3 / 2);
    html::push_html(&mut out, events);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_script() {
        let h = to_html("# T\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>");
        assert!(!h.contains("<script"), "{h}");
        assert!(!h.contains("<img"), "{h}");
        assert!(!h.contains("javascript:"), "{h}");
        assert!(h.contains("<h1>T</h1>"));
        assert!(to_html("| a | b |\n|---|---|\n| 1 | 2 |").contains("<table>"));
    }
}
