//! the lens panel end to end: widgets driven through kittest, the
//! fisheye pass read back through `App::passes`, and the pass's pixels
//! on a synthetic frame
mod common;

use std::time::Instant;

use eframe::egui_wgpu::RenderState;
use egui::accesskit::Toggled;
use egui::{Key, Role};
use egui_kittest::kittest::{NodeT, Queryable};
use transmog::capture::{Frame, PixelLayout};
use transmog::fx::lens::fisheye::{Edge, Fisheye, Projection};
use transmog::render::{Pass, Renderer};

use common::*;

/// the app with its lens widgets; None without a renderer, which is
/// the nix sandbox, where the panel only says "no renderer"
fn lens_harness() -> Option<AppHarness> {
    let mut harness = harness_no_camera();
    harness.step();
    if harness.state().passes().is_empty() {
        eprintln!("skipped: no renderer, so no lens panel");
        return None;
    }
    Some(harness)
}

fn fisheye(harness: &AppHarness) -> &Fisheye {
    harness.state().passes()[0]
        .as_any()
        .downcast_ref::<Fisheye>()
        .expect("the first pass is the fisheye")
}

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "got {actual}, expected {expected}"
    );
}

/// types `text` into the number field beside the slider named `label`
/// and confirms it with enter. the field becomes a text edit on the
/// frame after it gains focus, with its text selected, so the typed
/// text replaces the old value
fn type_value(harness: &mut AppHarness, label: &str, text: &str) {
    harness
        .get_by_role_and_label(Role::SpinButton, label)
        .focus();
    harness.step();
    harness.event(egui::Event::Text(text.to_owned()));
    harness.key_press(Key::Enter);
    harness.run_steps(3);
}

fn toggle(harness: &mut AppHarness, label: &str) {
    harness.get_by_role_and_label(Role::CheckBox, label).click();
    harness.step();
}

fn checked(harness: &AppHarness, label: &str) -> bool {
    harness
        .get_by_role_and_label(Role::CheckBox, label)
        .accesskit_node()
        .toggled()
        == Some(Toggled::True)
}

fn slider_enabled(harness: &AppHarness, label: &str) -> bool {
    !harness
        .get_by_role_and_label(Role::Slider, label)
        .accesskit_node()
        .is_disabled()
}

/// the centre fields are the only number fields without a label; the
/// sliders' fields carry their slider's text
fn set_centre(harness: &mut AppHarness, axis: usize, value: f64) {
    harness
        .get_all_by_role(Role::SpinButton)
        .filter(|n| n.accesskit_node().label().is_none())
        .nth(axis)
        .expect("centre field")
        .set_value_accesskit(value);
    harness.step();
}

#[test]
fn every_projection_is_selectable() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    let mut current = fisheye(&harness).projection();
    assert_eq!(current, Projection::Equisolid);
    for projection in Projection::ALL {
        if projection != current {
            combo_select(&mut harness, current.label(), projection.label());
        }
        assert_eq!(fisheye(&harness).projection(), projection);
        current = projection;
    }
}

#[test]
fn lens_angle_typed_and_clamped() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    for degrees in [1.0, 90.0, 180.0, 360.0] {
        type_value(&mut harness, "lens angle", &format!("{degrees}"));
        close(fisheye(&harness).lens_fov(), degrees);
    }
    type_value(&mut harness, "lens angle", "0");
    close(fisheye(&harness).lens_fov(), 1.0);
    type_value(&mut harness, "lens angle", "400");
    close(fisheye(&harness).lens_fov(), 360.0);
}

#[test]
fn camera_angle_clamps() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    for degrees in [10.0, 70.0, 170.0] {
        set_slider(&mut harness, "camera angle", degrees);
        close(fisheye(&harness).source_fov(), degrees as f32);
    }
    set_slider(&mut harness, "camera angle", 5.0);
    close(fisheye(&harness).source_fov(), 10.0);
    set_slider(&mut harness, "camera angle", 200.0);
    close(fisheye(&harness).source_fov(), 170.0);
}

#[test]
fn fit_checkbox_gates_the_zoom_slider() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    assert!(fisheye(&harness).fit());
    assert!(checked(&harness, "fit source to width"));
    assert!(!slider_enabled(&harness, "zoom"));

    toggle(&mut harness, "fit source to width");
    assert!(!fisheye(&harness).fit());
    assert!(!checked(&harness, "fit source to width"));
    assert!(slider_enabled(&harness, "zoom"));

    for zoom in [0.1, 4.0] {
        set_slider(&mut harness, "zoom", zoom);
        close(fisheye(&harness).zoom(), zoom as f32);
        close(fisheye(&harness).effective_zoom(), zoom as f32);
    }
    set_slider(&mut harness, "zoom", 0.05);
    close(fisheye(&harness).zoom(), 0.1);
    set_slider(&mut harness, "zoom", 10.0);
    close(fisheye(&harness).zoom(), 4.0);

    toggle(&mut harness, "fit source to width");
    assert!(fisheye(&harness).fit());
    assert!(!slider_enabled(&harness, "zoom"));
}

#[test]
fn k1_and_rotation_sliders() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    for k1 in [-1.0, 0.0, 1.0] {
        set_slider(&mut harness, "barrel k1", k1);
        close(fisheye(&harness).k1(), k1 as f32);
    }
    for (axis, label) in ["yaw", "pitch", "roll"].into_iter().enumerate() {
        for degrees in [-180.0, 0.0, 180.0] {
            set_slider(&mut harness, label, degrees);
            close(fisheye(&harness).rotation()[axis], degrees as f32);
        }
        set_slider(&mut harness, label, 0.0);
    }
}

#[test]
fn edge_combo_cycles() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    assert_eq!(fisheye(&harness).edge(), Edge::Black);
    combo_select(&mut harness, "black", "clamp");
    assert_eq!(fisheye(&harness).edge(), Edge::Clamp);
    combo_select(&mut harness, "clamp", "mirror");
    assert_eq!(fisheye(&harness).edge(), Edge::Mirror);
    combo_select(&mut harness, "mirror", "black");
    assert_eq!(fisheye(&harness).edge(), Edge::Black);
}

#[test]
fn centre_fields_clamp() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    for axis in 0..2 {
        for value in [-0.5, 0.0, 0.5] {
            set_centre(&mut harness, axis, value);
            close(fisheye(&harness).centre()[axis], value as f32);
        }
        set_centre(&mut harness, axis, -0.7);
        close(fisheye(&harness).centre()[axis], -0.5);
        set_centre(&mut harness, axis, 0.7);
        close(fisheye(&harness).centre()[axis], 0.5);
        set_centre(&mut harness, axis, 0.0);
    }
}

#[test]
fn reset_restores_defaults() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    combo_select(&mut harness, "equisolid", "rectilinear");
    set_slider(&mut harness, "lens angle", 90.0);
    set_slider(&mut harness, "camera angle", 120.0);
    toggle(&mut harness, "fit source to width");
    set_slider(&mut harness, "zoom", 2.0);
    set_slider(&mut harness, "barrel k1", 0.5);
    set_slider(&mut harness, "yaw", 30.0);
    set_slider(&mut harness, "pitch", -30.0);
    set_slider(&mut harness, "roll", 45.0);
    combo_select(&mut harness, "black", "mirror");
    set_centre(&mut harness, 0, 0.2);
    set_centre(&mut harness, 1, -0.2);
    {
        let f = fisheye(&harness);
        assert_eq!(f.projection(), Projection::Rectilinear);
        close(f.lens_fov(), 90.0);
        close(f.source_fov(), 120.0);
        assert!(!f.fit());
        close(f.zoom(), 2.0);
        close(f.k1(), 0.5);
        assert_eq!(f.rotation(), [30.0, -30.0, 45.0]);
        assert_eq!(f.edge(), Edge::Mirror);
        close(f.centre()[0], 0.2);
        close(f.centre()[1], -0.2);
    }

    click(&mut harness, "reset");
    // the widgets above the button drew the old values on the frame
    // that reset them
    harness.step();
    let f = fisheye(&harness);
    assert_eq!(f.projection(), Projection::Equisolid);
    close(f.lens_fov(), 180.0);
    close(f.source_fov(), 70.0);
    assert!(f.fit());
    close(f.zoom(), 1.0);
    close(f.k1(), 0.0);
    assert_eq!(f.rotation(), [0.0, 0.0, 0.0]);
    assert_eq!(f.edge(), Edge::Black);
    assert_eq!(f.centre(), [0.0, 0.0]);
    assert!(f.enabled());
    assert!(checked(&harness, "fit source to width"));
    assert!(!slider_enabled(&harness, "zoom"));
}

#[test]
fn unchecking_fisheye_disables_the_pass() {
    let Some(mut harness) = lens_harness() else {
        return;
    };
    assert!(fisheye(&harness).enabled());
    assert!(checked(&harness, "fisheye"));
    toggle(&mut harness, "fisheye");
    assert!(!fisheye(&harness).enabled());
    assert!(!checked(&harness, "fisheye"));
    // the parameter widgets stay reachable while the pass is off
    harness.get_by_role_and_label(Role::Slider, "lens angle");
    toggle(&mut harness, "fisheye");
    assert!(fisheye(&harness).enabled());
}

// pixel cases. the app has no way to inject a frame, so the pass the
// widgets edited is mirrored onto a second fisheye on a headless
// renderer, parameter for parameter, and the synthetic frame goes
// through that

const W: u32 = 64;
const H: u32 = 32;
/// rows of the grey bar
const BAR: std::ops::Range<u32> = 12..20;

/// yuyv: left half white, right half black, a grey bar across the
/// middle rows
fn test_frame() -> Frame {
    let mut data = Vec::with_capacity((W * H * 2) as usize);
    for y in 0..H {
        for x in 0..W {
            let luma = if BAR.contains(&y) {
                128
            } else if x < W / 2 {
                235
            } else {
                16
            };
            data.push(luma);
            data.push(128);
        }
    }
    Frame {
        width: W,
        height: H,
        layout: PixelLayout::Yuyv,
        data,
        seq: 1,
        received: Instant::now(),
    }
}

fn copy_params(from: &Fisheye, to: &mut Fisheye) {
    to.set_enabled(from.enabled());
    to.set_projection(from.projection());
    to.set_lens_fov(from.lens_fov());
    to.set_source_fov(from.source_fov());
    to.set_fit(from.fit());
    to.set_zoom(from.zoom());
    to.set_k1(from.k1());
    to.set_rotation(from.rotation());
    to.set_edge(from.edge());
    to.set_centre(from.centre());
}

/// reads a texture back as tightly packed rgba8 rows
fn read_back(
    rs: &RenderState,
    mut encoder: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
) -> Vec<u8> {
    let padded_row =
        (W * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = rs.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size: u64::from(padded_row * H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    rs.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).ok();
    });
    rs.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    rx.recv().expect("map callback").expect("map");
    let view = slice.get_mapped_range().expect("mapped range");
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for row in view.chunks_exact(padded_row as usize) {
        out.extend_from_slice(&row[..(W * 4) as usize]);
    }
    out
}

/// the test frame through a fisheye set up like `source`
fn render_like(rs: &RenderState, source: &Fisheye) -> Vec<u8> {
    let mut renderer = Renderer::new(&rs.device, &rs.queue);
    let mut fisheye = Fisheye::new(&rs.device, renderer.input_layout());
    copy_params(source, &mut fisheye);
    renderer.passes_mut().push(Box::new(fisheye));
    renderer.upload(&rs.device, &rs.queue, &test_frame());
    let mut encoder = rs.device.create_command_encoder(&Default::default());
    renderer.render(&rs.device, &rs.queue, &mut encoder);
    let texture = renderer.output_texture().expect("output after upload");
    let pixels = read_back(rs, encoder, texture);
    assert_eq!(pixels.len(), (W * H * 4) as usize);
    assert_sane(&pixels);
    pixels
}

/// opaque everywhere and never the magenta a nan turns into
fn assert_sane(pixels: &[u8]) {
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(px[3], 255, "pixel {i} alpha in {px:?}");
        assert!(
            !(px[0] == 255 && px[1] == 0 && px[2] == 255),
            "pixel {i} is magenta"
        );
    }
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> &[u8] {
    let i = ((y * W + x) * 4) as usize;
    &pixels[i..i + 4]
}

fn is_black(px: &[u8]) -> bool {
    px[..3].iter().all(|c| *c <= 2)
}

fn is_grey(px: &[u8]) -> bool {
    px[..3].iter().all(|c| (i32::from(*c) - 130).abs() <= 2)
}

fn headless() -> RenderState {
    render_state().expect("no gpu adapter")
}

/// a rectilinear lens that sees exactly the camera's angle maps every
/// pixel onto itself, so the pass changes nothing against the pass off
#[test]
#[ignore = "needs a gpu"]
fn rectilinear_at_the_camera_angle_is_identity() {
    let mut harness = lens_harness().expect("no gpu adapter");
    let rs = headless();
    combo_select(&mut harness, "equisolid", "rectilinear");
    type_value(&mut harness, "lens angle", "70");
    toggle(&mut harness, "fit source to width");
    {
        let f = fisheye(&harness);
        assert_eq!(f.projection(), Projection::Rectilinear);
        close(f.lens_fov(), f.source_fov());
        assert!(!f.fit());
        close(f.effective_zoom(), 1.0);
        assert!(f.enabled());
    }
    let with = render_like(&rs, fisheye(&harness));

    toggle(&mut harness, "fisheye");
    assert!(!fisheye(&harness).enabled());
    let without = render_like(&rs, fisheye(&harness));

    // the frame itself came through, so equality is not two blanks
    assert_eq!(&pixel(&without, 8, 4)[..3], [255, 255, 255]);
    assert!(is_black(pixel(&without, 56, 4)));
    assert!(is_grey(pixel(&without, 8, 16)));
    for (i, (a, b)) in with
        .chunks_exact(4)
        .zip(without.chunks_exact(4))
        .enumerate()
    {
        for c in 0..3 {
            assert!(
                (i32::from(a[c]) - i32::from(b[c])).abs() <= 2,
                "pixel {i}: {a:?} with the pass, {b:?} without"
            );
        }
    }
}

/// turned right around, every ray leaves through the back of the
/// source, which is black in every edge mode
#[test]
#[ignore = "needs a gpu"]
fn yaw_180_shows_nothing() {
    let mut harness = lens_harness().expect("no gpu adapter");
    let rs = headless();
    set_slider(&mut harness, "yaw", 180.0);
    assert_eq!(fisheye(&harness).rotation(), [180.0, 0.0, 0.0]);
    let pixels = render_like(&rs, fisheye(&harness));
    let lit = pixels.chunks_exact(4).filter(|px| !is_black(px)).count();
    assert_eq!(lit, 0, "{lit} pixels are not black");
}

/// yawed 60 degrees the centre of the output looks past the source's
/// right edge: black leaves that region black, mirror reflects the
/// source into it, and the reflection is not what clamp shows
#[test]
#[ignore = "needs a gpu"]
fn mirror_edge_reflects_the_source() {
    let mut harness = lens_harness().expect("no gpu adapter");
    let rs = headless();
    set_slider(&mut harness, "yaw", 60.0);
    assert_eq!(fisheye(&harness).rotation(), [60.0, 0.0, 0.0]);
    assert_eq!(fisheye(&harness).edge(), Edge::Black);
    let black = render_like(&rs, fisheye(&harness));

    combo_select(&mut harness, "black", "mirror");
    assert_eq!(fisheye(&harness).edge(), Edge::Mirror);
    let mirror = render_like(&rs, fisheye(&harness));

    combo_select(&mut harness, "mirror", "clamp");
    assert_eq!(fisheye(&harness).edge(), Edge::Clamp);
    let clamp = render_like(&rs, fisheye(&harness));

    let mirrored: Vec<usize> = (0..(W * H) as usize)
        .filter(|&i| is_black(&black[i * 4..i * 4 + 4]) && !is_black(&mirror[i * 4..i * 4 + 4]))
        .collect();
    assert!(!mirrored.is_empty(), "no mirrored region");
    assert!(
        mirrored
            .iter()
            .any(|&i| mirror[i * 4..i * 4 + 3] != clamp[i * 4..i * 4 + 3]),
        "the mirrored region matches the clamped one in all {} pixels",
        mirrored.len()
    );
}

/// a 360 degree equidistant lens at zoom 1 puts the corners past the
/// back of the source while the centre still sees the grey bar
#[test]
#[ignore = "needs a gpu"]
fn equidistant_360_leaves_the_corners_black() {
    let mut harness = lens_harness().expect("no gpu adapter");
    let rs = headless();
    combo_select(&mut harness, "equisolid", "equidistant");
    type_value(&mut harness, "lens angle", "360");
    toggle(&mut harness, "fit source to width");
    {
        let f = fisheye(&harness);
        assert_eq!(f.projection(), Projection::Equidistant);
        close(f.lens_fov(), 360.0);
        assert!(!f.fit());
        close(f.effective_zoom(), 1.0);
    }
    let pixels = render_like(&rs, fisheye(&harness));
    for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        let px = pixel(&pixels, x, y);
        assert!(is_black(px), "corner ({x}, {y}) is {px:?}");
    }
    let centre = pixel(&pixels, W / 2, H / 2);
    assert!(!is_black(centre), "centre is {centre:?}");
    assert!(is_grey(centre), "centre is {centre:?}");
}
