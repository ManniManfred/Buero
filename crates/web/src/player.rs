// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Audio direkt abspielen: ein gemeinsamer Player für die ganze Oberfläche
//! (Knopf in der Dokumentliste) mit einer Leiste am unteren Rand.

use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlAudioElement;

use crate::icons::Icon;

#[derive(Clone, Copy)]
pub struct Player {
    /// Dokument, das gerade geladen ist
    pub doc: RwSignal<Option<i64>>,
    pub title: RwSignal<String>,
    pub playing: RwSignal<bool>,
    pub time: RwSignal<f64>,
    pub duration: RwSignal<f64>,
}

thread_local! {
    static AUDIO: RefCell<Option<HtmlAudioElement>> = const { RefCell::new(None) };
}

fn audio() -> Option<HtmlAudioElement> {
    AUDIO.with(|a| a.borrow().clone())
}

pub fn player() -> Player {
    expect_context::<Player>()
}

/// Einmal beim Start anlegen (stellt den Player als Kontext bereit)
pub fn provide() -> Player {
    let p = Player {
        doc: RwSignal::new(None),
        title: RwSignal::new(String::new()),
        playing: RwSignal::new(false),
        time: RwSignal::new(0.0),
        duration: RwSignal::new(0.0),
    };
    if let Ok(el) = HtmlAudioElement::new() {
        el.set_preload("metadata");
        let on = |name: &str, f: Box<dyn FnMut()>| {
            let cb = Closure::<dyn FnMut()>::wrap(f);
            let _ = el.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
            cb.forget();
        };
        let e = el.clone();
        on("timeupdate", Box::new(move || p.time.set(e.current_time())));
        let e = el.clone();
        on("loadedmetadata", Box::new(move || {
            let d = e.duration();
            p.duration.set(if d.is_finite() { d } else { 0.0 });
        }));
        on("play", Box::new(move || p.playing.set(true)));
        on("pause", Box::new(move || p.playing.set(false)));
        on("ended", Box::new(move || p.playing.set(false)));
        on("error", Box::new(move || p.playing.set(false)));
        AUDIO.with(|a| *a.borrow_mut() = Some(el));
    }
    provide_context(p);
    p
}

/// Abspielen bzw. beim selben Dokument Pause/Weiter
pub fn toggle(p: Player, id: i64, url: &str, title: &str) {
    let Some(el) = audio() else { return };
    if p.doc.get_untracked() == Some(id) {
        if el.paused() {
            let _ = el.play();
        } else {
            let _ = el.pause();
        }
        return;
    }
    el.set_src(url);
    p.doc.set(Some(id));
    p.title.set(title.to_string());
    p.time.set(0.0);
    p.duration.set(0.0);
    let _ = el.play();
}

/// Anhalten, z. B. wenn die Detailansicht selbst abspielt
pub fn pause() {
    if let Some(el) = audio() {
        let _ = el.pause();
    }
}

fn stop(p: Player) {
    if let Some(el) = audio() {
        let _ = el.pause();
        el.remove_attribute("src").ok();
        el.load();
    }
    p.doc.set(None);
    p.playing.set(false);
}

fn fmt_time(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return "0:00".into();
    }
    let s = s as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Leiste am unteren Rand, solange ein Stück geladen ist
#[component]
pub fn PlayerBar() -> impl IntoView {
    let p = player();
    move || {
        p.doc.get()?;
        Some(view! {
            <div class="player-bar card">
                <button class="icon play-btn" title=move || if p.playing.get() { "Pause" } else { "Abspielen" }
                    on:click=move |_| {
                        if let Some(el) = audio() {
                            if el.paused() { let _ = el.play(); } else { let _ = el.pause(); }
                        }
                    }>
                    {move || if p.playing.get() { view! { <Icon name="pause" /> } } else { view! { <Icon name="play" /> } }}
                </button>
                <div class="player-main">
                    <span class="player-title ellipsis">{move || p.title.get()}</span>
                    <div class="player-seek">
                        <span class="small muted">{move || fmt_time(p.time.get())}</span>
                        <input type="range" min="0" step="0.1"
                            prop:max=move || p.duration.get().max(0.1)
                            prop:value=move || p.time.get()
                            on:input=move |ev| {
                                if let (Some(el), Ok(v)) = (audio(), event_target_value(&ev).parse::<f64>()) {
                                    el.set_current_time(v);
                                    p.time.set(v);
                                }
                            } />
                        <span class="small muted">{move || fmt_time(p.duration.get())}</span>
                    </div>
                </div>
                <button class="icon" title="Schließen" on:click=move |_| stop(p)><Icon name="x" /></button>
            </div>
        })
    }
}
