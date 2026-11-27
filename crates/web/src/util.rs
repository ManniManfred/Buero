// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use wasm_bindgen::JsValue;

pub fn format_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", b / 1024.0 / 1024.0 / 1024.0)
    }
}

/// Unix-Zeit -> `01.03.2024, 14:05` (lokale Zeit)
pub fn format_ts(ts: i64) -> String {
    let d = js_sys::Date::new(&JsValue::from_f64(ts as f64 * 1000.0));
    let opts = js_sys::Object::new();
    for (k, v) in [("day", "2-digit"), ("month", "2-digit"), ("year", "numeric"), ("hour", "2-digit"), ("minute", "2-digit")] {
        let _ = js_sys::Reflect::set(&opts, &k.into(), &v.into());
    }
    d.to_locale_string("de-DE", &opts).into()
}

/// `2024-03-01` -> `01.03.2024`
pub fn format_date(d: &str) -> String {
    let p: Vec<&str> = d.split('-').collect();
    if p.len() == 3 { format!("{}.{}.{}", p[2], p[1], p[0]) } else { d.to_string() }
}

pub fn status_label(s: &str) -> &'static str {
    match s {
        "pending" => "wartet auf Verarbeitung",
        "processing" => "wird verarbeitet (Text/OCR)…",
        "failed" => "Verarbeitung fehlgeschlagen",
        _ => "",
    }
}

pub const COLORS: &[&str] = &[
    "", "#ef4444", "#f97316", "#eab308", "#22c55e", "#14b8a6", "#3b82f6", "#8b5cf6", "#ec4899", "#64748b",
];

/// Programmversion; Release-Builds setzen `BUERO_VERSION` aus dem Git-Tag (`v0.2.0`).
pub fn version() -> &'static str {
    match option_env!("BUERO_VERSION").map(|v| v.trim().trim_start_matches('v')) {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    }
}
