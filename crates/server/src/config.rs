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
    pub workers: usize,
    pub session_days: i64,
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
            workers: env("BUERO_WORKERS")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| num_cpus::get().clamp(1, 8)),
            session_days: env("BUERO_SESSION_DAYS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
        }
    }
}
