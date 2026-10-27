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

pub(super) struct Worker {
    pub device: Device,
    pub width: u32,
    pub height: u32,
    pub format: SourceFormat,
    pub shared: Arc<Shared>,
}

enum Recovery {
    Retry,
    Stop,
}

impl Worker {
    pub(super) fn run(self) {
        let mut stream = match Stream::with_buffers(&self.device, Type::VideoCapture, BUFFER_COUNT)
        {
            Ok(s) => s,
            Err(e) => {
                error!("cannot map capture buffers: {e}");
                return;
            }
        };
        stream.set_timeout(POLL_TIMEOUT);

        while !self.stopped() {
            let err = match stream.next() {
                Ok((buf, meta)) => {
                    let used = (meta.bytesused as usize).min(buf.len());
                    if let Some(frame) = self.build_frame(&buf[..used]) {
                        self.shared.publish(frame);
                    }
                    continue;
                }
                Err(e) => e,
            };
            if let Recovery::Stop = classify(&err, "capture") {
                return;
            }
            // next() queues the last dequeued buffer before it dequeues the
            // next one, so after a failed dequeue every buffer is queued and
            // a second next() would queue one twice (EINVAL). dequeue one by
            // hand to get back to the expected state; that frame is lost.
            loop {
                if self.stopped() {
                    return;
                }
                match CaptureStream::dequeue(&mut stream) {
                    Ok(_) => break,
                    Err(e) => {
                        if let Recovery::Stop = classify(&e, "recover") {
                            return;
                        }
                    }
                }
            }
        }
    }

    fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Relaxed)
    }

    fn build_frame(&self, bytes: &[u8]) -> Option<Frame> {
        let received = Instant::now();
        match self.format {
            SourceFormat::Yuyv => {
                let expected = (self.width * self.height * 2) as usize;
                if bytes.len() < expected {
                    debug!("short yuyv buffer: {} of {expected} bytes", bytes.len());
                    return None;
                }
                Some(Frame {
                    width: self.width,
                    height: self.height,
                    layout: PixelLayout::Yuyv,
                    data: bytes[..expected].to_vec(),
                    seq: 0,
                    received,
                })
            }
            SourceFormat::Mjpeg => {
                if bytes.is_empty() {
                    debug!("empty mjpeg buffer");
                    return None;
                }
                let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
                let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
                let data = match decoder.decode() {
                    Ok(d) => d,
                    Err(e) => {
                        debug!("mjpeg decode failed: {e}");
                        return None;
                    }
                };
                let info = decoder.info()?;
                Some(Frame {
                    width: u32::from(info.width),
                    height: u32::from(info.height),
                    layout: PixelLayout::Rgba8,
                    data,
                    seq: 0,
                    received,
                })
            }
        }
    }
}

fn classify(err: &io::Error, what: &str) -> Recovery {
    if err.raw_os_error() == Some(ENODEV) {
        error!("{what}: device went away");
        return Recovery::Stop;
    }
    match err.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {
            debug!("{what}: {err}, retrying");
        }
        _ => {
            warn!("{what}: {err}");
            std::thread::sleep(RETRY_DELAY);
        }
    }
    Recovery::Retry
}
