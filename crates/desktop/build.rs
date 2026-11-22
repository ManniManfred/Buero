// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

fn main() {
    // WinFsp nur bei Bedarf laden (Laufwerk); muss in der ausführbaren Datei stehen
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
    tauri_build::build()
}
