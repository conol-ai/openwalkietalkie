//! `UiState`: everything the deck shows and how controls change it, without
//! graphics or I/O. This file is the composition and dispatch skeleton; the
//! grammar lives in the sibling files.
//!
//! # Call order
//!
//! `apply(control, at_ms, out)`
//!   1. `global::on_control` sees EVERY control first (SHIFT, double-SHIFT,
//!      SHIFT+SCROLL, PTT session, release routing to the latched owner,
//!      swallowed orphans, guards, last-input time). `true` = consumed.
//!      While the tutorial is open it hands everything to `tutorial` instead.
//!   2. otherwise a `Touch` goes to `touch::on_touch` (edge gesture, the unified
//!      recogniser), which routes through `touch_hit` / `touch_deliver` below;
//!   3. otherwise a wheel detent or button PRESS goes to `dispatch`: the open
//!      overlay (`manager` for Manager/Gallery/Naming/ConfirmRemove, `notify`
//!      for the Drawer) captures it, else the current face's `on_control`.
//!      Global owns Release/SHIFT/PTT; Arcade receives local PTT presses.
//!   4. `global::after_control(.., consumed, ..)` (default owner, bookkeeping).
//! `host(msg, at_ms, out)`: `Deck::apply` validates and applies the message
//!   into `Effects`; a rejected message only reaches `global::on_reject`.
//!   Otherwise `on_host` of global, notify, agents, omarchy, macropad, usage,
//!   settings, timer, manager run in that order, each seeing `msg` and the effects.
//! `tick(now_ms, idle_ms, screen_on, out)`: `Deck::tick` (clock, heartbeat),
//!   then `on_tick` of global (slide, PTT cap, idle-home, host-lost teardown),
//!   touch (long-press), notify, agents, omarchy, macropad, usage, settings,
//!   timer, manager; then the demo script while `deck.demo_active()`.
//! Every entry point ends in `finish`: a dirty update bumps `revision`, which is
//! what the fallback signature regions hash, and `out.wake` is dropped while
//! `deck.demo_active()` ("demo notices never wake"): `notify::raise` sets
//! `wake` for every new notice and nobody else has to know about the demo.
//!
//! # Helper API the sibling files may call
//!
//! here:     `dispatch`, `owner_released`, `touch_hit`, `touch_deliver`,
//!           `UiState::{current_face, face_at, index_of_face, visible_faces,
//!           strip_at_rest, still_default, invalidate}`
//! global:   `hold`, `held`, `unhold`, `release_source`, `anything_held`,
//!           `release_owned`, `go_home`, `jump_to`, `set_overlay`, `arm_guard`,
//!           `guard_passed`, `emit_channel`, `emit_face`, `quiet_for`
//! touch:    `orphan`, `forget`, `Recognizer::{active, down_ms}`
//! notify:   `raise`, `remove`, `open_drawer`, `close_drawer`
//! agents:   `show_roster`, `show_card`, `ptt_target`
//! manager:  `open`, `close`, `cancel_all`
//!
//! # Ownership of presses
//!
//! A handler that tells the host about a press (`Event::Key` / `Event::Macro`
//! with act 1) calls `global::hold(ui, source, owner)`. Global
//! emits the matching release from the latched `Owner` when the physical
//! release arrives or at any teardown (`release_owned`), then calls
//! `owner_released` so the owner's view can drop its pressed look. Handlers
//! never emit act 0 themselves.
//!
//! # Touch
//!
//! `hit` functions are pure: they name the `Target` under a point. The
//! recogniser latches it on Down and afterwards delivers `TouchAct`s to the
//! same layer whatever is on screen by then. Every latched touch ends with
//! exactly one of `Activate`, `Swipe` or `Cancel`.
use crate::battery::BatteryState;
use crate::control::{Control, Wheel};
use crate::deck::demo;
use crate::deck::model::{Deck, Effects};
use crate::deck::msg::{ErrWhy, Event, FaceEntry, HostMsg, KeyId, Template};

use super::layout::Rect;

pub mod agents;
pub mod arcade;
pub mod global;
pub mod macropad;
pub mod manager;
pub mod notify;
pub mod omarchy;
pub mod settings;
pub mod timer;
pub mod touch;
pub mod tutorial;
pub mod usage;

// Not every consumer names every re-export.
#[allow(unused_imports)]
pub use agents::AgentsView;
#[allow(unused_imports)]
pub use global::{Overlay, PttSession};
#[allow(unused_imports)]
pub use notify::{Toast, ToastKind};

/// Width of one face on the strip, and the fixed-point scale of `strip_x`.
pub const FACE_W: i32 = 480;
pub const STRIP_ONE: i32 = 256;

/// Wheel navigation through pages, menu items and list content.
/// Keep physical rotation separate from value adjustment and game controls.
pub(super) fn selection_step(wheel: Wheel) -> i32 {
    if wheel == Wheel::Ccw { 1 } else { -1 }
}
pub const MAX_FACES: usize = 8;
/// Sessions this board's firmware holds: 255 in PSRAM on Rev A and Rev B,
/// the old 8 in SRAM elsewhere (the LilyGo dev board, the native image).
pub const SESSIONS: usize = if cfg!(feature = "hw-a") { crate::deck::model::MAX_AGENTS } else { crate::deck::model::DEFAULT_SESSIONS };

/// Who a held press belongs to; the latched identity its release must repeat.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    /// Handled on the device; the release means nothing to anyone.
    Local,
    /// Local game input; teardown clears held movement.
    Arcade(crate::control::Button),
    /// Forwarded as `Event::Key` from a host-rendered face.
    HostKey { face: u8, key: KeyId, shift: bool },
    /// A macropad key, pressed by its hardware binding or by touch.
    Macro { face: u8, index: u8 },
}

/// What is physically down: one of the six buttons, or the finger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Button(crate::control::Button),
    Touch,
}

/// The handler file that owns a latched touch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// Toast and drawer (`notify.rs`).
    Notify,
    /// Manager, gallery, naming, confirm (`manager.rs`).
    Manager,
    /// A face of the strip, by template and instance id.
    Face(Template, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetKind {
    /// Highlight on Down, activate on Up inside; > 10 px off the rect cancels.
    Tap,
    /// A `Tap` row of a list: a vertical drag >= 16 px scrolls instead, one
    /// `Scroll` step per `Target::pitch` px of travel (the first at 16 px).
    ListRow,
    /// Pressed on Down, released on Up / leave / Cancel (macro keys).
    Press,
    /// Manager card: tap, quick horizontal drag, or long-press then drag.
    /// Never cancelled by sliding off: the owner watches `Move`. After a
    /// `LongPress` the final Up is `Activate` (= drop) wherever it lands;
    /// without one, an Up outside the rect is `Cancel`.
    Drag,
}

/// A touchable thing: where it is and what it is, in its owner's terms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target {
    pub rect: Rect,
    /// Owner-defined identity (agent id, choice, key index, notice…), NOT a
    /// row number that a host push could shift under the finger.
    pub id: u16,
    pub kind: TargetKind,
    /// `ListRow`: px of finger travel per scrolled row = the distance between
    /// row tops (`layout::*_PITCH`), which is more than `rect.h`. 0 otherwise.
    pub pitch: u8,
}

impl Target {
    pub const fn tap(rect: Rect, id: u16) -> Self {
        Self { rect, id, kind: TargetKind::Tap, pitch: 0 }
    }
    pub const fn list_row(rect: Rect, id: u16, pitch: u8) -> Self {
        Self { rect, id, kind: TargetKind::ListRow, pitch }
    }
    pub const fn press(rect: Rect, id: u16) -> Self {
        Self { rect, id, kind: TargetKind::Press, pitch: 0 }
    }
    pub const fn drag(rect: Rect, id: u16) -> Self {
        Self { rect, id, kind: TargetKind::Drag, pitch: 0 }
    }
}

/// What the recogniser tells the layer that owns the latched target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TouchAct {
    /// Just latched: highlight it (`Press` targets press now).
    Down,
    /// Still latched and not cancelled; (dx, dy) since Down.
    Move { x: i32, y: i32, dx: i32, dy: i32 },
    /// `ListRow`: scroll by whole rows (+ = later rows); the tap is void.
    Scroll(i8),
    /// `Drag`: held 350 ms within 10 px (promoted by `tick`).
    LongPress,
    /// Up inside the target, never cancelled: do it.
    Activate,
    /// Up after a horizontal swipe (|dx| >= 60, |dx| > 2|dy|).
    Swipe { dx: i32, dy: i32 },
    /// Slid off, `TouchPhase::Cancel`, teardown, or an Up that activates
    /// nothing: undo the highlight / release the press.
    Cancel,
}

/// What one `apply` / `host` / `tick` produced. Small: pass it as `&mut`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Update {
    pub dirty: bool,
    /// Channel 5, for the deck host; gated on `deck.host_present` (or demo).
    pub deck: [Option<Event>; 8],
    /// An event did not fit: the runtime sends `up` and drops the link.
    pub overflow: bool,
    /// A new notice deserves a lit screen (`Policy::notify`). Never set while
    /// `deck.demo_active()`: `UiState` clears it at the end of every entry
    /// point, so the runtimes act on it unconditionally.
    pub wake: bool,
    /// CANCEL was held for `global::POWER_HOLD_MS`: the runtime powers the
    /// deck off (`UiState::power_down` first, then the hardware). Set once
    /// per hold.
    pub power_off: bool,
    /// Settings > About > Factory reset was confirmed: the runtime erases the
    /// deck's storage (pairing, preferences) and restarts.
    pub factory_reset: bool,
    /// The tutorial was finished or skipped: the runtime remembers it.
    pub tutorial_done: bool,
    /// A device-local language change for the runtime's preference store.
    pub language: Option<super::i18n::Language>,
}

impl Update {
    pub const fn new() -> Self {
        Self {
            dirty: false,
            deck: [None; 8],
            overflow: false,
            wake: false,
            power_off: false,
            factory_reset: false,
            tutorial_done: false,
            language: None,
        }
    }
    pub fn clear(&mut self) {
        *self = Self::new();
    }
    pub fn push_deck(&mut self, event: Event) {
        match self.deck.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(event),
            None => self.overflow = true,
        }
    }
    pub fn deck_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.deck.iter().flatten().copied()
    }
}

/// Power-on strip; template IDs remain stable when new faces are inserted.
pub const DEFAULT_STRIP: [Template; 7] = [
    Template::Agents,
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::System,
    Template::Arcade,
    Template::Settings,
];

/// One face of the strip that is (partly) on the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Visible {
    /// `None`: beyond an end of the strip (the bounce shows canvas there).
    pub face: Option<FaceEntry>,
    /// Panel x of the face's logical x 0.
    pub origin_x: i32,
}

/// Not `Copy`: the deck's session table is storage its owner attaches
/// (`deck.agents`); [`UiState::copy_into`] copies its contents.
pub struct UiState {
    pub language: super::i18n::Language,
    pub language_revision: u32,
    /// A versioned preference handshake proves this host understands it.
    pub language_sync_supported: bool,
    pub image: crate::ui::image::View,
    pub battery: BatteryState,
    /// HID link.
    pub connected: bool,
    /// BLE quality only: a USB connection must not light the Bluetooth bars.
    pub bluetooth: super::bluetooth::Status,
    /// The only clock `draw` may read.
    pub now_ms: u32,
    /// Bumped by every dirty update; the fallback signature hash.
    pub revision: u32,
    /// Read-only mirror of the power policy: completed unlock wheel ticks.
    pub screen_lock: Option<u8>,
    pub deck: Deck,
    pub global: global::Global,
    pub touch: touch::Recognizer,
    pub notify: notify::Notify,
    pub agents: agents::View,
    pub omarchy: omarchy::View,
    pub macropad: macropad::View,
    pub usage: usage::View,
    pub arcade: arcade::View,
    pub settings: settings::View,
    pub timer: timer::View,
    pub manager: manager::View,
    pub tutorial: tutorial::View,
    pub demo: demo::Script,
}

/// Host builds (tests, the preview): a clone has a table of its own.
#[cfg(not(target_os = "none"))]
impl Clone for UiState {
    fn clone(&self) -> Self {
        let mut out = UiState::new();
        self.copy_into(&mut out);
        out
    }
}

impl UiState {
    /// For statics. An empty strip and no data: call `reset` before use.
    pub const fn new() -> Self {
        Self {
            language: super::i18n::Language::English,
            language_revision: 0,
            language_sync_supported: false,
            image: crate::ui::image::View::new(),
            battery: BatteryState::UNKNOWN,
            connected: false,
            bluetooth: super::bluetooth::Status::Disconnected,
            now_ms: 0,
            revision: 0,
            screen_lock: None,
            deck: Deck::new(),
            global: global::Global::new(),
            touch: touch::Recognizer::new(),
            notify: notify::Notify::new(),
            agents: agents::View::new(),
            omarchy: omarchy::View::new(),
            macropad: macropad::View::new(),
            usage: usage::View::new(),
            arcade: arcade::View::new(),
            settings: settings::View::new(),
            timer: timer::View::new(),
            manager: manager::View::new(),
            tutorial: tutorial::View::new(),
            demo: demo::Script::new(),
        }
    }

    /// Power-on state, in place: the default strip on the Agents face, default
    /// settings, and the demo data those defaults ask for.
    pub fn reset(&mut self) {
        self.ensure_sessions();
        self.language = super::i18n::Language::English;
        self.language_revision = 0;
        self.language_sync_supported = false;
        self.image.open = false;
        self.battery = BatteryState::UNKNOWN;
        self.connected = false;
        self.bluetooth = super::bluetooth::Status::Disconnected;
        self.now_ms = 0;
        self.revision = self.revision.wrapping_add(1);
        self.screen_lock = None;
        self.deck.reset();
        self.global = global::Global::new();
        self.touch = touch::Recognizer::new();
        self.notify = notify::Notify::new();
        self.agents = agents::View::new();
        self.omarchy = omarchy::View::new();
        self.macropad = macropad::View::new();
        self.usage = usage::View::new();
        self.arcade = arcade::View::new();
        self.settings = settings::View::new();
        self.timer = timer::View::new();
        self.manager = manager::View::new();
        self.tutorial = tutorial::View::new();
        self.demo = demo::Script::new();
        for (index, template) in DEFAULT_STRIP.into_iter().enumerate() {
            let entry = &mut self.global.strip[index];
            entry.id = template.default_id();
            entry.template = template;
            entry.name.set(template.label());
        }
        self.global.strip_len = DEFAULT_STRIP.len() as u8;
        if self.deck.settings.demo {
            demo::load(&mut self.deck);
        }
        agents::on_reset(self);
    }

    /// Open the tutorial: at power-on by the runtime of a deck that never
    /// finished it (`replay` false), or from Settings (`replay` true).
    pub fn start_tutorial(&mut self, replay: bool) {
        let mut out = Update::new();
        let now = self.now_ms;
        tutorial::open(self, replay, now, &mut out);
        self.finish(&mut out);
    }

    /// Turn the demo on and load it, as Settings > Demo data does without a
    /// host. The demo is off at power-on; the preview and tests start here.
    pub fn load_demo(&mut self) {
        self.ensure_sessions();
        self.deck.settings.demo = true;
        demo::load(&mut self.deck);
        agents::on_reset(self);
    }

    /// Translate an explicitly marked built-in interface string.
    pub fn tr<'a>(&self, english: &'a str) -> &'a str {
        self.language.text(english)
    }

    /// Default face names follow the language; custom names stay as supplied.
    pub fn face_name<'a>(&self, face: &'a crate::deck::msg::FaceEntry) -> &'a str {
        if face.name.is_empty() || face.name == face.template.label() {
            self.tr(face.template.label())
        } else {
            face.name.as_str()
        }
    }

    /// Snapshot for the renderer, `static` to `static`, without a temporary.
    pub fn copy_into(&self, dst: &mut UiState) {
        dst.ensure_sessions();
        // Everything but the session table is plain data: copy it bitwise,
        // then give `dst` its own table back and copy the sessions into it.
        let table = core::mem::replace(&mut dst.deck.agents, crate::deck::model::Agents::empty());
        // SAFETY: both are valid and distinct (`&` vs `&mut`); the only field
        // that is not plain data, the table, is taken out of `dst` above and
        // its bitwise copy of `self`'s is forgotten, never dropped, below.
        unsafe { core::ptr::copy_nonoverlapping(self, dst, 1) };
        core::mem::forget(core::mem::replace(&mut dst.deck.agents, table));
        let n = self.deck.agents.len().min(dst.deck.agents.len());
        dst.deck.agents[..n].copy_from_slice(&self.deck.agents[..n]);
    }

    /// Host builds (tests, the preview) get a heap table when nobody
    /// attached one: as many sessions as this board's firmware holds.
    pub fn ensure_sessions(&mut self) {
        #[cfg(not(target_os = "none"))]
        if self.deck.agents.is_empty() {
            self.deck.agents.allocate(SESSIONS);
        }
    }

    /// Synchronize the lock overlay even when its input was not dispatched.
    pub fn set_screen_lock(&mut self, progress: Option<u8>) -> bool {
        if self.screen_lock == progress {
            return false;
        }
        self.screen_lock = progress;
        self.invalidate();
        true
    }

    pub fn apply(&mut self, control: Control, at_ms: u32, out: &mut Update) {
        self.advance(at_ms);
        // The firmware hushes before it decides whether to deliver (a
        // waking press is not); here for every other driver of the state.
        timer::hush(self, control);
        let mut consumed = global::on_control(self, control, at_ms, out);
        if !consumed {
            consumed = match control {
                Control::Touch(touch) => {
                    touch::on_touch(self, touch, at_ms, out);
                    true
                }
                _ => dispatch(self, control, at_ms, out),
            };
        }
        global::after_control(self, control, consumed, at_ms, out);
        self.finish(out);
    }

    /// Never emits the mirror event of what it applied.
    pub fn host(&mut self, msg: &HostMsg, at_ms: u32, out: &mut Update) {
        self.advance(at_ms);
        if let HostMsg::Language { value, revision } = msg {
            if *value <= 3 {
                self.language_sync_supported = true;
                if *revision >= self.language_revision && (*revision != self.language_revision || *value != self.language as u8) {
                    self.language = super::i18n::Language::from_id(*value);
                    self.language_revision = *revision;
                    out.language = Some(self.language);
                    out.dirty = true;
                }
            }
            self.finish(out);
            return;
        }
        if matches!(msg, HostMsg::Hello { .. }) && !self.deck.host_present {
            self.language_sync_supported = false;
        }
        if let HostMsg::Transcript { press: Some(press), .. } = msg {
            if self.global.ptt.or(self.global.ptt_pending).is_none_or(|p| p.press != *press) {
                return;
            }
        }
        let mut fx = Effects::new();
        self.deck.apply(msg, at_ms, &mut fx);
        if fx.new_session { self.language_sync_supported = false; }
        if let Some(why) = fx.reject {
            global::on_reject(self, why, at_ms, out);
        } else {
            out.dirty |= fx.dirty;
            global::on_host(self, msg, &fx, at_ms, out);
            notify::on_host(self, msg, &fx, at_ms, out);
            agents::on_host(self, msg, &fx, at_ms, out);
            omarchy::on_host(self, msg, &fx, at_ms, out);
            macropad::on_host(self, msg, &fx, at_ms, out);
            usage::on_host(self, msg, &fx, at_ms, out);
            settings::on_host(self, msg, &fx, at_ms, out);
            timer::on_host(self, msg, &fx, at_ms, out);
            manager::on_host(self, msg, &fx, at_ms, out);
        }
        self.finish(out);
    }

    /// A line the runtime could not turn into a `HostMsg` (`ErrWhy::Size` from
    /// the line assembler, `ErrWhy::Parse`/`Range`/`Unknown` from
    /// `proto::parse`): reported through the same 1/s `err` limiter as a
    /// message the model rejects, so the runtime keeps no limiter of its own.
    pub fn reject(&mut self, why: ErrWhy, at_ms: u32, out: &mut Update) {
        self.advance(at_ms);
        global::on_reject(self, why, at_ms, out);
        self.finish(out);
    }

    /// Timers, animation, demo script, idle-home. `idle_ms` is the power
    /// policy's single idle clock (0 while an unlocked control is held).
    pub fn tick(&mut self, now_ms: u32, idle_ms: u32, screen_on: bool, out: &mut Update) {
        self.advance(now_ms);
        out.dirty |= self.deck.tick(now_ms);
        global::on_tick(self, now_ms, idle_ms, screen_on, out);
        touch::on_tick(self, now_ms, out);
        notify::on_tick(self, now_ms, out);
        agents::on_tick(self, now_ms, out);
        omarchy::on_tick(self, now_ms, out);
        macropad::on_tick(self, now_ms, out);
        usage::on_tick(self, now_ms, out);
        if !self.deck.system_stale && self.deck.system_at.is_some_and(|at| now_ms.wrapping_sub(at) >= 10_000) {
            self.deck.system_stale = true;
            out.dirty |= self.current_face().template == Template::System;
        }
        arcade::on_tick(self, now_ms, screen_on, out);
        settings::on_tick(self, now_ms, out);
        timer::on_tick(self, now_ms, out);
        manager::on_tick(self, now_ms, out);
        tutorial::on_tick(self, now_ms, out);
        if self.deck.demo_active() {
            demo::on_tick(self, now_ms, screen_on, out);
        }
        self.finish(out);
    }

    /// Something is moving: poll at the animation rate. The caller ANDs this
    /// with screen != Off.
    pub fn animating(&self) -> bool {
        (self.deck.demo_active() && demo::animating(self))
            || global::animating(self)
            || touch::animating(self)
            || notify::animating(self)
            || agents::animating(self)
            || omarchy::animating(self)
            || macropad::animating(self)
            || usage::animating(self)
            || arcade::playing(self)
            || settings::animating(self)
            || timer::animating(self)
            || manager::animating(self)
            || tutorial::animating(self)
    }

    /// A link drop clears every hold WITHOUT emitting, and the deck goes
    /// stale/offline.
    pub fn set_connected(&mut self, connected: bool, out: &mut Update) {
        if !connected { self.language_sync_supported = false; }
        out.dirty |= self.connected != connected;
        self.connected = connected;
        global::on_link(self, connected, out);
        self.finish(out);
    }

    /// The power-off hold dialog's bar, 0..=100, while CANCEL has been held
    /// past `global::POWER_HOLD_SHOW_MS`; `None` = no dialog.
    pub fn power_hold(&self) -> Option<u8> {
        global::power_hold(self)
    }

    /// Power-off teardown, before the runtime cuts the hardware: every
    /// host-visible hold is released, a PTT session aborted, and the deck
    /// host told `up` (the runtime gates it on a present host).
    pub fn power_down(&mut self, out: &mut Update) {
        global::power_down(self, out);
        self.finish(out);
    }

    /// The first complete frame after a wake is on the glass: timers that the
    /// user must see in full (the toast) start now.
    pub fn frame_shown(&mut self, now_ms: u32) {
        notify::on_frame_shown(self, now_ms);
    }

    /// Strip and settings are still the power-on defaults (`hello.boot`): the
    /// host may replay what it persisted.
    pub fn still_default(&self) -> bool {
        self.deck.settings == crate::deck::model::Settings::new()
            && self.global.strip_len as usize == DEFAULT_STRIP.len()
            && DEFAULT_STRIP.iter().enumerate().all(|(index, template)| {
                let entry = &self.global.strip[index];
                entry.id == template.default_id()
                    && entry.template == *template
                    && entry.name == template.label()
            })
    }

    /// 1..=5.
    pub fn user_brightness(&self) -> u8 {
        self.deck.settings.brightness.clamp(1, 5)
    }

    pub fn current_face(&self) -> FaceEntry {
        self.face_at(self.global.current)
            .unwrap_or(FaceEntry::new(1, Template::Agents, "Agents"))
    }

    pub fn face_at(&self, index: u8) -> Option<FaceEntry> {
        let len = (self.global.strip_len as usize).min(MAX_FACES);
        self.global.strip[..len].get(index as usize).copied()
    }

    /// Strip index of the face with this id.
    pub fn index_of_face(&self, id: u8) -> Option<u8> {
        let len = (self.global.strip_len as usize).min(MAX_FACES);
        self.global.strip[..len]
            .iter()
            .position(|entry| entry.id == id)
            .map(|index| index as u8)
    }

    /// Test/preview helper: jump-cut to the first face of this template with
    /// every overlay closed. No teardown, no events.
    pub fn on_face(&mut self, template: Template) {
        let len = (self.global.strip_len as usize).min(MAX_FACES);
        if let Some(index) = self.global.strip[..len]
            .iter()
            .position(|entry| entry.template == template)
        {
            self.global.current = index as u8;
            self.global.strip_x = index as i32 * FACE_W * STRIP_ONE;
        }
        self.global.overlay = Overlay::None;
        self.invalidate();
    }

    /// After changing pub fields directly (tests, previews): make the next
    /// signature differ.
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The one or two faces on the panel for the current `strip_x`.
    pub fn visible_faces(&self) -> [Option<Visible>; 2] {
        let px = self.global.strip_x >> 8;
        let first = px.div_euclid(FACE_W);
        let origin_x = first * FACE_W - px;
        let face = |index: i32| {
            u8::try_from(index)
                .ok()
                .and_then(|index| self.face_at(index))
        };
        [
            Some(Visible {
                face: face(first),
                origin_x,
            }),
            (origin_x != 0).then(|| Visible {
                face: face(first + 1),
                origin_x: origin_x + FACE_W,
            }),
        ]
    }

    /// The strip is at rest on `current`.
    pub fn strip_at_rest(&self) -> bool {
        self.global.strip_x == self.global.current as i32 * FACE_W * STRIP_ONE
    }

    /// True when a redraw is due.
    pub fn update_battery(&mut self, battery: BatteryState) -> bool {
        let changed = self.battery.percentage() != battery.percentage()
            || self.battery.charge != battery.charge;
        self.battery = battery;
        if changed {
            self.invalidate();
        }
        changed
    }

    /// A signal update redraws only the indicator and never wakes the panel.
    /// Its own damage signature tracks changes without bumping `revision`.
    pub fn update_bluetooth(&mut self, status: super::bluetooth::Status) -> bool {
        let changed = self.bluetooth != status;
        self.bluetooth = status;
        changed
    }

    /// Demo loopback for the runtimes: while `deck.demo_active()`, deck events
    /// go to the built-in host instead of the wire, and its answers are
    /// applied. `scratch` is the runtime's parse buffer.
    pub fn demo_loopback(&mut self, now_ms: u32, out: &mut Update, scratch: &mut HostMsg) {
        if !self.deck.demo_active() {
            return;
        }
        let events = out.deck;
        out.deck = [None; 8];
        for event in events.into_iter().flatten() {
            if demo::host(event, self, now_ms, scratch) {
                self.host(scratch, now_ms, out);
            }
        }
    }

    fn advance(&mut self, at_ms: u32) {
        // Queued controls may carry stamps older than the last tick.
        if at_ms.wrapping_sub(self.now_ms) < u32::MAX / 2 {
            self.now_ms = at_ms;
        }
    }

    fn finish(&mut self, out: &mut Update) {
        if !self.deck.host_present { self.language_sync_supported = false; }
        if out.dirty {
            self.invalidate();
        }
        // Demo notices never wake the screen; a real hello ends the demo
        // before its first notice can arrive.
        if self.deck.demo_active() {
            out.wake = false;
        }
    }
}

/// A wheel detent or button press for whatever is on top: the open overlay
/// captures it, else the current face gets it. `true` = consumed.
pub(super) fn dispatch(ui: &mut UiState, control: Control, at_ms: u32, out: &mut Update) -> bool {
    match ui.global.overlay {
        Overlay::Naming | Overlay::ConfirmRemove | Overlay::Manager | Overlay::Gallery => {
            manager::on_control(ui, control, at_ms, out)
        }
        Overlay::Drawer => notify::on_control(ui, control, at_ms, out),
        Overlay::ConfirmReset => settings::on_reset_control(ui, control, at_ms, out),
        Overlay::MacroEdit => macropad::on_edit_control(ui, control, at_ms, out),
        // Global handed the tutorial every control already.
        Overlay::Tutorial => true,
        Overlay::None => {
            let face = ui.current_face();
            match face.template {
                Template::Agents => agents::on_control(ui, face.id, control, at_ms, out),
                Template::Omarchy => omarchy::on_control(ui, face.id, control, at_ms, out),
                Template::Macropad => macropad::on_control(ui, face.id, control, at_ms, out),
                Template::System => false,
                Template::Usage => usage::on_control(ui, face.id, control, at_ms, out),
                Template::Arcade => arcade::on_control(ui, control, at_ms, out),
                Template::Settings => settings::on_control(ui, face.id, control, at_ms, out),
                Template::Timer => timer::on_control(ui, face.id, control, at_ms, out),
            }
        }
    }
}

/// Global released `owner` (physical release or teardown) and has already
/// emitted its act 0: let the owner's view drop the pressed look.
pub(super) fn owner_released(ui: &mut UiState, owner: Owner, out: &mut Update) {
    match owner {
        Owner::Macro { face, index } => macropad::on_owner_released(ui, face, index, out),
        Owner::HostKey { face, key, .. } => omarchy::on_owner_released(ui, face, key, out),
        Owner::Arcade(button) => arcade::released(ui, button),
        Owner::Local => {}
    }
}

/// What a Down at (x, y) would latch, by overlay precedence. The edge zone and
/// the toast are the recogniser's business before it asks.
pub(super) fn touch_hit(ui: &UiState, x: i32, y: i32) -> Option<(Layer, Target)> {
    match ui.global.overlay {
        Overlay::Naming | Overlay::ConfirmRemove | Overlay::Manager | Overlay::Gallery => {
            manager::hit(ui, x, y).map(|target| (Layer::Manager, target))
        }
        Overlay::Drawer => notify::hit(ui, x, y).map(|target| (Layer::Notify, target)),
        // The dialog's buttons belong to the Settings face beneath it.
        Overlay::ConfirmReset => settings::reset_hit(ui, x, y)
            .map(|target| (Layer::Face(Template::Settings, ui.current_face().id), target)),
        // The editor's rows belong to the Macropad instance being edited.
        Overlay::MacroEdit => macropad::edit_hit(ui, x, y)
            .map(|target| (Layer::Face(Template::Macropad, ui.macropad.edit.face), target)),
        Overlay::Tutorial => None,
        Overlay::None => {
            let face = ui.current_face();
            match face.template {
                Template::Agents => agents::hit(ui, face.id, x, y),
                Template::Omarchy => omarchy::hit(ui, face.id, x, y),
                Template::Macropad => macropad::hit(ui, face.id, x, y),
                Template::System => None,
                Template::Usage => usage::hit(ui, face.id, x, y),
                Template::Arcade => arcade::hit(ui, x, y),
                Template::Settings => settings::hit(ui, face.id, x, y),
                Template::Timer => timer::hit(ui, face.id, x, y),
            }
            .map(|target| (Layer::Face(face.template, face.id), target))
        }
    }
}

/// Deliver one act to the layer that latched `target`, even if that layer is
/// no longer on top (it still has to see its Cancel).
pub(super) fn touch_deliver(
    ui: &mut UiState,
    layer: Layer,
    target: Target,
    act: TouchAct,
    at_ms: u32,
    out: &mut Update,
) {
    match layer {
        Layer::Notify => notify::on_touch(ui, target, act, at_ms, out),
        Layer::Manager => manager::on_touch(ui, target, act, at_ms, out),
        Layer::Face(template, id) => match template {
            Template::Agents => agents::on_touch(ui, id, target, act, at_ms, out),
            Template::Omarchy => omarchy::on_touch(ui, id, target, act, at_ms, out),
            Template::Macropad => macropad::on_touch(ui, id, target, act, at_ms, out),
            Template::System => {},
            Template::Usage => usage::on_touch(ui, id, target, act, at_ms, out),
            Template::Arcade => arcade::on_touch(ui, target, act, at_ms, out),
            Template::Settings => settings::on_touch(ui, id, target, act, at_ms, out),
            Template::Timer => timer::on_touch(ui, id, target, act, at_ms, out),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Button, Edge};
    use crate::deck::msg::CardKind;

    fn fresh() -> std::boxed::Box<UiState> {
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        ui.load_demo();
        ui
    }


    #[test]
    fn state_and_update_stay_within_their_ram_budget() {
        // Includes the 16.5 KiB indexed preview buffer. Never retain a full
        // 480×480 image framebuffer alongside the chat windows.
        assert!(core::mem::size_of::<UiState>() <= 64 * 1024);
        assert!(core::mem::size_of::<Update>() <= 160);
    }

    #[test]
    fn runtime_and_model_rejections_share_one_err_per_second() {
        let mut ui = fresh();
        let errs = |out: &Update| {
            out.deck_events()
                .filter(|event| matches!(event, Event::Err { .. }))
                .count()
        };
        let mut out = Update::new();
        ui.reject(ErrWhy::Size, 5000, &mut out);
        assert_eq!(out.deck_events().next(), Some(Event::Err { why: ErrWhy::Size }));
        ui.reject(ErrWhy::Parse, 5400, &mut out);
        // A card for an agent that does not exist: the model says Range.
        let bad = HostMsg::Reply { agent: 8, text: crate::deck::text::Text::new(), rev: 0 };
        ui.host(&bad, 5900, &mut out);
        assert_eq!(errs(&out), 1);
        ui.host(&bad, 6000, &mut out);
        assert_eq!(errs(&out), 2);
    }

    #[test]
    fn a_message_toast_is_brief_and_leaves_no_notice() {
        let mut ui = fresh();
        ui.deck.clear_host_data();
        // A real host: `raise` wakes for it; the demo never wakes.
        ui.deck.host_present = true;
        let mut out = Update::new();
        notify::raise(&mut ui, crate::deck::Notice::new(2, 7, CardKind::Ready, 100), 100, &mut out);
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((2, 7)));
        assert!(out.wake);
        // "Not delivered" displaces the card toast, whose notice is filed.
        let mut out = Update::new();
        notify::raise_message(&mut ui, ToastKind::NotDelivered, 1, 1, 500, &mut out);
        assert!(out.dirty && !out.wake);
        let toast = ui.notify.toast.unwrap();
        assert_eq!((toast.kind, toast.agent, toast.card), (ToastKind::NotDelivered, 1, 1));
        assert_eq!(toast.kind.message(), Some("Not delivered"));
        assert_eq!(toast.notice(), None);
        assert_eq!(ui.deck.notice_count(), 1);
        // A card kind is not a message.
        notify::raise_message(&mut ui, ToastKind::Card(CardKind::Ready), 1, 1, 600, &mut out);
        assert_eq!(ui.notify.toast, Some(toast));
        let mut out = Update::new();
        ui.tick(500 + notify::MESSAGE_MS - 1, 0, true, &mut out);
        assert!(ui.notify.toast.is_some());
        ui.tick(500 + notify::MESSAGE_MS, 0, true, &mut out);
        assert!(ui.notify.toast.is_none() && out.dirty);
        assert_eq!(ui.deck.notice_count(), 1);
    }

    fn card(agent: u8, id: u16) -> HostMsg {
        let mut text = crate::deck::text::Text::new();
        text.set("Ship it?");
        HostMsg::Card {
            agent,
            id,
            clear: false,
            kind: CardKind::Question,
            text,
            choices: [crate::deck::text::Text::new(); 4],
            choice_count: 0,
            recommended: None,
        }
    }

    #[test]
    fn a_new_card_wakes_the_screen_for_a_real_host_but_never_in_the_demo() {
        let mut ui = fresh();
        assert!(ui.deck.demo_active());
        let mut out = Update::new();
        ui.host(&card(3, 40), 1000, &mut out);
        assert!(out.dirty && !out.wake);
        assert_eq!(ui.notify.toast.and_then(|t| t.notice()), Some((3, 40)));
        // Even a stale flag in a reused `Update` does not survive the demo.
        out.wake = true;
        ui.tick(1001, 0, true, &mut out);
        assert!(!out.wake);

        let mut out = Update::new();
        ui.host(&HostMsg::Hello { v: 1, sid: 9, minute_of_day: None }, 2000, &mut out);
        let agent = HostMsg::Agent {
            id: 3,
            gone: false,
            kind: None,
            state: None,
            name: None,
            project: None,
            title: None,
            since_s: None, task: None, subs: None,
        };
        ui.host(&agent, 2001, &mut out);
        assert!(!ui.deck.demo_active() && !out.wake);
        ui.host(&card(3, 41), 2002, &mut out);
        assert!(out.wake);
    }

    #[test]
    fn reset_builds_the_default_strip_on_the_agents_face() {
        // Power-on state as is: no demo.
        let mut ui = std::boxed::Box::new(UiState::new());
        ui.reset();
        assert!(!ui.deck.demo_active());
        let templates: std::vec::Vec<_> = (0..ui.global.strip_len)
            .map(|index| ui.face_at(index).unwrap())
            .map(|face| (face.id, face.template, face.name))
            .collect();
        assert_eq!(templates.len(), 7);
        for (index, template) in [
            Template::Agents,
            Template::Omarchy,
            Template::Macropad,
            Template::Usage,
            Template::System,
            Template::Arcade,
            Template::Settings,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(templates[index].0, template.default_id());
            assert_eq!(templates[index].1, template);
            assert_eq!(templates[index].2, template.label());
        }
        assert_eq!(ui.current_face().template, Template::Agents);
        assert!(ui.strip_at_rest());
        assert_eq!(ui.face_at(7), None);
        assert_eq!(ui.index_of_face(5), Some(6));
        assert_eq!(ui.user_brightness(), 5);
        assert!(ui.still_default());
        let mut renamed = fresh();
        renamed.global.strip[1].name.set("Desk");
        assert!(!renamed.still_default());
        let mut dimmed = fresh();
        dimmed.deck.settings.brightness = 3;
        assert!(!dimmed.still_default());
    }

    #[test]
    fn a_never_reset_state_is_still_safe_to_query() {
        let ui = UiState::new();
        assert_eq!(ui.current_face().template, Template::Agents);
        assert_eq!(ui.visible_faces()[0].unwrap().face, None);
    }

    #[test]
    fn snapshot_copies_everything_in_place() {
        let mut ui = fresh();
        ui.on_face(Template::Usage);
        let mut out = Update::new();
        ui.apply(Control::Button(Button::Shift, Edge::Press), 5, &mut out);
        let mut frame = std::boxed::Box::new(UiState::new());
        ui.copy_into(&mut frame);
        assert_eq!(frame.revision, ui.revision);
        assert_eq!(frame.current_face(), ui.current_face());
        assert_eq!((frame.now_ms, frame.global.shift), (5, true));
        assert_eq!(frame.deck, ui.deck);
    }

    #[test]
    fn a_clean_update_leaves_the_revision_alone_and_stale_stamps_do_not_rewind() {
        let mut ui = fresh();
        let mut out = Update::new();
        ui.tick(1000, 0, true, &mut out);
        let revision = ui.revision;
        let mut out = Update::new();
        ui.tick(1001, 0, true, &mut out);
        assert!(!out.dirty);
        assert_eq!(ui.revision, revision);
        ui.apply(Control::Button(Button::Shift, Edge::Press), 900, &mut out);
        assert_eq!(ui.now_ms, 1001);
    }

    #[test]
    fn update_queues_overflow_instead_of_panicking() {
        let mut out = Update::new();
        for _ in 0..8 {
            out.push_deck(Event::Up);
        }
        assert!(!out.overflow);
        out.push_deck(Event::Up);
        assert!(out.overflow);
        out.clear();
        assert_eq!(out, Update::new());
    }

    #[test]
    fn visible_faces_follow_the_strip_position() {
        let mut ui = fresh();
        assert_eq!(ui.visible_faces()[1], None);
        ui.global.strip_x = (FACE_W + 120) * STRIP_ONE;
        let [first, second] = ui.visible_faces();
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!((first.face.unwrap().id, first.origin_x), (2, -120));
        assert_eq!((second.face.unwrap().id, second.origin_x), (3, 360));
        // The bounce overshoots an end: canvas on that side.
        ui.global.strip_x = -12 * STRIP_ONE;
        let [first, second] = ui.visible_faces();
        assert_eq!((first.unwrap().face, first.unwrap().origin_x), (None, -468));
        assert_eq!(second.unwrap().face.unwrap().id, 1);
        assert_eq!(second.unwrap().origin_x, 12);
        assert!(!ui.strip_at_rest());
    }
}

/// The speaker's local command: a ringing Timer alarm first, else the
/// arcade's music and effects.
pub fn sound_command(ui: &UiState) -> u32 {
    timer::alarm_command(ui).unwrap_or_else(|| arcade::sound_command(ui))
}
