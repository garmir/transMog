//! state transitions, preview sizing and whole-window snapshots driven
//! through egui_kittest
mod common;

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::accesskit::Toggled;
use egui::{Key, Rect, Role};
use egui_kittest::kittest::{By, NodeT, Queryable};
use egui_kittest::{Node, SnapshotError};
use transmog::fx::lens::fisheye::Fisheye;
use transmog::render::Pass;

use common::*;

/// the device combo box text, "{name} ({path})", or the placeholder the
/// nix sandbox sees
fn device_text(harness: &AppHarness) -> String {
    match harness.state().selected_device() {
        Some(device) => format!("{} ({})", device.name, device.path.display()),
        None => "no device".to_owned(),
    }
}

/// one camera, and a second capture on its node fails with EBUSY, so the
/// camera tests take turns; the guard is declared before the harness so
/// the capture stops before the next test gets the lock
static CAMERA: Mutex<()> = Mutex::new(());

fn take_camera() -> MutexGuard<'static, ()> {
    CAMERA.lock().unwrap_or_else(|e| e.into_inner())
}

fn fisheye(harness: &AppHarness) -> &Fisheye {
    harness.state().passes()[0]
        .as_any()
        .downcast_ref::<Fisheye>()
        .expect("the first pass is the fisheye")
}

/// the top bar readout, "{fps:5.1} fps  seq {seq}" while capturing
fn readout(harness: &AppHarness) -> (f32, u64) {
    let text = label_containing(harness, "fps");
    let mut parts = text.split_whitespace();
    let fps = parts.next().and_then(|f| f.parse().ok());
    let seq = parts.nth(2).and_then(|s| s.parse().ok());
    match (fps, seq) {
        (Some(fps), Some(seq)) => (fps, seq),
        _ => panic!("readout {text:?} is not capturing"),
    }
}

fn seq(harness: &AppHarness) -> u64 {
    readout(harness).1
}

fn focused(harness: &AppHarness) -> Option<Node<'_>> {
    harness.root().children_recursive().find(Node::is_focused)
}

fn toggle_section(harness: &mut AppHarness, header: &str) {
    harness
        .get_by_role_and_label(Role::DisclosureTriangle, header)
        .click();
    harness.step();
    // the open animation runs over a few frames
    harness.run_ok();
}

fn expanded(harness: &AppHarness, header: &str) -> Option<bool> {
    harness
        .get_by_role_and_label(Role::DisclosureTriangle, header)
        .accesskit_node()
        .data()
        .is_expanded()
}

/// the preview image, once a frame has been drawn
fn preview_rect(harness: &AppHarness) -> Option<Rect> {
    harness
        .query_all_by_role(Role::Image)
        .next()
        .map(|n| n.rect())
}

/// the central panel the preview is drawn into. egui gives its pane
/// node no bounds, so the rect is read off a render: the panel is the
/// only thing painted in gray 12, and the letterbox always leaves some
/// of it showing
fn preview_pane(harness: &mut AppHarness) -> Rect {
    let image = harness.render().expect("render the window");
    let mut pane = Rect::NOTHING;
    for (x, y, px) in image.enumerate_pixels() {
        if px.0 == [12, 12, 12, 255] {
            pane.extend_with(egui::pos2(x as f32, y as f32));
        }
    }
    assert!(pane.is_positive(), "no central panel fill in the render");
    // pixel centres to edges
    Rect::from_min_max(pane.min, pane.max + egui::vec2(1.0, 1.0))
}

/// the top bar, the status line, both side panel sections and the
/// preview placeholder, whatever the window size
fn assert_chrome(harness: &AppHarness) {
    harness.get_by_role_and_label(Role::Button, "rescan");
    harness.get(By::new().role(Role::ComboBox).value(&device_text(harness)));
    label_containing(harness, "fps");
    harness.get_by_role_and_label(Role::Label, harness.state().status());
    harness.get_by_role_and_label(Role::DisclosureTriangle, "lens");
    harness.get_by_role_and_label(Role::DisclosureTriangle, "camera");
}

fn assert_no_frame(harness: &AppHarness) {
    assert!(preview_rect(harness).is_none());
    if harness.state().passes().is_empty() {
        harness.get_by_role_and_label(Role::Label, "preview unavailable");
    } else {
        harness.get_by_role_and_label(Role::Label, "no frame yet");
    }
}

/// compares the current frame against `{name}.png` in the snapshot dir;
/// the first run has no reference, so the fresh render becomes it and
/// the second run is the first real comparison
fn compare_snapshot(harness: &mut AppHarness, name: &str) {
    match harness.try_snapshot(name) {
        Ok(()) => {}
        Err(SnapshotError::OpenSnapshot { path, .. }) => {
            let fresh = path.with_extension("new.png");
            std::fs::copy(&fresh, &path).expect("seed the snapshot baseline");
            eprintln!("seeded {}", path.display());
        }
        Err(e) => panic!("{e}"),
    }
}

/// paints over what changes between runs: the live frame and the fps
/// readout
fn mask_live_parts(harness: &mut AppHarness) {
    let image = preview_rect(harness).expect("a frame is on screen");
    let readout = harness
        .get(By::new().role(Role::Label).label_contains("fps"))
        .rect();
    harness.mask(image.expand(1.0));
    harness.mask(readout.expand(1.0));
}

fn wait_for_seq(harness: &mut AppHarness, at_least: u64, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        wait_frames(harness, 1);
        if seq(harness) >= at_least {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "seq stuck at {} below {at_least}: {}",
            seq(harness),
            harness.state().status()
        );
    }
}

fn set_size(harness: &mut AppHarness, width: f32, height: f32) {
    harness.set_size(egui::vec2(width, height));
    // the panels lay out against the new rect on the next frame and the
    // central panel on the one after
    harness.step();
    harness.step();
}

/// the window as the builder leaves it: a device selected, nothing
/// running, a plain status
#[test]
fn idle_window_has_a_plain_status() {
    let mut harness = harness_no_camera();
    harness.step();
    let app = harness.state();
    assert!(!app.capture_running());
    assert!(app.control_infos().is_empty());
    let status = app.status();
    let plain = status.is_empty()
        || status == "idle"
        || status.starts_with("no capture devices")
        || status.starts_with("no wgpu render state");
    assert!(plain, "status: {status:?}");
    if app.selected_device().is_some() {
        assert!(app.selected_format().is_some());
        assert!(app.capture_config().is_some());
        harness.get_by_role_and_label(Role::Button, "start");
    }
    harness.get_by_role_and_label(Role::Label, "  -.- fps  seq -");
    harness.get_by_role_and_label(Role::Label, "no controls (start a capture)");
    assert_no_frame(&harness);
}

#[test]
fn sixty_idle_frames_change_nothing() {
    let mut harness = harness_no_camera();
    harness.step();
    let snapshot = |h: &AppHarness| {
        let app = h.state();
        (
            app.status().to_owned(),
            app.passes().iter().map(|p| p.name()).collect::<Vec<_>>(),
            app.passes().iter().map(|p| p.enabled()).collect::<Vec<_>>(),
            app.selected_device().map(|d| d.path.clone()),
            app.capture_config().map(|c| format!("{c:?}")),
            app.control_infos().len(),
            h.root().children_recursive().count(),
        )
    };
    let before = snapshot(&harness);
    for _ in 0..60 {
        harness.step();
        assert_eq!(snapshot(&harness), before);
    }
    // without a capture nothing asks for a repaint, so the ui settles
    assert!(harness.run_ok().is_some(), "the idle ui keeps repainting");
    assert_eq!(snapshot(&harness), before);
}

#[test]
fn window_sizes_keep_the_chrome() {
    let mut harness = harness_no_camera();
    for (w, h) in [(700.0, 500.0), (1280.0, 800.0), (1900.0, 1050.0)] {
        set_size(&mut harness, w, h);
        assert_chrome(&harness);
        assert_no_frame(&harness);
        let lens = harness
            .get_by_role_and_label(Role::DisclosureTriangle, "lens")
            .rect();
        let rescan = harness.get_by_role_and_label(Role::Button, "rescan").rect();
        assert!(lens.max.x <= w + 0.5, "{w}x{h}: side panel off screen");
        assert!(
            rescan.max.y <= lens.min.y,
            "{w}x{h}: top bar below the panel"
        );
    }
}

/// the frame keeps its aspect, fills one axis of the central panel and
/// sits centred in it at every window size
#[test]
#[ignore = "needs a camera"]
fn window_sizes_letterbox_the_frame() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    let cfg = harness.state().capture_config().expect("capture config");
    let aspect = cfg.width as f32 / cfg.height as f32;
    wait_for_seq(&mut harness, 1, Duration::from_secs(3));
    for (w, h) in [(700.0, 500.0), (1280.0, 800.0), (1900.0, 1050.0)] {
        set_size(&mut harness, w, h);
        assert_chrome(&harness);
        let image = preview_rect(&harness).expect("preview image");
        let pane = preview_pane(&mut harness);
        let fills_width = (image.width() - pane.width()).abs() <= 1.0;
        let fills_height = (image.height() - pane.height()).abs() <= 1.0;
        assert!(
            fills_width || fills_height,
            "{w}x{h}: image {image:?} does not fill pane {pane:?}"
        );
        assert!(
            (image.aspect_ratio() - aspect).abs() < 0.01,
            "{w}x{h}: aspect {} vs frame {aspect}",
            image.aspect_ratio()
        );
        assert!(
            image.center().distance(pane.center()) <= 1.0,
            "{w}x{h}: image {image:?} off centre in {pane:?}"
        );
        assert!(pane.contains_rect(image.shrink(0.5)));
    }
}

#[test]
fn side_panel_sections_collapse_and_expand() {
    let mut harness = harness_no_camera();
    harness.step();
    let has_lens_widgets = !harness.state().passes().is_empty();
    assert_eq!(expanded(&harness, "lens"), Some(true));
    assert_eq!(expanded(&harness, "camera"), Some(true));
    if has_lens_widgets {
        harness.get_by_role_and_label(Role::CheckBox, "fisheye");
    }
    harness.get_by_role_and_label(Role::Label, "no controls (start a capture)");

    toggle_section(&mut harness, "lens");
    assert_eq!(expanded(&harness, "lens"), Some(false));
    assert!(harness
        .query_by_role_and_label(Role::CheckBox, "fisheye")
        .is_none());

    toggle_section(&mut harness, "camera");
    assert_eq!(expanded(&harness, "camera"), Some(false));
    assert!(harness
        .query_by_role_and_label(Role::Label, "no controls (start a capture)")
        .is_none());

    toggle_section(&mut harness, "lens");
    toggle_section(&mut harness, "camera");
    assert_eq!(expanded(&harness, "lens"), Some(true));
    assert_eq!(expanded(&harness, "camera"), Some(true));
    if has_lens_widgets {
        harness.get_by_role_and_label(Role::CheckBox, "fisheye");
    }
    harness.get_by_role_and_label(Role::Label, "no controls (start a capture)");
}

/// tab walks the top bar left to right: the four combo boxes, start,
/// rescan
#[test]
fn tab_walks_the_top_bar() {
    let mut harness = harness_no_camera();
    harness.step();
    if harness.state().capture_config().is_none() {
        eprintln!("skipped: no device, so the top bar is mostly disabled");
        return;
    }
    assert!(focused(&harness).is_none());
    let top = harness.get_by_role_and_label(Role::Button, "rescan").rect();

    let mut visited = Vec::new();
    for _ in 0..6 {
        harness.key_press(Key::Tab);
        harness.step();
        let node = focused(&harness).expect("tab focuses something");
        let rect = node.rect();
        assert!(
            rect.max.y <= top.max.y + 1.0,
            "focus left the top bar: {node:?}"
        );
        let id = node.accesskit_node().id();
        assert!(!visited.contains(&id), "focus returned to {node:?}");
        visited.push(id);
        let role = node.accesskit_node().role();
        let label = node.accesskit_node().label();
        let value = node.value();
        match visited.len() {
            1..=4 => assert_eq!(role, Role::ComboBox, "{node:?}"),
            5 => assert_eq!((role, label.as_deref()), (Role::Button, Some("start"))),
            _ => assert_eq!((role, label.as_deref()), (Role::Button, Some("rescan"))),
        }
        if visited.len() == 1 {
            assert_eq!(value, Some(device_text(&harness)));
        }
    }
    assert!(!harness.state().capture_running());
}

#[test]
fn arrow_keys_move_a_focused_slider() {
    let mut harness = harness_no_camera();
    harness.step();
    if harness.state().passes().is_empty() {
        eprintln!("skipped: no renderer, so no lens widgets");
        return;
    }
    set_slider(&mut harness, "lens angle", 90.0);
    assert_eq!(fisheye(&harness).lens_fov(), 90.0);

    harness
        .get_by_role_and_label(Role::Slider, "lens angle")
        .focus();
    harness.step();
    // egui only installs the slider's arrow key lock once the widget has
    // had focus for a full frame; an arrow before that also moves focus
    // to the number field
    harness.step();
    let node = focused(&harness).expect("the slider took focus");
    assert_eq!(node.accesskit_node().role(), Role::Slider);
    assert_eq!(node.accesskit_node().label().as_deref(), Some("lens angle"));

    harness.key_press(Key::ArrowRight);
    harness.step();
    // the slider reads the key in the frame it is pressed and writes
    // the value the frame after
    harness.step();
    let up = fisheye(&harness).lens_fov();
    assert!(up > 90.0, "arrow right left the angle at {up}");

    for _ in 0..2 {
        harness.key_press(Key::ArrowLeft);
        harness.step();
        harness.step();
    }
    let down = fisheye(&harness).lens_fov();
    assert!(down < 90.0, "two arrow lefts left the angle at {down}");
    assert!(focused(&harness).is_some_and(|n| n.accesskit_node().role() == Role::Slider));
}

#[test]
#[ignore = "needs a gpu"]
fn snapshot_window_no_camera() {
    let mut harness = harness_no_camera();
    assert!(
        !harness.state().passes().is_empty(),
        "no renderer: {}",
        harness.state().status()
    );
    harness.run_ok();
    harness.remove_cursor();
    harness.step();
    compare_snapshot(&mut harness, "window_no_camera");
}

/// no frame can be injected from outside the app, so these two run on
/// the built-in camera and mask the frame and the fps readout before
/// comparing the rest of the window
#[test]
#[ignore = "needs a camera"]
fn snapshot_window_with_frame_fisheye_default() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_for_seq(&mut harness, 1, Duration::from_secs(3));
    assert!(fisheye(&harness).enabled());
    harness.remove_cursor();
    harness.step();
    mask_live_parts(&mut harness);
    compare_snapshot(&mut harness, "window_frame_fisheye_default");
}

#[test]
#[ignore = "needs a camera"]
fn snapshot_window_with_frame_fisheye_off() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_for_seq(&mut harness, 1, Duration::from_secs(3));
    harness
        .get_by_role_and_label(Role::CheckBox, "fisheye")
        .click();
    wait_frames(&mut harness, 3);
    assert!(!fisheye(&harness).enabled());
    assert_eq!(
        harness
            .get_by_role_and_label(Role::CheckBox, "fisheye")
            .accesskit_node()
            .toggled(),
        Some(Toggled::False)
    );
    harness.remove_cursor();
    harness.step();
    mask_live_parts(&mut harness);
    compare_snapshot(&mut harness, "window_frame_fisheye_off");
}

fn brightness(harness: &AppHarness) -> i64 {
    harness
        .state()
        .control_infos()
        .iter()
        .find(|c| c.name == "Brightness")
        .map(|c| c.value)
        .expect("a Brightness control")
}

/// a control write queued in the same frame as the stop click: the top
/// bar runs first, so the stop wins and the write lands nowhere
#[test]
#[ignore = "needs a camera"]
fn stop_with_a_control_write_pending() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_frames(&mut harness, 5);
    let original = brightness(&harness);
    let wanted = if original < 0 {
        original + 5
    } else {
        original - 5
    };

    let stop_at = harness
        .get_by_role_and_label(Role::Button, "stop")
        .rect()
        .center();
    harness
        .get_by_role_and_label(Role::Slider, "Brightness")
        .set_value_accesskit(wanted as f64);
    let modifiers = egui::Modifiers::default();
    harness.input_mut().events.extend([
        egui::Event::PointerMoved(stop_at),
        egui::Event::PointerButton {
            pos: stop_at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers,
        },
        egui::Event::PointerButton {
            pos: stop_at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers,
        },
    ]);
    harness.step();
    wait_frames(&mut harness, 3);

    assert!(!harness.state().capture_running());
    assert_eq!(harness.state().status(), "stopped");
    assert!(harness.state().control_infos().is_empty());
    assert!(harness
        .query_by_role_and_label(Role::Slider, "Brightness")
        .is_none());

    click(&mut harness, "start");
    wait_frames(&mut harness, 5);
    assert!(harness.state().capture_running());
    assert_eq!(brightness(&harness), original, "the pending write landed");
}

/// picking the already selected device while stopped is a no-op: no
/// start, no rescan of formats, the status untouched
#[test]
#[ignore = "needs a camera"]
fn selecting_the_same_device_while_stopped_changes_nothing() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_frames(&mut harness, 3);
    click(&mut harness, "stop");
    assert!(!harness.state().capture_running());
    let before = (
        harness.state().status().to_owned(),
        format!("{:?}", harness.state().capture_config()),
    );

    let device = device_text(&harness);
    combo_select(&mut harness, &device, &device);
    wait_frames(&mut harness, 3);
    assert!(!harness.state().capture_running());
    let after = (
        harness.state().status().to_owned(),
        format!("{:?}", harness.state().capture_config()),
    );
    assert_eq!(after, before);
    harness.get_by_role_and_label(Role::Button, "start");
    harness.get_by_role_and_label(Role::Label, "no controls (start a capture)");
}

/// a second start restarts the capture from seq 1; stop is reachable
/// only through the toggle button, so stopping twice means stop, start,
/// stop
#[test]
#[ignore = "needs a camera"]
fn start_twice_and_stop_twice() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_for_seq(&mut harness, 5, Duration::from_secs(3));
    let status = harness.state().status().to_owned();

    harness.state_mut().autostart_with("USB 2.0 Camera");
    harness.step();
    assert!(harness.state().capture_running());
    assert_eq!(harness.state().status(), status);
    assert!(seq(&harness) < 5, "seq {} did not restart", seq(&harness));
    wait_for_seq(&mut harness, 5, Duration::from_secs(3));
    assert!(!harness.state().control_infos().is_empty());

    for _ in 0..2 {
        click(&mut harness, "stop");
        wait_frames(&mut harness, 2);
        assert!(!harness.state().capture_running());
        assert_eq!(harness.state().status(), "stopped");
        assert!(harness.state().control_infos().is_empty());
        harness.get_by_role_and_label(Role::Label, "  -.- fps  seq -");
        assert!(harness
            .query_by_role_and_label(Role::Button, "stop")
            .is_none());

        click(&mut harness, "start");
        wait_for_seq(&mut harness, 1, Duration::from_secs(3));
        assert_eq!(harness.state().status(), status);
    }
    click(&mut harness, "stop");
    assert!(!harness.state().capture_running());
}

#[test]
#[ignore = "needs a camera"]
fn hundred_steps_advance_the_sequence() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_for_seq(&mut harness, 1, Duration::from_secs(3));
    // the camera advertises 30 fps at 640x480 but auto exposure drops
    // that to 10-16 in dim light, so a step that sees no new frame after
    // its 140 ms gets up to a second more before it counts as a stall
    let mut last = seq(&harness);
    let mut late = 0;
    for step in 0..100 {
        std::thread::sleep(Duration::from_millis(140));
        harness.step();
        let waited = Instant::now();
        let mut stalled = false;
        while seq(&harness) == last {
            assert!(
                waited.elapsed() < Duration::from_secs(1),
                "step {step}: seq stuck at {last}"
            );
            stalled = true;
            std::thread::sleep(Duration::from_millis(20));
            harness.step();
        }
        let now = seq(&harness);
        assert!(now > last, "step {step}: seq {last} -> {now}");
        late += usize::from(stalled);
        last = now;
    }
    eprintln!("{late} of 100 steps waited past their 140 ms");
    let (fps, _) = readout(&harness);
    eprintln!("measured {fps} fps over the last second");
    assert!(fps > 3.0, "measured {fps} fps");
    assert!(harness.state().capture_running());
}

/// each size change restarts the capture with fresh textures; the
/// status names the new size and the sequence restarts and advances
#[test]
#[ignore = "needs a camera"]
fn switching_sizes_recreates_the_textures() {
    let _camera = take_camera();
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    let cfg = harness.state().capture_config().expect("capture config");
    assert_eq!((cfg.width, cfg.height), (640, 480), "{cfg:?}");
    wait_for_seq(&mut harness, 2, Duration::from_secs(3));
    let first = preview_rect(&harness).expect("preview image");

    // 1600x1200 yuyv exceeds what usb 2 carries, so it arrives slowly
    for (from, to, wait) in [
        ("640x480", "1600x1200", Duration::from_secs(8)),
        ("1600x1200", "640x480", Duration::from_secs(3)),
    ] {
        combo_select(&mut harness, from, to);
        harness.step();
        let app = harness.state();
        assert!(app.capture_running(), "status: {}", app.status());
        assert!(app.status().contains(to), "status: {}", app.status());
        let cfg = app.capture_config().expect("capture config");
        assert_eq!(format!("{}x{}", cfg.width, cfg.height), to);
        assert!(seq(&harness) < 2, "seq {} did not restart", seq(&harness));
        wait_for_seq(&mut harness, 2, wait);
        harness.get(By::new().role(Role::ComboBox).value(to));
        let image = preview_rect(&harness).expect("preview image after the switch");
        assert!((image.aspect_ratio() - 4.0 / 3.0).abs() < 0.01);
    }
    assert!(seq(&harness) >= 2);
    assert_eq!(preview_rect(&harness), Some(first));
}
