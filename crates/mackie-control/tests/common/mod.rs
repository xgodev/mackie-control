//! A driver with no gear behind it, and the profiles the bridge tests share.
//!
//! The bridge owns the drivers it builds, so the fake's state lives behind an `Arc`: the test keeps a
//! handle and looks at what the bridge did to it.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use mackie_control::bridge::{Bridge, Factory};
use mackie_control::drivers::{unsupported, Driver, LiveFader, Result, Unsupported};
use mackie_control::profile::parse_str;
use mackie_control::protocol::Out;
use mackie_control::state::State;

/// Everything the fake answers and remembers. The knobs stand for the Python fakes' subclasses.
pub struct Gear {
    pub values: HashMap<String, f64>,
    pub loaded: Option<String>,
    /// Every index load_scene was asked for.
    pub loads: Vec<usize>,
    pub current: Option<usize>,
    /// Commands run instead of refused.
    pub commands: bool,
    pub ran: Vec<(String, String)>,
    pub buttons: &'static [(&'static str, &'static str)],
    /// This target's toggle fails.
    pub refuse_toggle: Option<String>,
    /// Reads fail.
    pub blind: bool,
    pub reads: Vec<String>,
    pub playing: Option<bool>,
    pub asked: Vec<String>,
    /// A live bank's strips.
    pub strips: Vec<String>,
    pub live_down: bool,
}

impl Default for Gear {
    fn default() -> Self {
        let values = [
            ("main", 0.5),
            ("phones", 0.4),
            ("g1", 0.2),
            ("g2", 0.8),
            ("main/mute", 0.0),
            ("g1/mute", 0.0),
            ("g2/mute", 0.0),
        ];
        Gear {
            values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            loaded: None,
            loads: Vec::new(),
            current: None,
            commands: false,
            ran: Vec::new(),
            buttons: &[("rec", "scene")], // like a mixer: R loads a stored scene
            refuse_toggle: None,
            blind: false,
            reads: Vec::new(),
            playing: None,
            asked: Vec::new(),
            strips: vec!["main".into(), "phones".into()],
            live_down: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<Gear>>);

impl Fake {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> MutexGuard<'_, Gear> {
        self.0.lock().unwrap()
    }

    pub fn value(&self, target: &str) -> f64 {
        self.get().values[target]
    }

    pub fn set(&self, target: &str, value: f64) {
        self.get().values.insert(target.into(), value);
    }

    pub fn driver(&self, name: &str) -> Box<dyn Driver> {
        Box::new(FakeDriver { name: name.into(), gear: self.clone() })
    }
}

const SCENES: [&str; 2] = ["ONE", "TWO"];

struct FakeDriver {
    name: String,
    gear: Fake,
}

impl Driver for FakeDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn buttons(&self) -> &'static [(&'static str, &'static str)] {
        self.gear.get().buttons
    }

    fn live_faders(&mut self, _which: &str) -> Result<BTreeMap<u32, LiveFader>> {
        let g = self.gear.get();
        if g.live_down {
            return unsupported("openrig: not reachable");
        }
        Ok(g.strips
            .iter()
            .zip(1..)
            .map(|(t, n)| {
                (n, LiveFader { target: t.clone(), label: t.to_uppercase(), group: String::new(), mute: None })
            })
            .collect())
    }

    fn read(&mut self, target: &str) -> Result<Option<f64>> {
        let mut g = self.gear.get();
        g.reads.push(target.into());
        if g.blind {
            return unsupported("no reading");
        }
        g.values.get(target).copied().map(Some).ok_or_else(|| Unsupported(format!("no {target}")))
    }

    fn write(&mut self, target: &str, value: f64) -> Result<()> {
        self.gear.get().values.insert(target.into(), value);
        Ok(())
    }

    fn toggle(&mut self, target: &str) -> Result<bool> {
        let mut g = self.gear.get();
        if g.refuse_toggle.as_deref() == Some(target) {
            return unsupported("the daemon did not confirm the write");
        }
        let new = if g.values.get(target).copied().unwrap_or(0.0) >= 0.5 { 0.0 } else { 1.0 };
        g.values.insert(target.into(), new);
        Ok(new >= 0.5)
    }

    fn command(&mut self, target: &str, name: &str) -> Result<()> {
        let mut g = self.gear.get();
        if !g.commands {
            return unsupported(format!("fake: command '{name}'"));
        }
        g.ran.push((target.into(), name.into()));
        Ok(())
    }

    fn playing(&mut self, target: &str) -> Result<Option<bool>> {
        let mut g = self.gear.get();
        g.asked.push(target.into());
        Ok(g.playing)
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        Ok(SCENES.iter().map(|s| s.to_string()).collect())
    }

    fn current_scene(&mut self) -> Result<Option<usize>> {
        Ok(self.gear.get().current)
    }

    fn load_scene(&mut self, index: usize) -> Result<()> {
        let mut g = self.gear.get();
        g.loads.push(index);
        let Some(name) = SCENES.get(index) else { return unsupported("no such scene") };
        g.loaded = Some(name.to_string());
        Ok(())
    }
}

pub type Lines = Arc<Mutex<Vec<String>>>;

/// A bridge on this profile whose every driver is the fake, and the lines it logged.
pub fn bridge(yaml: &str, fake: &Fake) -> (Bridge, Lines) {
    with_state(yaml, fake, None)
}

pub fn with_state(yaml: &str, fake: &Fake, state: Option<State>) -> (Bridge, Lines) {
    let profile = parse_str(yaml).expect("a valid test profile");
    let f = fake.clone();
    let factory: Factory = Box::new(move |name| Ok(f.driver(name)));
    let lines = Lines::default();
    let l = lines.clone();
    let b = Bridge::new(profile, factory, Box::new(move |s| l.lock().unwrap().push(s.to_string())), state);
    (b, lines)
}

pub fn logged(lines: &Lines) -> Vec<String> {
    lines.lock().unwrap().clone()
}

const RIG_FADERS: &str = "{1: {driver: fake, target: main, label: MAIN, group: out, mute: main/mute}, \
     2: {driver: fake, target: phones, label: PHONES, group: out}, \
     5: {driver: fake, target: g1, label: G1, group: in, mute: g1/mute}, \
     6: {driver: fake, target: g2, label: G2, group: in, mute: g2/mute}}";

/// The two banks of the Python PROFILE; `extra` goes into the first one.
pub fn banks(extra: &str) -> String {
    format!("banks: [{{name: rig, faders: {RIG_FADERS}{extra}}}, {{name: mac, faders: {{1: {{driver: fake, target: main}}}}}}]")
}

pub fn profile() -> String {
    format!("positions: true\n{}", banks(""))
}

pub const SCENES_PROFILE: &str = "banks:
  - {name: HD 8, driver: fake, faders: {1: {driver: fake, target: main}}}
  - {name: Mac, faders: {1: {driver: fake, target: main}}}
  - {name: Third, faders: {1: {driver: fake, target: main}}}
";

/// `n` banks with nothing on them.
pub fn many(n: usize) -> String {
    let banks: Vec<String> = (0..n).map(|i| format!("{{name: b{i}, faders: {{}}}}")).collect();
    format!("banks: [{}]", banks.join(", "))
}

/// The columns lit in the row of eight starting at `base`, in order.
pub fn lit(out: &[Out], base: u8) -> Vec<u8> {
    out.iter()
        .filter_map(|o| match *o {
            Out::Led { note, on: true } if (base..base + 8).contains(&note) => Some(note - base),
            _ => None,
        })
        .collect()
}

pub fn bends(out: &[Out]) -> Vec<(u8, u16)> {
    out.iter()
        .filter_map(|o| match *o {
            Out::Position { channel, raw } => Some((channel, raw)),
            _ => None,
        })
        .collect()
}

/// Every state the LED of `note` was sent, in order.
pub fn led(out: &[Out], note: u8) -> Vec<bool> {
    out.iter()
        .filter_map(|o| match *o {
            Out::Led { note: n, on } if n == note => Some(on),
            _ => None,
        })
        .collect()
}

pub fn note_on(note: u8) -> [u8; 3] {
    [0x90, note, 127]
}
