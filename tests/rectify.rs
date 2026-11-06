//! rectification on synthetic camera frames: the marks drawn through a
//! known homography and lens, area-sampled as a sensor would, then the
//! corners, maps and stamps recovered
use egui::{pos2, Pos2};
use transmog::aim::detect::squares;
use transmog::aim::geom::Homography;
use transmog::aim::marks::{self, mark_at, Rect, FIDS, FRAME, STAMPS};
use transmog::aim::{analyse, Luma, Name};
use transmog::rect::edges::inset;
use transmog::rect::map::{self, Lens, Points, ScreenMap};
use transmog::rect::out::{Record, Writer, HEADER_LEN, MAGIC, TILE_HEADER_LEN};
use transmog::rect::track::ScreenTrack;
use transmog::rect::warp;
use transmog::rect::Geometry;

const SIZE: (u32, u32) = (1920, 1080);
/// subsamples a side per camera pixel
const SUB: usize = 3;

struct Shown {
    h: Homography,
    white: u8,
    black: u8,
    values: [u16; 3],
}

/// the frame's corners land on `quad` (top left, top right, bottom
/// right, bottom left), which sets the keystone and the skew
fn onto(quad: [Pos2; 4]) -> Homography {
    let pairs: Vec<(Pos2, Pos2)> = FRAME.corners().into_iter().zip(quad).collect();
    Homography::projective(&pairs).expect("a quad")
}

fn projector() -> Shown {
    Shown {
        h: onto([
            pos2(420.0, 60.0),
            pos2(1320.0, 25.0),
            pos2(1345.0, 560.0),
            pos2(430.0, 540.0),
        ]),
        white: 240,
        black: 20,
        values: [40123, 2211, 17],
    }
}

/// at the mini's real size, about 0.15 camera px a frame px, seen from
/// the side
fn mini() -> Shown {
    Shown {
        h: onto([
            pos2(560.0, 700.0),
            pos2(860.0, 688.0),
            pos2(852.0, 882.0),
            pos2(566.0, 868.0),
        ]),
        white: 180,
        black: 5,
        values: [5000, 2210, 17],
    }
}

/// a dark room with each screen drawn through its map and `lens`, every
/// camera pixel the mean of SUB x SUB points, then a 3x3 box blur when
/// `blur` is set, for the c920's soft focus
fn render(screens: &[Shown], lens: Lens, blur: bool) -> Luma {
    let (w, h) = (SIZE.0 as usize, SIZE.1 as usize);
    let mut luma = Luma::new(w, h, 8);
    for s in screens {
        let inv = s.h.inverse().expect("invertible");
        let outline = s.h.apply_all(FRAME.corners()).map(|p| lens.distort(p));
        let b = transmog::aim::geom::bounds(outline).expect("four points");
        let (x0, y0) = (b.min.x.max(0.0) as usize, b.min.y.max(0.0) as usize);
        let (x1, y1) = (
            (b.max.x.ceil() as usize + 2).min(w),
            (b.max.y.ceil() as usize + 2).min(h),
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let (mut sum, mut n) = (0.0f32, 0);
                for j in 0..SUB {
                    for i in 0..SUB {
                        let q = pos2(
                            x as f32 + (i as f32 + 0.5) / SUB as f32,
                            y as f32 + (j as f32 + 0.5) / SUB as f32,
                        );
                        let f = inv.apply(lens.undistort(q));
                        if !FRAME.contains(f.x, f.y) {
                            continue;
                        }
                        n += 1;
                        sum += match mark_at(f.x, f.y, s.values) {
                            Some(true) => f32::from(s.white),
                            Some(false) => f32::from(s.black),
                            None => 30.0,
                        };
                    }
                }
                if n > 0 {
                    let outside = (SUB * SUB - n) as f32 * 8.0;
                    luma.data[y * w + x] = ((sum + outside) / (SUB * SUB) as f32).round() as u8;
                }
            }
        }
    }
    if blur {
        let src = luma.data.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut s = 0u32;
                for dy in 0..3 {
                    for dx in 0..3 {
                        s += u32::from(src[(y + dy - 1) * w + x + dx - 1]);
                    }
                }
                luma.data[y * w + x] = ((s + 4) / 9) as u8;
            }
        }
    }
    luma
}

/// where frame pixel `p` of `s` really lands
fn truth(s: &Shown, lens: Lens, p: Pos2) -> Pos2 {
    lens.distort(s.h.apply(p))
}

fn tracks(luma: &Luma) -> Vec<ScreenTrack> {
    let squares = squares(luma);
    let mut out: Vec<ScreenTrack> = analyse(luma, 1.0)
        .iter()
        .filter_map(|s| ScreenTrack::start(luma, s, &squares))
        .collect();
    out.sort_by_key(|t| t.name);
    out
}

fn track(tracks: &[ScreenTrack], name: Name) -> &ScreenTrack {
    tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("{} not tracked", name.label()))
}

/// largest distance of the track's corners from where they really are
fn corner_error(t: &ScreenTrack, s: &Shown, lens: Lens) -> f32 {
    let mut worst = 0f32;
    for m in &t.markers {
        let outer = FIDS[m.fid].rect.corners();
        for (f, c) in outer.iter().zip(m.corners.outer) {
            worst = worst.max(c.distance(truth(s, lens, *f)));
        }
        for (f, c) in inset(outer).iter().zip(m.corners.inner) {
            worst = worst.max(c.distance(truth(s, lens, *f)));
        }
    }
    worst
}

#[test]
fn corners_refine_to_half_a_pixel() {
    let shown = [projector(), mini()];
    let lens = Lens::none(SIZE);
    for blur in [false, true] {
        let luma = render(&shown, lens, blur);
        let tracks = tracks(&luma);
        for (name, s) in [(Name::Projector, &shown[0]), (Name::Mini, &shown[1])] {
            let t = track(&tracks, name);
            assert_eq!(t.ids(), vec![0, 1, 2, 3], "{}", name.label());
            let e = corner_error(t, s, lens);
            assert!(e < 0.5, "{} blur {blur}: {e:.3} px", name.label());
        }
    }
}

/// every point of `rects` through the fitted map against the truth
fn map_error(m: &ScreenMap, s: &Shown, lens: Lens, rects: &[Rect]) -> f32 {
    rects
        .iter()
        .flat_map(|r| r.corners())
        .map(|p| m.apply(p).distance(truth(s, lens, p)))
        .fold(0.0, f32::max)
}

#[test]
fn lens_fit_recovers_barrel_across_both_screens() {
    let shown = [projector(), mini()];
    let lens = Lens {
        k1: -0.08,
        k2: 0.0,
        ..Lens::none(SIZE)
    };
    let luma = render(&shown, lens, true);
    let tracks = tracks(&luma);
    let pts: Vec<Points> = tracks.iter().map(ScreenTrack::points).collect();
    let (fit, before, after) = map::fit_lens(&pts, SIZE).expect("two screens of four markers");
    assert!(after < 0.6 * before, "rms {before:.3} -> {after:.3}");
    assert!((fit.k1 - lens.k1).abs() < 0.03, "{fit:?}");
    let maps = map::fit_maps(&pts, fit);
    let rects = [STAMPS, FIDS[0].rect, FIDS[2].rect, marks::BOTTOM.0];
    for ((t, m), s) in tracks.iter().zip(&maps).zip(&shown) {
        let m = m.expect("a map");
        let e = map_error(&m, s, lens, &rects);
        assert!(e < 0.5, "{}: {e:.3} px", t.name.label());
    }
}

/// the mini turned well away from the camera and leaning: 20 % keystone
/// and a few degrees of skew, at its real size
fn steep_mini() -> Shown {
    Shown {
        h: onto([
            pos2(560.0, 690.0),
            pos2(820.0, 712.0),
            pos2(842.0, 856.0),
            pos2(548.0, 878.0),
        ]),
        ..mini()
    }
}

#[test]
fn keystoned_and_skewed_stamps_decode_from_the_tiles() {
    let s = steep_mini();
    let lens = Lens::none(SIZE);
    let luma = render(std::slice::from_ref(&s), lens, true);
    let tracks = tracks(&luma);
    let t = track(&tracks, Name::Mini);
    let pts = t.points();
    let m = map::fit_maps(std::slice::from_ref(&pts), lens)[0].expect("a map");
    let g = Geometry::of(&m.h, SIZE, 1.0);
    assert!(g.keystone_h.abs() > 10.0, "{g:?}");
    assert!(g.skew.abs() > 2.0, "{g:?}");
    let tiles: Vec<_> = warp::regions()
        .into_iter()
        .map(|(name, r)| warp::warp(&luma, &m, name, r, 0.5))
        .collect();
    let stamps = marks::decode(|r| warp::level(&tiles, r).unwrap_or(0.0));
    assert_eq!(stamps.values, s.values.map(Some), "{stamps:?}");
    // the rectified tile is straight: the stamps' start cells line up
    // with the table
    let stamp_tile = tiles.iter().find(|t| t.name == "stamps").expect("a tile");
    let start = marks::stamp_cell(0, 0).inner(0.25);
    let quiet = Rect::new(STAMPS.x0 - 12.0, STAMPS.y0 + 4.0, 8.0, 30.0);
    assert!(stamp_tile.level(start).unwrap() > 150.0);
    assert!(stamp_tile.level(quiet).unwrap() < 30.0);
}

#[test]
fn tracking_smooths_a_small_move_and_takes_a_jump() {
    let base = mini();
    let lens = Lens::none(SIZE);
    let moved = |dx: f32| Shown {
        h: Homography([1.0, 0.0, f64::from(dx), 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]).then(&base.h),
        ..mini()
    };
    let first = render(std::slice::from_ref(&base), lens, true);
    let mut t = track(&tracks(&first), Name::Mini).clone();
    assert_eq!(t.ids(), vec![0, 1, 2, 3]);
    let small = moved(0.4);
    let frame = render(std::slice::from_ref(&small), lens, true);
    for _ in 0..4 {
        assert!(t.update(&frame));
    }
    let e = corner_error(&t, &small, lens);
    assert!(e < 0.5, "after a 0.4 px move: {e:.3} px");
    // past the smoothing but inside the profiles: taken at once
    let jump = moved(2.0);
    let frame = render(std::slice::from_ref(&jump), lens, true);
    assert!(t.update(&frame));
    let e = corner_error(&t, &jump, lens);
    assert!(e < 0.5, "after a 2 px jump: {e:.3} px");
    // further than a profile reaches: the markers miss, and after three
    // frames the track asks for a search
    let far = render(std::slice::from_ref(&moved(12.0)), lens, true);
    let alive: Vec<bool> = (0..3).map(|_| t.update(&far)).collect();
    assert_eq!(alive, vec![true, true, false]);
}

#[test]
fn record_encodes_its_header_and_tiles() {
    let s = mini();
    let lens = Lens::none(SIZE);
    let luma = render(std::slice::from_ref(&s), lens, false);
    let map = ScreenMap { h: s.h, lens };
    let tiles: Vec<_> = warp::regions()
        .into_iter()
        .map(|(name, r)| warp::warp(&luma, &map, name, r, 0.5))
        .collect();
    let record = Record {
        name: Name::Mini,
        seq: 70,
        tap_seq: Some(70),
        frame_seq: 69,
        kernel_ns: 123_456_789,
        camera: SIZE,
        map,
        geometry: Geometry::of(&s.h, SIZE, 1.0),
        rms: 0.2,
        worst: 0.4,
        ids: vec![0, 1, 3],
        tracked: true,
        lens_fitted: false,
        stamps: None,
        rows: vec![("N", 700.0)],
        tiles: tiles.clone(),
    };
    let mut out = Vec::new();
    record.encode(5, &mut out);
    let u32_at = |i: usize| u32::from_le_bytes(out[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(out[i..i + 8].try_into().unwrap());
    let f64_at = |i: usize| f64::from_le_bytes(out[i..i + 8].try_into().unwrap());
    assert_eq!(&out[..8], MAGIC);
    assert_eq!(u64_at(8), 70);
    assert_eq!(u64_at(24), 123_456_789);
    assert_eq!((u32_at(40), u32_at(44)), SIZE);
    assert_eq!(u32_at(52) as usize, tiles.len());
    let h = s.h.normalised().0;
    assert!((f64_at(56) - h[0]).abs() < 1e-12 && (f64_at(120) - 1.0).abs() < 1e-12);
    assert_eq!(u32_at(172), 1);
    assert_eq!(u32_at(176), 0b1011);
    let first = &tiles[0];
    assert_eq!(&out[HEADER_LEN..HEADER_LEN + 5], b"ruler");
    assert_eq!(u32_at(HEADER_LEN + 24) as usize, first.width);
    let body = HEADER_LEN + TILE_HEADER_LEN;
    assert_eq!(&out[body..body + first.data.len()], &first.data[..]);
    let total: usize = tiles.iter().map(|t| TILE_HEADER_LEN + t.data.len()).sum();
    assert_eq!(out.len(), HEADER_LEN + total);
    let line = record.json();
    assert!(
        line.starts_with("{\"screen\":\"mini\",\"seq\":70,"),
        "{line}"
    );
    assert!(line.contains("\"rows\":{\"N\":700.00}"), "{line}");
}

#[test]
fn writer_puts_slots_latest_and_lines_beside_the_tap() {
    let s = mini();
    let lens = Lens::none(SIZE);
    let luma = render(std::slice::from_ref(&s), lens, false);
    let map = ScreenMap { h: s.h, lens };
    let dir = std::env::temp_dir().join(format!("transmog-rect-test-{}", std::process::id()));
    let mut w = Writer::new(&dir);
    for seq in [1u64, 2, 66] {
        w.write(&Record {
            name: Name::Mini,
            seq,
            tap_seq: Some(seq),
            frame_seq: seq,
            kernel_ns: 1,
            camera: SIZE,
            map,
            geometry: Geometry::of(&s.h, SIZE, 1.0),
            rms: 0.1,
            worst: 0.2,
            ids: vec![0, 3],
            tracked: true,
            lens_fitted: false,
            stamps: None,
            rows: Vec::new(),
            tiles: vec![warp::warp(&luma, &map, "stamps", STAMPS, 0.5)],
        });
    }
    assert_eq!((w.written, w.errors), (3, 0), "{:?}", w.last_error);
    let sub = Writer::dir_of(&dir, Name::Mini);
    assert_eq!(
        std::fs::read_to_string(sub.join("latest")).unwrap(),
        "66 64\n"
    );
    let slot = std::fs::read(sub.join("002.rect")).unwrap();
    assert_eq!(u64::from_le_bytes(slot[8..16].try_into().unwrap()), 66);
    let lines = std::fs::read_to_string(sub.join("h.jsonl")).unwrap();
    assert_eq!(lines.lines().count(), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

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
    for q in squares(&luma) {
        eprintln!("square {:?} side {:.1}", q.centre(), q.side);
    }
    let tracks = tracks(&luma);
    let pts: Vec<Points> = tracks.iter().map(ScreenTrack::points).collect();
    let lens = map::fit_lens(&pts, SIZE);
    eprintln!("lens fit {lens:?}");
    for (t, m) in tracks.iter().zip(map::fit_maps(&pts, Lens::none(SIZE))) {
        let Some(m) = m else { continue };
        let tiles: Vec<_> = warp::regions()
            .into_iter()
            .map(|(name, r)| warp::warp(&luma, &m, name, r, 0.5))
            .collect();
        let stamps = marks::decode(|r| warp::level(&tiles, r).unwrap_or(0.0));
        let rms: Vec<f32> = t.markers.iter().map(|k| k.corners.rms).collect();
        eprintln!(
            "{} fids {:?} edge rms {rms:?} fit {:?} stamps {stamps:?}\n  {:?}",
            t.name.label(),
            t.ids(),
            map::residual(&m, &t.points().pairs),
            Geometry::of(&m.h, SIZE, 1.0)
        );
    }
}
