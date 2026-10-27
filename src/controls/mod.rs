use std::path::Path;

use anyhow::Result;

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
    _device: v4l::Device,
}

impl Controls {
    pub fn open(path: &Path) -> Result<Controls> {
        let device = v4l::Device::with_path(path)?;
        Ok(Controls { _device: device })
    }

    /// all controls in enumeration order with current values
    pub fn query(&self) -> Result<Vec<ControlInfo>> {
        Ok(Vec::new())
    }

    pub fn set(&self, _id: u32, _value: i64) -> Result<()> {
        todo!("VIDIOC_S_EXT_CTRLS")
    }

    /// set to default
    pub fn reset(&self, _id: u32) -> Result<()> {
        todo!("query the default and set it")
    }

    pub fn reset_all(&self) -> Result<()> {
        todo!("reset every writable control")
    }
}
