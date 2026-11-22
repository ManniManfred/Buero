// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub password: Option<String>,
    pub ocr_enabled: bool,
    pub ocr_lang: String,
    pub max_upload_bytes: usize,
    /// Höchstgröße einer Datei beim Hochladen über einen Freigabelink
    pub share_max_upload_bytes: usize,
    /// Höchstens so viele Dateien pro Stunde und Freigabelink
    pub share_uploads_per_hour: u32,
    /// Höchstens so viele PDF-Seiten per OCR erkennen
    pub ocr_max_pages: u32,
    pub workers: usize,
    pub session_days: i64,
    /// Öffentliche Adresse von Büro (z. B. `https://buero.example.com`), wird für
    /// Freigabelinks und Euro-Office gebraucht. Ohne Angabe aus den Anfrage-Headern.
    pub public_url: Option<String>,
    /// Adresse des Euro-Office/OnlyOffice Document Servers
    pub office_url: Option<String>,
    /// JWT-Schlüssel des Document Servers (`JWT_SECRET`)
    pub office_jwt_secret: Option<String>,
    /// Web-Version des Tupfer Designers (Bearbeiten von `.tupfer`-Projekten), wird unter `/tupfer/` ausgeliefert
    pub tupfer_dir: PathBuf,
    /// Installationsdateien des Desktop-Clients, werden unter `/desktop/` zum Herunterladen angeboten
    pub desktop_dir: PathBuf,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        let port = env("PORT").unwrap_or_else(|| "8080".into());
        Config {
            bind: env("BUERO_BIND").unwrap_or_else(|| format!("0.0.0.0:{port}")),
            data_dir: env("BUERO_DATA_DIR").unwrap_or_else(|| "./data".into()).into(),
            web_dir: env("BUERO_WEB_DIR")
                .unwrap_or_else(|| "./crates/web/dist".into())
                .into(),
            password: env("BUERO_PASSWORD"),
            ocr_enabled: env("BUERO_OCR").map(|v| v != "0" && v != "false").unwrap_or(true),
            ocr_lang: env("BUERO_OCR_LANG").unwrap_or_else(|| "deu+eng".into()),
            max_upload_bytes: env("BUERO_MAX_UPLOAD_MB")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1024)
                * 1024
                * 1024,
            share_max_upload_bytes: env("BUERO_SHARE_MAX_UPLOAD_MB")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(100)
                * 1024
                * 1024,
            share_uploads_per_hour: env("BUERO_SHARE_UPLOADS_PER_HOUR").and_then(|v| v.parse().ok()).unwrap_or(50),
            ocr_max_pages: env("BUERO_OCR_MAX_PAGES").and_then(|v| v.parse().ok()).unwrap_or(100),
            workers: env("BUERO_WORKERS")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| num_cpus::get().clamp(1, 8)),
            session_days: env("BUERO_SESSION_DAYS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
            public_url: env("BUERO_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string()),
            office_url: env("BUERO_OFFICE_URL").map(|u| u.trim_end_matches('/').to_string()),
            office_jwt_secret: env("BUERO_OFFICE_JWT_SECRET"),
            tupfer_dir: env("BUERO_TUPFER_DIR").unwrap_or_else(|| "./tupfer".into()).into(),
            desktop_dir: env("BUERO_DESKTOP_DIR").unwrap_or_else(|| "./desktop-dist".into()).into(),
        }
    }
}
