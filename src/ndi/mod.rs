//! NDI out: the effected frame, scaled to the send size and packed to
//! uyvy on the gpu with the stamp band burned into its top rows, read back through a ring of mapped
//! buffers and sent by a thread of its own, so neither the readback nor
//! the SDK ever waits on the ui thread.

pub mod ffi;
pub mod stamp;

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use crate::capture::FrameMeta;
use crate::render::{
    create_fullscreen_pipeline, create_input_bind_group, fullscreen_vertex_source,
};

/// NDI puts the host in front, so receivers list `ROTMACHINE (transMog)`
pub const SOURCE_NAME: &str = "transMog";
/// one being sent, one mapping, one being drawn; with all three busy the
/// frame is skipped
const SLOTS: usize = 3;
const MAP_TIMEOUT: Duration = Duration::from_secs(1);
/// a 1080p uyvy stream is about 125 Mb/s of SpeedHQ, more than the ssh
/// tunnel to TD carries over Wi-Fi (1.6 fps), and TD's cam_raster scales
/// to 960x540 anyway
pub const DEFAULT_SIZE: (u32, u32) = (640, 360);
const MAX_SIZE: (u32, u32) = (3840, 2160);

/// `WxH` with an even width (uyvy pairs pixels), wide enough for the
/// stamp decoder's 4 px a cell and tall enough for a 4 row band
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{text}: wants WxH"))?;
    let (w, h): (u32, u32) = match (w.trim().parse(), h.trim().parse()) {
        (Ok(w), Ok(h)) => (w, h),
        _ => return Err(format!("{text}: wants WxH in whole pixels")),
    };
    if w % 2 != 0 {
        return Err(format!("{text}: the width must be even"));
    }
    if w < stamp::CELLS * 4 || stamp::band_height(h) < 4 {
        return Err(format!("{text}: too small for the stamp band"));
    }
    if w > MAX_SIZE.0 || h > MAX_SIZE.1 {
        return Err(format!("{text}: above {}x{}", MAX_SIZE.0, MAX_SIZE.1));
    }
    Ok((w, h))
}

/// mirrors `Params` in pack.wgsl
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Params {
    width: u32,
    height: u32,
    band: u32,
    value: u32,
    src_width: u32,
    src_height: u32,
    _pad: [u32; 2],
}

/// what the panel shows; times are means over the last report window
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub version: String,
    pub sent: u64,
    pub skipped: u64,
    pub failed: u64,
    pub connections: i32,
    /// submit on the ui thread to the buffer being mapped on the sender
    pub readback_ms: f32,
    /// the SDK's async send call
    pub send_ms: f32,
    pub last_seq: u64,
}

struct Slot {
    buffer: wgpu::Buffer,
    busy: Arc<AtomicBool>,
}

struct Packed {
    view: wgpu::TextureView,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    /// the effected view the input group was built for
    input: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    slots: Vec<Slot>,
}

struct Job {
    buffer: wgpu::Buffer,
    busy: Arc<AtomicBool>,
    submission: wgpu::SubmissionIndex,
    mapped: Receiver<Result<(), wgpu::BufferAsyncError>>,
    submitted: Instant,
    width: u32,
    height: u32,
    stride: u32,
    fps: u32,
    seq: u64,
    kernel_ns: i64,
}

/// a frame the gpu has been asked to pack, to be handed to the sender
/// once its commands are submitted
pub struct Pending {
    slot: usize,
    seq: u64,
    kernel_ns: i64,
}

pub struct Output {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    input_layout: wgpu::BindGroupLayout,
    packed: Option<Packed>,
    size: (u32, u32),
    fps: u32,
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    stats: Arc<Mutex<Stats>>,
    skipped: u64,
}

impl Output {
    /// loads libndi and starts the sender; fails when the SDK is missing
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Result<Output> {
        let lib = ffi::lib()?;
        let source = format!(
            "{}\n{}",
            fullscreen_vertex_source(),
            include_str!("pack.wgsl")
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ndi pack"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ndi pack params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ndi pack"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform_layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "ndi pack", &module, "fs_main", &layout);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ndi pack params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ndi pack params"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        // pack.wgsl uses textureLoad, the sampler only fills the layout
        let sampler = device.create_sampler(&Default::default());
        let stats = Arc::new(Mutex::new(Stats {
            version: lib.version.clone(),
            ..Default::default()
        }));
        let (jobs, rx) = crossbeam_channel::bounded::<Job>(SLOTS);
        let thread = {
            let device = device.clone();
            let stats = Arc::clone(&stats);
            std::thread::Builder::new()
                .name("ndi send".into())
                .spawn(move || send_loop(lib, &device, &rx, &stats))
                .context("spawn ndi thread")?
        };
        tracing::info!(version = %lib.version, name = SOURCE_NAME, "ndi out on");
        Ok(Output {
            pipeline,
            uniform,
            uniform_group,
            sampler,
            input_layout: input_layout.clone(),
            packed: None,
            size: DEFAULT_SIZE,
            fps: 30,
            jobs: Some(jobs),
            thread: Some(thread),
            stats,
            skipped: 0,
        })
    }

    pub fn set_fps(&mut self, fps: u32) {
        self.fps = fps.max(1);
    }

    /// the sent frame's size; the next frame goes out at it
    pub fn set_size(&mut self, size: (u32, u32)) {
        self.size = size;
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn stats(&self) -> Stats {
        let mut s = self.stats.lock().clone();
        s.skipped = self.skipped;
        s
    }

    /// records the pack pass and the copy into a free slot; none when
    /// every slot is still busy, and the frame is then skipped
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        src_size: (u32, u32),
        meta: &FrameMeta,
    ) -> Option<Pending> {
        let (width, height) = self.size;
        if src_size.0 == 0 || src_size.1 == 0 {
            return None;
        }
        if self
            .packed
            .as_ref()
            .is_none_or(|p| (p.width, p.height) != (width, height))
        {
            self.packed = Some(create_packed(device, width, height));
        }
        let packed = self.packed.as_mut().expect("created above");
        let Some(slot) = packed
            .slots
            .iter()
            .position(|s| !s.busy.load(Ordering::Acquire))
        else {
            self.skipped += 1;
            return None;
        };
        if packed.input.as_ref().is_none_or(|(v, _)| v != input) {
            let group = create_input_bind_group(device, &self.input_layout, input, &self.sampler);
            packed.input = Some((input.clone(), group));
        }
        let seq = meta.tap_seq.unwrap_or(meta.seq);
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Params {
                width,
                height,
                band: stamp::band_height(height),
                value: u32::from(stamp::value(seq)),
                src_width: src_size.0,
                src_height: src_size.1,
                _pad: [0; 2],
            }),
        );
        let group = &packed.input.as_ref().expect("set above").1;
        crate::render::draw_fullscreen(
            encoder,
            "ndi pack",
            &self.pipeline,
            &[group, &self.uniform_group],
            &packed.view,
        );
        let stride = stride(width);
        encoder.copy_texture_to_buffer(
            packed.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &packed.slots[slot].buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width: width / 2,
                height,
                depth_or_array_layers: 1,
            },
        );
        packed.slots[slot].busy.store(true, Ordering::Release);
        Some(Pending {
            slot,
            seq,
            kernel_ns: meta.kernel_ns,
        })
    }

    /// starts the map of a slot whose copy went out in `submission` and
    /// hands it to the sender thread; returns at once
    pub fn submitted(&mut self, pending: Pending, submission: wgpu::SubmissionIndex) {
        let (Some(packed), Some(jobs)) = (&self.packed, &self.jobs) else {
            return;
        };
        let slot = &packed.slots[pending.slot];
        let (tx, mapped) = crossbeam_channel::bounded(1);
        slot.buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            tx.send(r).ok();
        });
        let job = Job {
            buffer: slot.buffer.clone(),
            busy: Arc::clone(&slot.busy),
            submission,
            mapped,
            submitted: Instant::now(),
            width: packed.width,
            height: packed.height,
            stride: stride(packed.width),
            fps: self.fps,
            seq: pending.seq,
            kernel_ns: pending.kernel_ns,
        };
        // the channel holds SLOTS jobs and a job needs a free slot, so
        // this never finds it full
        if let Err(e) = jobs.try_send(job) {
            let job = e.into_inner();
            job.busy.store(false, Ordering::Release);
            self.skipped += 1;
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        tracing::info!("ndi out off");
    }
}

/// uyvy is 2 bytes a pixel; 1920, 1280 and 640 wide rows are already a
/// multiple of the 256 byte copy alignment, so NDI gets them unpadded. a
/// 960 row is padded to 2048 bytes and sent with that line stride
fn stride(width: u32) -> u32 {
    (width / 2 * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

fn create_packed(device: &wgpu::Device, width: u32, height: u32) -> Packed {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ndi uyvy"),
        size: wgpu::Extent3d {
            width: width / 2,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::render::TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let slots = (0..SLOTS)
        .map(|i| Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("ndi readback {i}")),
                size: u64::from(stride(width) * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            busy: Arc::new(AtomicBool::new(false)),
        })
        .collect();
    tracing::debug!(width, height, "ndi readback ring recreated");
    Packed {
        view,
        texture,
        width,
        height,
        input: None,
        slots,
    }
}

/// the metadata attached to every video frame
pub fn metadata(seq: u64, kernel_ns: i64) -> String {
    format!(r#"<tm seq="{seq}" ts="{kernel_ns}"/>"#)
}

/// a frame the SDK is still reading from: async send keeps the pointer
/// until the next send returns
struct InFlight {
    job: Job,
    _view: wgpu::BufferView,
    _metadata: CString,
}

impl InFlight {
    fn release(self) {
        let InFlight { job, _view, .. } = self;
        drop(_view);
        job.buffer.unmap();
        job.busy.store(false, Ordering::Release);
    }
}

const REPORT_EVERY: u64 = 300;

fn send_loop(lib: &ffi::Lib, device: &wgpu::Device, jobs: &Receiver<Job>, stats: &Mutex<Stats>) {
    let name = CString::new(SOURCE_NAME).expect("no nul in the name");
    let create = ffi::SendCreate {
        ndi_name: name.as_ptr(),
        groups: std::ptr::null(),
        // frames go out as the camera delivers them; the SDK's clock would
        // add up to a frame of wait
        clock_video: false,
        clock_audio: false,
    };
    let instance = unsafe { (lib.send_create)(&create) };
    if instance.is_null() {
        tracing::error!("NDIlib_send_create failed");
        for job in jobs {
            job.busy.store(false, Ordering::Release);
        }
        return;
    }
    let mut held: Option<InFlight> = None;
    let (mut readback_sum, mut send_sum, mut window) = (0.0f32, 0.0f32, 0u64);
    for job in jobs {
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: Some(job.submission.clone()),
            timeout: Some(MAP_TIMEOUT),
        });
        match job.mapped.recv_timeout(MAP_TIMEOUT) {
            Ok(Ok(())) => {}
            other => {
                tracing::warn!(?other, "ndi readback did not map, frame dropped");
                if matches!(other, Ok(Err(_))) {
                    job.buffer.unmap();
                }
                job.busy.store(false, Ordering::Release);
                stats.lock().failed += 1;
                continue;
            }
        }
        let readback = job.submitted.elapsed();
        let view = match job.buffer.slice(..).get_mapped_range() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ndi readback range: {e}");
                job.buffer.unmap();
                job.busy.store(false, Ordering::Release);
                stats.lock().failed += 1;
                continue;
            }
        };
        let meta = CString::new(metadata(job.seq, job.kernel_ns)).expect("no nul in digits");
        let frame = ffi::VideoFrameV2 {
            xres: job.width as i32,
            yres: job.height as i32,
            fourcc: ffi::FOURCC_UYVY,
            frame_rate_n: job.fps as i32,
            frame_rate_d: 1,
            picture_aspect_ratio: 0.0,
            frame_format_type: ffi::FRAME_FORMAT_PROGRESSIVE,
            timecode: ffi::TIMECODE_SYNTHESIZE,
            data: view.as_ptr(),
            line_stride_in_bytes: job.stride as i32,
            metadata: meta.as_ptr(),
            timestamp: 0,
        };
        let start = Instant::now();
        unsafe { (lib.send_video_async_v2)(instance, &frame) };
        let send = start.elapsed();
        if let Some(previous) = held.take() {
            previous.release();
        }
        let seq = job.seq;
        held = Some(InFlight {
            job,
            _view: view,
            _metadata: meta,
        });

        readback_sum += readback.as_secs_f32() * 1000.0;
        send_sum += send.as_secs_f32() * 1000.0;
        window += 1;
        let mut s = stats.lock();
        s.sent += 1;
        s.last_seq = seq;
        if window == 30 || s.sent.is_multiple_of(REPORT_EVERY) {
            s.readback_ms = readback_sum / window as f32;
            s.send_ms = send_sum / window as f32;
            s.connections = unsafe { (lib.send_get_no_connections)(instance, 0) };
            if s.sent.is_multiple_of(REPORT_EVERY) {
                tracing::info!(
                    sent = s.sent,
                    failed = s.failed,
                    connections = s.connections,
                    readback_ms = s.readback_ms,
                    send_ms = s.send_ms,
                    "ndi out"
                );
            }
            (readback_sum, send_sum, window) = (0.0, 0.0, 0);
        }
    }
    // a null frame waits for the SDK to let go of the last one
    unsafe { (lib.send_video_async_v2)(instance, std::ptr::null()) };
    if let Some(previous) = held.take() {
        previous.release();
    }
    unsafe { (lib.send_destroy)(instance) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_the_tm_element() {
        assert_eq!(
            metadata(42, 123_456_789_000),
            r#"<tm seq="42" ts="123456789000"/>"#
        );
    }

    #[test]
    fn pack_shader_validates() {
        use wgpu::naga;
        let source = format!(
            "{}\n{}",
            fullscreen_vertex_source(),
            include_str!("pack.wgsl")
        );
        let module = naga::front::wgsl::parse_str(&source).expect("pack.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("pack.wgsl validates");
    }

    #[test]
    fn sizes_parse_as_w_by_h() {
        assert_eq!(parse_size("640x360"), Ok((640, 360)));
        assert_eq!(parse_size("960X540"), Ok((960, 540)));
        assert!(parse_size("641x360").is_err());
        assert!(parse_size("640").is_err());
        assert!(parse_size("32x360").is_err());
        assert!(parse_size("640x100").is_err());
        assert!(parse_size("7680x4320").is_err());
    }

    #[test]
    fn hd_rows_need_no_padding() {
        assert_eq!(stride(1920), 3840);
        assert_eq!(stride(1280), 2560);
        assert_eq!(stride(960), 2048);
        assert_eq!(stride(640), 1280);
        assert_eq!(stride(1000), 2048);
    }
}
