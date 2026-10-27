mod common;

use egui::accesskit::Toggled;
use egui::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::SnapshotError;
use transmog::fx::lens::fisheye::{Fisheye, Projection};

use common::*;

fn fisheye(harness: &AppHarness) -> &Fisheye {
    harness.state().passes()[0]
        .as_any()
        .downcast_ref::<Fisheye>()
        .expect("the first pass is the fisheye")
}

fn seq(harness: &AppHarness) -> u64 {
    let text = label_containing(harness, "seq ");
    let (_, seq) = text.rsplit_once("seq ").expect("seq readout");
    seq.trim()
        .parse()
        .unwrap_or_else(|_| panic!("seq in {text:?}"))
}

/// the idle window: status, rescan, the lens and camera sections. the
/// lens widgets only exist with a renderer, which the nix sandbox lacks
#[test]
fn idle_window_is_reachable() {
    let mut harness = harness_no_camera();
    harness.step();
    let app = harness.state();
    assert!(!app.capture_running());
    assert!(!app.status().is_empty());
    harness.get_by_role_and_label(Role::Label, app.status());
    harness.get_by_role_and_label(Role::Button, "rescan");
    harness.get_by_role_and_label(Role::Label, "no controls (start a capture)");

    if app.passes().is_empty() {
        harness.get_by_role_and_label(Role::Label, "no renderer");
        return;
    }
    let checkbox = harness.get_by_role_and_label(Role::CheckBox, "fisheye");
    assert_eq!(checkbox.accesskit_node().toggled(), Some(Toggled::True));

    set_slider(&mut harness, "lens angle", 90.0);
    assert_eq!(fisheye(&harness).lens_fov(), 90.0);

    combo_select(&mut harness, "equisolid", "rectilinear");
    assert_eq!(fisheye(&harness).projection(), Projection::Rectilinear);

    click(&mut harness, "reset");
    assert_eq!(fisheye(&harness).lens_fov(), 180.0);
    assert_eq!(fisheye(&harness).projection(), Projection::Equisolid);

    click(&mut harness, "rescan");
    assert!(!harness.state().capture_running());

    // a fresh snapshot dir has no reference image; the first run seeds
    // it as idle_window.new.png and later runs compare against
    // idle_window.png once it is renamed
    harness.remove_cursor();
    harness.step();
    match harness.try_snapshot("idle_window") {
        Ok(()) | Err(SnapshotError::OpenSnapshot { .. }) => {}
        Err(e) => panic!("{e}"),
    }
}

#[test]
#[ignore = "needs a camera"]
fn builtin_camera_captures() {
    let Some(mut harness) = harness_builtin_camera() else {
        eprintln!("skipped: no usable /dev/video0");
        return;
    };
    wait_frames(&mut harness, 20);
    let status = harness.state().status().to_owned();
    assert!(status.contains("capturing"), "status: {status}");
    assert!(status.contains("/dev/video0"), "status: {status}");
    harness.get_by_role_and_label(Role::Label, &status);

    let before = seq(&harness);
    wait_frames(&mut harness, 10);
    let after = seq(&harness);
    assert!(after > before, "seq {before} -> {after}");

    assert!(
        harness
            .query_by_role_and_label(Role::Slider, "Brightness")
            .is_some(),
        "controls: {:?}",
        harness
            .state()
            .control_infos()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
    );

    click(&mut harness, "stop");
    assert!(!harness.state().capture_running());
    assert_eq!(harness.state().status(), "stopped");
}
