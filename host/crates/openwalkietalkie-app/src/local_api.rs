//! Local API, Service's sub-page: the HTTP door local tools reach the
//! service through — its port, the browser origins it lets in and a `curl`
//! to try it with — and the bearer tokens that open it.
//!
//! Nothing shipped with the deck uses the door: the deck's own link, the
//! Claude Code plugin's hooks and the connected apps all reach the service
//! over its socket. So the door is off unless the owner switches it on (the
//! Service page's switch, `[api] tcp`), the page is reached from its
//! **Details** only while it is on, and the door and its tokens go in the
//! next release. A new port is only a `config.set` and a restart (the
//! service binds its door when it starts), and nothing has to be re-pointed
//! at it.
//!
//! **The page hands out no more than it has to.** Its `curl` asks for
//! `service.info`, which an `observe` token may call and which carries
//! nothing said in a session, and the new-token form keeps the scopes that
//! read a session's words or act in one ([`ADVANCED`]) behind **Advanced**.
//! Whether a token may have them at all is the owner's open decision, so
//! nothing is refused: they are a press away.
//!
//! **A created token's plaintext lives in exactly one place and dies there.**
//! It is held by [`Shown`] in the Service page's pane, never in the mirror,
//! never in `HostState`, and never in a `String` that a toast or a log line
//! could carry: `Secret` has no `Clone`, no `Display` and a `Debug` that
//! prints a length. Its dialog reads the value from there while it is
//! drawn, so there is no second copy, and only its own **Done** closes it
//! (`shell::Dialog::dismissable`): closing it is the end of the value.
use crate::present;
use crate::shell::{Screen, Sheet, Shell};
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Chip, Row, Type as _};
use deck_gui_core::admin::{method_missing, ApiConfig, Info, Secret, TokenRow};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{Scope, Ts};
use gpui::{
    div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Div, SharedString, Window,
};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;

/// The shown-once dialog's id (the one dialog Escape does not close), and
/// how wide it is drawn.
pub(crate) const TOKEN_DIALOG: &str = "token";
const TOKEN_WIDTH: f32 = 460.;

/// How wide the port field is.
const PORT_FIELD: f32 = 96.;

/// The scopes **Advanced** keeps out of the way: the two that read what is
/// said in a session (`observe:content`, `observe:raw`) and the one that
/// acts in one (`operate`).
pub const ADVANCED: [Scope; 3] = [Scope::ObserveContent, Scope::ObserveRaw, Scope::Operate];

/// **Advanced**'s word, as its row's key. The row is this page's, held in
/// the Integrations page's table until the Service page's has it. Looked up
/// through this name, it is a key the Service page's word check does not
/// read, so this file's own tests check it has all four languages.
const ADVANCED_WORD: &str = "Advanced";

/// The plaintext of a token this window just created or rotated, while it
/// is on screen.
///
/// It is a type and not an `Option<String>` so that the one thing that must
/// never happen — the value in a formatted string — cannot be written by
/// accident: `Shown` has no `Display` and its `Debug` prints the token's
/// **name** and its length.
pub struct Shown {
    /// What it is called, which is safe to say anywhere.
    pub name: String,
    /// Which call produced it, in English (the key the dialog's heading is
    /// looked up by): `Created` or `Rotated`.
    pub verb: &'static str,
    secret: Secret,
}

impl Shown {
    pub fn new(name: impl Into<String>, verb: &'static str, secret: Secret) -> Shown {
        Shown {
            name: name.into(),
            verb,
            secret,
        }
    }

    /// The plaintext, borrowed for exactly as long as it is drawn or copied.
    pub fn reveal(&self) -> &str {
        self.secret.reveal()
    }
}

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown")
            .field("name", &self.name)
            .field("verb", &self.verb)
            .field("secret", &self.secret)
            .finish()
    }
}

/// Dismiss the one-time plaintext. `true` when there was one to dismiss.
///
/// This is the whole of "shown once": the value is dropped here (and
/// overwritten as it goes, [`Secret`]), and the service keeps only a hash,
/// so there is no second way to it from anywhere.
pub fn close(shown: &mut Option<Shown>) -> bool {
    shown.take().is_some()
}

// ---- what the page says, as pure functions ---------------------------------

/// Why what was typed is not a port the service can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortRefusal {
    /// Nothing was typed.
    Empty,
    /// Not a number from 1 to 65535.
    NotAPort,
    /// Below 1024, which needs root; the service runs as the owner.
    Privileged,
}

impl PortRefusal {
    /// What the toast says.
    pub fn words(self, language: Language) -> &'static str {
        match self {
            PortRefusal::Empty => language.text("Type the new port first."),
            PortRefusal::NotAPort => language.text("A port is a number from 1 to 65535."),
            PortRefusal::Privileged => {
                language.text("Ports below 1024 need root, and this daemon is a per-user agent.")
            }
        }
    }
}

/// The port `typed` into the field, or why it cannot be one.
pub fn port_choice(typed: &str) -> Result<u16, PortRefusal> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err(PortRefusal::Empty);
    }
    match typed.parse::<u16>() {
        Ok(port) if port >= 1024 => Ok(port),
        Ok(0) | Err(_) => Err(PortRefusal::NotAPort),
        Ok(_) => Err(PortRefusal::Privileged),
    }
}

/// The port line: the port the service holds, one configured and not held
/// (the service answers no port while somebody else has it, or while its
/// door is off), or none.
pub fn port_line(held: Option<u16>, config: &ApiConfig, language: Language) -> String {
    match (held, config.port) {
        (Some(port), _) => {
            language.format("http://127.0.0.1:{0} · listening", &[&port.to_string()])
        }
        (None, Some(configured)) => language.format(
            "{0} is configured and not held: the daemon is on the socket only",
            &[&configured.to_string()],
        ),
        (None, None) => language
            .text("no TCP port: the daemon is on the socket only")
            .to_string(),
    }
}

/// The browser origins the door lets in. None is the default, and a policy:
/// a browser's `Origin` is refused.
pub fn origins_line(origins: &[String], language: Language) -> String {
    if origins.is_empty() {
        return language
            .text("none — a browser Origin header is refused 403 (the default)")
            .to_string();
    }
    origins.join(", ")
}

/// The method the `curl` calls: the service's own facts (its version, its
/// port), which an `observe` token may read and which carry nothing said in
/// a session.
pub const CURL_METHOD: &str = "service.info";

/// A `curl` that calls the door with a token: `POST /v1/rpc`, not the
/// unauthenticated `GET /v1/info`, so it proves the token works.
pub fn curl_example(port: Option<u16>) -> String {
    let port = port.unwrap_or(47201);
    format!(
        "curl -sS -X POST http://127.0.0.1:{port}/v1/rpc \\\n  \
         -H 'Authorization: Bearer <token>' \\\n  \
         -H 'Content-Type: application/json' \\\n  \
         -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{CURL_METHOD}\",\"params\":{{}}}}'"
    )
}

/// A scope as the wire spells it (`observe:content`).
fn scope_word(scope: &Scope) -> Option<String> {
    serde_json::to_value(scope)
        .ok()
        .and_then(|word| word.as_str().map(str::to_string))
}

/// The scopes a token may be created with, as the wire spells them.
pub fn scope_words() -> Vec<String> {
    Scope::ALL.iter().filter_map(scope_word).collect()
}

/// Whether the scope the wire spells `word` is one of the [`ADVANCED`].
pub fn advanced(word: &str) -> bool {
    ADVANCED
        .iter()
        .any(|scope| scope_word(scope).is_some_and(|known| known == word))
}

/// A token's scopes, or that it has none: a token that can do nothing,
/// which is worth seeing as such.
pub fn token_scopes_line(token: &TokenRow, language: Language) -> String {
    if token.scopes.is_empty() {
        return language.text("no scopes").to_string();
    }
    token.scopes.join(", ")
}

/// When the token was made and when it stops working, at `now`, on this
/// Mac's clock as every page says a moment (`today, 09:12`,
/// `2026-11-02, 09:12`: [`present::day_and_time`]). A stamp that does not
/// read as the service writes one is printed as it came.
pub fn token_life_line(token: &TokenRow, now: Ts, language: Language) -> String {
    let when = |at: &str| {
        serde_json::from_value::<Ts>(Value::from(at)).map_or_else(
            |_| at.to_string(),
            |ts| present::day_and_time(ts, now, language),
        )
    };
    let created = token.created.as_deref().map_or_else(
        || language.text("created at an unrecorded time").to_string(),
        |at| language.format("created {0}", &[&when(at)]),
    );
    match token.expires.as_deref() {
        Some(at) => language.format("{0} · expires {1}", &[&created, &when(at)]),
        None => language.format("{0} · no expiry", &[&created]),
    }
}

/// What **Revoke** would break: the managed blocks the service says carry
/// the token's plaintext.
pub fn token_embedded_line(token: &TokenRow, language: Language) -> Option<String> {
    (!token.embedded_in.is_empty()).then(|| {
        language.format(
            "embedded in {0} — revoking it stops those hooks until Repair re-embeds a new one",
            &[&token.embedded_in.join(", ")],
        )
    })
}

/// What a created or rotated token is called in a toast. **It names the
/// token and never carries the plaintext.**
pub fn token_made_toast(verb: &str, name: &str, language: Language) -> String {
    language.format(
        "{0} the token “{1}”. Copy its value now: it is shown once.",
        &[language.text(verb), name],
    )
}

/// The new-token form's scope chips: the everyday ones, then the
/// [`ADVANCED`] ones, folded behind **Advanced** until it is pressed, and
/// shown at once while one of them is chosen (a choice is never hidden).
/// The press is kept under the form's id, the way a control keeps its focus,
/// so the page holds no state for it and finds the chips folded again the
/// next time it opens.
#[derive(IntoElement)]
struct ScopeChips {
    everyday: Vec<Chip>,
    advanced: Vec<Chip>,
    /// One of the advanced scopes is chosen.
    chosen: bool,
    /// **Advanced**, in the window's language.
    word: SharedString,
    disabled: bool,
}

impl RenderOnce for ScopeChips {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let opened = window.use_keyed_state("token-scopes", cx, |_, _| false);
        let shown = self.chosen || *opened.read(cx);
        h_flex()
            .flex_wrap()
            .gap(px(6.))
            .children(self.everyday)
            .when(shown, |chips| chips.children(self.advanced))
            .when(!shown, |chips| {
                chips.child(
                    Btn::new("token-scopes-advanced", self.word)
                        .quiet()
                        .icon(ic::CHEVRON_RIGHT)
                        .disabled(self.disabled)
                        .on_click(move |_, _, cx| {
                            opened.update(cx, |opened, cx| {
                                *opened = true;
                                cx.notify();
                            });
                        }),
                )
            })
    }
}

impl Shell {
    /// The page: its way back and head, the door (port, origins, a `curl`),
    /// the tokens, and the form for a new one.
    pub(crate) fn render_local_api(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let language = self.language();
        let header = ui::Header::new(
            self.tr(Screen::LocalApi.label()),
            self.tr("The HTTP port and the tokens local tools use."),
        )
        .back(
            "local-api-back",
            self.tr(Screen::Service.label()),
            cx.listener(|this, _, window, cx| this.go(Screen::Service, window, cx)),
        );
        let held = self.service.info.as_ref().and_then(|info: &Info| info.port);
        let config = self
            .service
            .api
            .as_ref()
            .map(|api| api.config.clone())
            .unwrap_or_default();
        let port = Row::new()
            .title(self.tr("Port"))
            .sub(port_line(held, &config, language))
            .right(
                div()
                    .w(px(PORT_FIELD))
                    .child(ui::field(&self.service.port, &p).disabled(dim).mono()),
            )
            .right(
                Btn::new("local-api-port", self.tr("Change Port"))
                    .disabled(dim)
                    .on_click(cx.listener(|this, _, window, cx| this.change_api_port(window, cx))),
            );
        let origins = Row::new()
            .title(self.tr("Allowed origins"))
            .sub(origins_line(&config.allowed_origins, language));
        let curl = curl_example(held.or(config.port));
        let try_it = v_flex()
            .child(
                Row::new()
                    .title(self.tr("Try it"))
                    .right(ui::CopyButton::new(
                        "local-api-copy",
                        curl.clone(),
                        self.tr("Copy Command"),
                        self.tr("Copied"),
                    )),
            )
            .child(
                div()
                    .px(px(16.))
                    .pb(px(14.))
                    .child(ui::code_block(curl, &p)),
            );
        let door = ui::card(
            vec![
                port.into_any_element(),
                origins.into_any_element(),
                try_it.into_any_element(),
            ],
            &p,
        );
        let tokens: Vec<AnyElement> = if self.service.tokens.is_empty() {
            vec![Row::new()
                .lead(ui::tile(ic::KEY, &p))
                .title(self.tr("No tokens yet"))
                .into_any_element()]
        } else {
            self.service
                .tokens
                .iter()
                .enumerate()
                .map(|(i, token)| {
                    self.local_api_token_row(i, token, dim, &p, cx)
                        .into_any_element()
                })
                .collect()
        };
        let body = v_flex()
            .gap(px(20.))
            .child(header)
            .child(door)
            .child(ui::section(self.tr("Tokens"), ui::card(tokens, &p), &p))
            .child(ui::section(
                self.tr("New token"),
                ui::card(
                    vec![self.local_api_token_form(dim, &p, cx).into_any_element()],
                    &p,
                ),
                &p,
            ));
        ui::page("local-api", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// One token: its name, its scopes and its life, what revoking it would
    /// break, and **Rotate** and **Revoke**.
    fn local_api_token_row(
        &self,
        i: usize,
        token: &TokenRow,
        dim: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> Row {
        let language = self.language();
        let mut about = vec![
            token_scopes_line(token, language),
            token_life_line(token, present::now(), language),
        ];
        about.extend(token.client.clone());
        let (rotate, revoke) = (token.name.clone(), token.name.clone());
        Row::new()
            .lead(ui::tile(ic::KEY, p))
            .title_el(div().mono().child(token.name.clone()))
            .sub_el(
                v_flex()
                    .gap(px(2.))
                    .child(ui::para(about.join(" · ")))
                    .children(
                        token_embedded_line(token, language)
                            .map(|line| div().text_color(p.warn_text).child(ui::para(line))),
                    ),
            )
            .right(
                Btn::new(("token-rotate", i), self.tr("Rotate"))
                    .quiet()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let action = UiAction::TokenRotate {
                            name: rotate.clone(),
                        };
                        this.api_token_call(action, &rotate, "Rotated", window, cx);
                    })),
            )
            .right(
                Btn::new(("token-revoke", i), self.tr("Revoke"))
                    .quiet()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let action = UiAction::TokenRevoke {
                            name: revoke.clone(),
                        };
                        this.api_token_call(action, &revoke, "Revoked", window, cx);
                    })),
            )
    }

    /// A new token: its name, its scopes (`observe` alone to start with, the
    /// one that gives a script nothing it could break; the [`ADVANCED`] ones
    /// behind **Advanced**), and **Create**.
    fn local_api_token_form(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let ready = !dim && !self.service.scopes.is_empty();
        let mut chips = ScopeChips {
            everyday: Vec::new(),
            advanced: Vec::new(),
            chosen: false,
            word: self.tr(ADVANCED_WORD).into(),
            disabled: dim,
        };
        for (i, scope) in scope_words().into_iter().enumerate() {
            let on = self.service.scopes.contains(&scope);
            let fold = advanced(&scope);
            let chip = Chip::word(("token-scope", i), scope.clone(), on)
                .disabled(dim)
                .on_toggle(cx.listener(move |this, on: &bool, _, cx| {
                    let scopes = &mut this.service.scopes;
                    scopes.retain(|s| s != &scope);
                    if *on {
                        scopes.push(scope.clone());
                    }
                    cx.notify();
                }));
            if fold {
                chips.chosen |= on;
                chips.advanced.push(chip);
            } else {
                chips.everyday.push(chip);
            }
        }
        v_flex()
            .gap(px(12.))
            .p(px(16.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(ui::field(&self.service.token_name, p).disabled(dim)),
                    )
                    .child(
                        Btn::new("token-create", self.tr("Create"))
                            .primary()
                            .icon(ic::KEY)
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create_api_token(window, cx);
                            })),
                    ),
            )
            .child(chips)
            .child(ui::hint(
                self.tr("A token is the whole of what a script can do (§4.2). Give it the least that works — observe:raw is raw hook payloads and is almost never it."),
                p,
            ))
    }

    /// The one-time plaintext, while there is one: what it is, the warning
    /// that this is the only time, the value, **Copy token** and **Done**.
    pub(crate) fn render_shown_token(&self, cx: &Context<Self>) -> Option<(AnyElement, f32)> {
        let p = *palette(cx);
        let shown = self.service.shown.as_ref()?;
        let body = v_flex()
            .p(px(20.))
            .gap(px(12.))
            .ts(13., 18.)
            .child(
                v_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .semibold()
                            .child(format!("{} \u{201c}{}\u{201d}", self.tr(shown.verb), shown.name)),
                    )
                    .child(div().text_color(p.text2).child(ui::para(self.tr(
                        "Copy this now. The daemon keeps only a hash of it, so this window is the only place it will ever exist — closing this dialog is the end of it.",
                    )))),
            )
            .child(ui::code_block(shown.reveal().to_string(), &p))
            .child(
                h_flex()
                    .mt(px(4.))
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        Btn::new("token-done", self.tr("Done — I have copied it")).on_click(
                            cx.listener(|this, _, window, cx| {
                                close(&mut this.service.shown);
                                this.close_dialog(window, cx);
                            }),
                        ),
                    )
                    .child(
                        Btn::new("token-copy", self.tr("Copy token"))
                            .primary()
                            .icon(ic::COPY)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(shown) = &this.service.shown {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        shown.reveal().to_string(),
                                    ));
                                }
                            })),
                    ),
            );
        Some((body.into_any_element(), TOKEN_WIDTH))
    }

    /// `token.list`. Asked when the page opens.
    pub(crate) fn read_api_tokens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.service.tokens_read = true;
        self.ask(
            UiAction::TokenList,
            window,
            cx,
            |this, result, _window, _cx| {
                if let Ok(value) = result {
                    this.service.tokens = TokenRow::list(&value);
                }
            },
        );
    }

    /// **Change Port**: a port the service can take, then the question (the
    /// service restarts to bind it), then the write and the restart.
    pub(crate) fn change_api_port(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.service.port.read(cx).value().to_string();
        let port = match port_choice(&typed) {
            Ok(port) => port,
            Err(refusal) => {
                if refusal == PortRefusal::Empty {
                    self.service
                        .port
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                self.toast(refusal.words(self.language()), window, cx);
                return;
            }
        };
        let sheet = Sheet {
            title: self.fmt("Change the port to {0}?", &[&port.to_string()]),
            body: self.tr("The service restarts.").into(),
            confirm: self.tr("Change Port").into(),
            danger: false,
        };
        self.ask_first(sheet, window, cx, move |this, window, cx| {
            this.ask(
                UiAction::ConfigSet {
                    key: "api.port".into(),
                    value: ConfigValue::Int(i64::from(port)),
                },
                window,
                cx,
                |this, result, window, cx| match result {
                    Ok(_) => {
                        this.service
                            .port
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.service_lifecycle(UiAction::ServiceRestart, window, cx);
                    }
                    Err(e) if method_missing(&e) => {
                        this.toast(
                            this.tr("This needs a newer background service."),
                            window,
                            cx,
                        );
                    }
                    Err(e) => this.toast(
                        this.fmt("Could not set the port: {0}", &[&e.to_string()]),
                        window,
                        cx,
                    ),
                },
            );
        });
    }

    /// **Create**: the name typed and the scopes picked.
    pub(crate) fn create_api_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.service.token_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.toast(self.tr("A token needs a name."), window, cx);
            return;
        }
        let scopes = self.service.scopes.clone();
        self.service
            .token_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        let action = UiAction::TokenCreate {
            name: name.clone(),
            scopes,
            expires: None,
        };
        self.api_token_call(action, &name, "Created", window, cx);
    }

    /// Every `token.*` call, in one place. The reply is read **once**, here,
    /// into a [`Secret`]; the toast is built from the token's name and never
    /// from the reply, so nothing that carries the value becomes an ordinary
    /// `String`.
    fn api_token_call(
        &mut self,
        action: UiAction,
        name: &str,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.to_string();
        self.ask(action, window, cx, move |this, result, window, cx| {
            let language = this.language();
            match result {
                Ok(value) => {
                    match Secret::from_reply(&value) {
                        Some(secret) => {
                            // A new plaintext replaces one not yet dismissed,
                            // which drops (and overwrites) the old one: there
                            // is never more than one in this process.
                            this.service.shown = Some(Shown::new(&name, verb, secret));
                            this.open_dialog(TOKEN_DIALOG, window);
                            this.toast(token_made_toast(verb, &name, language), window, cx);
                        }
                        // A revoke answers no value, and its toast must not
                        // promise one.
                        None => this.toast(
                            language.format("{0} the token “{1}”.", &[language.text(verb), &name]),
                            window,
                            cx,
                        ),
                    }
                    this.read_api_tokens(window, cx);
                }
                Err(e) => this.toast(
                    language.format(
                        "{0} “{1}” failed: {2}",
                        &[language.text(verb), &name, &e.to_string()],
                    ),
                    window,
                    cx,
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLAINTEXT: &str = "owt_live_9f3c1b7a55e04d2c";

    fn shown() -> Shown {
        let secret = Secret::from_reply(&json!({"token": PLAINTEXT})).expect("a created token");
        Shown::new("script", "Created", secret)
    }

    #[test]
    fn the_curl_asks_for_the_services_own_facts_and_never_for_a_session() {
        let curl = curl_example(Some(52_098));
        assert!(curl.contains("\"method\":\"service.info\""), "{curl}");
        assert!(!curl.contains("state.get"), "the roster is not an example");
        assert!(curl.contains("http://127.0.0.1:52098/v1/rpc"));
        // What the example sends is one JSON-RPC request the service reads.
        let body = curl
            .split("-d '")
            .nth(1)
            .and_then(|rest| rest.strip_suffix('\''))
            .expect("the request body");
        let request: serde_json::Value = serde_json::from_str(body).expect("JSON");
        assert_eq!(request["method"], CURL_METHOD);
        assert_eq!(request["params"], json!({}));
    }

    #[test]
    fn the_scopes_that_read_or_steer_a_session_are_folded_and_none_is_left_out() {
        let words = scope_words();
        assert_eq!(words.len(), Scope::ALL.len(), "every scope is still offered");
        let folded: Vec<&str> = words
            .iter()
            .filter(|word| advanced(word))
            .map(String::as_str)
            .collect();
        assert_eq!(folded, ["observe:content", "observe:raw", "operate"]);
        for everyday in ["observe", "hooks:ingest", "agent", "voice", "device", "admin"] {
            assert!(!advanced(everyday), "{everyday}");
        }
        assert!(!advanced("teleport"));
    }

    #[test]
    fn advanced_has_its_word_in_every_language() {
        crate::i18n::install();
        assert!(deck_gui_core::i18n::has_entry(ADVANCED_WORD));
        for language in [
            Language::SimplifiedChinese,
            Language::TraditionalChinese,
            Language::Japanese,
        ] {
            assert_ne!(language.text(ADVANCED_WORD), ADVANCED_WORD, "{language:?}");
        }
    }

    /// The plaintext is readable once and unreachable after it is
    /// dismissed; the one formatted rendering of it there is prints the name.
    #[test]
    fn a_created_tokens_plaintext_is_readable_once_and_unreachable_after_it_is_dismissed() {
        let mut slot = Some(shown());
        assert_eq!(slot.as_ref().map(Shown::reveal), Some(PLAINTEXT));
        let debug = format!("{slot:?}");
        assert!(!debug.contains(PLAINTEXT), "{debug}");
        assert!(debug.contains("script"), "the name is safe to print");
        assert!(close(&mut slot), "there was one to dismiss");
        assert!(slot.is_none());
        assert!(!close(&mut slot), "and only one");
        assert!(!format!("{slot:?}").contains(PLAINTEXT));
    }
}
