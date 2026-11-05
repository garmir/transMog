//! a websocket subscriber on std alone: plain ws:// only, text frames,
//! pings answered. each message from viz_ctrl carries the whole state,
//! so only the newest one is kept, and the ui is woken only when the
//! transmog object differs from the one before

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;

use super::json::{self, Json};

const READ_TICK: Duration = Duration::from_millis(250);
/// viz_ctrl pushes every TD frame; this long without one is a dead link
const SILENCE: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const MAX_MESSAGE: usize = 1 << 20;

/// the path through the json to the object that holds transMog's pairs
pub const OBJECT_PATH: [&str; 3] = ["assets", "camera", "transmog"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(url: &str) -> Result<Url> {
        let Some(rest) = url.strip_prefix("ws://") else {
            bail!("only ws:// urls are supported: {url}");
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().with_context(|| format!("port in {url}"))?),
            None => (authority, 80),
        };
        if host.is_empty() {
            bail!("no host in {url}");
        }
        Ok(Url {
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }
}

/// the newest transmog object and the seq of the message it came in
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub seq: Option<u64>,
    pub pairs: Vec<(String, String)>,
}

/// pulls the pairs out of one message; none when the message has no
/// transmog object
pub fn extract(text: &str) -> Option<Update> {
    let v = json::parse(text).ok()?;
    let mut node = &v;
    for key in OBJECT_PATH {
        node = node.get(key)?;
    }
    let Json::Object(members) = node else {
        return None;
    };
    let seq = match v.get("seq") {
        Some(Json::Number(n)) => n.parse().ok(),
        _ => None,
    };
    Some(Update {
        seq,
        pairs: members
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_value()?)))
            .collect(),
    })
}

#[derive(Default)]
struct Shared {
    latest: Option<Update>,
    status: String,
    messages: u64,
}

pub struct Subscription {
    url: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Subscription {
    /// connects out on a thread of its own and keeps reconnecting, from
    /// 0.5 s up to 10 s apart; `wake` runs when a new update is waiting
    pub fn start(url: &str, wake: impl Fn() + Send + 'static) -> Result<Subscription> {
        let parsed = Url::parse(url)?;
        let shared = Arc::new(Mutex::new(Shared {
            status: "connecting".into(),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("live-ws".into())
                .spawn(move || run(&parsed, &shared, &stop, &wake))
                .context("spawn websocket thread")?;
        }
        Ok(Subscription {
            url: url.to_string(),
            shared,
            stop,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// the update that arrived since the last take
    pub fn take(&self) -> Option<Update> {
        self.shared.lock().latest.take()
    }

    /// `connected`, `connecting` or `retrying (why)`
    pub fn status(&self) -> String {
        self.shared.lock().status.clone()
    }

    /// messages read since the start, over every connection
    pub fn messages(&self) -> u64 {
        self.shared.lock().messages
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // the thread sees this within one read tick or backoff step
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(url: &Url, shared: &Mutex<Shared>, stop: &AtomicBool, wake: &dyn Fn()) {
    let mut backoff = BACKOFF_MIN;
    let mut last: Option<Vec<(String, String)>> = None;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let result = session(url, shared, stop, &mut |update| {
            if last.as_ref() == Some(&update.pairs) {
                return;
            }
            last = Some(update.pairs.clone());
            shared.lock().latest = Some(update);
            wake();
        });
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let reason = match result {
            Ok(()) => "closed by server".to_string(),
            Err(e) => format!("{e:#}"),
        };
        if started.elapsed() > BACKOFF_MAX {
            backoff = BACKOFF_MIN;
        }
        tracing::warn!(
            "control ws {}:{}{}: {reason}; retry in {:?}",
            url.host,
            url.port,
            url.path,
            backoff
        );
        shared.lock().status = format!("retrying ({reason})");
        let until = Instant::now() + backoff;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn connect(url: &Url) -> Result<TcpStream> {
    let addrs = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .with_context(|| format!("resolve {}", url.host))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| anyhow::anyhow!("no address"), Into::into))
}

/// a fresh 16 byte nonce, base64; the server only echoes a hash of it
fn nonce() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let bytes = (t ^ (u128::from(std::process::id()) << 64)).to_le_bytes();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= n {
                out.push(A[(v >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn session(
    url: &Url,
    shared: &Mutex<Shared>,
    stop: &AtomicBool,
    on_update: &mut dyn FnMut(Update),
) -> Result<()> {
    let mut stream = connect(url)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_TICK))?;
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        url.path,
        url.host,
        url.port,
        nonce()
    )?;
    let mut buf = Vec::new();
    let mut heard = Instant::now();
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 16 * 1024 {
            bail!("handshake reply too long");
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        fill(&mut stream, &mut buf, &mut heard, stop)?;
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let status = head.lines().next().unwrap_or("");
    if status.split_whitespace().nth(1) != Some("101") {
        bail!("handshake refused: {status}");
    }
    buf.drain(..head_end);
    {
        let mut s = shared.lock();
        s.status = "connected".into();
    }
    tracing::info!(
        "control ws connected to {}:{}{}",
        url.host,
        url.port,
        url.path
    );

    let mut message: Vec<u8> = Vec::new();
    loop {
        while let Some(frame) = take_frame(&mut buf)? {
            match frame.opcode {
                0x0..=0x2 => {
                    if frame.opcode != 0x0 {
                        message.clear();
                    }
                    message.extend_from_slice(&frame.payload);
                    if message.len() > MAX_MESSAGE {
                        bail!("message over {MAX_MESSAGE} bytes");
                    }
                    if frame.fin {
                        shared.lock().messages += 1;
                        if let Some(update) = extract(&String::from_utf8_lossy(&message)) {
                            on_update(update);
                        }
                        message.clear();
                    }
                }
                0x8 => {
                    let _ = stream.write_all(&client_frame(0x8, &[]));
                    return Ok(());
                }
                0x9 => stream.write_all(&client_frame(0xa, &frame.payload))?,
                _ => {}
            }
        }
        if stop.load(Ordering::Relaxed) {
            let _ = stream.write_all(&client_frame(0x8, &[]));
            return Ok(());
        }
        fill(&mut stream, &mut buf, &mut heard, stop)?;
    }
}

/// reads what is there; a quiet tick is fine until SILENCE has passed
fn fill(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    heard: &mut Instant,
    stop: &AtomicBool,
) -> Result<()> {
    let mut chunk = [0u8; 16 * 1024];
    match stream.read(&mut chunk) {
        Ok(0) => bail!("connection closed"),
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            *heard = Instant::now();
            Ok(())
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if heard.elapsed() > SILENCE {
                bail!("nothing heard for {} s", SILENCE.as_secs());
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// one whole frame off the front of `buf`, or none until more arrives
fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Frame>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0f;
    let masked = buf[1] & 0x80 != 0;
    let (len, mut at) = match buf[1] & 0x7f {
        126 if buf.len() >= 4 => (u64::from(u16::from_be_bytes([buf[2], buf[3]])), 4),
        127 if buf.len() >= 10 => (u64::from_be_bytes(buf[2..10].try_into().expect("8")), 10),
        126 | 127 => return Ok(None),
        n => (u64::from(n), 2),
    };
    if len > MAX_MESSAGE as u64 {
        bail!("frame of {len} bytes");
    }
    let len = len as usize;
    let mask = if masked {
        if buf.len() < at + 4 {
            return Ok(None);
        }
        at += 4;
        Some([buf[at - 4], buf[at - 3], buf[at - 2], buf[at - 1]])
    } else {
        None
    };
    if buf.len() < at + len {
        return Ok(None);
    }
    let mut payload: Vec<u8> = buf[at..at + len].to_vec();
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    buf.drain(..at + len);
    Ok(Some(Frame {
        fin,
        opcode,
        payload,
    }))
}

/// a client frame must be masked; the key need not be secret here
fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let payload = &payload[..payload.len().min(125)];
    let mask = [0x5a, 0x3c, 0x96, 0xe1];
    let mut out = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_host_port_path() {
        assert_eq!(
            Url::parse("ws://192.168.1.242:9980/viz_ctrl/ws").unwrap(),
            Url {
                host: "192.168.1.242".into(),
                port: 9980,
                path: "/viz_ctrl/ws".into()
            }
        );
        assert_eq!(Url::parse("ws://host").unwrap().path, "/");
        assert!(Url::parse("wss://host/x").is_err());
        assert!(Url::parse("ws://:80/x").is_err());
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(nonce().len(), 24);
    }

    #[test]
    fn frames_come_off_whole_and_in_order() {
        let mut buf = vec![0x81, 3, b'a', b'b', b'c', 0x01, 126, 0, 200];
        buf.extend(std::iter::repeat_n(b'x', 199));
        let first = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!((first.fin, first.opcode), (true, 1));
        assert_eq!(first.payload, b"abc");
        // 199 of 200 payload bytes: wait for more
        assert_eq!(take_frame(&mut buf).unwrap(), None);
        buf.push(b'x');
        let second = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!((second.fin, second.payload.len()), (false, 200));
        assert!(buf.is_empty());
    }

    #[test]
    fn masked_frames_unmask() {
        let mut buf = client_frame(0x9, b"ping");
        let frame = take_frame(&mut buf).unwrap().unwrap();
        assert_eq!(frame.opcode, 0x9);
        assert_eq!(frame.payload, b"ping");
    }

    #[test]
    fn extract_reads_the_transmog_object() {
        let msg = r#"{"seq":5,"assets":{"camera":{"fx":0,"transmog":{"zoom_absolute":120,"qa_lock":1}}}}"#;
        assert_eq!(
            extract(msg),
            Some(Update {
                seq: Some(5),
                pairs: vec![
                    ("zoom_absolute".into(), "120".into()),
                    ("qa_lock".into(), "1".into())
                ]
            })
        );
        assert_eq!(extract(r#"{"seq":5,"assets":{"camera":{}}}"#), None);
        assert_eq!(extract("not json"), None);
    }

    /// a local server speaking just enough of the protocol: the client
    /// connects, takes the update, and reconnects after the server drops it
    #[test]
    fn subscribes_and_reconnects() {
        use std::net::TcpListener;
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for zoom in [110, 130] {
                let (mut s, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    s.read_exact(&mut b).unwrap();
                    head.push(b[0]);
                }
                assert!(String::from_utf8_lossy(&head).starts_with("GET /viz_ctrl/ws "));
                s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                    .unwrap();
                let msg = format!(
                    r#"{{"seq":{zoom},"assets":{{"camera":{{"transmog":{{"zoom_absolute":{zoom}}}}}}}}}"#
                );
                let mut frame = vec![0x81, 126];
                frame.extend_from_slice(&(msg.len() as u16).to_be_bytes());
                frame.extend_from_slice(msg.as_bytes());
                s.write_all(&frame).unwrap();
                thread::sleep(Duration::from_millis(100));
            }
        });
        let (tx, rx) = mpsc::channel();
        let sub = Subscription::start(&format!("ws://127.0.0.1:{port}/viz_ctrl/ws"), move || {
            let _ = tx.send(());
        })
        .unwrap();
        let mut seen = Vec::new();
        while seen.len() < 2 {
            rx.recv_timeout(Duration::from_secs(5)).expect("an update");
            if let Some(u) = sub.take() {
                seen.push(u.pairs[0].1.clone());
            }
        }
        assert_eq!(seen, vec!["110".to_string(), "130".to_string()]);
    }
}
