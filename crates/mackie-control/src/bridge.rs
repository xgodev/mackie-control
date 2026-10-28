//! The bridge: a Mackie surface in, drivers out.
//!
//! Everything the surface cannot know lives here: which fader owns which target (the profile), whether a
//! fader may take over yet (the surface has no motor and never reports its position), what "solo" means on
//! a mixer with no solo bus, and which LEDs to light back.
//!
//! One thread owns a `Bridge` (see `run`): it hands it surface events, calls [`Bridge::tick`] every
//! [`INTERVAL`], and sends whatever [`Bridge::take_out`] returns. Nothing here sleeps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use crate::describe::most_common;
use crate::drivers::{self, Driver, Unsupported};
use crate::profile::{Bank, Command, Destination, Profile};
use crate::protocol::{self, Button, Event, Out};
use crate::state::State;

/// s: a fader sends ~40 messages/s, so write only the last.
pub const INTERVAL: f64 = 0.03;
/// Takeover tolerance, in 0..1.
pub const NEAR: f64 = 0.02;
/// How much one encoder detent moves a value.
pub const STEP: f64 = 1.0 / 64.0;
/// How many times the bank number blinks after a change.
pub const BLINKS: u32 = 3;
/// s of each on and each off phase.
pub const BLINK: f64 = 0.12;
/// s between asks of "is it playing?" and between refreshes of a live bank.
pub const TRANSPORT_EVERY: f64 = 1.0;

pub type Factory = Box<dyn FnMut(&str) -> Result<Box<dyn Driver>, String> + Send>;
pub type Log = Box<dyn FnMut(&str) + Send>;
pub type Clock = Box<dyn Fn() -> f64 + Send>;

static NO_FADERS: BTreeMap<u32, Destination> = BTreeMap::new();

/// The note a transport button kind lights.
pub fn transport_note(kind: &str) -> Option<u8> {
    protocol::note_of(kind).or(match kind {
        "rec" => Some(protocol::REC),
        "solo" => Some(protocol::SOLO),
        "mute" => Some(protocol::MUTE),
        "select" => Some(protocol::SELECT),
        _ => None,
    })
}

/// A number shown as a grid, played out as timed steps.
struct Flash {
    start: f64,
    row: u8,
    column_row: u8,
    column: u8,
    /// How many of the blink phases have been sent.
    sent: u32,
}

impl Flash {
    fn led(&self, phase: u32) -> Out {
        protocol::led(self.column_row + self.column, phase.is_multiple_of(2))
    }
}

pub struct Bridge {
    pub profile: Profile,
    pub state: Option<State>,
    log: Log,
    factory: Factory,
    clock: Clock,
    drivers: HashMap<String, Box<dyn Driver>>,
    out: Vec<Out>,
    pub bank_index: usize,
    /// Fader currently soloed, if any.
    pub soloed: Option<u32>,
    /// Mute state before a solo, per fader.
    muted_before: HashMap<u32, bool>,
    /// Value of targets silenced without a mute.
    pub value_before: HashMap<String, f64>,
    /// Fader whose scene was loaded last.
    pub scene_loaded: Option<u32>,
    /// Device (bank) -> position in its scene list.
    scene_of: BTreeMap<usize, Option<usize>>,
    /// (bank, channel) already in control.
    pub took_over: HashSet<(usize, u8)>,
    /// Last position seen per fader.
    pub last_seen: HashMap<(usize, u8), f64>,
    pending: Vec<(u8, f64)>,
    /// Encoder detents waiting for the next drain.
    nudges: Vec<(Destination, i32)>,
    /// Bank index -> faders a live bank has now.
    live: HashMap<usize, BTreeMap<u32, Destination>>,
    /// Ask the live bank's driver on the next tick.
    live_due: bool,
    live_at: f64,
    flash: Option<Flash>,
}

impl Bridge {
    pub fn new(profile: Profile, factory: Factory, log: Log, state: Option<State>) -> Self {
        let t0 = Instant::now();
        Bridge {
            profile,
            state,
            log,
            factory,
            clock: Box::new(move || t0.elapsed().as_secs_f64()),
            drivers: HashMap::new(),
            out: Vec::new(),
            bank_index: 0,
            soloed: None,
            muted_before: HashMap::new(),
            value_before: HashMap::new(),
            scene_loaded: None,
            scene_of: BTreeMap::new(),
            took_over: HashSet::new(),
            last_seen: HashMap::new(),
            pending: Vec::new(),
            nudges: Vec::new(),
            live: HashMap::new(),
            live_due: true,
            live_at: 0.0,
            flash: None,
        }
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// What the surface should be sent, oldest first.
    pub fn take_out(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    fn send(&mut self, out: Out) {
        self.out.push(out);
    }

    fn log(&mut self, line: &str) {
        (self.log)(line);
    }

    // -- drivers -------------------------------------------------------------------------------------

    pub fn driver(&mut self, name: &str) -> drivers::Result<&mut Box<dyn Driver>> {
        if !self.drivers.contains_key(name) {
            let built = (self.factory)(name).map_err(Unsupported)?;
            self.drivers.insert(name.to_string(), built);
        }
        Ok(self.drivers.get_mut(name).expect("just inserted"))
    }

    /// The value as the fader sees it: the destination's range mapped back onto the full travel.
    fn read(&mut self, dest: &Destination) -> Option<f64> {
        let raw = self.driver(&dest.driver).and_then(|d| d.read(dest.target())).ok()??;
        let (low, high) = dest.range;
        Some(((raw - low) / (high - low)).clamp(0.0, 1.0))
    }

    fn write(&mut self, dest: &Destination, value: f64) -> bool {
        let (low, high) = dest.range;
        match self.driver(&dest.driver).and_then(|d| d.write(dest.target(), low + value * (high - low))) {
            Ok(()) => true,
            Err(e) => {
                self.log(&format!("  !! {}: {e}", dest.label));
                false
            }
        }
    }

    fn read_mute(&mut self, dest: &Destination, mute: &str) -> drivers::Result<bool> {
        match self.driver(&dest.driver)?.read(mute)? {
            Some(v) => Ok(v >= 0.5),
            None => Err(Unsupported(format!("{mute} cannot be read"))),
        }
    }

    // -- state ---------------------------------------------------------------------------------------

    /// The bank as the profile writes it, before a live bank's faders are filled in.
    pub fn base(&self) -> &Bank {
        self.profile.bank(self.bank_index as i64)
    }

    /// The faders of the current bank: a live bank's are the ones its driver handed over last.
    pub fn faders(&self) -> &BTreeMap<u32, Destination> {
        if self.base().live.is_some() {
            self.live.get(&self.bank_index).unwrap_or(&NO_FADERS)
        } else {
            &self.base().faders
        }
    }

    /// Ask a live bank's driver which faders it has now. OpenRig's strips change with every project and
    /// chain opened, so they cannot be written in a profile; this runs once a second and right after a bank
    /// change, never on the MIDI thread.
    pub fn refresh_live(&mut self) {
        self.live_due = false;
        let i = self.bank_index;
        let base = self.base().clone();
        let (Some(which), Some(name)) = (base.live.as_deref(), base.driver.as_deref()) else { return };
        let faders: BTreeMap<u32, Destination> = match self.driver(name).and_then(|d| d.live_faders(which)) {
            Ok(raw) => raw
                .into_iter()
                .filter(|(n, _)| (1..=8).contains(n))
                .map(|(n, f)| {
                    let label = if f.label.is_empty() { f.target.clone() } else { f.label };
                    let dest =
                        Destination { label, group: f.group, mute: f.mute, ..Destination::new(name, Some(&f.target)) };
                    (n, dest)
                })
                .collect(),
            Err(e) => {
                if self.live.get(&i).is_none_or(|f| !f.is_empty()) {
                    self.log(&format!("  !! {}: {e}", base.name));
                }
                BTreeMap::new()
            }
        };
        if self.live.get(&i) == Some(&faders) {
            return;
        }
        let shown: Vec<String> = faders.iter().map(|(n, d)| format!("{n} {}", d.label)).collect();
        let shown = if shown.is_empty() { "no faders".to_string() } else { shown.join(", ") };
        self.live.insert(i, faders);
        self.took_over.retain(|k| k.0 != i);
        self.log(&format!("  .. {}: {shown}", base.name));
        if i == self.bank_index {
            self.push_state();
        }
    }

    /// A global fader wins over the bank's: it is the one that is always there, whichever bank is selected.
    pub fn destination(&self, fader: u32) -> Option<Destination> {
        self.profile.globals.faders.get(&fader).or_else(|| self.faders().get(&fader)).cloned()
    }

    /// Stop at the ends instead of wrapping: one press too many must not put the rig in a device the user
    /// was walking away from.
    pub fn step_bank(&mut self, step: i64) {
        let last = self.profile.banks.len() as i64 - 1;
        let target = (self.bank_index as i64 + step).clamp(0, last.max(0)) as usize;
        if target != self.bank_index {
            self.select_bank(target);
        }
    }

    // -- scenes --------------------------------------------------------------------------------------

    /// Where the current device is in its scene list. Kept per device: one shared position made the HD 8
    /// think it was on the Ampero's scene 7 (2026-09-22).
    pub fn scene_index(&self) -> Option<usize> {
        self.scene_of.get(&self.bank_index).copied().flatten()
    }

    fn set_scene_index(&mut self, value: Option<usize>) {
        self.scene_of.insert(self.bank_index, value);
        self.remember();
    }

    fn remember(&self) {
        let Some(state) = &self.state else { return };
        let names: Vec<&str> = self.profile.banks.iter().map(|b| b.name.as_str()).collect();
        let scenes = self.scene_of.iter().filter_map(|(&i, &s)| Some((names.get(i)?.to_string(), s?))).collect();
        state.save(names[self.bank_index], &scenes);
    }

    /// Go back to the device and scene positions of the last run. Loads nothing: it only sets where the
    /// arrows start from.
    pub fn restore(&mut self) {
        let Some(state) = &self.state else { return };
        let data = state.load();
        let names: Vec<String> = self.profile.banks.iter().map(|b| b.name.clone()).collect();
        if let Some(scenes) = data.get("scenes").and_then(|s| s.as_object()) {
            for (name, scene) in scenes {
                if let (Some(i), Some(s)) = (names.iter().position(|n| n == name), scene.as_u64()) {
                    self.scene_of.insert(i, Some(s as usize));
                }
            }
        }
        if let Some(i) = data.get("device").and_then(|d| d.as_str()).and_then(|d| names.iter().position(|n| n == d)) {
            self.bank_index = i;
        }
    }

    /// The driver of this bank's device: its own `driver:`, else the one most of its faders use.
    fn device(&self) -> Option<String> {
        self.base().driver.clone().or_else(|| self.bank_driver())
    }

    /// Which scenes this device offers, in order: the profile's list when it has one, otherwise whatever
    /// the device itself reports.
    pub fn scene_names(&mut self) -> Vec<String> {
        if !self.base().scenes.is_empty() {
            return self.base().scenes.clone();
        }
        let Some(name) = self.device() else { return Vec::new() };
        self.driver(&name).and_then(|d| d.scenes()).unwrap_or_default()
    }

    /// Page through the device's scenes, loading each as it is reached -- João asked for it to load on the
    /// press (2026-09-22), the way a pedalboard behaves. The ends hold.
    pub fn step_scene(&mut self, step: i64) {
        let names = self.scene_names();
        if names.is_empty() {
            return;
        }
        if self.scene_index().is_none() {
            let on_device = self.scene_on_device(&names);
            self.set_scene_index(on_device);
        }
        let start = self.scene_index().map_or(0, |s| s as i64 + step);
        let i = start.clamp(0, names.len() as i64 - 1) as usize;
        // At the end of the list the arrow does nothing: reloading the same scene throws away every unsaved
        // tweak on a pedal (2026-09-22).
        if Some(i) == self.scene_index() {
            return;
        }
        if self.load_scene_at(i) {
            self.flash_number(i, protocol::SOLO);
        }
    }

    /// Where the device itself is, as a position in `names` -- or None when it cannot say. A scene picked
    /// on the device must be the starting point of the next arrow, not overwritten by the first scene.
    fn scene_on_device(&mut self, names: &[String]) -> Option<usize> {
        let name = self.device()?;
        let drv = self.driver(&name).ok()?;
        let i = drv.current_scene().ok()??;
        let all = drv.scenes().ok()?;
        let scene = all.get(i)?;
        names.iter().position(|n| n == scene)
    }

    pub fn load_scene_at(&mut self, i: usize) -> bool {
        let names = self.scene_names();
        let Some(name) = self.device() else { return false };
        if i >= names.len() {
            return false;
        }
        let loaded = self.driver(&name).and_then(|drv| {
            let all = drv.scenes()?;
            drv.load_scene(all.iter().position(|s| *s == names[i]).unwrap_or(i))
        });
        if let Err(e) = loaded {
            self.log(&format!("  !! scene {}: {e}", names[i]));
            return false;
        }
        self.set_scene_index(Some(i));
        self.live_due = true; // a live bank's faders follow its scene
        self.soloed = None;
        self.muted_before.clear();
        self.took_over.clear();
        self.log(&format!("  -> scene {}/{}: {}", i + 1, names.len(), names[i]));
        self.push_state();
        true
    }

    pub fn select_bank(&mut self, i: usize) {
        if i >= self.profile.banks.len() {
            return;
        }
        self.bank_index = i;
        self.live_due = true;
        self.remember();
        self.took_over.clear();
        let line = format!("** bank {}/{}: {}", i + 1, self.profile.banks.len(), self.base().name);
        self.log(&line);
        self.push_state();
        self.flash_bank();
    }

    pub fn flash_bank(&mut self) {
        self.flash_number(self.bank_index, protocol::REC);
    }

    /// Show a number as a grid, then get out of the way.
    ///
    /// The lamp under each knob is the **fader-position lamp**: nothing but a pitch bend addresses it, and
    /// it blinks until the physical fader matches (measured 2026-09-22). That is the lamp João reads as the
    /// row, so the row of eight is a pitch bend on that channel; the column is a button lamp -- R for the
    /// device, S for the scene.
    ///
    /// The row is always shown: not showing it was worse than the blink it avoids (2026-09-22, "a luz que
    /// marca a linha NAO esta acendendo"). The flash plays out over [`Bridge::tick`]; a new one finishes
    /// the one still playing first.
    pub fn flash_number(&mut self, n: usize, column_row: u8) {
        self.finish_flash();
        let (row, column) = (n / 8, n % 8);
        if row > 7 {
            return; // beyond 64 there is nothing to show
        }
        let flash = Flash { start: (self.clock)(), row: row as u8, column_row, column: column as u8, sent: 1 };
        self.send(protocol::fader_position(flash.row, 0.0));
        self.send(flash.led(0));
        self.flash = Some(flash);
    }

    /// The whole flash at once, with no waiting between its phases.
    pub fn flash_number_now(&mut self, n: usize, column_row: u8) {
        self.flash_number(n, column_row);
        self.finish_flash();
    }

    /// Send whatever phases of the flash are due by now, and end it when its time is up.
    fn tick_flash(&mut self, now: f64) {
        let Some(flash) = &mut self.flash else { return };
        let due = (((now - flash.start) / BLINK).floor().max(0.0) as u32 + 1).min(2 * BLINKS);
        let mut phases = Vec::new();
        while flash.sent < due {
            phases.push(flash.led(flash.sent));
            flash.sent += 1;
        }
        let done = now - flash.start >= 2.0 * f64::from(BLINKS) * BLINK;
        self.out.extend(phases);
        if done {
            self.finish_flash();
        }
    }

    /// The blink would otherwise never stop, so the flash ends by handing that channel back the position it
    /// was last seen at -- the surface compares against **where the physical fader is**, and only the
    /// position that fader itself reported settles it (measured 2026-09-22). Until that channel has been
    /// touched there is nothing to send, so the row stays blinking: one touch of that fader stops it.
    /// It is a flash, not a state: `push_state` puts the real LEDs back right after.
    fn finish_flash(&mut self) {
        let Some(mut flash) = self.flash.take() else { return };
        while flash.sent < 2 * BLINKS {
            self.out.push(flash.led(flash.sent));
            flash.sent += 1;
        }
        if let Some(&home) = self.last_seen.get(&(self.bank_index, flash.row)) {
            self.send(protocol::fader_position(flash.row, home));
        }
        self.push_state();
    }

    /// Everything the surface can show: fader positions and LEDs.
    pub fn push_state(&mut self) {
        let select_is_bank = self.base().buttons.get("select").map(String::as_str) == Some("bank");
        for fader in 1..=8u32 {
            let ch = (fader - 1) as u8;
            let dest = self.destination(fader);
            // The value is read only when it is going to be sent: a read costs a round trip (117 ms to
            // Spotify through AppleScript, measured 2026-09-22). Only a fader with a destination AND a
            // readable value gets a position: sending one to an unmapped fader makes the surface blink it
            // forever, because nothing will ever align.
            if self.profile.positions {
                if let Some(value) = dest.as_ref().and_then(|d| self.read(d)) {
                    self.send(protocol::fader_position(ch, value));
                }
            }
            let muted = self.is_muted(fader);
            self.send(protocol::led(protocol::MUTE + ch, muted));
            self.send(protocol::led(protocol::SOLO + ch, self.soloed == Some(fader)));
            self.send(protocol::led(protocol::REC + ch, self.scene_loaded == Some(fader)));
            // The square button only gets a LED when the profile gave it a job. Banks page with the arrows
            // and there can be any number of them, so eight lamps cannot stand for "the active bank".
            self.send(protocol::led(protocol::SELECT + ch, select_is_bank && self.bank_index == ch as usize));
        }
    }

    /// Light a transport button while its player is there, and leave it dark while the app is closed. Asking
    /// costs a round trip, so the bridge thread leaves this to a thread of its own (see `run`).
    pub fn push_transport(&mut self) {
        let leds = transport_leds(&self.profile, self.bank_index, &mut self.drivers, &mut self.factory);
        self.out.extend(leds);
    }

    fn is_muted(&mut self, fader: u32) -> bool {
        let Some(dest) = self.destination(fader) else { return false };
        match dest.mute.clone() {
            None => self.value_before.contains_key(dest.target()),
            Some(mute) => self.read_mute(&dest, &mute).unwrap_or(false),
        }
    }

    // -- faders --------------------------------------------------------------------------------------

    pub fn fader(&mut self, channel: u8, value: f64) {
        let Some(dest) = self.destination(u32::from(channel) + 1) else { return };
        let key = (self.bank_index, channel);
        if !dest.takeover {
            self.took_over.insert(key);
        }
        if !self.took_over.contains(&key) {
            let current = self.read(&dest);
            let previous = self.last_seen.get(&key).copied();
            // Gear that cannot answer (OpenRig closed) cannot hold the fader.
            let crossed = matches!((current, previous), (Some(c), Some(p)) if (p - c) * (value - c) < 0.0);
            match current {
                Some(c) if (value - c).abs() > NEAR && !crossed => {
                    if previous.is_none() {
                        self.log(&format!(
                            "  .. fader {} ({}) is held: move it to {c:.2} to take over (now at {value:.2})",
                            channel + 1,
                            dest.label
                        ));
                    }
                    self.last_seen.insert(key, value);
                    return;
                }
                _ => {
                    self.took_over.insert(key);
                    self.log(&format!("  .. fader {} ({}) took over", channel + 1, dest.label));
                }
            }
        }
        self.last_seen.insert(key, value);
        match self.pending.iter_mut().find(|(c, _)| *c == channel) {
            Some(p) => p.1 = value,
            None => self.pending.push((channel, value)),
        }
    }

    // -- encoders ------------------------------------------------------------------------------------

    /// An endless knob nudges its destination: no position, no takeover.
    ///
    /// The detents are only added up here. Applying one costs a read and a write, and a write can take a
    /// tenth of a second -- 117 ms to Spotify through AppleScript, measured 2026-09-22 -- so the drain
    /// applies the sum, once.
    pub fn encoder(&mut self, channel: u8, delta: i32) {
        let n = u32::from(channel) + 1;
        let Some(dest) = self.profile.globals.encoders.get(&n).or_else(|| self.base().encoders.get(&n)).cloned() else {
            return;
        };
        match self.nudges.iter_mut().find(|(d, _)| *d == dest) {
            Some(n) => n.1 += delta,
            None => self.nudges.push((dest, delta)),
        }
    }

    fn apply_nudge(&mut self, dest: &Destination, delta: i32) {
        let Some(current) = self.read(dest) else {
            self.log(&format!("  !! {}: cannot be read, so it cannot be nudged", dest.label));
            return;
        };
        let new = (current + f64::from(delta) * STEP).clamp(0.0, 1.0);
        if self.write(dest, new) {
            self.log(&format!("  .. {} {new:.2}", dest.label));
        }
    }

    /// Apply the last position of each fader and the summed encoder detents.
    pub fn drain(&mut self) {
        for (dest, delta) in std::mem::take(&mut self.nudges) {
            if delta != 0 {
                self.apply_nudge(&dest, delta);
            }
        }
        for (channel, value) in std::mem::take(&mut self.pending) {
            if let Some(dest) = self.destination(u32::from(channel) + 1) {
                if self.write(&dest, value) && self.profile.positions {
                    self.send(protocol::fader_position(channel, value)); // aligns the LED
                }
            }
        }
    }

    /// Everything that is due: the drain, a live bank's faders (right after a change, then once a second),
    /// and the phases of a flash.
    pub fn tick(&mut self) {
        let now = (self.clock)();
        self.drain();
        if self.live_due || now - self.live_at >= TRANSPORT_EVERY {
            self.live_at = now;
            self.refresh_live();
        }
        self.tick_flash(now);
    }

    // -- buttons -------------------------------------------------------------------------------------

    pub fn mute(&mut self, fader: u32) {
        let Some(dest) = self.destination(fader) else { return };
        let (low, high) = dest.range;
        let target = dest.target().to_string();
        let result = if let Some(mute) = &dest.mute {
            self.driver(&dest.driver).and_then(|d| d.toggle(mute)).map(drop)
        } else if let Some(before) = self.value_before.remove(&target) {
            // No mute: hand it back.
            self.driver(&dest.driver).and_then(|d| d.write(&target, low + before * (high - low)))
        } else {
            // No mute: zero it.
            let before = self.read(&dest).unwrap_or(0.0);
            self.value_before.insert(target.clone(), before);
            self.driver(&dest.driver).and_then(|d| d.write(&target, 0.0))
        };
        if let Err(e) = result {
            self.log(&format!("  !! mute {}: {e}", dest.label));
        }
        self.push_state();
    }

    /// Only this fader's destination plays -- within its own group.
    pub fn solo(&mut self, fader: u32) {
        let Some(chosen) = self.destination(fader) else { return };
        let mut all = self.faders().clone();
        all.extend(self.profile.globals.faders.clone());
        let peers: Vec<(u32, Destination)> = all.into_iter().filter(|(_, d)| d.group == chosen.group).collect();
        if self.soloed == Some(fader) {
            for (n, dest) in &peers {
                if dest.mute.is_some() {
                    let was = self.muted_before.get(n).copied().unwrap_or(false);
                    self.set_mute(dest, was);
                } else if let Some(before) = self.value_before.remove(dest.target()) {
                    self.write(dest, before);
                }
            }
            self.soloed = None;
            self.muted_before.clear();
            self.log("  -> solo off");
        } else {
            if self.soloed.is_none() {
                self.muted_before = peers.iter().map(|(n, _)| (*n, self.is_muted(*n))).collect();
            }
            for (n, dest) in &peers {
                let silence = *n != fader;
                if dest.mute.is_some() {
                    self.set_mute(dest, silence);
                } else if silence {
                    if !self.value_before.contains_key(dest.target()) {
                        let before = self.read(dest).unwrap_or(0.0);
                        self.value_before.insert(dest.target().to_string(), before);
                    }
                    self.write(dest, 0.0);
                } else if let Some(before) = self.value_before.remove(dest.target()) {
                    self.write(dest, before);
                }
            }
            self.soloed = Some(fader);
            self.log(&format!("  -> solo on {}", chosen.label));
        }
        self.push_state();
    }

    /// One refused write must not abort the rest of a solo.
    fn set_mute(&mut self, dest: &Destination, muted: bool) {
        let mute = dest.mute.clone().unwrap_or_default();
        let result = self.read_mute(dest, &mute).and_then(|now| {
            if now != muted {
                self.driver(&dest.driver)?.toggle(&mute)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            self.log(&format!("  !! mute {}: {e}", dest.label));
        }
    }

    /// R n loads the n-th scene **of the same list the arrows page** -- the profile's when it has one. They
    /// used to disagree: R went by the device's own order and the arrows by the profile's.
    pub fn scene(&mut self, fader: u32) {
        if self.load_scene_at(fader as usize - 1) {
            self.scene_loaded = Some(fader);
            self.flash_number(fader as usize - 1, protocol::SOLO);
        }
    }

    // -- input ---------------------------------------------------------------------------------------

    pub fn on_midi(&mut self, msg: &[u8]) {
        match protocol::decode(msg) {
            Some(Event::Fader { channel, value }) => self.fader(channel, value),
            Some(Event::Encoder { channel, delta }) => self.encoder(channel, delta),
            Some(Event::Button(b)) if b.pressed => self.button(b),
            _ => {}
        }
    }

    /// The driver a bank's own buttons belong to: the one most of its faders use. With a single driver per
    /// bank (the usual case) this is just that driver.
    pub fn bank_driver(&self) -> Option<String> {
        most_common(self.faders().values().map(|d| d.driver.as_str()))
    }

    /// What the bank's driver says this button does, if the profile did not say.
    pub fn default_button(&mut self, kind: &str) -> Option<Command> {
        let name = self.bank_driver()?;
        let drv = self.driver(&name).ok()?;
        let command = drv.buttons().iter().find(|(k, _)| *k == kind)?.1;
        let target = self.faders().values().find(|d| d.driver == name).and_then(|d| d.target.clone());
        Some(Command { driver: name, target, command: command.into() })
    }

    pub fn button(&mut self, b: Button) {
        let kind = b.kind();
        let channel = u32::from(b.channel().unwrap_or(0));
        let actions = self.base().buttons.clone();
        match kind {
            "arrow_right" => self.step_scene(1),
            "arrow_left" => self.step_scene(-1),
            "bank_right" | "arrow_down" => self.step_bank(1),
            "bank_left" | "arrow_up" => self.step_bank(-1),
            "mute" => self.mute(channel + 1),
            "solo" => self.solo(channel + 1),
            "rec"
                if actions.get("rec").map(String::as_str) == Some("scene")
                    || (!actions.contains_key("rec")
                        && self.default_button("rec").is_some_and(|c| c.command == "scene")) =>
            {
                self.scene(channel + 1)
            }
            "select" if actions.get("select").map(String::as_str) == Some("bank") => self.select_bank(channel as usize),
            _ => {
                let bound = self.profile.globals.transport(kind).or_else(|| self.base().transport(kind)).cloned();
                if let Some(cmd) = bound.or_else(|| self.default_button(kind)) {
                    self.run_command(&cmd, kind);
                }
            }
        }
    }

    pub fn run_command(&mut self, cmd: &Command, label: &str) {
        let target = cmd.target.clone().unwrap_or_default();
        match self.driver(&cmd.driver).and_then(|d| d.command(&target, &cmd.command)) {
            Ok(()) => self.log(&format!("  -> {label}: {}", cmd.command)),
            Err(e) => self.log(&format!("  !! {label}: {e}")),
        }
    }
}

/// The transport lamps of a bank: lit while the app is there at all (a bound button that does nothing
/// because the app is closed should not look available). One ask per player, not per button: four buttons
/// bound to Spotify meant four round trips a second (2026-09-22).
pub fn transport_leds(
    profile: &Profile,
    bank_index: usize,
    drivers: &mut HashMap<String, Box<dyn Driver>>,
    factory: &mut Factory,
) -> Vec<Out> {
    let mut bound: Vec<(String, Command)> = profile.bank(bank_index as i64).transport.clone();
    for (kind, cmd) in &profile.globals.transport {
        match bound.iter_mut().find(|(k, _)| k == kind) {
            Some(b) => b.1 = cmd.clone(),
            None => bound.push((kind.clone(), cmd.clone())),
        }
    }
    // (driver, target) -> what it answered.
    type Player = (String, Option<String>);
    let mut known: Vec<(Player, Option<bool>)> = Vec::new();
    let mut leds = Vec::new();
    for (kind, cmd) in bound {
        let Some(note) = transport_note(&kind) else { continue };
        let key = (cmd.driver.clone(), cmd.target.clone());
        let playing = match known.iter().find(|(k, _)| *k == key) {
            Some((_, p)) => *p,
            None => {
                let target = cmd.target.clone().unwrap_or_default();
                if !drivers.contains_key(&cmd.driver) {
                    if let Ok(d) = factory(&cmd.driver) {
                        drivers.insert(cmd.driver.clone(), d);
                    }
                }
                let p = drivers.get_mut(&cmd.driver).and_then(|d| d.playing(&target).ok()).flatten();
                known.push((key, p));
                p
            }
        };
        leds.push(protocol::led(note, playing.is_some()));
    }
    leds
}
