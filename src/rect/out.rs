use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use super::map::{Geometry, ScreenMap};
use super::warp::Tile;
use crate::aim::marks::Stamps;
use crate::aim::Name;

pub const MAGIC: &[u8; 8] = b"TMQARCT1";
pub const HEADER_LEN: usize = 192;
pub const TILE_HEADER_LEN: usize = 32;
/// the tap's ring length, so a reader finds the same seq in both
pub const SLOTS: u64 = 64;
/// h.jsonl moves to h.jsonl.1 after this many lines, 36 min at 30 fps
const LINES: u64 = 65_536;

/// everything known about one screen in one camera frame
#[derive(Clone, Debug)]
pub struct Record {
    pub name: Name,
    /// the tap's seq of the source frame when the tap is on, else the
    /// capture's frame seq
    pub seq: u64,
    pub tap_seq: Option<u64>,
    pub frame_seq: u64,
    /// v4l2 kernel timestamp of the source frame, ns, CLOCK_MONOTONIC
    pub kernel_ns: i64,
    pub camera: (u32, u32),
    pub map: ScreenMap,
    pub geometry: Geometry,
    /// corner fit error, camera pixels: rms and largest
    pub rms: f32,
    pub worst: f32,
    pub ids: Vec<u8>,
    /// corners refined this frame (else the last detection's)
    pub tracked: bool,
    pub lens_fitted: bool,
    pub stamps: Option<Stamps>,
    /// camera row of each mark's centre, for the rolling shutter
    pub rows: Vec<(&'static str, f32)>,
    pub tiles: Vec<Tile>,
}

impl Record {
    /// the fixed header and each tile with its own header; see the readme
    pub fn encode(&self, written_ns: i64, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.frame_seq.to_le_bytes());
        out.extend_from_slice(&self.kernel_ns.to_le_bytes());
        out.extend_from_slice(&written_ns.to_le_bytes());
        out.extend_from_slice(&self.camera.0.to_le_bytes());
        out.extend_from_slice(&self.camera.1.to_le_bytes());
        let scale = self.tiles.first().map_or(0.0, Tile::scale);
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend_from_slice(&(self.tiles.len() as u32).to_le_bytes());
        for v in self.map.h.normalised().0 {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let lens = &self.map.lens;
        for v in [
            lens.k1,
            lens.k2,
            f64::from(lens.centre.x),
            f64::from(lens.centre.y),
            lens.norm,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.rms.to_le_bytes());
        let flags = u32::from(self.tracked) | u32::from(self.lens_fitted) << 1;
        out.extend_from_slice(&flags.to_le_bytes());
        let mask = self.ids.iter().fold(0u32, |m, &id| m | 1 << id);
        out.extend_from_slice(&mask.to_le_bytes());
        out.resize(start + HEADER_LEN, 0);
        for t in &self.tiles {
            let at = out.len();
            let mut name = [0u8; 8];
            for (d, s) in name.iter_mut().zip(t.name.bytes()) {
                *d = s;
            }
            out.extend_from_slice(&name);
            for v in [t.rect.x0, t.rect.y0, t.rect.x1, t.rect.y1] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(t.width as u32).to_le_bytes());
            out.extend_from_slice(&(t.height as u32).to_le_bytes());
            out.resize(at + TILE_HEADER_LEN, 0);
            out.extend_from_slice(&t.data);
        }
    }

    /// one line of h.jsonl
    pub fn json(&self) -> String {
        let g = &self.geometry;
        let h = self
            .map
            .h
            .normalised()
            .0
            .iter()
            .map(|v| format!("{v:.9e}"))
            .collect::<Vec<_>>()
            .join(",");
        let ids = self
            .ids
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let rows = self
            .rows
            .iter()
            .map(|(k, v)| format!("\"{k}\":{v:.2}"))
            .collect::<Vec<_>>()
            .join(",");
        let value = |i: usize| {
            self.stamps
                .and_then(|s| s.values[i])
                .map_or("null".into(), |v| v.to_string())
        };
        let tap = self.tap_seq.map_or("null".into(), |v| v.to_string());
        let lens = &self.map.lens;
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"screen\":\"{}\",\"seq\":{},\"tap_seq\":{tap},\"frame_seq\":{},\"kernel_ns\":{},\
             \"H\":[{h}],\"k1\":{:.6e},\"k2\":{:.6e},\"centre\":[{:.1},{:.1}],\"norm\":{:.1},\
             \"rms_px\":{:.3},\"max_px\":{:.3},\"fids\":[{ids}],\"tracked\":{},\
             \"scale\":{:.4},\"scale_x\":{:.4},\"scale_y\":{:.4},\"rotation_deg\":{:.2},\
             \"skew_deg\":{:.2},\"keystone_h_pct\":{:.2},\"keystone_v_pct\":{:.2},\
             \"view_deg\":{:.1},\"rows\":{{{rows}}},\"N\":{},\"A\":{},\"M\":{}}}",
            self.name.key(),
            self.seq,
            self.frame_seq,
            self.kernel_ns,
            lens.k1,
            lens.k2,
            lens.centre.x,
            lens.centre.y,
            lens.norm,
            self.rms,
            self.worst,
            self.tracked,
            g.scale,
            g.scale_x,
            g.scale_y,
            g.rotation,
            g.skew,
            g.keystone_h,
            g.keystone_v,
            g.view,
            value(0),
            value(1),
            value(2),
        );
        s
    }
}

struct Dir {
    path: PathBuf,
    log: Option<BufWriter<File>>,
    lines: u64,
}

/// the rectified frames beside the tap: `<tap>/rect-<screen>/NNN.rect`
/// slots by seq, a `latest` file and the per-frame maps in h.jsonl
pub struct Writer {
    base: PathBuf,
    dirs: [Option<Dir>; 2],
    scratch: Vec<u8>,
    pub written: u64,
    pub errors: u64,
    pub last_error: Option<String>,
}

impl Writer {
    pub fn new(tap_dir: &Path) -> Writer {
        Writer {
            base: tap_dir.to_path_buf(),
            dirs: [None, None],
            scratch: Vec::new(),
            written: 0,
            errors: 0,
            last_error: None,
        }
    }

    pub fn dir_of(tap_dir: &Path, name: Name) -> PathBuf {
        tap_dir.join(format!("rect-{}", name.key()))
    }

    /// clears what a previous run left, as the tap does
    fn open(&mut self, name: Name) -> io::Result<&mut Dir> {
        let slot = &mut self.dirs[name.index()];
        if slot.is_none() {
            let path = Writer::dir_of(&self.base, name);
            fs::create_dir_all(&path)?;
            for entry in fs::read_dir(&path)? {
                let p = entry?.path();
                let stale = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".rect") || n.ends_with(".tmp") || n == "latest");
                if stale {
                    let _ = fs::remove_file(&p);
                }
            }
            *slot = Some(Dir {
                path,
                log: None,
                lines: 0,
            });
        }
        Ok(slot.as_mut().expect("just set"))
    }

    pub fn write(&mut self, r: &Record) {
        if let Err(e) = self.write_inner(r) {
            self.errors += 1;
            self.last_error = Some(e.to_string());
        } else {
            self.written += 1;
        }
    }

    fn write_inner(&mut self, r: &Record) -> io::Result<()> {
        let mut buf = std::mem::take(&mut self.scratch);
        buf.clear();
        r.encode(crate::capture::tap::monotonic_ns(), &mut buf);
        let line = r.json();
        let dir = self.open(r.name)?;
        let slot = r.seq % SLOTS;
        let tmp = dir.path.join(format!(".{slot:03}.tmp"));
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, dir.path.join(format!("{slot:03}.rect")))?;
        let latest = dir.path.join(".latest.tmp");
        fs::write(&latest, format!("{} {SLOTS}\n", r.seq))?;
        fs::rename(&latest, dir.path.join("latest"))?;
        if dir.log.is_none() || dir.lines >= LINES {
            let path = dir.path.join("h.jsonl");
            if dir.log.is_some() {
                dir.log = None;
                fs::rename(&path, dir.path.join("h.jsonl.1"))?;
            }
            let f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            dir.log = Some(BufWriter::new(f));
            dir.lines = 0;
        }
        if let Some(log) = dir.log.as_mut() {
            writeln!(log, "{line}")?;
            // a reader tails the file, so every line goes out whole
            log.flush()?;
        }
        dir.lines += 1;
        self.scratch = buf;
        Ok(())
    }
}
