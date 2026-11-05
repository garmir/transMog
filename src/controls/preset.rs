use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::{ControlInfo, ControlKind, Controls};
use crate::capture::{CaptureConfig, SourceFormat};

/// a format line and an ordered list of control writes. controls are
/// named the way v4l2-ctl names them, so a preset can be checked or
/// written by hand against `v4l2-ctl --list-ctrls`
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preset {
    pub format: Option<PresetFormat>,
    pub controls: Vec<(String, i64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresetFormat {
    pub format: SourceFormat,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl From<&CaptureConfig> for PresetFormat {
    fn from(c: &CaptureConfig) -> PresetFormat {
        PresetFormat {
            format: c.format,
            width: c.width,
            height: c.height,
            fps: c.fps,
        }
    }
}

impl std::fmt::Display for PresetFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.format {
            SourceFormat::Mjpeg => "mjpeg",
            SourceFormat::Yuyv => "yuyv",
        };
        write!(f, "{kind}:{}x{}@{}", self.width, self.height, self.fps)
    }
}

/// one line of the read-back after an apply
#[derive(Clone, Debug)]
pub struct Readback {
    pub name: String,
    pub wanted: i64,
    /// none when the control is missing or could not be read
    pub got: Option<i64>,
    pub error: Option<String>,
}

impl Readback {
    pub fn ok(&self) -> bool {
        self.got == Some(self.wanted)
    }
}

/// "Exposure Time, Absolute" becomes "exposure_time_absolute", as in
/// v4l2-ctl
pub fn ctl_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

impl Preset {
    pub fn parse(text: &str) -> Result<Preset> {
        let mut preset = Preset::default();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut words = line.split_whitespace();
            let (Some(key), Some(value), None) = (words.next(), words.next(), words.next()) else {
                bail!("line {}: expected `name value`, got `{raw}`", n + 1);
            };
            if key == "format" {
                preset.format =
                    Some(parse_format(value).with_context(|| format!("line {}: format", n + 1))?);
            } else {
                let value: i64 = value
                    .parse()
                    .with_context(|| format!("line {}: {key} needs an integer", n + 1))?;
                preset.controls.push((key.to_string(), value));
            }
        }
        Ok(preset)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("# transmog camera preset: `name value`, applied in order\n");
        if let Some(f) = &self.format {
            let _ = writeln!(out, "format {f}");
        }
        for (name, value) in &self.controls {
            let _ = writeln!(out, "{name} {value}");
        }
        out
    }

    /// the writable controls as they stand, mode switches first so that
    /// applying the preset unlocks the values that depend on them
    pub fn from_current(format: Option<PresetFormat>, infos: &[ControlInfo]) -> Preset {
        let writable = |i: &&ControlInfo| {
            !i.read_only
                && !i.inactive
                && !matches!(i.kind, ControlKind::Button | ControlKind::Other)
        };
        let is_mode = |i: &&ControlInfo| {
            matches!(
                i.kind,
                ControlKind::Boolean { .. } | ControlKind::Menu { .. }
            )
        };
        let modes = infos.iter().filter(writable).filter(is_mode);
        let values = infos.iter().filter(writable).filter(|i| !is_mode(i));
        Preset {
            format,
            controls: modes
                .chain(values)
                .map(|i| (ctl_name(&i.name), i.value))
                .collect(),
        }
    }

    /// writes every control in file order, retries the ones that failed
    /// once the others have landed (a value is refused while its auto
    /// mode is on), then reads each one back from the camera
    pub fn apply(&self, controls: &Controls) -> Result<Vec<Readback>> {
        let infos = controls.query()?;
        let id_of = |name: &str| {
            infos
                .iter()
                .find(|i| ctl_name(&i.name) == name)
                .map(|i| i.id)
        };
        let mut errors: Vec<Option<String>> = vec![None; self.controls.len()];
        for pass in 0..2 {
            for (i, (name, value)) in self.controls.iter().enumerate() {
                if pass == 1 && errors[i].is_none() {
                    continue;
                }
                errors[i] = match id_of(name) {
                    None => Some("no such control".into()),
                    Some(id) => controls.set(id, *value).err().map(|e| format!("{e:#}")),
                };
            }
        }
        let after = controls.query()?;
        Ok(self
            .controls
            .iter()
            .zip(errors)
            .map(|((name, wanted), error)| Readback {
                name: name.clone(),
                wanted: *wanted,
                got: after
                    .iter()
                    .find(|i| ctl_name(&i.name) == *name)
                    .map(|i| i.value),
                error,
            })
            .collect())
    }
}

fn parse_format(value: &str) -> Result<PresetFormat> {
    // mjpeg:1920x1080@30
    let (kind, rest) = value.split_once(':').context("want kind:WxH@fps")?;
    let format = match kind {
        "mjpeg" => SourceFormat::Mjpeg,
        "yuyv" => SourceFormat::Yuyv,
        other => bail!("unknown pixel format {other}"),
    };
    let (size, fps) = rest.split_once('@').context("want WxH@fps")?;
    let (w, h) = size.split_once('x').context("want WxH")?;
    Ok(PresetFormat {
        format,
        width: w.parse()?,
        height: h.parse()?,
        fps: fps.parse()?,
    })
}

/// $XDG_CONFIG_HOME/transmog/presets, one `<name>.preset` file each
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("transmog")
        .join("presets")
}

pub fn path(name: &str) -> PathBuf {
    dir().join(format!("{name}.preset"))
}

/// preset names in the preset directory, sorted
pub fn list() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".preset").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

pub fn load(name: &str) -> Result<Preset> {
    load_path(&path(name))
}

pub fn load_path(path: &Path) -> Result<Preset> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Preset::parse(&text).with_context(|| path.display().to_string())
}

pub fn save(name: &str, preset: &Preset) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '.']) {
        bail!("a preset name has no slash or dot: `{name}`");
    }
    let path = path(name);
    fs::create_dir_all(dir()).with_context(|| format!("create {}", dir().display()))?;
    fs::write(&path, preset.to_text()).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_v4l2_ctl() {
        assert_eq!(
            ctl_name("Exposure Time, Absolute"),
            "exposure_time_absolute"
        );
        assert_eq!(
            ctl_name("White Balance, Automatic"),
            "white_balance_automatic"
        );
        assert_eq!(
            ctl_name("Focus, Automatic Continuous"),
            "focus_automatic_continuous"
        );
        assert_eq!(ctl_name("Power Line Frequency"), "power_line_frequency");
        assert_eq!(ctl_name("Auto Exposure"), "auto_exposure");
    }

    #[test]
    fn text_round_trips() {
        let text = "# qa\nformat mjpeg:1920x1080@30\nauto_exposure 1\n\
                    exposure_time_absolute 83  # 8.3 ms\n";
        let preset = Preset::parse(text).unwrap();
        assert_eq!(
            preset.format,
            Some(PresetFormat {
                format: SourceFormat::Mjpeg,
                width: 1920,
                height: 1080,
                fps: 30
            })
        );
        assert_eq!(
            preset.controls,
            vec![
                ("auto_exposure".into(), 1),
                ("exposure_time_absolute".into(), 83)
            ]
        );
        assert_eq!(Preset::parse(&preset.to_text()).unwrap(), preset);
    }

    #[test]
    fn bad_lines_are_refused() {
        assert!(Preset::parse("gain").is_err());
        assert!(Preset::parse("gain high").is_err());
        assert!(Preset::parse("format h264:1x1@1").is_err());
    }
}
