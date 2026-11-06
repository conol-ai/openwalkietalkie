//! The strip: a face at rest is exactly the static face, a slide shows two
//! faces side by side, and no origin can make a face or overlay panic.
use super::*;
use crate::{
    control::{Button, Control, Edge, Wheel},
    ui::{bluetooth, overlay, state::{Overlay, PttSession, Toast, FACE_W, STRIP_ONE}, Update},
};

const TEMPLATES: [Template; 5] = [
    Template::Agents,
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::Settings,
];

/// No notices, so no overlay pixel disturbs a face-only comparison.
fn quiet() -> Box<UiState> {
    let mut ui = ui();
    for notice in &mut ui.deck.notices {
        notice.used = false;
    }
    ui
}

fn pixel(frame: &[u8], x: usize, y: usize) -> [u8; 2] {
    let at = (y * 480 + x) * 2;
    [frame[at], frame[at + 1]]
}

#[test]
fn a_strip_at_rest_is_the_static_face_byte_for_byte() {
    let mut ui = ui();
    for index in 0..ui.global.strip_len {
        ui.global.current = index;
        ui.global.strip_x = index as i32 * FACE_W * STRIP_ONE;
        let face = ui.face_at(index).unwrap();
        assert!(ui.strip_at_rest());
        let expected = full_draw(|band| {
            scene::draw_face(band, &ui, face);
            overlay::draw(band, &ui);
            bluetooth::draw(band, ui.bluetooth);
        });
        assert_frame(&full(&ui), &expected, "a resting strip must be the face itself");
        let mut cut = on(face.template);
        cut.revision = ui.revision;
        assert_frame(&full(&cut), &expected, "on_face must land exactly on the face");
    }
}

#[test]
fn a_slide_shows_the_two_neighbours_side_by_side() {
    let mut ui = quiet();
    for from in 0..ui.global.strip_len - 1 {
        // Face pixels move; the Bluetooth indicator remains fixed on top.
        let left = full_draw(|band| scene::draw_face(band, &ui, ui.face_at(from).unwrap()));
        let right = full_draw(|band| scene::draw_face(band, &ui, ui.face_at(from + 1).unwrap()));
        let fixed = full(&on_index(&ui, from));
        for px in [1, 119, 240, 333, 479] {
            ui.global.current = from + 1;
            ui.global.strip_x = (from as i32 * FACE_W + px) * STRIP_ONE + 77;
            let frame = full(&ui);
            let px = px as usize;
            for y in (0..480).step_by(7) {
                for x in 0..480 {
                    let expected = if layout::BLUETOOTH.contains(x as i32, y as i32) {
                        pixel(&fixed, x, y)
                    } else if x < 480 - px {
                        pixel(&left, x + px, y)
                    } else {
                        pixel(&right, x - (480 - px), y)
                    };
                    assert_eq!(pixel(&frame, x, y), expected, "face {from} + {px} px at ({x}, {y})");
                }
            }
        }
    }
}

fn on_index(ui: &UiState, index: u8) -> Box<UiState> {
    let mut at = Box::new((*ui).clone());
    at.global.current = index;
    at.global.strip_x = index as i32 * FACE_W * STRIP_ONE;
    at
}

#[test]
fn beyond_the_ends_of_the_strip_is_canvas() {
    let mut ui = quiet();
    ui.global.strip_x = -12 * STRIP_ONE;
    let frame = full(&ui);
    let home = full_draw(|band| scene::draw_face(band, &ui, ui.face_at(0).unwrap()));
    let fixed = full(&on_index(&ui, 0));
    for y in (0..480).step_by(5) {
        for x in 0..480 {
            let expected = if layout::BLUETOOTH.contains(x as i32, y as i32) {
                pixel(&fixed, x, y)
            } else if x < 12 { [0, 0] } else { pixel(&home, x - 12, y) };
            assert_eq!(pixel(&frame, x, y), expected);
        }
    }
    ui.global.current = ui.global.strip_len - 1;
    ui.global.strip_x = ((ui.global.strip_len as i32 - 1) * FACE_W + 12) * STRIP_ONE;
    let frame = full(&ui);
    assert!((0..480).all(|y| (468..480).all(|x| pixel(&frame, x, y) == [0, 0])));
}

#[test]
fn a_retargeted_slide_refreshes_like_a_complete_redraw_in_every_frame() {
    let mut previous = ui();
    let mut screen = Screens::new(&previous);
    let mut now = 0;
    let mut step = |previous: &mut Box<UiState>, control: Option<Control>, dt: u32| {
        let mut next = Box::new((**previous).clone());
        let mut out = Update::new();
        now += dt;
        match control {
            Some(control) => next.apply(control, now, &mut out),
            None => next.tick(now, 0, true, &mut out),
        }
        update(&mut screen, previous, &next);
        *previous = next;
    };
    step(&mut previous, Some(Control::Button(Button::Shift, Edge::Press)), 5);
    // 168 ms apart: slower than the 150 ms detent limit (every detent counts),
    // faster than the 220 ms slide (every one but the first retargets it).
    // The walk never reaches an end of the strip, so no bounce is involved.
    for wheel in [Wheel::Ccw, Wheel::Ccw, Wheel::Cw, Wheel::Ccw, Wheel::Ccw] {
        step(&mut previous, Some(Control::Wheel(wheel)), 3);
        for _ in 0..5 {
            step(&mut previous, None, 33);
        }
    }
    step(&mut previous, Some(Control::Button(Button::Shift, Edge::Release)), 5);
    for _ in 0..30 {
        step(&mut previous, None, 33);
    }
    assert!(previous.strip_at_rest());
    assert!(!previous.animating());
    assert_eq!(previous.current_face().template, Template::Usage);
}

/// Every face and overlay, drawn through a band whose origin sweeps the whole
/// slide range (odd values too), whole bands and small clips.
#[test]
fn no_origin_from_minus_480_to_480_makes_any_face_or_overlay_panic() {
    let mut states: Vec<Box<UiState>> = TEMPLATES.iter().map(|&t| on(t)).collect();
    for overlay in [
        Overlay::Drawer,
        Overlay::Manager,
        Overlay::Gallery,
        Overlay::Naming,
        Overlay::ConfirmRemove,
    ] {
        let mut ui = ui();
        ui.global.overlay = overlay;
        states.push(ui);
    }
    let mut busy = on(Template::Omarchy);
    busy.global.ptt = Some(PttSession::new(2, crate::deck::msg::PttCtx::Talk, 0, 0));
    busy.deck.transcript.set(crate::deck::demo::TRANSCRIPT);
    busy.notify.toast = Some(Toast::card(1, 1, crate::deck::msg::CardKind::Question, 0));
    states.push(busy);
    let mut band = display::Band::new();
    for ui in &states {
        let face = ui.current_face();
        for origin in -480i32..=480 {
            // All bands on a coarse sweep, two busy bands on every origin.
            let bands: &[u16] = if origin % 16 == 0 || origin.abs() >= 478 {
                &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
            } else {
                &[1, 9]
            };
            for &index in bands {
                band.set_row(index);
                if origin % 3 == 0 {
                    let y = index as i32 * 48;
                    band.clip_to(Rectangle::new(
                        Point::new((origin + 480) / 3, y + 5),
                        Size::new(97, 21),
                    ));
                }
                band.set_origin_x(origin);
                scene::draw_face(&mut band, ui, face);
                overlay::draw(&mut band, ui);
            }
        }
    }
}

/// The harness' corner oracle, proven on every face: the hint bar runs
/// under the arcs but is as dark as they are.
#[test]
fn no_deck_face_or_overlay_lights_a_pixel_under_the_corner_arcs() {
    for template in TEMPLATES {
        assert_eq!(lit_outside_safe_area(&full(&on(template))), None, "{template:?}");
    }
    for overlay in [Overlay::Drawer, Overlay::Manager, Overlay::Gallery, Overlay::Naming] {
        let mut ui = ui();
        ui.global.overlay = overlay;
        assert_eq!(lit_outside_safe_area(&full(&ui)), None, "{overlay:?}");
    }
    let mut lit = full(&ui());
    lit[(3 * 480 + 3) * 2] = 0xff;
    assert_eq!(lit_outside_safe_area(&lit), Some((3, 3)));
}

/// `scene` neither draws nor signs the faces under an opaque overlay, so such
/// an overlay must own every pixel: whatever the band held before shows
/// nowhere, mid-slide included, and the hidden faces register no region.
#[test]
fn an_opaque_overlay_owns_every_pixel_and_hides_the_face_from_damage() {
    let mut states: Vec<Box<UiState>> = Vec::new();
    for overlay in [Overlay::Drawer, Overlay::Manager, Overlay::Gallery, Overlay::Naming] {
        let mut ui = on(Template::Omarchy);
        ui.global.overlay = overlay;
        states.push(ui);
    }
    let mut listening = on(Template::Usage);
    listening.global.ptt = Some(PttSession::new(2, crate::deck::msg::PttCtx::Talk, 0, 0));
    listening.global.strip_x += FACE_W * STRIP_ONE / 3;
    states.push(listening);
    for ui in &states {
        assert!(overlay::opaque(ui));
        let draw_over = |garbage: Rgb565| {
            full_draw(|band| {
                band.fill(garbage);
                scene::draw(band, ui);
            })
        };
        assert_frame(
            &draw_over(Rgb565::MAGENTA),
            &draw_over(Rgb565::BLACK),
            "an opaque overlay must fill the band before it draws",
        );
        // Only the overlays register regions: a hidden face causes no damage.
        let mut overlays = Box::new(Signature::new());
        let mut builder = crate::ui::signature::SigBuilder::new(&mut overlays);
        overlay::signature(ui, &mut builder);
        bluetooth::signature(ui.bluetooth, &mut builder);
        assert_eq!(signature(ui).regions(), overlays.regions());
    }
    // The remove-confirm dialog and a toast are not opaque: the face still draws.
    let mut confirm = on(Template::Omarchy);
    confirm.global.overlay = Overlay::ConfirmRemove;
    assert!(!overlay::opaque(&confirm));
    assert!(!overlay::opaque(&on(Template::Omarchy)));
}

/// Demo notices show on every face, as a real host's do.
#[test]
fn demo_notices_show_on_every_face() {
    for template in TEMPLATES {
        let demo = on(template);
        assert!(demo.deck.demo_active() && demo.deck.notice_count() != 0);
        let mut bare = on(template);
        bare.deck.clear_host_data();
        assert_ne!(full(&demo), full(&bare), "{template:?}");
    }
}
