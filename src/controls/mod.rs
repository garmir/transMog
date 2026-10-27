use std::mem;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tracing::debug;
use v4l::control::{Control, Description, Flags, MenuItem, Type, Value};
use v4l::v4l2;
use v4l::v4l_sys::{v4l2_ext_control, v4l2_query_ext_ctrl};
use v4l::Device;

// the v4l crate keeps its own v4l2_ext_controls private because bindgen
// turns the ctrl_class/which union into something it cannot name, so a
// matching layout lives here for the one raw VIDIOC_G_EXT_CTRLS call
#[repr(C)]
struct ExtControls {
    which: u32,
    count: u32,
    error_idx: u32,
    request_fd: i32,
    reserved: u32,
    controls: *mut v4l2_ext_control,
}

const CLASS_MASK: u32 = 0xffff_0000;
const CLASS_USER: u32 = 0x0098_0000;
const CLASS_CAMERA: u32 = 0x009a_0000;

#[derive(Clone, Debug)]
pub enum ControlKind {
    Integer {
        min: i64,
        max: i64,
        step: i64,
        default: i64,
    },
    Boolean {
        default: bool,
    },
    /// covers menu and integer menu
    Menu {
        items: Vec<(i64, String)>,
        default: i64,
    },
    Button,
    /// string, bitmask, u8 and the rest: shown read-only
    Other,
}

#[derive(Clone, Debug)]
pub struct ControlInfo {
    pub id: u32,
    pub name: String,
    pub kind: ControlKind,
    pub value: i64,
    pub inactive: bool,
    pub read_only: bool,
    /// "User Controls", "Camera Controls", ...
    pub class: String,
}

/// holds its own v4l device fd, safe alongside a capture on the same node
pub struct Controls {
    device: Device,
}

impl Controls {
    pub fn open(path: &Path) -> Result<Controls> {
        let device = Device::with_path(path).with_context(|| format!("open {}", path.display()))?;
        Ok(Controls { device })
    }

    /// all controls in enumeration order with current values
    pub fn query(&self) -> Result<Vec<ControlInfo>> {
        let mut out = Vec::new();
        for desc in self.device.query_controls().context("enumerate controls")? {
            if desc.typ == Type::CtrlClass {
                continue;
            }
            let kind = kind_of(&desc);
            let value = match kind {
                ControlKind::Button | ControlKind::Other => 0,
                _ => self.read_value(&desc).unwrap_or_else(|e| {
                    debug!("{}: cannot read value: {e:#}", desc.name);
                    desc.default
                }),
            };
            out.push(ControlInfo {
                id: desc.id,
                name: desc.name,
                kind,
                value,
                inactive: desc.flags.contains(Flags::INACTIVE),
                read_only: desc.flags.contains(Flags::READ_ONLY)
                    || desc.flags.contains(Flags::GRABBED),
                class: class_name(desc.id).to_string(),
            });
        }
        Ok(out)
    }

    pub fn set(&self, id: u32, value: i64) -> Result<()> {
        let desc = self.describe(id)?;
        let value = match desc.typ {
            Type::Integer | Type::Integer64 | Type::Menu | Type::IntegerMenu => {
                Value::Integer(value)
            }
            Type::Boolean => Value::Boolean(value != 0),
            Type::Button => Value::Integer(1),
            other => bail!("{}: {other} controls are read-only here", desc.name),
        };
        self.device
            .set_control(Control { id, value })
            .with_context(|| format!("set {}", desc.name))
    }

    /// set to default
    pub fn reset(&self, id: u32) -> Result<()> {
        let desc = self.describe(id)?;
        self.reset_described(&desc)
    }

    pub fn reset_all(&self) -> Result<()> {
        let mut first_err = None;
        for desc in self.device.query_controls().context("enumerate controls")? {
            // a control can go active or inactive as earlier resets land,
            // so the flags are re-read right before each write
            let Ok(current) = self.describe(desc.id) else {
                continue;
            };
            if !resettable(&current) {
                continue;
            }
            if let Err(e) = self.reset_described(&current) {
                debug!("reset_all: {e:#}");
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reset_described(&self, desc: &Description) -> Result<()> {
        match desc.typ {
            Type::Button => Ok(()),
            _ => self.set(desc.id, desc.default),
        }
    }

    /// VIDIOC_QUERY_EXT_CTRL for a single id; the v4l crate only exposes the
    /// enumerate-everything variant
    fn describe(&self, id: u32) -> Result<Description> {
        let mut raw = v4l2_query_ext_ctrl {
            id,
            ..unsafe { mem::zeroed() }
        };
        unsafe {
            v4l2::ioctl(
                self.device.handle().fd(),
                v4l2::vidioc::VIDIOC_QUERY_EXT_CTRL,
                &mut raw as *mut _ as *mut std::os::raw::c_void,
            )
        }
        .with_context(|| format!("query control {id:#x}"))?;
        if Type::try_from(raw.type_).is_err() {
            bail!("control {id:#x} has unknown type {}", raw.type_);
        }
        Ok(Description::from(raw))
    }

    fn read_value(&self, desc: &Description) -> Result<i64> {
        // Device::control rejects integer menus, so those go through a raw
        // VIDIOC_G_EXT_CTRLS; the kernel stores them in the 32 bit slot
        if desc.typ == Type::IntegerMenu {
            let mut ctrl = v4l2_ext_control {
                id: desc.id,
                ..unsafe { mem::zeroed() }
            };
            let mut ctrls = ExtControls {
                which: 0,
                count: 1,
                error_idx: 0,
                request_fd: 0,
                reserved: 0,
                controls: &mut ctrl,
            };
            unsafe {
                v4l2::ioctl(
                    self.device.handle().fd(),
                    v4l2::vidioc::VIDIOC_G_EXT_CTRLS,
                    &mut ctrls as *mut _ as *mut std::os::raw::c_void,
                )?;
                return Ok(i64::from(ctrl.__bindgen_anon_1.value));
            }
        }
        match self.device.control(desc.id)?.value {
            Value::Integer(v) => Ok(v),
            Value::Boolean(b) => Ok(i64::from(b)),
            other => bail!("unexpected value {other:?}"),
        }
    }
}

fn kind_of(desc: &Description) -> ControlKind {
    match desc.typ {
        Type::Integer | Type::Integer64 => ControlKind::Integer {
            min: desc.minimum,
            max: desc.maximum,
            step: desc.step as i64,
            default: desc.default,
        },
        Type::Boolean => ControlKind::Boolean {
            default: desc.default != 0,
        },
        Type::Menu | Type::IntegerMenu => ControlKind::Menu {
            items: desc
                .items
                .iter()
                .flatten()
                .map(|(index, item)| {
                    let label = match item {
                        MenuItem::Name(name) => name.clone(),
                        MenuItem::Value(v) => v.to_string(),
                    };
                    (i64::from(*index), label)
                })
                .collect(),
            default: desc.default,
        },
        Type::Button => ControlKind::Button,
        _ => ControlKind::Other,
    }
}

fn resettable(desc: &Description) -> bool {
    let writable = matches!(
        desc.typ,
        Type::Integer | Type::Integer64 | Type::Boolean | Type::Menu | Type::IntegerMenu
    );
    writable
        && !desc
            .flags
            .intersects(Flags::READ_ONLY | Flags::GRABBED | Flags::INACTIVE | Flags::DISABLED)
}

fn class_name(id: u32) -> &'static str {
    match id & CLASS_MASK {
        CLASS_USER => "User Controls",
        CLASS_CAMERA => "Camera Controls",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_follows_id_prefix() {
        assert_eq!(class_name(0x0098_0900), "User Controls");
        assert_eq!(class_name(0x009a_090d), "Camera Controls");
        assert_eq!(class_name(0x009f_0001), "Other");
    }
}
