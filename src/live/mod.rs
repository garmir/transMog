//! live control: `key value` pairs from a polled file and from a
//! websocket subscription. a source's pair is applied only when its
//! value differs from the last one that source sent, so a source that
//! repeats its whole state every frame never undoes a change made in the
//! window

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

pub mod json;
pub mod ws;

/// the controls the qa preset fixes. the latency ruler decodes stamp
/// cells off the camera picture and needs the exposure to stay put
pub fn is_locked(key: &str) -> bool {
    matches!(
        key,
        "exposure_time_absolute"
            | "auto_exposure"
            | "exposure_dynamic_framerate"
            | "gain"
            | "focus_absolute"
            | "focus_automatic_continuous"
            | "power_line_frequency"
    ) || key.starts_with("white_balance_")
}

/// the preset whose values the lock holds
pub const LOCK_PRESET: &str = "qa";

/// one `key value` a line; `#` starts a comment, and a key given twice
/// keeps its last value at its last place
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        out.retain(|(k, _)| k != key);
        out.push((key.to_string(), value.to_string()));
    }
    out
}

/// the last value a source sent for each key
#[derive(Debug, Default)]
pub struct Seen(HashMap<String, String>);

impl Seen {
    /// the pairs whose value differs from what this source sent before
    pub fn changed(&mut self, pairs: &[(String, String)]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (key, value) in pairs {
            if self.0.get(key) != Some(value) {
                self.0.insert(key.clone(), value.clone());
                out.push((key.clone(), value.clone()));
            }
        }
        out
    }
}

/// where a change came from, for the state file
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    File,
    /// the message's seq when it had one
    Ws(Option<u64>),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::File => f.write_str("file"),
            Source::Ws(Some(seq)) => write!(f, "ws:{seq}"),
            Source::Ws(None) => f.write_str("ws"),
        }
    }
}

/// the file's changes in file order, then the websocket's for keys the
/// file did not change in the same tick
pub fn merge(
    file: Vec<(String, String)>,
    ws: Vec<(String, String)>,
    seq: Option<u64>,
) -> Vec<(String, String, Source)> {
    let mut out: Vec<(String, String, Source)> = file
        .into_iter()
        .map(|(k, v)| (k, v, Source::File))
        .collect();
    for (k, v) in ws {
        if !out.iter().any(|(fk, _, _)| *fk == k) {
            out.push((k, v, Source::Ws(seq)));
        }
    }
    out
}

/// an integer: a whole or fractional number (rounded), or on, off,
/// true, false
pub fn int(value: &str) -> Option<i64> {
    match value {
        "on" | "true" => return Some(1),
        "off" | "false" => return Some(0),
        _ => {}
    }
    if let Ok(v) = value.parse::<i64>() {
        return Some(v);
    }
    let v: f64 = value.parse().ok()?;
    (v.is_finite() && v.abs() < 9.0e15).then(|| v.round() as i64)
}

pub fn float(value: &str) -> Option<f64> {
    match value {
        "on" | "true" => Some(1.0),
        "off" | "false" => Some(0.0),
        _ => value.parse().ok().filter(|v: &f64| v.is_finite()),
    }
}

/// wanted and read back agree as text or as numbers; a slider sends 100.0
/// for a control that reads back 100
pub fn same(wanted: &str, got: &str) -> bool {
    if wanted == got {
        return true;
    }
    match (float(wanted), float(got)) {
        (Some(a), Some(b)) => (a - b).abs() <= 1e-4 * a.abs().max(1.0),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// accepted, but the read-back differs (the C920 rounds gain 160
    /// to 159)
    Differs,
    Locked,
    Error(String),
}

/// the outcome of the latest change to one key
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub wanted: String,
    pub got: Option<String>,
    pub status: Status,
    pub source: Source,
}

impl Report {
    /// ok or differs from the read-back, else the given error
    pub fn checked(wanted: &str, got: Option<String>, source: Source) -> Report {
        let status = match &got {
            Some(g) if same(wanted, g) => Status::Ok,
            Some(_) => Status::Differs,
            None => Status::Error("no read-back".into()),
        };
        Report {
            wanted: wanted.to_string(),
            got,
            status,
            source,
        }
    }
}

/// what the state file says besides the reports
#[derive(Debug, Default)]
pub struct Snapshot<'a> {
    pub qa_lock: bool,
    pub ndi: bool,
    pub file: Option<&'a Path>,
    pub ws: Option<(String, String)>,
    /// `key value` of every v4l2 control as last read
    pub camera: Vec<(String, String)>,
    /// `key value` of every pass parameter
    pub lens: Vec<(String, String)>,
}

/// the body of control.state, without the time line, so two bodies
/// compare equal when nothing changed:
///
///   set KEY WANTED GOT STATUS SOURCE [MESSAGE]
///   cam KEY VALUE
///   lens KEY VALUE
pub fn render(snapshot: &Snapshot, reports: &[(String, Report)]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "qa_lock {}", u8::from(snapshot.qa_lock));
    let _ = writeln!(out, "ndi {}", if snapshot.ndi { "on" } else { "off" });
    if let Some(file) = snapshot.file {
        let _ = writeln!(out, "file {}", file.display());
    }
    if let Some((url, status)) = &snapshot.ws {
        let _ = writeln!(out, "ws {url} {status}");
    }
    for (key, r) in reports {
        let got = r.got.as_deref().unwrap_or("-");
        let status = match &r.status {
            Status::Ok => "ok",
            Status::Differs => "differs",
            Status::Locked => "locked",
            Status::Error(_) => "error",
        };
        let _ = write!(out, "set {key} {} {got} {status} {}", r.wanted, r.source);
        if let Status::Error(e) = &r.status {
            let _ = write!(out, " {}", e.replace('\n', " "));
        }
        out.push('\n');
    }
    for (key, value) in &snapshot.camera {
        let _ = writeln!(out, "cam {key} {value}");
    }
    for (key, value) in &snapshot.lens {
        let _ = writeln!(out, "lens {key} {value}");
    }
    out
}

/// writes a dot file beside `path` and renames it into place, as the tap
/// does, so a reader sees the old file or the whole new one
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    let tmp = path.with_file_name(format!(".{}", name.to_string_lossy()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_lines_comments_and_repeats() {
        let got = parse(
            "# camera\nzoom_absolute 150\n\ngain  200 # dim\nnoise\npreset   qa\n\
             zoom_absolute 120\nfisheye_projection equisolid\n",
        );
        assert_eq!(
            got,
            pairs(&[
                ("gain", "200"),
                ("preset", "qa"),
                ("zoom_absolute", "120"),
                ("fisheye_projection", "equisolid"),
            ])
        );
        assert!(parse("key\n   \n#only\n").is_empty());
    }

    #[test]
    fn only_changed_values_come_back() {
        let mut seen = Seen::default();
        let first = pairs(&[("zoom_absolute", "100"), ("gain", "160")]);
        assert_eq!(seen.changed(&first), first);
        assert!(seen.changed(&first).is_empty());
        let next = pairs(&[("zoom_absolute", "150"), ("gain", "160")]);
        assert_eq!(seen.changed(&next), pairs(&[("zoom_absolute", "150")]));
        // a key that disappears and comes back with the same value is not
        // a change
        assert!(seen.changed(&pairs(&[("gain", "160")])).is_empty());
    }

    #[test]
    fn file_wins_in_the_same_tick() {
        let merged = merge(
            pairs(&[("zoom_absolute", "200")]),
            pairs(&[("zoom_absolute", "150"), ("fisheye_k1", "0.2")]),
            Some(7),
        );
        assert_eq!(
            merged,
            vec![
                ("zoom_absolute".into(), "200".into(), Source::File),
                ("fisheye_k1".into(), "0.2".into(), Source::Ws(Some(7))),
            ]
        );
    }

    #[test]
    fn values_parse_loosely_and_compare_as_numbers() {
        assert_eq!(int("100.0"), Some(100));
        assert_eq!(int("on"), Some(1));
        assert_eq!(int("-3600"), Some(-3600));
        assert_eq!(int("wide"), None);
        assert_eq!(float("off"), Some(0.0));
        assert!(same("100.0", "100"));
        assert!(same("equisolid", "equisolid"));
        assert!(!same("160", "159"));
    }

    #[test]
    fn lock_covers_the_qa_exposure_but_not_the_framing() {
        for key in [
            "exposure_time_absolute",
            "auto_exposure",
            "gain",
            "focus_absolute",
            "white_balance_temperature",
            "white_balance_automatic",
            "power_line_frequency",
        ] {
            assert!(is_locked(key), "{key}");
        }
        for key in ["zoom_absolute", "pan_absolute", "brightness", "fisheye_k1"] {
            assert!(!is_locked(key), "{key}");
        }
    }

    #[test]
    fn state_lines_keep_their_columns() {
        let reports = vec![
            (
                "gain".to_string(),
                Report {
                    wanted: "300".into(),
                    got: Some("159".into()),
                    status: Status::Locked,
                    source: Source::Ws(Some(12)),
                },
            ),
            (
                "focus_absolute".to_string(),
                Report {
                    wanted: "7".into(),
                    got: None,
                    status: Status::Error("set Focus: bad\nvalue".into()),
                    source: Source::File,
                },
            ),
        ];
        let snapshot = Snapshot {
            qa_lock: true,
            lens: pairs(&[("fisheye_k1", "0")]),
            ..Default::default()
        };
        let text = render(&snapshot, &reports);
        assert!(text.contains("\nset gain 300 159 locked ws:12\n"), "{text}");
        assert!(
            text.contains("\nset focus_absolute 7 - error file set Focus: bad value\n"),
            "{text}"
        );
        assert!(text.ends_with("lens fisheye_k1 0\n"));
        assert!(text.starts_with("qa_lock 1\nndi off\n"));
    }

    #[test]
    fn write_atomic_replaces_the_whole_file() {
        let dir = std::env::temp_dir().join(format!("transmog-live-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.state");
        write_atomic(&path, "one\n").unwrap();
        write_atomic(&path, "two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert!(!dir.join(".control.state").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
