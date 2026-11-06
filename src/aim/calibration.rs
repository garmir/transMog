use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::detect::Screen;
use super::fit::Ptz;

/// calibration.json as the latency tools' stamps.py reads it: one entry
/// a screen under "proj" or "mini", with "layout": "marks", the frame to
/// camera homography "H" and the stamp block's camera corners. the
/// screen's outline, the framing and the decode rate ride along in keys
/// stamps.py does not read
pub fn to_json(screens: &[(&Screen, f32)], size: (u32, u32), ptz: Ptz, saved: &str) -> String {
    let pt = |p: egui::Pos2| format!("[{:.2}, {:.2}]", p.x, p.y);
    let quad = |q: &[egui::Pos2; 4]| q.iter().map(|&p| pt(p)).collect::<Vec<_>>().join(", ");
    let mut out = String::from("{\n");
    for (i, (s, rate)) in screens.iter().enumerate() {
        let h =
            s.h.rows()
                .iter()
                .map(|r| format!("[{:.9e}, {:.9e}, {:.9e}]", r[0], r[1], r[2]))
                .collect::<Vec<_>>()
                .join(", ");
        let fids = s
            .seen_ids()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            out,
            " \"{}\": {{\n  \"layout\": \"marks\",\n  \"H\": [{h}],\n  \"corners\": [{}],\n  \"thr\": 128.0,\n  \"fids\": [{fids}],\n  \"module_px\": {:.2},\n  \"fit_err_px\": {:.2},\n  \"by\": \"transmog aim\",\n  \"screen\": [{}],\n  \"camera\": [{}, {}],\n  \"zoom_absolute\": {},\n  \"pan_absolute\": {},\n  \"tilt_absolute\": {},\n  \"decode_rate\": {:.3},\n  \"saved\": \"{saved}\"\n }}{}\n",
            s.name.key(),
            quad(&s.stamp_block),
            s.module_px,
            s.fit_err,
            quad(&s.outline),
            size.0,
            size.1,
            ptz.zoom,
            ptz.pan,
            ptz.tilt,
            rate,
            if i + 1 < screens.len() { "," } else { "" },
        );
    }
    out.push_str("}\n");
    out
}

/// local time as "2026-11-05 22:41:07", and the same without spaces for
/// a file name
pub fn local_time() -> (String, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r writes only into `tm`
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    let (y, mo, d, h, mi, s) = (
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    );
    (
        format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"),
        format!("{y}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"),
    )
}

/// writes `json` to `path` through a dot file and a rename, after
/// copying any file already there to `path.STAMP.bak`; returns the
/// backup's path
pub fn save(path: &Path, json: &str, stamp: &str) -> Result<Option<PathBuf>> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let backup = if path.exists() {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{stamp}.bak"));
        let backup = path.with_file_name(name);
        fs::copy(path, &backup).with_context(|| format!("back up to {}", backup.display()))?;
        Some(backup)
    } else {
        None
    };
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    let tmp = path.with_file_name(name);
    fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(backup)
}
