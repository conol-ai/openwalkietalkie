//! **Context aware transcription** on the Voice page, and its **Model
//! access** sub-page.
//!
//! The section holds the switch and two pickers — the window recognition
//! model and the rewriting model — and never a URL, a model id or a key:
//! those belong to a provider, set up once on Model access and shared by
//! every feature (`deckd`'s provider registry). A model id is typed only
//! for a custom provider that lists none.
//!
//! **Experimental**, and shown as such: a badge beside the title, a note
//! the first time the switch is turned on (kept as read in the app's
//! preferences, so it does not come back), and the same note behind "What
//! does experimental mean?". Models on this computer or the local network
//! are marked "Local · slower" wherever they are offered, so a slow local
//! model is not taken for a broken feature.
//!
//! **Keys** go in masked fields, to the service (`providers.key.set`), which
//! keeps them in this computer's key store; the field is emptied once the
//! key is saved, and from then on the page shows what `providers.list`
//! publishes of it: its length and at most its last four characters
//! ([`shown_key`]). A key is never drawn in clear, never in a tooltip,
//! never kept by the window.
use crate::shell::{Screen, Shell};
use crate::style::{palette, Palette};
use crate::ui::{self, Btn, Row, Segmented, Switch, Type as _};
use deck_gui_core::care::BotKey;
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, Entity, SharedString, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use std::collections::BTreeMap;

/// The `config.set` names the section writes.
pub mod keys {
    pub const ENABLED: &str = "context.enabled";
    pub const SCENE_PROVIDER: &str = "context.scene.provider";
    pub const SCENE_MODEL: &str = "context.scene.model";
    pub const EDIT_PROVIDER: &str = "context.edit.provider";
    pub const EDIT_MODEL: &str = "context.edit.model";
}

/// The feature's name: the section's heading, in every language.
pub const NAME: &str = "Context aware transcription";
/// The badge beside it.
pub const EXPERIMENTAL: &str = "Experimental";
/// The switch row's title: what it does, not the feature's name again.
pub const SWITCH_TITLE: &str = "Rewrite what I dictate for the window it goes into";
/// The disclosure, under the switch, always visible.
pub const DISCLOSURE: &str =
    "What you dictate is sent to the models chosen below. Off until you turn it on.";
/// The experimental note, a paragraph each.
pub const NOTE: [&str; 3] = [
    "It may leave your words unchanged without saying so: when a model is slow, unreachable or answers oddly, what you said is typed as heard.",
    "Its settings may change in a later version.",
    "Some setups are untested: the window readers for macOS and Windows have never been built, no model running on this computer has been tried yet, and Anthropic has only been tried against a stand-in.",
];
/// The marking of a model on this computer or the local network.
pub const LOCAL: &str = "Local · slower";

/// The dialogs' ids.
pub(crate) const PICK: &str = "context-pick";
pub(crate) const NOTE_DIALOG: &str = "context-experimental";
pub(crate) const ADD: &str = "provider-add";
pub(crate) const KEY: &str = "provider-key";
/// How many models a provider shows in the picker before the search has
/// to narrow them (OpenRouter lists hundreds).
const SHOWN: usize = 40;

/// Which of the two models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Scene,
    Edit,
}

impl Model {
    pub const ALL: [Model; 2] = [Model::Scene, Model::Edit];

    pub const fn role(self) -> &'static str {
        match self {
            Model::Scene => "scene",
            Model::Edit => "edit",
        }
    }

    const fn keys(self) -> (&'static str, &'static str) {
        match self {
            Model::Scene => (keys::SCENE_PROVIDER, keys::SCENE_MODEL),
            Model::Edit => (keys::EDIT_PROVIDER, keys::EDIT_MODEL),
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Model::Scene => "Window recognition model",
            Model::Edit => "Rewriting model",
        }
    }

    /// The picker's title.
    pub const fn choose(self) -> &'static str {
        match self {
            Model::Scene => "Choose the window recognition model",
            Model::Edit => "Choose the rewriting model",
        }
    }

    /// What "none" means for it: the picker's first row, and the sub-line
    /// when nothing is chosen.
    pub const fn none(self) -> &'static str {
        match self {
            Model::Scene => "Rules only, no model",
            Model::Edit => "None: typed exactly as heard",
        }
    }
}

/// One role's choice as `config.get` publishes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub provider: String,
    pub model: String,
    /// Milestone 1's own address, for a role not yet moved into the
    /// registry.
    pub endpoint: String,
}

impl Choice {
    /// A model is chosen: a provider (or an own address) and a model.
    #[must_use]
    pub fn chosen(&self) -> bool {
        !self.model.trim().is_empty()
            && (!self.provider.trim().is_empty() || !self.endpoint.trim().is_empty())
    }
}

/// `config.get`'s `context` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextConfig {
    pub enabled: bool,
    pub scene: Choice,
    pub edit: Choice,
}

impl ContextConfig {
    /// The table, or `None` from a service that has none.
    #[must_use]
    pub fn parse(config_get: &Value) -> Option<ContextConfig> {
        let table = config_get.get("context")?;
        let text = |key: &str| table.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let choice = |role: &str| Choice {
            provider: text(&format!("{role}.provider")),
            model: text(&format!("{role}.model")),
            endpoint: text(&format!("{role}.endpoint")),
        };
        Some(ContextConfig {
            enabled: table.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            scene: choice("scene"),
            edit: choice("edit"),
        })
    }

    #[must_use]
    pub fn choice(&self, model: Model) -> &Choice {
        match model {
            Model::Scene => &self.scene,
            Model::Edit => &self.edit,
        }
    }
}

/// One provider as `providers.list` gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub base_url: String,
    pub custom: bool,
    pub local: bool,
    pub needs_key: bool,
    pub chat: bool,
    /// Found on this computer (Omarchy's local models), never added.
    pub discovered: bool,
    /// The key's hint (`…abcd (51 chars)`), `""` for none: never the key.
    pub key: String,
    pub models: Vec<String>,
    /// `built-in`, `updated` or `discovered`.
    pub source: String,
    pub updated_at_ms: Option<u64>,
    /// Why the entry is broken, when it is.
    pub problem: Option<String>,
}

/// `providers.list`'s providers.
#[must_use]
pub fn parse_providers(value: &Value) -> Vec<ProviderInfo> {
    value
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| {
            let text = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or("").to_string();
            let flag = |key: &str| p.get(key).and_then(Value::as_bool).unwrap_or(false);
            ProviderInfo {
                id: text("id"),
                kind: text("kind"),
                label: Some(text("label")).filter(|l| !l.is_empty()).unwrap_or_else(|| text("id")),
                base_url: text("base_url"),
                custom: flag("custom"),
                local: flag("local"),
                needs_key: flag("needs_key"),
                chat: p.get("chat").and_then(Value::as_bool).unwrap_or(true),
                discovered: flag("discovered"),
                key: text("key"),
                models: p
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.as_str().map(String::from).or_else(|| m.get("id").and_then(Value::as_str).map(String::from)))
                    .collect(),
                source: text("source"),
                updated_at_ms: p.get("updated_at_ms").and_then(Value::as_u64),
                problem: p.get("problem").and_then(Value::as_str).map(String::from),
            }
        })
        .filter(|p| !p.id.is_empty())
        .collect()
}

/// What the page may show of a saved key: the service's hint when it has
/// the hint's shape — `…` and at most four characters then `(N chars)`, or
/// `N chars` alone for a short key — and `None` otherwise, so that whatever
/// a service sends, nothing longer than four characters of a key is drawn.
#[must_use]
pub fn shown_key(hint: &str) -> Option<String> {
    let hint = hint.trim();
    let count_ok = |rest: &str| {
        rest.strip_suffix(" chars")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    if count_ok(hint) {
        return Some(hint.to_string());
    }
    let tail = hint.strip_prefix('\u{2026}')?;
    let (last, count) = tail.split_once(" (")?;
    let count = count.strip_suffix(')')?;
    (last.chars().count() <= 4 && count_ok(count)).then(|| hint.to_string())
}

/// The key as a page shows it: the last four of the hint (`…c2Ra`), never
/// its length, which says nothing a person needs; `None` when the hint has
/// no last four (a short key's `N chars`) or is not a hint at all.
#[must_use]
pub fn key_tail(hint: &str) -> Option<String> {
    let shown = shown_key(hint)?;
    let (tail, _) = shown.split_once(" (")?;
    Some(tail.to_string())
}

/// What a Model access row says under its name: only what the name does
/// not — the address of a custom or local provider, the key (its last four,
/// or that one is still missing), how many models it offers, and the last
/// check. Never the provider's name or kind again.
#[must_use]
pub fn provider_facts(provider: &ProviderInfo, check: Option<&str>, language: Language) -> Vec<String> {
    let mut facts = Vec::new();
    if (provider.custom || provider.local) && !provider.base_url.is_empty() {
        facts.push(provider.base_url.clone());
    }
    if provider.discovered {
        facts.push(language.text("Found on this computer").to_string());
    } else if let Some(tail) = key_tail(&provider.key) {
        facts.push(language.format("Key {0}", &[&tail]));
    } else if shown_key(&provider.key).is_some() {
        facts.push(language.text("Key saved").to_string());
    } else if provider.needs_key {
        facts.push(language.text("No key yet").to_string());
    }
    if !provider.models.is_empty() {
        facts.push(language.format("{0} models", &[&provider.models.len().to_string()]));
    }
    if let Some(word) = check {
        facts.push(language.text(check_words(word)).to_string());
    }
    facts
}

/// Words in a model id that mark a model that cannot rewrite text: speech
/// in or out, embeddings, reranking, moderation, image generation. Matched
/// against the id's words (split at `/ - _ . :`), so `tts` hides `tts-1`
/// and `gpt-4o-mini-tts` but not a name that merely contains the letters.
/// Kept short on purpose: a model not named here stays listed, since hiding
/// one that works is worse than showing one that does not.
const NOT_CHAT_WORDS: [&str; 15] = [
    "tts", "whisper", "audio", "transcribe", "embed", "embedding", "embeddings", "rerank",
    "reranker", "moderation", "dall", "imagen", "flux", "sdxl", "diffusion",
];

/// Whether `model_id` can be offered for rewriting text: not one of the
/// families [`NOT_CHAT_WORDS`] names, nor `gpt-image-*`.
#[must_use]
pub fn rewrites_text(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    let mut words = id.split(['/', '-', '_', '.', ':']);
    let gpt_image = id.rsplit('/').next().is_some_and(|name| name.starts_with("gpt-image"));
    !gpt_image && !words.any(|word| NOT_CHAT_WORDS.contains(&word))
}

/// What choosing `model_id` at `provider` for `model` writes, in one patch;
/// `None` writes "none" (both unset).
#[must_use]
pub fn pick_action(model: Model, choice: Option<(&str, &str)>) -> UiAction {
    let (provider_key, model_key) = model.keys();
    let (provider, id) = match choice {
        Some((provider, id)) => (
            ConfigValue::Text(provider.to_string()),
            ConfigValue::Text(id.trim().to_string()),
        ),
        None => (ConfigValue::Null, ConfigValue::Null),
    };
    UiAction::ConfigPatch {
        entries: vec![(provider_key.to_string(), provider), (model_key.to_string(), id)],
    }
}

/// Whether `model_id` at `provider` matches what is typed in the search.
#[must_use]
pub fn matches(search: &str, provider: &ProviderInfo, model_id: &str) -> bool {
    let search = search.trim().to_lowercase();
    search.is_empty()
        || model_id.to_lowercase().contains(&search)
        || provider.label.to_lowercase().contains(&search)
}

/// The id a new provider of `kind` gets: the kind's word, then `-2`, `-3`.
#[must_use]
pub fn next_id(kind: &str, taken: &[ProviderInfo]) -> String {
    (1..)
        .map(|n| if n == 1 { kind.to_string() } else { format!("{kind}-{n}") })
        .find(|id| !taken.iter().any(|p| &p.id == id))
        .unwrap_or_else(|| kind.to_string())
}

/// The kinds the Add dialog offers, with their names: the built-in ones
/// need no address; Custom asks for one.
pub const KINDS: [(&str, &str); 5] = [
    ("openai", "OpenAI"),
    ("openrouter", "OpenRouter"),
    ("anthropic", "Anthropic"),
    ("ollama", "Ollama"),
    ("custom", "Custom"),
];

/// A provider check's word, as the page says it.
#[must_use]
pub const fn check_words(word: &str) -> &'static str {
    match word.as_bytes() {
        b"ok" => "Works",
        b"no_key" => "No key yet",
        b"bad_key" => "The key was refused",
        b"unreachable" => "Could not reach it",
        b"wrong_model" => "It does not have that model",
        _ => "It answered with an error",
    }
}

/// What the section keeps.
pub(crate) struct ContextPane {
    pub(crate) config: Option<ContextConfig>,
    pub(crate) providers: Option<Vec<ProviderInfo>>,
    /// The service has no `providers.*` (older than the registry).
    pub(crate) providers_missing: bool,
    pub(crate) writing: bool,
    /// The picker that is open.
    pub(crate) picking: Option<Model>,
    pub(crate) search: Entity<InputState>,
    /// The picker's list, which scrolls.
    pub(crate) pick_scroll: gpui::ScrollHandle,
    /// A custom provider's model id, typed in the picker.
    pub(crate) custom_model: Entity<InputState>,
    /// The Add dialog's kind (an index of [`KINDS`]), address and key.
    pub(crate) add_kind: usize,
    pub(crate) add_url: Entity<InputState>,
    pub(crate) add_key: Entity<InputState>,
    /// The Set key dialog's provider, and its masked field.
    pub(crate) key_for: Option<String>,
    pub(crate) key: Entity<InputState>,
    /// The last check of each provider, by id: its word.
    pub(crate) checks: BTreeMap<String, String>,
}

impl ContextPane {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> (ContextPane, Vec<Subscription>) {
        let mut field = |placeholder: &str, masked: bool| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked))
        };
        let search = field("Search models", false);
        let custom_model = field("Model id", false);
        let add_url = field("http://192.168.1.10:11434/v1", false);
        let add_key = field("Paste the API key\u{2026}", true);
        let key = field("Paste the API key\u{2026}", true);
        // The picker's list follows the search as it is typed.
        let follow = cx.subscribe_in(&search, window, |_, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        (
            ContextPane {
                config: None,
                providers: None,
                providers_missing: false,
                writing: false,
                picking: None,
                search,
                pick_scroll: gpui::ScrollHandle::new(),
                custom_model,
                add_kind: 1,
                add_url,
                add_key,
                key_for: None,
                key,
                checks: BTreeMap::new(),
            },
            vec![follow],
        )
    }

    fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.as_ref()?.iter().find(|p| p.id == id)
    }
}

/// A small tag beside a title: the Experimental badge, the local marking.
fn tag(text: &str, p: &Palette) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(4.))
        .bg(p.well)
        .text_color(p.text2)
        .ts(11., 16.)
        .child(text.to_string())
}

impl Shell {
    // ---- the section -------------------------------------------------------

    /// The section on the Voice page.
    pub(crate) fn context_section(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let pane = &self.voice.context;
        let heading = h_flex()
            .gap(px(8.))
            .items_center()
            .child(ui::label(self.tr(NAME), p))
            .child(tag(self.tr(EXPERIMENTAL), p))
            .child(div().flex_1())
            .child(
                Btn::new("context-experimental-what", self.tr("What does experimental mean?"))
                    .quiet()
                    .on_click(cx.listener(|this, _, window, _| this.open_dialog(NOTE_DIALOG, window))),
            );
        let Some(config) = pane.config.as_ref() else {
            return v_flex()
                .gap(px(8.))
                .child(heading)
                .child(ui::hint(self.tr("This needs a newer background service."), p));
        };
        let busy = dim || pane.writing;
        let switch = Row::new()
            .title(self.tr(SWITCH_TITLE))
            .sub_el(div().child(ui::para(self.tr(DISCLOSURE))))
            .right(
                Switch::new("context-enabled", config.enabled)
                    .disabled(busy)
                    .on_toggle(cx.listener(|this, on: &bool, window, cx| {
                        this.set_context_enabled(*on, window, cx);
                    })),
            )
            .grow();
        let mut rows = vec![switch.into_any_element()];
        if config.enabled && !config.edit.chosen() {
            rows.push(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .child(ui::hint(
                        self.tr("It is on, but no rewriting model is chosen, so what you dictate is typed exactly as heard."),
                        p,
                    ))
                    .into_any_element(),
            );
        }
        for model in Model::ALL {
            rows.push(self.picker_row(model, config.choice(model), busy, p, cx));
        }
        rows.push(
            Row::new()
                .title(self.tr(Screen::Models.label()))
                .sub(self.tr("Providers and their keys, set once and shared by every feature."))
                .right(
                    Btn::new("context-model-access", self.tr("Open"))
                        .on_click(cx.listener(|this, _, window, cx| this.go(Screen::Models, window, cx))),
                )
                .into_any_element(),
        );
        v_flex().gap(px(8.)).child(heading).child(ui::card(rows, p))
    }

    /// A picker's row: the model chosen and its provider, with the local
    /// marking, and **Choose…**.
    fn picker_row(&self, model: Model, choice: &Choice, busy: bool, p: &Palette, cx: &Context<Self>) -> AnyElement {
        let pane = &self.voice.context;
        let sub: AnyElement = if choice.chosen() {
            let provider = pane.provider(&choice.provider);
            let name = provider.map_or_else(
                || if choice.provider.is_empty() { self.tr("Own address").to_string() } else { choice.provider.clone() },
                |p| p.label.clone(),
            );
            h_flex()
                .gap(px(6.))
                .items_center()
                .child(ui::mono(format!("{} \u{00b7} {name}", choice.model), p.text2))
                .when(provider.is_some_and(|p| p.local), |row| row.child(tag(self.tr(LOCAL), p)))
                .into_any_element()
        } else {
            ui::hint(self.tr(model.none()), p).into_any_element()
        };
        Row::new()
            .title(self.tr(model.title()))
            .sub_el(sub)
            .right(
                Btn::new(SharedString::from(format!("context-choose-{}", model.role())), self.tr("Choose\u{2026}"))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| this.open_picker(model, window, cx))),
            )
            .into_any_element()
    }

    // ---- the dialogs -------------------------------------------------------

    /// The section's dialogs, by id, with their widths.
    pub(crate) fn context_dialog(&self, id: &'static str, cx: &Context<Self>) -> Option<(AnyElement, f32)> {
        match id {
            PICK => Some((self.render_picker(cx), 520.)),
            NOTE_DIALOG => Some((self.render_note(cx), 440.)),
            ADD => Some((self.render_add(cx), 460.)),
            KEY => Some((self.render_key(cx), 420.)),
            _ => None,
        }
    }

    /// A dialog went: its typed values are let go of.
    pub(crate) fn context_dialog_closed(&mut self, id: &'static str, cx: &mut Context<Self>) {
        match id {
            PICK => self.voice.context.picking = None,
            KEY => self.voice.context.key_for = None,
            _ => {}
        }
        cx.notify();
    }

    /// The picker: search, the models grouped by provider (the local ones
    /// marked), a model id only for a custom provider with no list, and
    /// **Add more…**.
    fn render_picker(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let pane = &self.voice.context;
        let model = pane.picking.unwrap_or(Model::Edit);
        let search = pane.search.read(cx).value().to_string();
        let current = pane.config.as_ref().map(|c| c.choice(model).clone()).unwrap_or_default();
        let row = |id: SharedString, text: String, chosen: bool| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |s| s.bg(p.well))
                .when(chosen, |r| r.bg(p.well).font_weight(gpui::FontWeight::SEMIBOLD))
                .child(text)
        };
        let mut list = v_flex().gap(px(2.)).child(
            row(SharedString::from("pick-none"), self.tr(model.none()).to_string(), !current.chosen())
                .on_click(cx.listener(move |this, _, window, cx| this.pick(model, None, window, cx))),
        );
        for provider in pane.providers.iter().flatten().filter(|p| p.chat && p.problem.is_none()) {
            let shown: Vec<&String> =
                provider.models.iter().filter(|m| rewrites_text(m) && matches(&search, provider, m)).collect();
            let typed_id = provider.custom && provider.models.is_empty();
            if shown.is_empty() && !typed_id {
                continue;
            }
            let head = h_flex()
                .gap(px(6.))
                .items_center()
                .pt(px(10.))
                .child(ui::label(provider.label.clone(), &p))
                .when(provider.local, |h| h.child(tag(self.tr(LOCAL), &p)))
                .when(provider.needs_key && provider.key.is_empty(), |h| h.child(tag(self.tr("No key yet"), &p)));
            list = list.child(head);
            for model_id in shown.iter().take(SHOWN) {
                let (pid, mid) = (provider.id.clone(), (*model_id).clone());
                let chosen = current.provider == pid && current.model == mid;
                list = list.child(
                    row(SharedString::from(format!("pick-{pid}-{mid}")), mid.clone(), chosen)
                        .on_click(cx.listener(move |this, _, window, cx| this.pick(model, Some((pid.clone(), mid.clone())), window, cx))),
                );
            }
            if shown.len() > SHOWN {
                list = list.child(ui::hint(self.fmt("{0} more: type to narrow the list", &[&(shown.len() - SHOWN).to_string()]), &p));
            }
            if typed_id {
                let pid = provider.id.clone();
                list = list.child(
                    h_flex()
                        .gap(px(8.))
                        .child(div().flex_1().child(ui::field(&pane.custom_model, &p)))
                        .child(Btn::new(SharedString::from(format!("pick-use-{pid}")), self.tr("Use")).on_click(cx.listener(
                            move |this, _, window, cx| {
                                let id = this.voice.context.custom_model.read(cx).value().trim().to_string();
                                if !id.is_empty() {
                                    this.pick(model, Some((pid.clone(), id)), window, cx);
                                }
                            },
                        ))),
                );
            }
        }
        v_flex()
            .p(px(20.))
            .gap(px(10.))
            .ts(13., 18.)
            .child(div().semibold().child(self.tr(model.choose()).to_string()))
            .child(ui::field(&pane.search, &p))
            .child(
                // The same thin track the app's menus show, drawn only when
                // the list is longer than its 380 px. The bar sets no top or
                // left of its own, so it is pinned to the list's box by an
                // absolute wrapper, and the box clips: it cannot be painted
                // outside the list.
                div()
                    .relative()
                    .overflow_hidden()
                    .child(div().id("pick-list").max_h(px(380.)).overflow_y_scroll().track_scroll(&pane.pick_scroll).child(list))
                    .child(div().absolute().inset_0().child(Scrollbar::vertical(&pane.pick_scroll).scrollbar_show(ScrollbarShow::Always))),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Btn::new("pick-add-more", self.tr("Add more\u{2026}")).quiet().on_click(cx.listener(
                        |this, _, window, cx| {
                            this.close_dialog(window, cx);
                            this.go(Screen::Models, window, cx);
                        },
                    )))
                    .child(div().flex_1())
                    .child(Btn::new("pick-cancel", self.tr("Cancel")).on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx)))),
            )
            .into_any_element()
    }

    /// What "experimental" means, in plain words; **Got it** keeps it read.
    fn render_note(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        v_flex()
            .p(px(20.))
            .gap(px(10.))
            .ts(13., 18.)
            .child(h_flex().gap(px(8.)).items_center().child(div().semibold().child(self.tr(NAME).to_string())).child(tag(self.tr(EXPERIMENTAL), &p)))
            .children(NOTE.iter().map(|line| div().text_color(p.text2).child(ui::para(self.tr(line).to_string()))))
            .child(
                h_flex().child(div().flex_1()).child(
                    Btn::new("context-note-ok", self.tr("Got it"))
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.update_prefs(cx, |prefs| prefs.set_context_experimental_seen());
                            this.close_dialog(window, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    /// Add a provider: its kind, the address only for Custom, and its key.
    fn render_add(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let pane = &self.voice.context;
        let (kind, _) = KINDS[pane.add_kind.min(KINDS.len() - 1)];
        let custom = kind == "custom";
        let kinds = Segmented::new(
            "provider-add-kind",
            KINDS.iter().map(|(_, name)| SharedString::from(if *name == "Custom" { self.tr("Custom").to_string() } else { (*name).to_string() })).collect(),
            pane.add_kind,
        )
        .on_select(cx.listener(|this, at: &usize, _, cx| {
            this.voice.context.add_kind = *at;
            cx.notify();
        }));
        let labeled = |label: &str, el: AnyElement| {
            v_flex().gap(px(4.)).child(div().text_color(p.text2).child(self.tr(label).to_string())).child(el)
        };
        v_flex()
            .p(px(20.))
            .gap(px(12.))
            .ts(13., 18.)
            .child(div().semibold().child(self.tr("Add provider").to_string()))
            .child(kinds)
            .when(custom, |d| {
                d.child(labeled("Address", ui::field(&pane.add_url, &p).into_any_element()))
                    .child(ui::hint(self.tr("Enter the server's address, such as http://192.168.1.10:11434/v1."), &p))
            })
            .when(kind != "ollama", |d| d.child(labeled("API key", ui::field(&pane.add_key, &p).into_any_element())))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1())
                    .child(Btn::new("provider-add-cancel", self.tr("Cancel")).on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))))
                    .child(Btn::new("provider-add-ok", self.tr("Add")).primary().on_click(cx.listener(|this, _, window, cx| this.add_provider(window, cx)))),
            )
            .into_any_element()
    }

    /// A provider's key: the masked field, **Remove Key** when one is set.
    fn render_key(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let pane = &self.voice.context;
        let provider = pane.key_for.as_deref().and_then(|id| pane.provider(id));
        let label = provider.map_or_else(String::new, |p| p.label.clone());
        let has_key = provider.is_some_and(|p| !p.key.is_empty());
        v_flex()
            .p(px(20.))
            .gap(px(12.))
            .ts(13., 18.)
            .child(div().semibold().child(self.fmt("{0}: API key", &[&label])))
            .when(has_key, |d| {
                let shown = provider.and_then(|p| key_tail(&p.key)).map_or_else(|| self.tr("Key saved").to_string(), |tail| self.fmt("Key {0}", &[&tail]));
                d.child(ui::mono(shown, p.text2))
            })
            .child(ui::field(&pane.key, &p))
            .child(
                h_flex()
                    .gap(px(8.))
                    .when(has_key, |row| {
                        row.child(Btn::new("provider-key-remove", self.tr("Remove Key")).quiet().on_click(cx.listener(|this, _, window, cx| this.clear_provider_key(window, cx))))
                    })
                    .child(div().flex_1())
                    .child(Btn::new("provider-key-cancel", self.tr("Cancel")).on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))))
                    .child(Btn::new("provider-key-save", self.tr("Save Key")).primary().on_click(cx.listener(|this, _, window, cx| this.save_provider_key(window, cx)))),
            )
            .into_any_element()
    }

    // ---- the Model access page -------------------------------------------------

    /// Every provider: its kind, where it is (a custom one's address), its
    /// key's hint, its models' source, the last check; **Set key**,
    /// **Check**, **Refresh models**, **Remove**; and **Add provider**.
    pub(crate) fn render_models(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let pane = &self.voice.context;
        let header = ui::Header::new(
            self.tr(Screen::Models.label()),
            self.tr("Providers and their keys, set once and shared by every feature."),
        )
        .back("models-back", self.tr(Screen::Voice.label()), cx.listener(|this, _, window, cx| this.go(Screen::Voice, window, cx)))
        .right(Btn::new("models-add", self.tr("Add provider")).primary().disabled(dim).on_click(cx.listener(|this, _, window, cx| this.open_add_provider(window, cx))));
        let rows: Vec<AnyElement> = match pane.providers.as_ref() {
            None if pane.providers_missing => vec![Row::new().title(self.tr("This needs a newer background service.")).into_any_element()],
            None => vec![Row::new().title(self.tr("Reading\u{2026}")).into_any_element()],
            Some(list) if list.is_empty() => vec![Row::new().title(self.tr("No providers yet.")).into_any_element()],
            Some(list) => list.iter().map(|provider| self.provider_row(provider, dim, &p, cx)).collect(),
        };
        let body = v_flex().gap(px(20.)).child(header).child(ui::card(rows, &p));
        ui::page("models", &self.page_scroll, self.content_width, body).into_any_element()
    }

    fn provider_row(&self, provider: &ProviderInfo, dim: bool, p: &Palette, cx: &Context<Self>) -> AnyElement {
        let pane = &self.voice.context;
        let id = provider.id.clone();
        let facts = provider_facts(provider, pane.checks.get(&id).map(String::as_str), self.language());
        // The name and the buttons share the first line; the facts, which
        // can be long (an address), get the whole second line and wrap
        // there rather than being cut.
        let sub = h_flex()
            .gap(px(6.))
            .items_start()
            .when(provider.local, |row| row.child(tag(self.tr(LOCAL), p)))
            .child(div().flex_1().min_w_0().child(ui::hint(facts.join(" \u{00b7} "), p)));
        let button = |suffix: &str, label: &str| Btn::new(SharedString::from(format!("provider-{suffix}-{id}")), self.tr(label).to_string()).disabled(dim || pane.writing);
        let (key_id, check_id, refresh_id, remove_id) = (id.clone(), id.clone(), id.clone(), id.clone());
        let mut right = h_flex().gap(px(6.));
        if !provider.discovered && (provider.needs_key || provider.custom) {
            right = right.child(button("key", "Set key").on_click(cx.listener(move |this, _, window, cx| this.open_provider_key(&key_id, window, cx))));
        }
        right = right
            .child(button("check", "Check connection").on_click(cx.listener(move |this, _, window, cx| this.check_provider(&check_id, window, cx))))
            .child(button("refresh", "Refresh models").on_click(cx.listener(move |this, _, window, cx| this.refresh_provider(&refresh_id, window, cx))));
        if !provider.discovered {
            right = right.child(button("remove", "Remove provider").quiet().on_click(cx.listener(move |this, _, window, cx| this.remove_provider(&remove_id, window, cx))));
        }
        v_flex()
            .w_full()
            .px(px(16.))
            .py(px(8.))
            .gap(px(4.))
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .child(div().flex_1().min_w_0().ts(13., 18.).medium().text_color(p.text).child(provider.label.clone()))
                    .child(right.flex_none()),
            )
            .child(sub)
            .into_any_element()
    }

    // ---- reading -------------------------------------------------------------

    /// `config.get` answered: keep the table, then read the providers.
    pub(crate) fn took_context_config(&mut self, value: &Value, window: &mut Window, cx: &mut Context<Self>) {
        self.voice.context.config = ContextConfig::parse(value);
        self.refresh_providers(window, cx);
    }

    /// `providers.list`.
    pub(crate) fn refresh_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::ProvidersList, window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                this.voice.context.providers = Some(parse_providers(&value));
                this.voice.context.providers_missing = false;
            }
            Err(e) => {
                if this.missing_or(&e, window, cx).is_none() {
                    this.voice.context.providers_missing = true;
                }
            }
        });
    }

    /// The fields' placeholders, in the window's language.
    pub(crate) fn localize_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.voice.context;
        let fields = [
            (pane.search.clone(), self.tr("Search models").to_string()),
            (pane.custom_model.clone(), self.tr("Model id").to_string()),
            (pane.add_key.clone(), self.tr("Paste the API key\u{2026}").to_string()),
            (pane.key.clone(), self.tr("Paste the API key\u{2026}").to_string()),
        ];
        for (field, placeholder) in fields {
            field.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
    }

    // ---- writing -------------------------------------------------------------

    /// The switch. Turned on for the first time, the experimental note
    /// comes up once.
    fn set_context_enabled(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let action = UiAction::ConfigSet {
            key: keys::ENABLED.into(),
            value: ConfigValue::Bool(on),
        };
        self.write_context(action, |_, _, _| {}, window, cx);
        if on && !self.prefs(cx).context_experimental_seen() {
            self.open_dialog(NOTE_DIALOG, window);
        }
    }

    fn open_picker(&mut self, model: Model, window: &mut Window, cx: &mut Context<Self>) {
        self.voice.context.picking = Some(model);
        let pane = &self.voice.context;
        let (search, custom) = (pane.search.clone(), pane.custom_model.clone());
        search.update(cx, |input, cx| input.set_value("", window, cx));
        custom.update(cx, |input, cx| input.set_value("", window, cx));
        self.refresh_providers(window, cx);
        self.open_dialog(PICK, window);
        search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// A model chosen in the picker (`None`: none).
    fn pick(&mut self, model: Model, choice: Option<(String, String)>, window: &mut Window, cx: &mut Context<Self>) {
        let action = pick_action(model, choice.as_ref().map(|(p, m)| (p.as_str(), m.as_str())));
        self.close_dialog(window, cx);
        self.write_context(action, |this, window, cx| this.toast(this.tr("Saved."), window, cx), window, cx);
    }

    fn open_add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.voice.context;
        let (url, key) = (pane.add_url.clone(), pane.add_key.clone());
        url.update(cx, |input, cx| input.set_value("", window, cx));
        key.update(cx, |input, cx| input.set_value("", window, cx));
        self.open_dialog(ADD, window);
        cx.notify();
    }

    /// **Add**: the provider, then its key when one was typed.
    fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = &self.voice.context;
        let (kind, _) = KINDS[pane.add_kind.min(KINDS.len() - 1)];
        let id = next_id(kind, pane.providers.as_deref().unwrap_or(&[]));
        let url = pane.add_url.read(cx).value().trim().to_string();
        let typed = pane.add_key.read(cx).value().trim().to_string();
        let key_field = pane.add_key.clone();
        if kind == "custom" && url.is_empty() {
            self.toast(self.tr("Enter the server's address, such as http://192.168.1.10:11434/v1."), window, cx);
            return;
        }
        let action = UiAction::ProvidersAdd {
            id: id.clone(),
            kind: kind.into(),
            base_url: (kind == "custom").then_some(url),
        };
        self.close_dialog(window, cx);
        self.write_context(
            action,
            move |this, window, cx| {
                key_field.update(cx, |input, cx| input.set_value("", window, cx));
                if typed.is_empty() {
                    this.toast(this.tr("Saved."), window, cx);
                    return;
                }
                this.write_context(
                    UiAction::ProvidersKeySet { provider: id, key: BotKey::new(&typed) },
                    |this, window, cx| this.toast(this.tr("Saved."), window, cx),
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    fn open_provider_key(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.voice.context.key_for = Some(id.to_string());
        let key = self.voice.context.key.clone();
        key.update(cx, |input, cx| input.set_value("", window, cx));
        self.open_dialog(KEY, window);
        key.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn save_provider_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.voice.context.key_for.clone() else { return };
        let field = self.voice.context.key.clone();
        let typed = field.read(cx).value().trim().to_string();
        if typed.is_empty() {
            self.toast(self.tr("Paste the key first."), window, cx);
            return;
        }
        field.update(cx, |input, cx| input.set_value("", window, cx));
        self.close_dialog(window, cx);
        self.write_context(
            UiAction::ProvidersKeySet { provider: id, key: BotKey::new(&typed) },
            |this, window, cx| this.toast(this.tr("Saved."), window, cx),
            window,
            cx,
        );
    }

    fn clear_provider_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.voice.context.key_for.clone() else { return };
        self.close_dialog(window, cx);
        self.write_context(
            UiAction::ProvidersKeyClear { provider: id },
            |this, window, cx| this.toast(this.tr("Key removed."), window, cx),
            window,
            cx,
        );
    }

    fn check_provider(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        let model = self.voice.context.config.as_ref().and_then(|c| {
            Model::ALL.into_iter().map(|m| c.choice(m)).find(|choice| choice.provider == id).map(|choice| choice.model.clone())
        });
        let action = UiAction::ProvidersTest { provider: id.clone(), model };
        self.ask(action, window, cx, move |this, result, window, cx| match result {
            Ok(value) => {
                let word = value.get("result").and_then(Value::as_str).unwrap_or("failed").to_string();
                this.voice.context.checks.insert(id, word);
            }
            Err(e) => {
                if let Some(why) = this.missing_or(&e, window, cx) {
                    this.toast(this.fmt("Could not save: {0}", &[&why]), window, cx);
                }
            }
        });
    }

    fn refresh_provider(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.write_context(UiAction::ProvidersRefresh { provider: id.to_string() }, |_, _, _| {}, window, cx);
    }

    fn remove_provider(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        let in_use = self.voice.context.config.as_ref().is_some_and(|c| Model::ALL.into_iter().any(|m| c.choice(m).provider == id));
        let clear = id.clone();
        self.write_context(
            UiAction::ProvidersRemove { id, force: in_use },
            move |this, window, cx| {
                this.write_context(UiAction::ProvidersKeyClear { provider: clear }, |_, _, _| {}, window, cx);
            },
            window,
            cx,
        );
    }

    /// One write of the section's or the page's, then everything read
    /// again. One at a time.
    fn write_context(
        &mut self,
        action: UiAction,
        done: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voice.context.writing {
            return;
        }
        self.voice.context.writing = true;
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.voice.context.writing = false;
            match result {
                Ok(_) => {
                    done(this, window, cx);
                    this.refresh_voice(true, window, cx);
                }
                Err(e) => {
                    if let Some(why) = this.missing_or(&e, window, cx) {
                        this.toast(this.fmt("Could not save: {0}", &[&why]), window, cx);
                    }
                }
            }
        });
    }
}

/// The section's and the page's words in `language`, for the tests: the
/// disclosure and the experimental note first.
#[must_use]
pub fn words(language: Language) -> Vec<&'static str> {
    let mut all = vec![
        language.text(DISCLOSURE),
        language.text(NAME),
        language.text(EXPERIMENTAL),
        language.text(SWITCH_TITLE),
        language.text(LOCAL),
        language.text("What does experimental mean?"),
        language.text("Got it"),
    ];
    all.extend(NOTE.iter().map(|line| language.text(line)));
    for model in Model::ALL {
        all.extend([language.text(model.title()), language.text(model.choose()), language.text(model.none())]);
    }
    for word in ["ok", "no_key", "bad_key", "unreachable", "wrong_model", "failed"] {
        all.push(language.text(check_words(word)));
    }
    all.push(language.text(Screen::Models.label()));
    all.push(language.text("Key saved"));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn speech_and_other_non_text_models_are_not_offered_for_rewriting() {
        // Seen in OpenAI's built-in list on the Model access capture.
        for id in ["gpt-4o-mini-tts", "tts-1", "tts-1-hd", "whisper-1", "gpt-4o-transcribe",
            "openai/gpt-4o-audio-preview", "text-embedding-3-small", "nomic-embed-text",
            "cohere/rerank-v3.5", "omni-moderation-latest", "dall-e-3", "gpt-image-1",
            "openai/gpt-image-1", "black-forest-labs/flux-1.1-pro", "imagen-4"]
        {
            assert!(!rewrites_text(id), "{id} was offered");
        }
        // The chat models seen beside them stay, and so do names that only
        // contain the letters, and models this list knows nothing about.
        for id in ["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano", "claude-haiku-5-5", "claude-sonnet-5-5",
            "deepseek/deepseek-v4.1-flash-1017", "openai/gpt-4o-mini", "anthropic/claude-haiku-4.5",
            "~z-ai/glm-flash-latest", "mattsttsmodel", "google/gemini-2.5-flash-image", "qwen3:8b"]
        {
            assert!(rewrites_text(id), "{id} was hidden");
        }
    }

    #[test]
    fn a_provider_row_says_only_what_its_name_does_not() {
        let provider = |kind: &str, label: &str, url: &str, local: bool, key: &str, models: usize| ProviderInfo {
            id: kind.into(),
            kind: kind.into(),
            label: label.into(),
            base_url: url.into(),
            custom: kind == "custom",
            local,
            needs_key: kind != "ollama" && kind != "custom",
            chat: true,
            discovered: false,
            key: key.into(),
            models: (0..models).map(|n| format!("m{n}")).collect(),
            source: "built-in".into(),
            updated_at_ms: None,
            problem: None,
        };
        let en = Language::English;
        let openrouter = provider("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", false, "\u{2026}c2Ra (71 chars)", 476);
        assert_eq!(provider_facts(&openrouter, None, en), ["Key \u{2026}c2Ra", "476 models"]);
        let anthropic = provider("anthropic", "Anthropic", "https://api.anthropic.com/v1", false, "", 2);
        assert_eq!(provider_facts(&anthropic, Some("ok"), en)[..2], ["No key yet", "2 models"]);
        let studio = provider("custom", "LM Studio", "http://127.0.0.1:1234/v1", true, "", 0);
        assert_eq!(provider_facts(&studio, None, en), ["http://127.0.0.1:1234/v1"]);
        let ollama = provider("ollama", "Ollama", "http://127.0.0.1:11434/v1", true, "", 0);
        assert_eq!(provider_facts(&ollama, None, en), ["http://127.0.0.1:11434/v1"]);
        for p in [&openrouter, &anthropic, &studio, &ollama] {
            for language in [en, Language::SimplifiedChinese] {
                let facts = provider_facts(p, None, language).join(" ");
                assert!(!facts.contains(&p.label), "{facts}");
                assert!(!facts.contains("chars"), "{facts}");
            }
        }
    }

    #[test]
    fn a_key_is_shown_by_its_last_four_and_never_its_length() {
        assert_eq!(key_tail("\u{2026}c2Ra (71 chars)").as_deref(), Some("\u{2026}c2Ra"));
        assert_eq!(key_tail("8 chars"), None);
        assert_eq!(key_tail(""), None);
        assert_eq!(key_tail("\u{2026}VWXYZ (73 chars)"), None);
    }

    const KEY: &str = "sk-or-v1-0123456789abcdef0123456789abcdef-WXYZ";

    fn listed() -> Value {
        json!({"providers": [
            {"id": "openrouter", "kind": "openrouter", "label": "OpenRouter", "base_url": "https://openrouter.ai/api/v1",
             "custom": false, "local": false, "needs_key": true, "chat": true, "speech": false,
             "key": "\u{2026}WXYZ (73 chars)", "key_store": "secret_service",
             "models": ["deepseek/deepseek-v4.1-flash-1017", "openai/gpt-4o-mini"], "source": "updated", "updated_at_ms": 1}
            ,
            {"id": "box", "kind": "custom", "label": "Custom", "base_url": "http://192.168.1.10:11434/v1",
             "custom": true, "local": true, "needs_key": false, "chat": true, "key": "", "models": [], "source": "built-in"},
            {"id": "omarchy", "kind": "omarchy", "label": "Omarchy local models", "discovered": true, "local": true,
             "needs_key": true, "chat": true, "key": "Omarchy's own", "models": [{"id": "qwen3.8-27b-gufo", "running": true}], "source": "discovered"}
        ]})
    }

    /// The section reads `config.get` and `providers.list` the way the
    /// service publishes them, and a pick writes provider and model in one
    /// `config.set`; none unsets both.
    #[test]
    fn a_pick_round_trips_through_the_apps_own_path() {
        let config = ContextConfig::parse(&json!({"context": {
            "enabled": true, "edit.provider": "openrouter", "edit.model": "deepseek/deepseek-v4.1-flash-1017",
            "scene.provider": "", "scene.model": "", "edit.key": "\u{2026}WXYZ (73 chars)"
        }}))
        .unwrap();
        assert!(config.edit.chosen() && !config.scene.chosen());
        let providers = parse_providers(&listed());
        assert_eq!(providers.len(), 3);
        assert!(providers[1].custom && providers[1].local && providers[1].models.is_empty());
        assert!(providers[2].discovered && providers[2].models == ["qwen3.8-27b-gufo"]);
        let UiAction::ConfigPatch { entries } = pick_action(Model::Scene, Some(("box", " qwen2.5:3b "))) else { panic!() };
        assert_eq!(entries, vec![
            (keys::SCENE_PROVIDER.to_string(), ConfigValue::Text("box".into())),
            (keys::SCENE_MODEL.to_string(), ConfigValue::Text("qwen2.5:3b".into())),
        ]);
        let (method, params) = pick_action(Model::Edit, None).request();
        assert_eq!(method, "config.set");
        assert_eq!(params["patch"], json!({"context.edit.provider": null, "context.edit.model": null}));
        // A Milestone 1 role on its own address still counts as chosen.
        let legacy = Choice { provider: String::new(), model: "m".into(), endpoint: "https://x/v1".into() };
        assert!(legacy.chosen());
    }

    #[test]
    fn the_search_narrows_by_model_or_provider_and_ids_do_not_clash() {
        let providers = parse_providers(&listed());
        assert!(matches("", &providers[0], "anything"));
        assert!(matches("DEEPSEEK", &providers[0], "deepseek/deepseek-v4.1-flash-1017"));
        assert!(matches("openrouter", &providers[0], "openai/gpt-4o-mini"));
        assert!(!matches("claude", &providers[0], "openai/gpt-4o-mini"));
        assert_eq!(next_id("openrouter", &providers), "openrouter-2");
        assert_eq!(next_id("anthropic", &providers), "anthropic");
    }

    /// A saved key is drawn as its length and at most its last four
    /// characters, and anything else a service might send is not drawn.
    #[test]
    fn a_saved_key_shows_only_its_length_and_last_four() {
        assert_eq!(shown_key("\u{2026}WXYZ (73 chars)").as_deref(), Some("\u{2026}WXYZ (73 chars)"));
        assert_eq!(shown_key("8 chars").as_deref(), Some("8 chars"));
        for not_a_hint in ["", KEY, "\u{2026}VWXYZ (73 chars)", "\u{2026}WXYZ (many chars)", "Omarchy's own"] {
            assert_eq!(shown_key(not_a_hint), None, "{not_a_hint}");
        }
        let set = UiAction::ProvidersKeySet { provider: "openrouter".into(), key: BotKey::new(KEY) };
        assert!(!format!("{set:?}").contains("WXYZ"), "a key never shows in {{:?}}");
        assert_eq!(set.request().0, "providers.key.set");
    }

    /// The disclosure, the badge, the note and every sentence of the
    /// section, in every language the app ships.
    #[test]
    fn the_disclosure_and_the_experimental_note_are_in_every_language() {
        crate::i18n::install();
        for language in [Language::SimplifiedChinese, Language::TraditionalChinese, Language::Japanese] {
            for (english, said) in words(Language::English).into_iter().zip(words(language)) {
                assert_ne!(english, said, "{language:?} has no words for {english:?}");
                assert!(!said.trim().is_empty());
            }
        }
        assert_eq!(Language::SimplifiedChinese.text(NAME), "按窗口改写听写");
        assert_eq!(Language::SimplifiedChinese.text(EXPERIMENTAL), "实验性");
    }

    /// Every word the section hands to a lookup has a row.
    #[test]
    fn every_word_of_the_section_has_a_row() {
        crate::i18n::install();
        let source = include_str!("context.rs");
        let (body, _) = source.split_once("#[cfg(test)]").unwrap();
        let missing = crate::i18n::missing_rows(body);
        assert!(missing.is_empty(), "{missing:?}");
        assert!(crate::i18n::literals(body).len() >= 30);
    }
}
