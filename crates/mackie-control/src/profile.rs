//! The rig profile: what each fader and button of a bank is wired to.
//!
//! ```yaml
//! banks:
//!   - name: HD 8
//!     faders:
//!       1: {driver: hd8, target: global/mainOutVolume, label: MAIN, group: out}
//!       5: {driver: hd8, target: line/ch1/volume, label: GUITAR 1, group: in,
//!           mute: line/ch1/mute}
//!     buttons:
//!       rec: scene        # R n loads scene n of that fader's driver
//!       select: bank      # optional: the square button n jumps straight to bank n
//!                         # (banks always page with Channel / arrows)
//!     transport:          # the buttons along the bottom edge
//!       play: {driver: app, target: Spotify, command: playpause}
//!       forward: {driver: app, target: Spotify, command: next track}
//! ```
//!
//! A fader with `mute:` is muted with that parameter; one without is muted by zeroing its own value, which
//! is remembered and handed back. `group` keeps solo honest: soloing an input must not mute the outputs.
//!
//! Encoders are endless knobs: they nudge their destination up or down instead of jumping to a position,
//! so they need no takeover.
//!
//! A top-level `global:` block holds faders, encoders and transport that work in every bank:
//!
//! ```yaml
//! global:
//!   encoders:
//!     1: {driver: app, target: Spotify, label: Spotify}
//!   transport:
//!     play: {driver: app, target: Spotify, command: playpause}
//! ```
//!
//! See examples/ for complete profiles.

use std::collections::BTreeMap;
use std::path::Path;

use serde_yaml::{Mapping, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub driver: String,
    pub target: Option<String>,
    pub label: String,
    pub group: String,
    pub mute: Option<String>,
    /// Takeover protects gear where a jump hurts (a monitor bus). For a player's own volume it is just
    /// friction, so a destination can waive it.
    pub takeover: bool,
    /// The stretch of the parameter the fader is allowed to reach, as [low, high] in 0..1. A preamp goes
    /// to +75 dB: a fader at the top with no limit is a blown take, so a destination can keep the whole
    /// travel inside a safe range.
    pub range: (f64, f64),
}

impl Destination {
    pub fn new(driver: &str, target: Option<&str>) -> Self {
        Destination {
            driver: driver.into(),
            target: target.map(Into::into),
            label: target.unwrap_or("").into(),
            group: "out".into(),
            mute: None,
            takeover: true,
            range: (0.0, 1.0),
        }
    }

    /// The target as the drivers take it: "" when there is none.
    pub fn target(&self) -> &str {
        self.target.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub driver: String,
    pub target: Option<String>,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bank {
    pub name: String,
    /// The device this bank is, when its buttons and scenes belong to one: the driver whose scenes the
    /// arrows page through. Left out, it is the driver most of the bank's faders use.
    pub driver: Option<String>,
    /// Which scenes of that device, in which order. Left out, the device's own list is used: the profile
    /// speaks up only when a night needs four of the thirty presets the gear holds.
    pub scenes: Vec<String>,
    /// Which of the driver's own banks this one is ("IN", "OUT", ...). Left out on a bank with no faders,
    /// every one of them is used.
    pub default: Option<String>,
    /// A bank whose faders the driver hands over while the bridge runs, because the gear's channels change
    /// with what is loaded: the value is passed to the driver's live_faders().
    pub live: Option<String>,
    pub faders: BTreeMap<u32, Destination>,
    pub encoders: BTreeMap<u32, Destination>,
    pub buttons: BTreeMap<String, String>,
    /// In the order the profile writes them.
    pub transport: Vec<(String, Command)>,
}

impl Bank {
    pub fn named(name: &str) -> Self {
        Bank { name: name.into(), ..Bank::default() }
    }

    pub fn transport(&self, kind: &str) -> Option<&Command> {
        self.transport.iter().find(|(k, _)| k == kind).map(|(_, c)| c)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub surface: String,
    pub banks: Vec<Bank>,
    /// A bank of its own that is always in reach: its faders and transport work whichever bank is
    /// selected. Controlling the music has nothing to do with which set of faders you happen to be on.
    pub globals: Bank,
    /// Sending a fader's real value back makes the surface blink that channel's LED until the physical
    /// fader matches. Useful as a "you are out of sync" sign, annoying if you never intend to chase it.
    /// Off by default.
    pub positions: bool,
}

impl Profile {
    pub fn bank(&self, i: i64) -> &Bank {
        &self.banks[i.rem_euclid(self.banks.len() as i64) as usize]
    }
}

fn show(v: &Value) -> String {
    serde_yaml::to_string(v).map(|s| s.trim().replace('\n', ", ")).unwrap_or_default()
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn field(m: &Mapping, key: &str) -> Option<String> {
    m.get(key).and_then(text)
}

fn truthy(v: Option<&Value>, default: bool) -> bool {
    match v {
        None => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn mapping<'a>(m: &'a Mapping, key: &str) -> Option<&'a Mapping> {
    m.get(key).and_then(Value::as_mapping)
}

fn parse_range(raw: Option<&Value>) -> Result<(f64, f64), String> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok((0.0, 1.0));
    };
    let pair: Option<Vec<f64>> = raw.as_sequence().and_then(|s| {
        s.iter().map(|x| x.as_f64().or_else(|| x.as_str().and_then(|t| t.trim().parse().ok()))).collect()
    });
    let (low, high) = match pair.as_deref() {
        Some(&[low, high]) => (low, high),
        _ => return Err(format!("range must be [low, high] in 0..1: {}", show(raw))),
    };
    if !(0.0 <= low && low < high && high <= 1.0) {
        return Err(format!("range must be 0 <= low < high <= 1: {}", show(raw)));
    }
    Ok((low, high))
}

/// A destination as a profile (or a driver's default bank) writes it.
pub fn destination(raw: &Value) -> Result<Destination, String> {
    let m = raw.as_mapping().ok_or_else(|| format!("destination without a driver: {}", show(raw)))?;
    let driver = field(m, "driver").ok_or_else(|| format!("destination without a driver: {}", show(raw)))?;
    let target = field(m, "target");
    Ok(Destination {
        driver,
        label: field(m, "label").or_else(|| target.clone()).unwrap_or_default(),
        target,
        group: field(m, "group").unwrap_or_else(|| "out".into()),
        mute: field(m, "mute"),
        takeover: truthy(m.get("takeover"), true),
        range: parse_range(m.get("range"))?,
    })
}

fn command(raw: &Value) -> Result<Command, String> {
    let bad = || format!("transport entry needs driver and command: {}", show(raw));
    let m = raw.as_mapping().ok_or_else(bad)?;
    Ok(Command {
        driver: field(m, "driver").ok_or_else(bad)?,
        target: field(m, "target"),
        command: field(m, "command").ok_or_else(bad)?,
    })
}

fn fader_number(k: &Value) -> Result<u32, String> {
    k.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| k.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| format!("not a fader number: {}", show(k)))
}

pub fn destinations(m: Option<&Mapping>) -> Result<BTreeMap<u32, Destination>, String> {
    m.into_iter().flatten().map(|(k, v)| Ok((fader_number(k)?, destination(v)?))).collect()
}

fn buttons(m: Option<&Mapping>) -> BTreeMap<String, String> {
    m.into_iter().flatten().filter_map(|(k, v)| Some((text(k)?, text(v)?))).collect()
}

fn transport(m: Option<&Mapping>) -> Result<Vec<(String, Command)>, String> {
    m.into_iter().flatten().map(|(k, v)| Ok((text(k).unwrap_or_default(), command(v)?))).collect()
}

fn bank(m: &Mapping, name: String) -> Result<Bank, String> {
    Ok(Bank {
        name,
        driver: field(m, "driver"),
        scenes: m
            .get("scenes")
            .and_then(Value::as_sequence)
            .map(|s| s.iter().filter_map(text).collect())
            .unwrap_or_default(),
        default: field(m, "default"),
        live: field(m, "live"),
        faders: destinations(mapping(m, "faders"))?,
        encoders: destinations(mapping(m, "encoders"))?,
        buttons: buttons(mapping(m, "buttons")),
        transport: transport(mapping(m, "transport"))?,
    })
}

pub fn parse_profile(data: &Value) -> Result<Profile, String> {
    let empty = Mapping::new();
    let data = data.as_mapping().unwrap_or(&empty);
    let banks = data.get("banks").and_then(Value::as_sequence).filter(|b| !b.is_empty());
    let Some(banks) = banks else {
        return Err("profile has no banks".into());
    };
    let g = mapping(data, "global").unwrap_or(&empty);
    let mut globals = bank(g, "global".into())?;
    // The global block is only faders, encoders, buttons and transport.
    globals.driver = None;
    globals.scenes = Vec::new();
    globals.default = None;
    globals.live = None;
    Ok(Profile {
        surface: field(data, "surface").unwrap_or_else(|| "smc-mixer".into()),
        positions: truthy(data.get("positions"), false),
        globals,
        banks: banks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let m = b.as_mapping().unwrap_or(&empty);
                bank(m, field(m, "name").unwrap_or_else(|| format!("bank {}", i + 1)))
            })
            .collect::<Result<_, _>>()?,
    })
}

pub fn parse_str(yaml: &str) -> Result<Profile, String> {
    let data: Value = serde_yaml::from_str(yaml).map_err(|e| format!("profile is not valid YAML: {e}"))?;
    parse_profile(&data)
}

/// `~` expanded the way a shell would.
pub fn expand_user(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            Path::new(&home).join(rest.trim_start_matches('/'))
        }
        _ => path.into(),
    }
}

pub fn load_profile(path: &str) -> Result<Profile, String> {
    let path = expand_user(path);
    let text = std::fs::read_to_string(&path).map_err(|_| format!("profile not found: {}", path.display()))?;
    parse_str(&text)
}
