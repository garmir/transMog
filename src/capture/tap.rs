use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use v4l::buffer::Metadata;
use v4l::format::FourCC;

pub const MAGIC: &[u8; 8] = b"TMQATAP1";
pub const HEADER_LEN: usize = 64;
/// two seconds at 30 fps, so a reader that polls every few hundred ms
/// sees every frame
pub const DEFAULT_SLOTS: u64 = 64;

/// writes each dequeued buffer, as the driver handed it, into a ring of
/// slot files; see the readme for the layout
pub struct Tap {
    dir: PathBuf,
    slots: u64,
    seq: u64,
    errors: u64,
    last_error: Option<String>,
    scratch: Vec<u8>,
}

/// what the ui shows about a tap
#[derive(Clone, Debug)]
pub struct TapStatus {
    pub dir: PathBuf,
    pub written: u64,
    pub errors: u64,
    pub last_error: Option<String>,
}

impl Tap {
    /// creates `dir` and clears slot files a previous run left there, so
    /// a reader never pairs an old slot with a new `latest`
    pub fn open(dir: &Path, slots: u64) -> Result<Tap> {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let path = entry?.path();
            let stale = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".frame") || n.ends_with(".tmp") || n == "latest");
            if stale {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(Tap {
            dir: dir.to_path_buf(),
            slots: slots.max(2),
            seq: 0,
            errors: 0,
            last_error: None,
            scratch: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn status(&self) -> TapStatus {
        TapStatus {
            dir: self.dir.clone(),
            written: self.seq,
            errors: self.errors,
            last_error: self.last_error.clone(),
        }
    }

    /// `dequeued_ns` is CLOCK_MONOTONIC right after DQBUF returned; a
    /// failed write is counted and never stops the capture
    pub fn write(
        &mut self,
        bytes: &[u8],
        meta: &Metadata,
        fourcc: FourCC,
        size: (u32, u32),
        dequeued_ns: i64,
    ) {
        self.seq += 1;
        let header = Header {
            seq: self.seq,
            sequence: meta.sequence,
            flags: meta.flags.into(),
            timestamp_ns: timeval_ns(meta.timestamp.sec, meta.timestamp.usec),
            dequeued_ns,
            width: size.0,
            height: size.1,
            fourcc: fourcc.repr,
            len: bytes.len() as u32,
        };
        if let Err(e) = self.write_slot(&header, bytes) {
            self.errors += 1;
            self.last_error = Some(e.to_string());
        }
    }

    fn write_slot(&mut self, header: &Header, bytes: &[u8]) -> io::Result<()> {
        let slot = header.seq % self.slots;
        let tmp = self.dir.join(format!(".{slot:03}.tmp"));
        let path = self.dir.join(format!("{slot:03}.frame"));
        self.scratch.clear();
        header.encode(&mut self.scratch);
        self.scratch.extend_from_slice(bytes);
        fs::write(&tmp, &self.scratch)?;
        // rename is atomic, so a reader opens either the old frame or the
        // whole new one, never half of each
        fs::rename(&tmp, &path)?;
        let latest_tmp = self.dir.join(".latest.tmp");
        let mut f = fs::File::create(&latest_tmp)?;
        writeln!(f, "{} {}", header.seq, self.slots)?;
        drop(f);
        fs::rename(&latest_tmp, self.dir.join("latest"))
    }
}

/// the fixed 64 byte little-endian slot header
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub seq: u64,
    pub sequence: u32,
    pub flags: u32,
    pub timestamp_ns: i64,
    pub dequeued_ns: i64,
    pub width: u32,
    pub height: u32,
    pub fourcc: [u8; 4],
    pub len: u32,
}

impl Header {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.timestamp_ns.to_le_bytes());
        out.extend_from_slice(&self.dequeued_ns.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.fourcc);
        out.extend_from_slice(&self.len.to_le_bytes());
        out.resize(start + HEADER_LEN, 0);
    }

    pub fn decode(bytes: &[u8]) -> Option<Header> {
        if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        Some(Header {
            seq: u64_at(8),
            sequence: u32_at(16),
            flags: u32_at(20),
            timestamp_ns: u64_at(24) as i64,
            dequeued_ns: u64_at(32) as i64,
            width: u32_at(40),
            height: u32_at(44),
            fourcc: bytes[48..52].try_into().unwrap(),
            len: u32_at(52),
        })
    }
}

// time_t is 32 bits on some targets, so the casts are not always no-ops
#[allow(clippy::unnecessary_cast)]
fn timeval_ns(sec: libc::time_t, usec: libc::time_t) -> i64 {
    sec as i64 * 1_000_000_000 + usec as i64 * 1_000
}

/// CLOCK_MONOTONIC in nanoseconds, the clock uvcvideo stamps buffers on
pub fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // cannot fail for CLOCK_MONOTONIC with a valid pointer
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    timeval_ns(ts.tv_sec, 0) + ts.tv_nsec
}

/// $XDG_RUNTIME_DIR/transmog/tap, a tmpfs on systemd hosts
pub fn default_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("transmog")
        .join("tap")
}

#[cfg(test)]
mod tests {
    use super::*;
    use v4l::buffer::Flags;
    use v4l::timestamp::Timestamp;

    #[test]
    fn header_round_trips() {
        let h = Header {
            seq: 7,
            sequence: 1234,
            flags: 0x2001,
            timestamp_ns: 98_765_432_101_000,
            dequeued_ns: 98_765_440_000_000,
            width: 1920,
            height: 1080,
            fourcc: *b"MJPG",
            len: 300_000,
        };
        let mut out = Vec::new();
        h.encode(&mut out);
        assert_eq!(out.len(), HEADER_LEN);
        assert_eq!(Header::decode(&out), Some(h));
        out[0] = b'x';
        assert_eq!(Header::decode(&out), None);
    }

    #[test]
    fn ring_overwrites_by_slot() {
        let dir = std::env::temp_dir().join(format!("transmog-tap-test-{}", std::process::id()));
        let mut tap = Tap::open(&dir, 4).unwrap();
        let meta = Metadata {
            bytesused: 3,
            flags: Flags::from(0x2000),
            field: 0,
            timestamp: Timestamp::new(5, 250),
            sequence: 0,
        };
        for i in 0..6u8 {
            tap.write(&[i; 3], &meta, FourCC::new(b"MJPG"), (2, 2), 1);
        }
        let latest = fs::read_to_string(dir.join("latest")).unwrap();
        assert_eq!(latest.trim(), "6 4");
        let slot = fs::read(dir.join("002.frame")).unwrap();
        let header = Header::decode(&slot).unwrap();
        assert_eq!(header.seq, 6);
        assert_eq!(header.timestamp_ns, 5_000_250_000);
        assert_eq!(&slot[HEADER_LEN..], &[5, 5, 5]);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 5);
        fs::remove_dir_all(&dir).unwrap();
    }
}
