//! aim mode's hints on synthetic camera frames: the visualiser's marks
//! drawn through a known map into a dark room, then found again
mod common;

use egui::{pos2, Pos2, Role};
use egui_kittest::kittest::Queryable;
use transmog::aim::geom::Homography;
use transmog::aim::marks::{mark_at, FRAME};
use transmog::aim::{analyse, plan, Hint, Luma, Name, Screen, Track};

use common::*;

const SIZE: (usize, usize) = (1920, 1080);

/// a screen showing the marks: frame pixels to camera pixels, its white
/// and black levels and the stamp values
struct Shown {
    h: Homography,
    white: u8,
    black: u8,
    values: [u16; 3],
}

/// frame pixel (0, 0) lands on `origin`, then a scale and a roll in
/// degrees, positive turning clockwise on screen
fn placed(origin: Pos2, scale: f32, roll: f32) -> Homography {
    let (s, c) = (
        scale * roll.to_radians().sin(),
        scale * roll.to_radians().cos(),
    );
    Homography([
        f64::from(c),
        f64::from(-s),
        f64::from(origin.x),
        f64::from(s),
        f64::from(c),
        f64::from(origin.y),
        0.0,
        0.0,
        1.0,
    ])
}

fn projector(origin: Pos2, scale: f32, roll: f32) -> Shown {
    Shown {
        h: placed(origin, scale, roll),
        white: 240,
        black: 20,
        values: [40123, 2211, 17],
    }
}

fn mini(origin: Pos2, scale: f32, roll: f32) -> Shown {
    Shown {
        h: placed(origin, scale, roll),
        white: 180,
        black: 5,
        values: [5000, 2210, 17],
    }
}

/// a dark room with each screen's picture drawn through its map; the
/// picture away from the marks is a dim grey, as the visualiser's
/// background is
fn render(screens: &[Shown]) -> Luma {
    let (w, h) = SIZE;
    let mut luma = Luma::new(w, h, 8);
    for s in screens {
        let inv = s.h.inverse().expect("invertible");
        let outline = s.h.apply_all(FRAME.corners());
        let x0 = outline
            .iter()
            .map(|p| p.x)
            .fold(f32::MAX, f32::min)
            .max(0.0) as usize;
        let x1 = (outline
            .iter()
            .map(|p| p.x)
            .fold(f32::MIN, f32::max)
            .max(0.0) as usize)
            .min(w);
        let y0 = outline
            .iter()
            .map(|p| p.y)
            .fold(f32::MAX, f32::min)
            .max(0.0) as usize;
        let y1 = (outline
            .iter()
            .map(|p| p.y)
            .fold(f32::MIN, f32::max)
            .max(0.0) as usize)
            .min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let f = inv.apply(pos2(x as f32 + 0.5, y as f32 + 0.5));
                if !FRAME.contains(f.x, f.y) {
                    continue;
                }
                luma.data[y * w + x] = match mark_at(f.x, f.y, s.values) {
                    Some(true) => s.white,
                    Some(false) => s.black,
                    None => 30,
                };
            }
        }
    }
    luma
}

fn screens_and_hints(shown: &[Shown]) -> (Vec<Screen>, Vec<Hint>) {
    let screens = analyse(&render(shown), 1.0);
    let hints = plan(&screens, (SIZE.0 as f32, SIZE.1 as f32), 1.0).hints;
    (screens, hints)
}

fn find(screens: &[Screen], name: Name) -> &Screen {
    screens
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("{} not found in {screens:?}", name.label()))
}

/// the projector above, the mini below and smaller, both well inside
fn both_in_view() -> Vec<Shown> {
    vec![
        projector(pos2(480.0, 70.0), 0.4, 0.0),
        mini(pos2(760.0, 640.0), 0.16, 0.0),
    ]
}

#[test]
fn both_in_view_gives_no_hint_and_aim_ok() {
    let shown = both_in_view();
    let luma = render(&shown);
    let screens = analyse(&luma, 1.0);
    let p = find(&screens, Name::Projector);
    let m = find(&screens, Name::Mini);
    for (s, want) in [(p, &shown[0]), (m, &shown[1])] {
        assert!(!s.clipped.any(), "{s:?}");
        assert_eq!(s.seen_ids(), vec![0, 1, 2, 3], "{}", s.name.label());
        let st = s.stamps.expect("stamps in view");
        assert_eq!(st.values, want.values.map(Some), "{}", s.name.label());
        let corner = want.h.apply(pos2(1920.0, 1080.0));
        assert!(
            s.outline[2].distance(corner) < 3.0,
            "{:?} vs {corner:?}",
            s.outline
        );
    }
    let plan = plan(&screens, (1920.0, 1080.0), 1.0);
    assert!(plan.hints.is_empty(), "{:?}", plan.hints);
    assert!(plan.both_in_view);
    assert!(plan.level_deg.unwrap().abs() < 0.5);

    // ten analyses a second for 3.2 s, every one reading both stamps
    let mut track = Track::default();
    for i in 0..=32 {
        let t = f64::from(i) / 10.0;
        for s in &screens {
            track.view(t, s.name, !s.clipped.any());
            track.read(t, s.name, s.stamps.is_some_and(|st| st.ok()));
        }
        if i == 20 {
            assert!(!track.aim_ok(t), "aim ok after only 2 s");
        }
    }
    assert!(track.aim_ok(3.2), "{:?}", track.waiting_for(3.2));
}

#[test]
fn clipped_on_top_says_tilt_up() {
    let shown = vec![
        projector(pos2(480.0, -45.0), 0.4, 0.0),
        mini(pos2(760.0, 560.0), 0.16, 0.0),
    ];
    let (screens, hints) = screens_and_hints(&shown);
    let p = find(&screens, Name::Projector);
    assert!(p.clipped.top && !p.clipped.bottom, "{:?}", p.clipped);
    assert!(!find(&screens, Name::Mini).clipped.any());
    assert_eq!(hints, vec![Hint::TiltUp]);
}

#[test]
fn clipped_on_the_left_says_pan_left() {
    let shown = vec![
        projector(pos2(-20.0, 70.0), 0.4, 0.0),
        mini(pos2(200.0, 640.0), 0.16, 0.0),
    ];
    let (screens, hints) = screens_and_hints(&shown);
    assert!(find(&screens, Name::Projector).clipped.left);
    assert_eq!(hints, vec![Hint::PanLeft]);
}

#[test]
fn too_large_says_move_back() {
    let shown = vec![projector(pos2(20.0, 12.0), 0.95, 0.0)];
    let (screens, hints) = screens_and_hints(&shown);
    assert_eq!(screens.len(), 1);
    assert_eq!(screens[0].name, Name::Projector);
    assert_eq!(hints, vec![Hint::MoveBack]);
}

#[test]
fn rotated_says_rotate() {
    let shown = vec![
        projector(pos2(500.0, 60.0), 0.4, 4.0),
        mini(pos2(760.0, 640.0), 0.16, 4.0),
    ];
    let (screens, hints) = screens_and_hints(&shown);
    assert_eq!(screens.len(), 2, "{screens:?}");
    let Some(Hint::Rotate { clockwise, deg }) = hints.first().copied() else {
        panic!("{hints:?}");
    };
    assert!(clockwise);
    assert!((deg - 4.0).abs() < 0.5, "{deg}");
    assert_eq!(
        Hint::Rotate { clockwise, deg }.text(),
        "rotate clockwise 4°"
    );

    let (_, hints) = screens_and_hints(&[
        projector(pos2(500.0, 120.0), 0.4, -3.0),
        mini(pos2(760.0, 680.0), 0.16, -3.0),
    ]);
    assert_eq!(
        hints.first().map(|h| h.text()).as_deref(),
        Some("rotate counter-clockwise 3°")
    );
}

/// the mini alone leans in the camera's view; its level is shown but
/// is no reason to turn the camera
#[test]
fn mini_alone_gives_no_rotate_hint() {
    let (screens, hints) = screens_and_hints(&[mini(pos2(760.0, 500.0), 0.16, 6.0)]);
    assert_eq!(screens.len(), 1);
    assert_eq!(screens[0].name, Name::Mini);
    assert!(
        !hints.iter().any(|h| matches!(h, Hint::Rotate { .. })),
        "{hints:?}"
    );
    assert_eq!(hints, vec![Hint::TiltUp]);
}

#[test]
fn missing_mini_points_down() {
    let (screens, hints) = screens_and_hints(&[projector(pos2(480.0, 70.0), 0.4, 0.0)]);
    assert_eq!(screens.len(), 1);
    assert_eq!(hints, vec![Hint::TiltDown]);
}

#[test]
fn nothing_found_in_an_empty_room() {
    let (screens, hints) = screens_and_hints(&[]);
    assert!(screens.is_empty());
    assert!(hints.is_empty());
}

/// aim mode is off at start, and the top bar's button and the A key
/// both turn it on and off
#[test]
fn aim_toggles_by_button_and_key() {
    let mut harness = harness_no_camera();
    harness.step();
    assert!(!harness.state().aim_on());
    assert!(harness.query_by_label("fit both").is_none());

    harness.get_by_role_and_label(Role::Button, "aim").click();
    harness.step();
    harness.step();
    assert!(harness.state().aim_on());
    harness.get_by_role_and_label(Role::Button, "fit both");

    harness.key_press(egui::Key::A);
    harness.step();
    assert!(!harness.state().aim_on());
    harness.key_press(egui::Key::A);
    harness.step();
    assert!(harness.state().aim_on());
}

/// prints what aim mode makes of a jpeg from the tap, for checking the
/// detector against the real room: TRANSMOG_AIM_FRAME=path.jpg
#[test]
#[ignore = "needs a camera frame"]
fn real_frame() {
    let Some(path) = std::env::var_os("TRANSMOG_AIM_FRAME") else {
        eprintln!("skipped: TRANSMOG_AIM_FRAME is not set");
        return;
    };
    let bytes = std::fs::read(path).expect("frame");
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::Luma);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(
        zune_jpeg::zune_core::bytestream::ZCursor::new(&bytes[..]),
        options,
    );
    let data = dec.decode().expect("decode");
    let info = dec.info().expect("info");
    let luma = Luma {
        width: usize::from(info.width),
        height: usize::from(info.height),
        data,
    };
    let started = std::time::Instant::now();
    let squares = transmog::aim::detect::squares(&luma);
    let screens = analyse(&luma, 1.0);
    let took = started.elapsed();
    for q in &squares {
        eprintln!("square {:?} side {:.1}", q.centre(), q.side);
    }
    for s in &screens {
        eprintln!(
            "{} fids {:?} clipped {:?} scale {:.3} module {:.1} err {:.1} stamps {:?} focus {:?} outline {:?}",
            s.name.label(),
            s.seen_ids(),
            s.clipped.names(),
            s.scale,
            s.module_px,
            s.fit_err,
            s.stamps,
            s.focus,
            s.outline
        );
    }
    let plan = plan(&screens, (luma.width as f32, luma.height as f32), 1.0);
    eprintln!(
        "hints {:?} notes {:?} level {:?}",
        plan.hints, plan.notes, plan.level_deg
    );
    eprintln!("analysis took {took:?} (debug build)");
}
