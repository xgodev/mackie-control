//! UCNet framing (the protocol Universal Control speaks to ucdaemon). Pure, no I/O.
//!
//! Framing is measured against a real capture (jpfaria/quantum-hd8 docs/protocol.md), not the public
//! StudioLive UCNet hypothesis, where the two differ:
//!
//! ```text
//! "UC" 00 01 | size: u16 LE | code: 2 ASCII | cbytes: 4 | payload
//! size = 6 + len(payload)   (covers code + cbytes + payload)
//! ```
//!
//! - cbytes are the session address; the daemon replies with the two pairs swapped. [`CB`] is the pair of
//!   the first probe, which only reaches the root session (MIDI endpoints).
//! - JM: payload = u32 LE len + JSON.
//! - ZM: payload = u32 LE len + zlib body. The u32 does NOT bound the zlib body's length (measured):
//!   decompress the whole remainder of the payload, never a slice of it.
//! - KA: keepalive, empty payload.

use std::io::Read;

use serde_json::Value;

pub const MAGIC: &[u8; 4] = b"UC\x00\x01";
pub const CB: [u8; 4] = [0x68, 0x00, 0x65, 0x00];

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub code: String,
    pub cbytes: [u8; 4],
    pub payload: Vec<u8>,
}

pub fn encode(code: &str, payload: &[u8], cbytes: [u8; 4]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&((6 + payload.len()) as u16).to_le_bytes());
    out.extend_from_slice(code.as_bytes());
    out.extend_from_slice(&cbytes);
    out.extend_from_slice(payload);
    out
}

/// Buffers partial data and decodes complete UCNet frames. Bytes before a magic are dropped.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Message> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(i) = self.buf.windows(MAGIC.len()).position(|w| w == MAGIC) else {
                // Keep a tail long enough to contain a split magic.
                let keep = self.buf.len().min(MAGIC.len() - 1);
                self.buf.drain(..self.buf.len() - keep);
                return out;
            };
            self.buf.drain(..i);
            if self.buf.len() < 6 {
                return out;
            }
            let size = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
            if self.buf.len() < 6 + size {
                return out;
            }
            let body: Vec<u8> = self.buf.drain(..6 + size).skip(6).collect();
            if body.len() < 6 {
                continue;
            }
            out.push(Message {
                code: body[..2].iter().map(|&b| b as char).collect(),
                cbytes: [body[2], body[3], body[4], body[5]],
                payload: body[6..].to_vec(),
            });
        }
    }
}

/// A JSON value the way Python's `json.dumps` writes it (`ensure_ascii`), so the bytes match the captures.
pub enum Py<'a> {
    Str(&'a str),
    Int(i64),
}

/// A flat JSON object in Python's spelling, keys in the given order.
pub fn py_object(pairs: &[(&str, Py)], item_sep: &str, key_sep: &str) -> String {
    let items: Vec<String> = pairs
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Py::Str(s) => py_str(s),
                Py::Int(n) => n.to_string(),
            };
            format!("{}{key_sep}{v}", py_str(k))
        })
        .collect();
    format!("{{{}}}", items.join(item_sep))
}

fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// u32 LE length + the JSON text.
pub fn json_payload(json: &str) -> Vec<u8> {
    let mut out = (json.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(json.as_bytes());
    out
}

pub fn parse_json(m: &Message) -> Option<Value> {
    let n = u32::from_le_bytes(m.payload.get(..4)?.try_into().ok()?) as usize;
    serde_json::from_slice(m.payload.get(4..4 + n)?).ok()
}

pub fn pv_payload(path: &str, value: f32) -> Vec<u8> {
    let mut out = path.as_bytes().to_vec();
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&value.to_le_bytes());
    out
}

/// Python's `round(x, 4)`: the daemon's float32 shown to four places.
fn round4(v: f32) -> f64 {
    (v as f64 * 10_000.0).round() / 10_000.0
}

pub fn parse_pv(m: &Message) -> Option<(String, f64)> {
    let nul = m.payload.iter().position(|&b| b == 0)?;
    let rest = &m.payload[nul + 1..];
    let raw: [u8; 4] = rest.get(rest.len().checked_sub(4)?..)?.try_into().ok()?;
    Some((String::from_utf8_lossy(&m.payload[..nul]).into_owned(), round4(f32::from_le_bytes(raw))))
}

/// A PL payload (parameter + label list), measured: path + 0x00 + u16 LE flag + f32 LE normalized value +
/// labels joined by "\n" + a trailing 0x00.
pub fn parse_pl(m: &Message) -> Option<(String, f64, Vec<String>)> {
    let nul = m.payload.iter().position(|&b| b == 0)?;
    let rest = &m.payload[nul + 1..];
    let value = f32::from_le_bytes(rest.get(2..6)?.try_into().ok()?);
    let mut blob = &rest[6..];
    if blob.last() == Some(&0) {
        blob = &blob[..blob.len() - 1];
    }
    let labels = String::from_utf8_lossy(blob).split('\n').map(str::to_owned).collect();
    Some((String::from_utf8_lossy(&m.payload[..nul]).into_owned(), round4(value), labels))
}

/// The zlib-compressed JSON tree of a ZM/ZB payload. The leading u32 is NOT the length of the zlib body
/// (measured): the whole remainder goes to zlib as-is.
pub fn parse_state(m: &Message) -> Option<Value> {
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(m.payload.get(4..)?).read_to_end(&mut raw).ok()?;
    Some(
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| serde_json::json!({ "_raw": raw.iter().map(|&b| b as char).collect::<String>() })),
    )
}
