//! Control surfaces: M-VAVE gear that *sends* control instead of processing audio. A surface speaks Mackie
//! Control (`protocol`).

/// M-VAVE SMC-Mixer: 8 faders, 8 endless encoders, 43 buttons, USB-C + BLE.
///
/// In DAW mode it speaks Mackie Control; User/CC mode is set on the device itself with Shift + the two
/// bottom-right buttons. It has no motor and no internal preset: what a fader "is" comes from whoever it
/// is bridged to.
///
/// Measured 2026-09-21: CoreMIDI shows `SMC-Mixer-Master` (and a `-Private` twin); the Mackie Device Query
/// gets no position report back, so a host cannot ask where the faders are -- hence soft takeover in the
/// bridge.
///
/// Measured 2026-09-22: paired over BLE instead, the only port is `SMC-Mixer Bluetooth` -- no -Master at
/// all. Hence two hints, tried in order.
#[derive(Debug, PartialEq, Eq)]
pub struct Surface {
    pub name: &'static str,
    pub port_hints: &'static [&'static str],
    pub faders: u8,
    pub encoders: u8,
    pub channel_buttons: &'static [&'static str],
    pub notes: &'static str,
}

pub static SMC_MIXER: Surface = Surface {
    name: "SMC-Mixer",
    port_hints: &["SMC-Mixer-Master", "SMC-Mixer"],
    faders: 8,
    encoders: 8,
    channel_buttons: &["mute", "solo", "rec", "select"],
    notes: "DAW mode = Mackie Control. Shift + bottom-right pair switches mode; the lit button under Shift \
            is the mode in use. Shift blinking on its own is the low-battery warning.",
};

pub static SURFACES: [(&str, &Surface); 1] = [("smcmixer", &SMC_MIXER)];
pub const DEFAULT: &str = "smc-mixer";

pub fn get(name: Option<&str>) -> Result<&'static Surface, String> {
    let key = name.unwrap_or(DEFAULT).to_lowercase().replace(['-', '_'], "");
    SURFACES.iter().find(|(k, _)| *k == key).map(|(_, s)| *s).ok_or_else(|| {
        let known: Vec<&str> = SURFACES.iter().map(|(k, _)| *k).collect();
        format!("unknown surface {:?}; known: {}", name.unwrap_or(DEFAULT), known.join(", "))
    })
}
