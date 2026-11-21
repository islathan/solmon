//! Minimal WebSocket client codec (RFC 6455), no IO and no heap, so it can be tested on the
//! laptop and used on the ESP32 with any byte stream.
//!
//! Only what solmon needs: the HTTP upgrade request, unmasked frames from the server (text,
//! ping, close; no fragmentation needed for brainsolpi's small JSON frames) and masked frames
//! from the client (RFC 6455 section 5.3 requires client frames to be masked).
//!
//! The server's `Sec-WebSocket-Accept` is not verified (that needs SHA-1); we only check for
//! `101 Switching Protocols`. Fine for a tracker on a trusted local network.

pub const OP_CONT: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// Writes the upgrade request for `GET /ws` into `out`, returns its length (0 if `out` is too small).
/// `key` is 16 random bytes; it gets base64 encoded as `Sec-WebSocket-Key`.
pub fn handshake_request(host: &str, port: u16, key: &[u8; 16], out: &mut [u8]) -> usize {
    let mut key_b64 = [0u8; 24];
    base64_16(key, &mut key_b64);
    let key_b64 = core::str::from_utf8(&key_b64).unwrap_or("");

    let mut w = Writer { out, len: 0 };
    let _ = core::fmt::write(
        &mut w,
        format_args!(
            "GET /ws HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key_b64}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ),
    );
    if w.len > w.out.len() { 0 } else { w.len }
}

/// If `resp` holds a complete `101` answer, returns the length of the HTTP header (the frames
/// start right after). `Some(Err(()))` = complete but refused, `None` = need more bytes.
pub fn handshake_response(resp: &[u8]) -> Option<Result<usize, ()>> {
    let end = resp.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let ok = resp.starts_with(b"HTTP/1.1 101") || resp.starts_with(b"HTTP/1.0 101");
    Some(if ok { Ok(end) } else { Err(()) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub fin: bool,
    pub opcode: u8,
    /// Payload length in bytes.
    pub len: u64,
    /// Bytes taken by the header itself (2, 4 or 10, +4 if masked).
    pub hdr_len: usize,
    pub mask: Option<[u8; 4]>,
}

/// Parses a frame header at the start of `buf`. `None` = not enough bytes yet.
pub fn parse_header(buf: &[u8]) -> Option<Header> {
    if buf.len() < 2 {
        return None;
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let (len, mut hdr_len) = match buf[1] & 0x7F {
        126 => {
            if buf.len() < 4 {
                return None;
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as u64, 4)
        }
        127 => {
            if buf.len() < 10 {
                return None;
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[2..10]);
            (u64::from_be_bytes(b), 10)
        }
        n => (n as u64, 2),
    };
    let mask = if masked {
        if buf.len() < hdr_len + 4 {
            return None;
        }
        let m = [buf[hdr_len], buf[hdr_len + 1], buf[hdr_len + 2], buf[hdr_len + 3]];
        hdr_len += 4;
        Some(m)
    } else {
        None
    };
    Some(Header { fin, opcode, len, hdr_len, mask })
}

/// Writes one final, masked client frame into `out`; returns its length (0 if `out` is too small).
pub fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4], out: &mut [u8]) -> usize {
    let n = payload.len();
    let hdr = if n < 126 { 2 } else if n <= 0xFFFF { 4 } else { 10 };
    let total = hdr + 4 + n;
    if out.len() < total {
        return 0;
    }
    out[0] = 0x80 | (opcode & 0x0F);
    if n < 126 {
        out[1] = 0x80 | n as u8;
    } else if n <= 0xFFFF {
        out[1] = 0x80 | 126;
        out[2..4].copy_from_slice(&(n as u16).to_be_bytes());
    } else {
        out[1] = 0x80 | 127;
        out[2..10].copy_from_slice(&(n as u64).to_be_bytes());
    }
    out[hdr..hdr + 4].copy_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        out[hdr + 4 + i] = b ^ mask[i % 4];
    }
    total
}

/// Undoes (or applies) a mask in place; only needed if a server ever sent masked frames.
pub fn unmask(data: &mut [u8], mask: [u8; 4]) {
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
}

/// Base64 of exactly 16 bytes -> 24 characters (with `==` padding).
fn base64_16(input: &[u8; 16], out: &mut [u8; 24]) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = 0;
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out[o] = T[(v >> 18) as usize & 63];
        out[o + 1] = T[(v >> 12) as usize & 63];
        out[o + 2] = if chunk.len() > 1 { T[(v >> 6) as usize & 63] } else { b'=' };
        out[o + 3] = if chunk.len() > 2 { T[v as usize & 63] } else { b'=' };
        o += 4;
    }
}

/// `fmt::Write` into a byte slice; counts past the end so overflow can be detected.
struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.out.len() {
                self.out[self.len] = b;
            }
            self.len += 1;
        }
        Ok(())
    }
}
