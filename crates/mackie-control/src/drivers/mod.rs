//! One driver per thing a fader can control. Each speaks its own dialect; the bridge only ever calls the
//! [`Driver`] interface.
//!
//! A driver that cannot do something answers [`Unsupported`] -- the bridge turns that into a warning,
//! never a crash.

use std::collections::BTreeMap;

use serde_yaml::Value;

use crate::profile::{self, Destination};

pub mod hd8;
pub mod mac;
pub mod openrig;
pub mod pedal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

pub type Result<T> = std::result::Result<T, Unsupported>;

pub fn unsupported<T>(msg: impl Into<String>) -> Result<T> {
    Err(Unsupported(msg.into()))
}

/// A fader a live bank has right now, as the driver hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFader {
    pub target: String,
    pub label: String,
    pub group: String,
    pub mute: Option<String>,
}

/// Targets are `""` when the profile names none.
pub trait Driver: Send {
    fn name(&self) -> &str;

    /// Buttons this driver knows what to do with, as (button, command). The bridge uses these when the
    /// profile says nothing, so a profile does not have to repeat what a driver already knows. "scene" is
    /// special: it means load_scene(), everything else goes to command().
    fn buttons(&self) -> &'static [(&'static str, &'static str)] {
        buttons(self.name())
    }

    /// The faders of a bank the device decides while it runs -- for gear whose channels change with what is
    /// loaded (an OpenRig project). A default bank with `live: which` asks this instead of listing faders.
    fn live_faders(&mut self, _which: &str) -> Result<BTreeMap<u32, LiveFader>> {
        unsupported(format!("{}: live faders", self.name()))
    }

    /// 0..1, or None when the driver cannot read it back.
    fn read(&mut self, target: &str) -> Result<Option<f64>> {
        unsupported(format!("{}: read {target}", self.name()))
    }

    fn write(&mut self, target: &str, _value: f64) -> Result<()> {
        unsupported(format!("{}: write {target}", self.name()))
    }

    /// The new state.
    fn toggle(&mut self, target: &str) -> Result<bool> {
        unsupported(format!("{}: toggle {target}", self.name()))
    }

    /// Run a named command (play, next track, ...).
    fn command(&mut self, _target: &str, name: &str) -> Result<()> {
        unsupported(format!("{}: command '{name}'", self.name()))
    }

    /// True while the target is playing, False while it is not, None when there is nothing to ask -- the
    /// app is closed, or the driver has no player at all. The lamps of a transport button follow this.
    fn playing(&mut self, _target: &str) -> Result<Option<bool>> {
        Ok(None)
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// The index of the scene the device has loaded right now, or None when it cannot say -- the HD 8
    /// cannot (measured 2026-09-22).
    fn current_scene(&mut self) -> Result<Option<usize>> {
        Ok(None)
    }

    fn load_scene(&mut self, _index: usize) -> Result<()> {
        unsupported(format!("{}: scenes", self.name()))
    }
}

/// Every driver a profile can name.
pub const KNOWN: [&str; 6] = ["hd8", "mac", "app", "mk300", "ampero2", "openrig"];

pub fn build(name: &str) -> std::result::Result<Box<dyn Driver>, String> {
    Ok(match name {
        "hd8" => Box::new(hd8::Hd8::new()),
        "mac" => Box::new(mac::MacVolume::new()),
        "app" => Box::new(mac::AppVolume::new()),
        "mk300" => Box::new(pedal::Mk300::new()),
        "ampero2" => Box::new(pedal::Ampero2::new()),
        "openrig" => Box::new(openrig::OpenRig::new()),
        _ => return Err(format!("unknown driver '{name}'; known: {}", KNOWN.join(", "))),
    })
}

/// A driver's buttons, without building it: reading them must not open a connection to gear.
pub fn buttons(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "hd8" | "mk300" | "ampero2" => &[("rec", "scene")], // R n loads the n-th stored scene
        "app" => &[("play", "playpause"), ("stop", "pause"), ("forward", "next track"), ("rewind", "previous track")],
        _ => &[],
    }
}

/// One of a device's own banks, as a profile would write it.
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultBank {
    pub name: String,
    pub live: Option<String>,
    pub faders: BTreeMap<u32, Destination>,
}

fn default_bank(yaml: &str) -> DefaultBank {
    let v: Value = serde_yaml::from_str(yaml).expect("a driver's default bank is valid YAML");
    let m = v.as_mapping().expect("a default bank is a mapping");
    DefaultBank {
        name: m.get("name").and_then(Value::as_str).unwrap_or_default().into(),
        live: m.get("live").and_then(Value::as_str).map(Into::into),
        faders: profile::destinations(m.get("faders").and_then(Value::as_mapping))
            .expect("a driver's default bank is a valid profile bank"),
    }
}

/// The banks this device is worth on its own: the same gear has the same knobs in every rig, so the map
/// belongs to the driver. A profile that writes its own faders is never touched.
pub fn default_banks(name: &str) -> Vec<DefaultBank> {
    match name {
        "hd8" => hd8::default_banks(),
        "mk300" => vec![default_bank(
            "{name: MK-300, faders: {1: {driver: mk300, target: volume, label: MK-300 VOL, group: out}}}",
        )],
        "ampero2" => vec![default_bank(
            "{name: AMPERO, faders: {1: {driver: ampero2, target: volume, label: AMPERO VOL, group: out}}}",
        )],
        // One device: its outputs and its inputs are two scenes the arrows page through, not two banks
        // (2026-10-01).
        "openrig" => vec![DefaultBank { name: "OpenRig".into(), live: Some("mixer".into()), faders: BTreeMap::new() }],
        _ => Vec::new(),
    }
}
