use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, error, warn};
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::Device;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use super::{Frame, PixelLayout, Shared, SourceFormat};

const BUFFER_COUNT: u32 = 4;
// bounds how long a stop request waits on a camera that stops delivering
const POLL_TIMEOUT: Duration = Duration::from_millis(500);
const RETRY_DELAY: Duration = Duration::from_millis(100);
const ENODEV: i32 = 19;
// the ui holds at most one previous frame, so two spare buffers keep the
// steady state free of multi-megabyte allocations
const SPARE_BUFFERS: usize = 2;

pub(super) struct Worker {
    pub device: Device,
    pub width: u32,
    pub height: u32,
    pub format: SourceFormat,
    pub shared: Arc<Shared>,
}

enum Recovery {
    /// the dequeue timed out or was interrupted; every buffer is still
    /// queued, so one hand dequeue restores the state next() expects
    Dequeue,
    /// the queue, start or dequeue ioctl failed and the buffer state is
    /// unknown, so the stream is torn down and mapped again
    Restart,
    Stop,
}

impl Worker {
    pub(super) fn run(self) {
        let mut spare: Vec<Vec<u8>> = Vec::new();
        'stream: while !self.stopped() {
            let mut stream =
                match Stream::with_buffers(&self.device, Type::VideoCapture, BUFFER_COUNT) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("cannot map capture buffers: {e}");
                        self.shared.fail(format!("cannot map capture buffers: {e}"));
                        return;
                    }
                };
            stream.set_timeout(POLL_TIMEOUT);

            while !self.stopped() {
                let err = match stream.next() {
                    Ok((buf, meta)) => {
                        let used = (meta.bytesused as usize).min(buf.len());
                        let out = spare.pop().unwrap_or_default();
                        match self.build_frame(&buf[..used], out) {
                            Ok(frame) => {
                                if let Some(previous) = self.shared.publish(frame) {
                                    self.recycle(&mut spare, previous);
                                }
                            }
                            Err(out) => spare.push(out),
                        }
                        continue;
                    }
                    Err(e) => e,
                };
                match self.classify(&err, "capture") {
                    Recovery::Stop => return,
                    Recovery::Restart => continue 'stream,
                    Recovery::Dequeue => {}
                }
                loop {
                    if self.stopped() {
                        return;
                    }
                    match CaptureStream::dequeue(&mut stream) {
                        Ok(_) => break,
                        Err(e) => match self.classify(&e, "recover") {
                            Recovery::Stop => return,
                            Recovery::Restart => continue 'stream,
                            Recovery::Dequeue => {}
                        },
                    }
                }
            }
        }
    }

    fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Relaxed)
    }

    /// keeps the buffer of a frame nobody else holds any more
    fn recycle(&self, spare: &mut Vec<Vec<u8>>, previous: Arc<Frame>) {
        if spare.len() >= SPARE_BUFFERS {
            return;
        }
        if let Ok(frame) = Arc::try_unwrap(previous) {
            spare.push(frame.data);
        }
    }

    /// fills `out` with the frame; on a bad buffer the vec comes back so
    /// it can be reused
    fn build_frame(&self, bytes: &[u8], mut out: Vec<u8>) -> Result<Frame, Vec<u8>> {
        let received = Instant::now();
        match self.format {
            SourceFormat::Yuyv => {
                let expected = (self.width * self.height * 2) as usize;
                if bytes.len() < expected {
                    debug!("short yuyv buffer: {} of {expected} bytes", bytes.len());
                    return Err(out);
                }
                out.clear();
                out.extend_from_slice(&bytes[..expected]);
                Ok(Frame {
                    width: self.width,
                    height: self.height,
                    layout: PixelLayout::Yuyv,
                    data: out,
                    seq: 0,
                    received,
                })
            }
            SourceFormat::Mjpeg => {
                if bytes.is_empty() {
                    debug!("empty mjpeg buffer");
                    return Err(out);
                }
                let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
                let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
                let size = match decoder.decode_headers() {
                    Ok(()) => decoder.output_buffer_size().unwrap_or(0),
                    Err(e) => {
                        debug!("mjpeg header failed: {e}");
                        return Err(out);
                    }
                };
                out.resize(size, 0);
                if let Err(e) = decoder.decode_into(&mut out) {
                    debug!("mjpeg decode failed: {e}");
                    return Err(out);
                }
                let Some(info) = decoder.info() else {
                    return Err(out);
                };
                Ok(Frame {
                    width: u32::from(info.width),
                    height: u32::from(info.height),
                    layout: PixelLayout::Rgba8,
                    data: out,
                    seq: 0,
                    received,
                })
            }
        }
    }

    fn classify(&self, err: &io::Error, what: &str) -> Recovery {
        if err.raw_os_error() == Some(ENODEV) {
            error!("{what}: device went away");
            self.shared.fail("device went away".into());
            return Recovery::Stop;
        }
        match err.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {
                debug!("{what}: {err}, retrying");
                Recovery::Dequeue
            }
            _ => {
                warn!("{what}: {err}, remapping the stream");
                std::thread::sleep(RETRY_DELAY);
                Recovery::Restart
            }
        }
    }
}
