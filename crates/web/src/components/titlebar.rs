// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Eigene Titelleiste des Desktop-Clients (Fenster ohne Systemrahmen)

use leptos::prelude::*;

use crate::api::window_action;
use crate::components::toolbar::SearchRow;
use crate::icons::Icon;
use crate::state::ctx;

/// `ready`: angemeldet, Hauptansicht sichtbar
#[component]
pub fn TitleBar(#[prop(into)] ready: Signal<bool>) -> impl IntoView {
    let c = ctx();
    view! {
        // "deep": freie Flächen ziehen das Fenster, Doppelklick maximiert;
        // Eingabefelder und Knöpfe bleiben bedienbar
        <header class="titlebar" data-tauri-drag-region="deep">
            <div class="tb-brand">
                <span class="brand-name"><Icon name="logo" class="brand-logo" />"Büro"
                    <span class="brand-version" title="Version">{crate::util::version()}</span></span>
                <Show when=move || ready.get()>
                    <button class="icon" title="Einstellungen" on:click=move |_| c.show_settings.set(true)><Icon name="settings" /></button>
                </Show>
            </div>
            <div class="tb-main">
                <Show when=move || ready.get() && !c.dashboard.get()>
                    <SearchRow />
                </Show>
            </div>
            <div class="tb-window">
                <button class="tb-btn" title="Minimieren" on:click=|_| window_action("minimize")><Icon name="minimize" /></button>
                <button class="tb-btn" title="Maximieren" on:click=|_| window_action("toggleMaximize")><Icon name="maximize" class="sm" /></button>
                <button class="tb-btn close" title="Schließen" on:click=|_| window_action("close")><Icon name="x" /></button>
            </div>
        </header>
    }
}
