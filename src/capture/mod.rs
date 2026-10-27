use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub bus_info: String,
    pub usb_id: Option<(u16, u16)>,
}

/// capture-capable video nodes only, sorted by path
pub fn list_devices() -> Vec<DeviceInfo> {
    todo!("enumerate /dev/video* and keep the capture nodes")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceFormat {
    Yuyv,
    Mjpeg,
}

pub struct FrameSize {
    pub width: u32,
    pub height: u32,
    /// integer frame rates the driver offers at this size
    pub fps: Vec<u32>,
}

pub struct FormatInfo {
    pub format: SourceFormat,
    pub sizes: Vec<FrameSize>,
}

/// only yuyv and mjpeg entries are reported
pub fn list_formats(_path: &Path) -> Result<Vec<FormatInfo>> {
    todo!("enumerate formats, frame sizes and intervals")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelLayout {
    Yuyv,
    /// decoded mjpeg
    Rgba8,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub data: Vec<u8>,
    pub seq: u64,
    pub received: Instant,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub path: PathBuf,
    pub format: SourceFormat,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// owns a capture thread that stops on drop
pub struct Capture {
    config: CaptureConfig,
}

impl Capture {
    pub fn start(cfg: CaptureConfig) -> Result<Capture> {
        Ok(Capture { config: cfg })
    }

    /// newest frame, never blocks; the same arc comes back until a newer
    /// frame arrives
    pub fn latest(&self) -> Option<Arc<Frame>> {
        None
    }

    /// rolling one second estimate
    pub fn measured_fps(&self) -> f32 {
        0.0
    }

    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }
}
