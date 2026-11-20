//! Büro – Desktop-Client.
//!
//! Der Desktop-Client verwendet exakt dieselbe Oberfläche wie die Webseite
//! (Rust/Leptos, nach WebAssembly kompiliert) und spricht dieselbe HTTP-API an.
//! Unterschiede: Server-Adresse ist frei wählbar, Dateien werden mit dem
//! Standardprogramm des Systems geöffnet.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("Fehler beim Starten von Büro");
}
