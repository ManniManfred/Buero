// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Testprogramm: `cargo run -p buero-vfs --example mount -- <server> <token|-> <einhängepunkt>`

struct Logger;
impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        if r.target().starts_with("buero_vfs") {
            eprintln!("[{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    log::set_logger(&Logger).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [server, token, mp] = args.as_slice() else {
        eprintln!("Aufruf: mount <server> <token|-> <einhängepunkt>");
        std::process::exit(2);
    };
    let drive = buero_vfs::mount(buero_vfs::Options {
        server: server.clone(),
        token: (token != "-").then(|| token.clone()),
        mode: buero_vfs::Mode::Drive,
        mount_point: mp.into(),
        cache_dir: std::env::temp_dir().join("buero-vfs-test"),
    })
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    eprintln!("Eingebunden unter {} – Enter zum Aushängen", drive.mount_point().display());
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}
