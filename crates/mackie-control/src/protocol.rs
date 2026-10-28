//! Mackie Control: the language the SMC-Mixer speaks in DAW mode.
//!
//! Nothing here is M-EFCS -- this is the other protocol M-VAVE ships. It is plain MIDI: faders are 14-bit
//! pitch bend, encoders are relative CCs, buttons are notes, and every LED is lit by sending the same note
//! back. Measured on an SMC-Mixer on 2026-09-21 (see docs/smc-mixer.md).

// Button note = BASE + channel (channel is 0-based).
pub const REC: u8 = 0x00;
pub const SOLO: u8 = 0x08;
pub const MUTE: u8 = 0x10;
pub const SELECT: u8 = 0x18;
pub const BANK_LEFT: u8 = 0x2E;
pub const BANK_RIGHT: u8 = 0x2F;
pub const REWIND: u8 = 0x5B;
pub const FORWARD: u8 = 0x5C;
pub const STOP: u8 = 0x5D;
pub const PLAY: u8 = 0x5E;
pub const RECORD: u8 = 0x5F;
// Measured on the SMC-Mixer 2026-09-22: the vertical pair sends too.
pub const ARROW_UP: u8 = 0x60;
pub const ARROW_DOWN: u8 = 0x61;
pub const ARROW_LEFT: u8 = 0x62;
pub const ARROW_RIGHT: u8 = 0x63;

/// Encoder n = CC VPOT + n.
pub const VPOT: u8 = 0x10;
pub const FADER_MAX: u16 = 16383;

pub const ON: u8 = 127;
pub const OFF: u8 = 0;

const CHANNEL_BASES: [(u8, &str); 4] = [(REC, "rec"), (SOLO, "solo"), (MUTE, "mute"), (SELECT, "select")];

const NAMED: [(u8, &str); 11] = [
    (BANK_LEFT, "bank_left"),
    (BANK_RIGHT, "bank_right"),
    (ARROW_LEFT, "arrow_left"),
    (ARROW_RIGHT, "arrow_right"),
    (ARROW_UP, "arrow_up"),
    (ARROW_DOWN, "arrow_down"),
    (REWIND, "rewind"),
    (FORWARD, "forward"),
    (STOP, "stop"),
    (PLAY, "play"),
    (RECORD, "record"),
];

/// The note of a button kind that is one single button ("play", "bank_left", ...), if the surface has it.
pub fn note_of(kind: &str) -> Option<u8> {
    NAMED.iter().find(|(_, k)| *k == kind).map(|(n, _)| *n)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// channel 0-based, value 0..1
    Fader {
        channel: u8,
        value: f64,
    },
    /// channel 0-based, +n / -n detents
    Encoder {
        channel: u8,
        delta: i32,
    },
    Button(Button),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub note: u8,
    pub pressed: bool,
}

impl Button {
    /// Channel of a per-channel button (REC/SOLO/MUTE/SELECT), else None.
    pub fn channel(&self) -> Option<u8> {
        CHANNEL_BASES.iter().find(|(b, _)| (*b..*b + 8).contains(&self.note)).map(|(b, _)| self.note - b)
    }

    pub fn kind(&self) -> &'static str {
        if let Some((_, k)) = CHANNEL_BASES.iter().find(|(b, _)| (*b..*b + 8).contains(&self.note)) {
            return k;
        }
        NAMED.iter().find(|(n, _)| *n == self.note).map_or("unknown", |(_, k)| k)
    }
}

/// Raw MIDI bytes -> what the surface did, or None for anything else.
pub fn decode(msg: &[u8]) -> Option<Event> {
    let (&status, data) = msg.split_first()?;
    let channel = status & 0x0F;
    match (status & 0xF0, data) {
        (0xE0, &[lsb, msb, ..]) => {
            let raw = (u16::from(msb & 0x7F) << 7) | u16::from(lsb & 0x7F);
            Some(Event::Fader { channel, value: f64::from(raw) / f64::from(FADER_MAX) })
        }
        (0xB0, &[cc, v, ..]) if (VPOT..VPOT + 8).contains(&cc) => {
            // relative: 0x01..0x3F turns right, 0x41..0x7F turns left
            let delta = if v < 0x40 { i32::from(v) } else { -(i32::from(v) - 0x40) };
            Some(Event::Encoder { channel: cc - VPOT, delta })
        }
        (0x90, &[note, vel, ..]) => Some(Event::Button(Button { note, pressed: vel > 0 })),
        (0x80, &[note, ..]) => Some(Event::Button(Button { note, pressed: false })),
        _ => None,
    }
}

/// What the bridge sends back to the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    /// Light (or clear) a button's LED.
    Led { note: u8, on: bool },
    /// Where a fader *should* be, 0..FADER_MAX. The fader has no motor: the surface blinks that channel's
    /// LED until the physical position matches this value.
    Position { channel: u8, raw: u16 },
}

impl Out {
    pub fn bytes(&self) -> [u8; 3] {
        match *self {
            Out::Led { note, on } => [0x90, note, if on { ON } else { OFF }],
            Out::Position { channel, raw } => [0xE0 | (channel & 0x0F), (raw & 0x7F) as u8, (raw >> 7) as u8],
        }
    }
}

pub fn led(note: u8, on: bool) -> Out {
    Out::Led { note, on }
}

pub fn fader_position(channel: u8, value: f64) -> Out {
    let raw = (value * f64::from(FADER_MAX)).round().clamp(0.0, f64::from(FADER_MAX)) as u16;
    Out::Position { channel, raw }
}
