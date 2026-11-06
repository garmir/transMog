//! Logitech's vendor controls on the C920, reached through the uvcvideo
//! driver's raw extension-unit query (UVCIOC_CTRL_QUERY) on the same fd
//! as the v4l2 controls. The kernel maps none of the C920's units, so
//! nothing else in the stack sees these; guvcview's Logitech table only
//! knows the older v1 units the C920 does not have.
//!
//! Only the selectors in `DEFS` are ever written. The Test/Debug unit
//! also carries register, EEPROM, sensor and ISP access selectors, which
//! Logitech's own uvcdynctrl table comments out as dangerous; `FORBIDDEN`
//! names them and `Xu::set` refuses them whatever the caller asks.
//!
//! Selector names follow libwebcam's logitech.xml (rev 0.32). The ranges
//! are what the camera reports, widened to its current value, because
//! this C920 (046d:082d, firmware 0.11) reads pixel defect correction 3
//! against a maximum of 2 and integration time 0 against a minimum of 10.
//! None of these values survive a re-enumeration and the kernel does not
//! replay them, so the window puts them back after a reconnect.

use std::fs;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::{ControlInfo, ControlKind};

/// a GUID in descriptor byte order (the first three fields little-endian)
pub type Guid = [u8; 16];

pub const PERIPHERAL: Guid = guid(
    0xffe5_2d21,
    0x8030,
    0x4e2c,
    [0x82, 0xd9, 0xf5, 0x87, 0xd0, 0x05, 0x40, 0xbd],
);
pub const VIDEO_PIPE_V3: Guid = guid(
    0x49e4_0215,
    0xf434,
    0x47fe,
    [0xb1, 0x58, 0x0e, 0x88, 0x50, 0x23, 0xe5, 0x1b],
);
pub const TEST_DEBUG_V3: Guid = guid(
    0x1f5d_4ca9,
    0xde11,
    0x4487,
    [0x84, 0x0d, 0x50, 0x93, 0x3c, 0x8e, 0xc8, 0xd1],
);

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> Guid {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// Test/Debug selectors that address registers, EEPROM, the sensor, the
/// ISP or the peripheral bus
pub const FORBIDDEN: &[(Guid, u8)] = &[
    (TEST_DEBUG_V3, 1),
    (TEST_DEBUG_V3, 2),
    (TEST_DEBUG_V3, 3),
    (TEST_DEBUG_V3, 4),
    (TEST_DEBUG_V3, 5),
    (TEST_DEBUG_V3, 6),
    (TEST_DEBUG_V3, 7),
    (TEST_DEBUG_V3, 8),
    (TEST_DEBUG_V3, 9),
    (TEST_DEBUG_V3, 19),
    (TEST_DEBUG_V3, 20),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Menu(&'static [&'static str]),
}

/// one value inside one extension-unit control
#[derive(Clone, Copy, Debug)]
pub struct Def {
    pub name: &'static str,
    pub guid: Guid,
    pub selector: u8,
    /// the whole control's length; a write sends all of it back
    pub len: usize,
    pub offset: usize,
    pub width: usize,
    pub kind: Kind,
    /// changes the picture, so the qa lock holds it like the exposure
    pub picture: bool,
}

pub const DEFS: &[Def] = &[
    Def {
        name: "Logitech LED1 Mode",
        guid: PERIPHERAL,
        selector: 9,
        len: 5,
        offset: 1,
        width: 1,
        kind: Kind::Menu(&["off", "on", "blink", "auto"]),
        picture: false,
    },
    // in 0.05 Hz steps; only the blink mode uses it
    Def {
        name: "Logitech LED1 Frequency",
        guid: PERIPHERAL,
        selector: 9,
        len: 5,
        offset: 3,
        width: 1,
        kind: Kind::Int,
        picture: false,
    },
    Def {
        name: "Logitech RightLight",
        guid: VIDEO_PIPE_V3,
        selector: 4,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Bool,
        picture: true,
    },
    Def {
        name: "Logitech Test Gain",
        guid: TEST_DEBUG_V3,
        selector: 11,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Int,
        picture: true,
    },
    Def {
        name: "Logitech Low Light Priority",
        guid: TEST_DEBUG_V3,
        selector: 12,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Bool,
        picture: true,
    },
    Def {
        name: "Logitech Pixel Defect Correction",
        guid: TEST_DEBUG_V3,
        selector: 14,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Int,
        picture: true,
    },
    Def {
        name: "Logitech Lens Shading",
        guid: TEST_DEBUG_V3,
        selector: 15,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Int,
        picture: true,
    },
    Def {
        name: "Logitech Gamma",
        guid: TEST_DEBUG_V3,
        selector: 16,
        len: 1,
        offset: 0,
        width: 1,
        kind: Kind::Int,
        picture: true,
    },
    Def {
        name: "Logitech Integration Time",
        guid: TEST_DEBUG_V3,
        selector: 17,
        len: 4,
        offset: 0,
        width: 4,
        kind: Kind::Int,
        picture: true,
    },
];

/// v4l2 never hands out ids this high, so these cannot collide with a
/// real control; the low bits index `DEFS`
pub const ID_BASE: u32 = 0xffe0_0000;
pub const CLASS: &str = "Logitech Extension Units";

pub fn is_xu(id: u32) -> bool {
    id & 0xffff_0000 == ID_BASE
}

/// the control-file key of every value the qa lock holds
pub fn picture_keys() -> impl Iterator<Item = String> {
    DEFS.iter()
        .filter(|d| d.picture)
        .map(|d| super::preset::ctl_name(d.name))
}

/// an extension unit as the camera's VideoControl interface lists it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub id: u8,
    pub guid: Guid,
    /// bmControls: bit n set when selector n + 1 exists
    pub controls: Vec<u8>,
}

impl Unit {
    pub fn has(&self, selector: u8) -> bool {
        let bit = usize::from(selector.saturating_sub(1));
        selector > 0
            && self
                .controls
                .get(bit / 8)
                .is_some_and(|b| b >> (bit % 8) & 1 == 1)
    }
}

const DT_INTERFACE: u8 = 0x04;
const DT_CS_INTERFACE: u8 = 0x24;
const CC_VIDEO: u8 = 0x0e;
const SC_VIDEOCONTROL: u8 = 0x01;
const VC_EXTENSION_UNIT: u8 = 0x06;

/// the extension units in a device's raw USB descriptors (sysfs
/// `descriptors`). subtype 6 also means an MJPEG format in the streaming
/// interface and a feature unit in an audio interface, so only
/// class-specific descriptors inside a VideoControl interface count
pub fn units(desc: &[u8]) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut in_vc = false;
    let mut at = 0;
    while at + 2 <= desc.len() {
        let len = usize::from(desc[at]);
        if len < 2 || at + len > desc.len() {
            break;
        }
        let d = &desc[at..at + len];
        match d[1] {
            DT_INTERFACE if len >= 9 => in_vc = d[5] == CC_VIDEO && d[6] == SC_VIDEOCONTROL,
            DT_CS_INTERFACE if in_vc && len >= 24 && d[2] == VC_EXTENSION_UNIT => {
                let pins = usize::from(d[21]);
                let size_at = 22 + pins;
                if let Some(&size) = d.get(size_at) {
                    let size = usize::from(size);
                    if let Some(bits) = d.get(size_at + 1..size_at + 1 + size) {
                        let mut guid = [0; 16];
                        guid.copy_from_slice(&d[4..20]);
                        out.push(Unit {
                            id: d[3],
                            guid,
                            controls: bits.to_vec(),
                        });
                    }
                }
            }
            _ => {}
        }
        at += len;
    }
    out
}

/// the USB device's descriptors file behind a /dev/videoN path
pub fn descriptors_path(video: &Path) -> Result<PathBuf> {
    let real = fs::canonicalize(video).with_context(|| format!("resolve {}", video.display()))?;
    let name = real
        .file_name()
        .context("device path has no file name")?
        .to_string_lossy()
        .into_owned();
    let interface = fs::canonicalize(
        Path::new("/sys/class/video4linux")
            .join(&name)
            .join("device"),
    )
    .with_context(|| format!("sysfs entry of {name}"))?;
    Ok(interface.with_file_name("descriptors"))
}

// include/uapi/linux/uvcvideo.h
#[repr(C)]
struct XuQuery {
    unit: u8,
    selector: u8,
    query: u8,
    size: u16,
    data: *mut u8,
}

const UVCIOC_CTRL_QUERY: libc::c_ulong = 0xc010_7521;
const SET_CUR: u8 = 0x01;
const GET_CUR: u8 = 0x81;
const GET_MIN: u8 = 0x82;
const GET_MAX: u8 = 0x83;
const GET_DEF: u8 = 0x87;

fn query(fd: RawFd, unit: u8, selector: u8, query: u8, data: &mut [u8]) -> std::io::Result<()> {
    let mut q = XuQuery {
        unit,
        selector,
        query,
        size: data.len() as u16,
        data: data.as_mut_ptr(),
    };
    // SAFETY: q points at a live buffer of q.size bytes for the call
    let r = unsafe { libc::ioctl(fd, UVCIOC_CTRL_QUERY as _, &mut q as *mut XuQuery) };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn forbidden(guid: &Guid, selector: u8) -> bool {
    FORBIDDEN.iter().any(|(g, s)| g == guid && *s == selector)
}

fn field(bytes: &[u8], def: &Def) -> i64 {
    let mut v = 0u64;
    for (i, b) in bytes[def.offset..def.offset + def.width].iter().enumerate() {
        v |= u64::from(*b) << (8 * i);
    }
    v as i64
}

fn put(bytes: &mut [u8], def: &Def, value: i64) {
    for i in 0..def.width {
        bytes[def.offset + i] = (value >> (8 * i)) as u8;
    }
}

/// one entry of `DEFS` found on this camera, with the range it reported
#[derive(Clone, Debug)]
struct Bound {
    index: usize,
    unit: u8,
    min: i64,
    max: i64,
    default: i64,
}

/// the vendor controls of one open camera
#[derive(Debug, Default)]
pub struct Xu {
    bound: Vec<Bound>,
    /// why a unit or control was skipped, for the status line
    pub notes: Vec<String>,
}

impl Xu {
    /// finds the units in the camera's descriptors and reads each known
    /// control's range; a camera without them binds nothing
    pub fn bind(fd: RawFd, video: &Path) -> Xu {
        let mut xu = Xu::default();
        let units = match descriptors_path(video)
            .and_then(|p| fs::read(&p).with_context(|| format!("read {}", p.display())))
        {
            Ok(desc) => units(&desc),
            Err(e) => {
                xu.notes.push(format!("no extension units: {e:#}"));
                return xu;
            }
        };
        xu.bind_units(&units, |unit, selector, q, buf| {
            query(fd, unit, selector, q, buf)
        });
        xu
    }

    fn bind_units(
        &mut self,
        units: &[Unit],
        mut q: impl FnMut(u8, u8, u8, &mut [u8]) -> std::io::Result<()>,
    ) {
        for (index, def) in DEFS.iter().enumerate() {
            let Some(unit) = units
                .iter()
                .find(|u| u.guid == def.guid && u.has(def.selector))
            else {
                continue;
            };
            let mut read = |code| {
                let mut buf = vec![0; def.len];
                q(unit.id, def.selector, code, &mut buf).map(|()| field(&buf, def))
            };
            let (cur, min, max, default) = match (
                read(GET_CUR),
                read(GET_MIN),
                read(GET_MAX),
                read(GET_DEF),
            ) {
                (Ok(c), Ok(lo), Ok(hi), Ok(d)) => (c, lo, hi, d),
                (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => {
                    let hint = if e.raw_os_error() == Some(libc::EACCES) && def.guid == PERIPHERAL {
                        " (kernel 7.2 and later need uvcvideo allow_privacy_override=1 for the LED)"
                    } else {
                        ""
                    };
                    self.notes.push(format!("{}: {e}{hint}", def.name));
                    continue;
                }
            };
            let (min, max) = match def.kind {
                Kind::Bool => (0, 1),
                Kind::Menu(items) => (0, items.len() as i64 - 1),
                Kind::Int => (min.min(cur), max.max(cur)),
            };
            self.bound.push(Bound {
                index,
                unit: unit.id,
                min,
                max,
                default: default.clamp(min, max),
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bound.is_empty()
    }

    /// the bound controls as ControlInfo, read now
    pub fn query(&self, fd: RawFd) -> Vec<ControlInfo> {
        self.query_with(|unit, selector, q, buf| query(fd, unit, selector, q, buf))
    }

    fn query_with(
        &self,
        mut q: impl FnMut(u8, u8, u8, &mut [u8]) -> std::io::Result<()>,
    ) -> Vec<ControlInfo> {
        let mut out = Vec::new();
        for b in &self.bound {
            let def = &DEFS[b.index];
            let mut buf = vec![0; def.len];
            if let Err(e) = q(b.unit, def.selector, GET_CUR, &mut buf) {
                tracing::debug!("{}: {e}", def.name);
                continue;
            }
            let value = field(&buf, def);
            let kind = match def.kind {
                Kind::Bool => ControlKind::Boolean {
                    default: b.default != 0,
                },
                Kind::Menu(items) => ControlKind::Menu {
                    items: items
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i as i64, s.to_string()))
                        .collect(),
                    default: b.default,
                },
                Kind::Int => ControlKind::Integer {
                    min: b.min.min(value),
                    max: b.max.max(value),
                    step: 1,
                    default: b.default,
                },
            };
            out.push(ControlInfo {
                id: ID_BASE | b.index as u32,
                name: def.name.to_string(),
                kind,
                value,
                inactive: false,
                read_only: false,
                class: CLASS.to_string(),
            });
        }
        out
    }

    pub fn set(&self, fd: RawFd, id: u32, value: i64) -> Result<()> {
        self.set_with(id, value, |unit, selector, q, buf| {
            query(fd, unit, selector, q, buf)
        })
    }

    fn set_with(
        &self,
        id: u32,
        value: i64,
        mut q: impl FnMut(u8, u8, u8, &mut [u8]) -> std::io::Result<()>,
    ) -> Result<()> {
        let b = self.bound(id)?;
        let def = &DEFS[b.index];
        if forbidden(&def.guid, def.selector) {
            bail!("{}: selector {} is never written", def.name, def.selector);
        }
        if value < b.min || value > b.max {
            bail!("{}: {value} is outside {}..={}", def.name, b.min, b.max);
        }
        // the other values sharing the control (the LED's mode and
        // frequency) go back as they are
        let mut buf = vec![0; def.len];
        q(b.unit, def.selector, GET_CUR, &mut buf).with_context(|| format!("read {}", def.name))?;
        put(&mut buf, def, value);
        q(b.unit, def.selector, SET_CUR, &mut buf).with_context(|| {
            let hint = if def.guid == PERIPHERAL {
                "; kernel 7.2 and later need uvcvideo allow_privacy_override=1 for the LED"
            } else {
                ""
            };
            format!("set {}{hint}", def.name)
        })
    }

    pub fn default_of(&self, id: u32) -> Result<i64> {
        Ok(self.bound(id)?.default)
    }

    fn bound(&self, id: u32) -> Result<&Bound> {
        let index = (id & 0xffff) as usize;
        self.bound
            .iter()
            .find(|b| b.index == index && is_xu(id))
            .with_context(|| format!("extension control {id:#x} is not on this camera"))
    }

    /// the ids of every bound control
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.bound.iter().map(|b| ID_BASE | b.index as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const C920: &[u8] = include_bytes!("../../tests/fixtures/c920-082d.descriptors");

    #[test]
    fn finds_the_c920s_six_units_and_their_selectors() {
        let got = units(C920);
        let ids: Vec<u8> = got.iter().map(|u| u.id).collect();
        assert_eq!(ids, [6, 8, 9, 10, 11, 12]);
        let peripheral = got.iter().find(|u| u.guid == PERIPHERAL).unwrap();
        assert_eq!(peripheral.id, 11);
        assert!(peripheral.has(9) && peripheral.has(15) && !peripheral.has(1));
        let pipe = got.iter().find(|u| u.guid == VIDEO_PIPE_V3).unwrap();
        assert_eq!(pipe.id, 10);
        assert!(pipe.has(4) && !pipe.has(5));
        let debug = got.iter().find(|u| u.guid == TEST_DEBUG_V3).unwrap();
        assert_eq!(debug.id, 9);
        for s in [11, 12, 14, 15, 16, 17] {
            assert!(debug.has(s), "{s}");
        }
    }

    #[test]
    fn ignores_subtype_six_outside_videocontrol() {
        // an interface of another class followed by a class-specific
        // descriptor with subtype 6 and the extension unit's length
        let mut d = vec![9, DT_INTERFACE, 1, 0, 0, 0x01, 0x02, 0, 0];
        let mut cs = vec![0u8; 26];
        cs[0] = 26;
        cs[1] = DT_CS_INTERFACE;
        cs[2] = VC_EXTENSION_UNIT;
        d.extend(cs);
        assert!(units(&d).is_empty());
    }

    #[test]
    fn the_table_never_names_an_access_selector() {
        for d in DEFS {
            assert!(!forbidden(&d.guid, d.selector), "{}", d.name);
            assert!(d.offset + d.width <= d.len, "{}", d.name);
        }
    }

    #[test]
    fn keys_follow_the_control_names() {
        let keys: Vec<String> = DEFS
            .iter()
            .map(|d| super::super::preset::ctl_name(d.name))
            .collect();
        assert_eq!(keys[0], "logitech_led1_mode");
        assert_eq!(keys[1], "logitech_led1_frequency");
        assert!(keys.contains(&"logitech_low_light_priority".to_string()));
        let held: Vec<String> = picture_keys().collect();
        assert!(held.contains(&"logitech_gamma".to_string()));
        assert!(!held.iter().any(|k| k.starts_with("logitech_led1")));
    }

    /// a camera that answers with this C920's readings of 2026-11-06
    fn fake() -> RefCell<HashMap<(u8, u8), Vec<u8>>> {
        RefCell::new(HashMap::from([
            ((11, 9), vec![0x01, 0x03, 0x01, 0x00, 0x03]),
            ((10, 4), vec![0]),
            ((9, 11), vec![0]),
            ((9, 12), vec![0]),
            ((9, 14), vec![3]),
            ((9, 15), vec![1]),
            ((9, 16), vec![1]),
            ((9, 17), vec![0, 0, 0, 0]),
        ]))
    }

    fn answer(
        cur: &RefCell<HashMap<(u8, u8), Vec<u8>>>,
        unit: u8,
        selector: u8,
        q: u8,
        buf: &mut [u8],
    ) -> std::io::Result<()> {
        let range = |lo: &[u8], hi: &[u8], code: u8| {
            if code == GET_MIN {
                lo.to_vec()
            } else {
                hi.to_vec()
            }
        };
        let v = match q {
            GET_CUR => cur.borrow()[&(unit, selector)].clone(),
            SET_CUR => {
                cur.borrow_mut().insert((unit, selector), buf.to_vec());
                return Ok(());
            }
            GET_DEF => vec![0; buf.len()],
            code => match (unit, selector) {
                (11, 9) => range(&[0; 5], &[0xff; 5], code),
                (9, 11) => range(&[0], &[3], code),
                (9, 14) => range(&[0], &[2], code),
                (9, 15) => range(&[0], &[0x3f], code),
                (9, 16) => range(&[0], &[2], code),
                (9, 17) => range(&[10, 0, 0, 0], &[0xe8, 3, 0, 0], code),
                _ => range(&[0], &[1], code),
            },
        };
        buf.copy_from_slice(&v);
        Ok(())
    }

    #[test]
    fn reads_widens_and_writes_through_the_shared_bytes() {
        let cur = fake();
        let mut xu = Xu::default();
        xu.bind_units(&units(C920), |u, s, q, b| answer(&cur, u, s, q, b));
        assert_eq!(xu.ids().count(), DEFS.len());
        let infos = xu.query_with(|u, s, q, b| answer(&cur, u, s, q, b));
        let by = |name: &str| infos.iter().find(|i| i.name == name).unwrap().clone();
        assert_eq!(by("Logitech LED1 Mode").value, 3);
        // reads 3 against a reported maximum of 2, and 0 against 10..1000
        assert!(matches!(
            by("Logitech Pixel Defect Correction").kind,
            ControlKind::Integer { max: 3, .. }
        ));
        assert!(matches!(
            by("Logitech Integration Time").kind,
            ControlKind::Integer {
                min: 0,
                max: 1000,
                ..
            }
        ));
        let freq = by("Logitech LED1 Frequency").id;
        xu.set_with(freq, 20, |u, s, q, b| answer(&cur, u, s, q, b))
            .unwrap();
        assert_eq!(cur.borrow()[&(11, 9)], vec![0x01, 0x03, 0x01, 20, 0x03]);
        let gamma = by("Logitech Gamma").id;
        assert!(xu
            .set_with(gamma, 3, |u, s, q, b| answer(&cur, u, s, q, b))
            .is_err());
        xu.set_with(gamma, 2, |u, s, q, b| answer(&cur, u, s, q, b))
            .unwrap();
        assert_eq!(cur.borrow()[&(9, 16)], vec![2]);
        let time = by("Logitech Integration Time").id;
        xu.set_with(time, 500, |u, s, q, b| answer(&cur, u, s, q, b))
            .unwrap();
        assert_eq!(cur.borrow()[&(9, 17)], vec![0xf4, 1, 0, 0]);
    }

    #[test]
    fn a_camera_without_the_units_binds_nothing() {
        let mut xu = Xu::default();
        xu.bind_units(&[], |_, _, _, _| Ok(()));
        assert!(xu.is_empty());
    }
}
