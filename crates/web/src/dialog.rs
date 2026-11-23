// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Eigene Dialoge statt `window.confirm` / `window.prompt`: im Stil der
//! Oberfläche, mit Tastatur (Enter = OK, Esc = Abbrechen) und als `async`-
//! Funktionen, die auf die Antwort warten.
//!
//! ```ignore
//! if dialog::confirm("Löschen?", "Wirklich?").ok("Löschen").danger().ask().await.is_some() { … }
//! let name = dialog::prompt("Neue Sammlung", "Name").ask_text().await;
//! ```

use std::cell::{Cell, RefCell};

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::{JsCast, JsValue};

use crate::icons::Icon;

/// Eintrag eines Auswahlmenüs
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub key: String,
    pub label: String,
    pub icon: &'static str,
    pub danger: bool,
}

pub fn choice(key: &str, label: impl Into<String>, icon: &'static str) -> Choice {
    Choice { key: key.into(), label: label.into(), icon, danger: false }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    None,
    Text { value: String, placeholder: String },
    /// Auswahlliste: (Wert, Beschriftung)
    Select { options: Vec<(String, String)>, value: String },
    /// Farbfelder aus `util::COLORS` („“ = keine Farbe)
    Colors { value: String },
    /// Menü: ein Klick wählt und schließt
    Choices(Vec<Choice>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub text: String,
    pub field: Field,
    /// Zusätzliches Häkchen (Beschriftung, Vorgabe)
    pub check: Option<(String, bool)>,
    pub ok: String,
    pub danger: bool,
}

/// Antwort; `None` beim Abbrechen
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub value: String,
    pub checked: bool,
}

pub fn confirm(title: impl Into<String>, text: impl Into<String>) -> Dialog {
    Dialog { title: title.into(), text: text.into(), field: Field::None, check: None, ok: "OK".into(), danger: false }
}

pub fn prompt(title: impl Into<String>, label: impl Into<String>) -> Dialog {
    Dialog {
        title: title.into(),
        text: label.into(),
        field: Field::Text { value: String::new(), placeholder: String::new() },
        check: None,
        ok: "OK".into(),
        danger: false,
    }
}

pub fn menu(title: impl Into<String>, choices: Vec<Choice>) -> Dialog {
    Dialog { title: title.into(), text: String::new(), field: Field::Choices(choices), check: None, ok: String::new(), danger: false }
}

impl Dialog {
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = t.into();
        self
    }
    pub fn ok(mut self, label: impl Into<String>) -> Self {
        self.ok = label.into();
        self
    }
    /// OK-Knopf rot (Löschen, Widerrufen, …)
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn value(mut self, v: impl Into<String>) -> Self {
        let v = v.into();
        match &mut self.field {
            Field::Text { value, .. } | Field::Select { value, .. } | Field::Colors { value } => *value = v,
            _ => {}
        }
        self
    }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        if let Field::Text { placeholder, .. } = &mut self.field {
            *placeholder = p.into();
        }
        self
    }
    pub fn select(mut self, options: Vec<(String, String)>) -> Self {
        let value = options.first().map(|o| o.0.clone()).unwrap_or_default();
        self.field = Field::Select { options, value };
        self
    }
    pub fn colors(mut self) -> Self {
        self.field = Field::Colors { value: String::new() };
        self
    }
    pub fn check(mut self, label: impl Into<String>, default: bool) -> Self {
        self.check = Some((label.into(), default));
        self
    }

    /// Dialog zeigen und auf die Antwort warten
    pub async fn ask(self) -> Option<Answer> {
        // Ohne eingebundenen DialogHost gibt es keine Antwort
        let sig = HOST.with(Cell::get)?;
        // Ein offener Dialog wird abgebrochen
        finish(None);
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            PENDING.with(|p| *p.borrow_mut() = Some(resolve));
        });
        sig.set(Some(self));
        let v = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
        if v.is_null() || v.is_undefined() {
            return None;
        }
        let checked = js_sys::Reflect::get(&v, &"checked".into()).ok().and_then(|b| b.as_bool()).unwrap_or(false);
        let value = js_sys::Reflect::get(&v, &"value".into()).ok().and_then(|s| s.as_string()).unwrap_or_default();
        Some(Answer { value, checked })
    }

    /// Nur bestätigen: `true` bei OK
    pub async fn ask_ok(self) -> bool {
        self.ask().await.is_some()
    }

    /// Text abfragen: getrimmt, `None` bei Abbrechen oder leer
    pub async fn ask_text(self) -> Option<String> {
        self.ask().await.map(|a| a.value.trim().to_string()).filter(|s| !s.is_empty())
    }
}

thread_local! {
    static PENDING: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
    /// Signal des eingebundenen DialogHost (kein Kontext: `ask` läuft in `spawn_local`)
    static HOST: Cell<Option<RwSignal<Option<Dialog>>>> = const { Cell::new(None) };
}

fn finish(answer: Option<Answer>) {
    let Some(resolve) = PENDING.with(|p| p.borrow_mut().take()) else { return };
    let v = match answer {
        Some(a) => {
            let o = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&o, &"value".into(), &a.value.into());
            let _ = js_sys::Reflect::set(&o, &"checked".into(), &a.checked.into());
            o.into()
        }
        None => JsValue::NULL,
    };
    let _ = resolve.call1(&JsValue::NULL, &v);
}

/// Bestätigen ohne `async` im Aufrufer: führt `then` nach „OK“ aus
pub fn confirm_then(d: Dialog, then: impl FnOnce() + 'static) {
    spawn_local(async move {
        if d.ask_ok().await {
            then();
        }
    });
}

/// Ist gerade ein Dialog offen? (z. B. damit Esc nichts anderes schließt)
pub fn is_open() -> bool {
    HOST.with(Cell::get).is_some_and(|s| s.with_untracked(Option::is_some))
}

/// Einmal in der Oberfläche einbinden
#[component]
pub fn DialogHost() -> impl IntoView {
    let sig = RwSignal::new(None::<Dialog>);
    HOST.with(|h| h.set(Some(sig)));
    on_cleanup(move || HOST.with(|h| h.set(None)));
    move || {
        let d = sig.get()?;
        Some(view! { <DialogView d=d sig=sig /> })
    }
}

#[component]
fn DialogView(d: Dialog, sig: RwSignal<Option<Dialog>>) -> impl IntoView {
    let value = RwSignal::new(match &d.field {
        Field::Text { value, .. } | Field::Select { value, .. } | Field::Colors { value } => value.clone(),
        _ => String::new(),
    });
    let checked = RwSignal::new(d.check.as_ref().is_some_and(|c| c.1));
    let close = move |a: Option<Answer>| {
        sig.set(None);
        finish(a);
    };
    let submit = move || close(Some(Answer { value: value.get_untracked(), checked: checked.get_untracked() }));

    // Fokus: Eingabefeld (Text markiert) bzw. OK-Knopf
    let input = NodeRef::<leptos::html::Input>::new();
    let ok_btn = NodeRef::<leptos::html::Button>::new();
    let first_choice = NodeRef::<leptos::html::Button>::new();
    request_animation_frame(move || {
        if let Some(i) = input.get_untracked() {
            let _ = i.focus();
            i.select();
        } else if let Some(b) = first_choice.get_untracked() {
            let _ = b.focus();
        } else if let Some(b) = ok_btn.get_untracked() {
            let _ = b.focus();
        }
    });

    let handle = window_event_listener(leptos::ev::keydown, move |ev| match ev.key().as_str() {
        "Escape" => {
            ev.prevent_default();
            ev.stop_propagation();
            close(None);
        }
        "Enter" => {
            let on_button = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .is_some_and(|e| e.tag_name() == "BUTTON");
            if !on_button {
                ev.prevent_default();
                submit();
            }
        }
        _ => {}
    });
    on_cleanup(move || handle.remove());

    let is_menu = matches!(d.field, Field::Choices(_));
    let field = match d.field.clone() {
        Field::None => ().into_any(),
        Field::Text { placeholder, .. } => view! {
            <input node_ref=input type="text" placeholder=placeholder
                prop:value=move || value.get() on:input=move |ev| value.set(event_target_value(&ev)) />
        }
        .into_any(),
        Field::Select { options, .. } => view! {
            <select prop:value=move || value.get() on:change=move |ev| value.set(event_target_value(&ev))>
                {options.into_iter().map(|(v, l)| {
                    let sel = v == value.get_untracked();
                    view! { <option value=v selected=sel>{l}</option> }
                }).collect_view()}
            </select>
        }
        .into_any(),
        Field::Colors { .. } => view! {
            <div class="dialog-colors">
                {crate::util::COLORS.iter().map(|col| {
                    let col = col.to_string();
                    let col2 = col.clone();
                    let col3 = col.clone();
                    let title = if col.is_empty() { "Keine Farbe".to_string() } else { col.clone() };
                    view! {
                        <button type="button" class="swatch" class:none=col.is_empty() title=title
                            class:active=move || value.get() == col2
                            style=format!("--sw: {}", if col.is_empty() { "transparent" } else { &col })
                            on:click=move |_| value.set(col3.clone())
                            on:dblclick=move |_| submit()>
                            {col.is_empty().then(|| view! { <Icon name="x" class="sm" /> })}
                        </button>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
        Field::Choices(choices) => view! {
            <div class="dialog-menu">
                {choices.into_iter().enumerate().map(|(i, ch)| {
                    let key = ch.key.clone();
                    let node = if i == 0 { first_choice } else { NodeRef::new() };
                    view! {
                        <button type="button" node_ref=node class:danger=ch.danger
                            on:click=move |_| close(Some(Answer { value: key.clone(), checked: false }))>
                            <Icon name=ch.icon />{ch.label}
                        </button>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    };

    view! {
        <div class="modal-bg dialog-bg" on:click=move |_| close(None)>
            <div class="card modal dialog" role="dialog" aria-modal="true" on:click=|ev| ev.stop_propagation()>
                <div class="dialog-head">
                    <h3>{d.title.clone()}</h3>
                    <button type="button" class="icon" title="Schließen" on:click=move |_| close(None)><Icon name="x" /></button>
                </div>
                {(!d.text.is_empty()).then(|| view! {
                    <div class="dialog-text">
                        {d.text.split('\n').map(|l| view! { <p>{l.to_string()}</p> }).collect_view()}
                    </div>
                })}
                {field}
                {d.check.clone().map(|(label, _)| view! {
                    <label class="dialog-check">
                        <input type="checkbox" prop:checked=move || checked.get()
                            on:change=move |ev| checked.set(event_target_checked(&ev)) />
                        {label}
                    </label>
                })}
                {(!is_menu).then(|| view! {
                    <div class="dialog-actions">
                        <button type="button" on:click=move |_| close(None)>"Abbrechen"</button>
                        <button type="button" node_ref=ok_btn class=if d.danger { "danger-solid" } else { "primary" }
                            on:click=move |_| submit()>{d.ok.clone()}</button>
                    </div>
                })}
            </div>
        </div>
    }
}
