//! Runs the bridge against real MIDI ports, until the process is stopped.
//!
//! The surface can go away under the loop: switching the SMC-Mixer off takes its port out of CoreMIDI
//! **without any error reaching the reader** -- the bridge stays alive and silent forever (measured
//! 2026-09-22). So the loop watches the port list instead of waiting for an error, and reopens the surface
//! when it comes back.
//!
//! This loop is the bridge thread: it owns the [`Bridge`] and its drivers. The MIDI callback only queues
//! bytes; the transport lamps are asked from a thread of their own, with drivers of their own.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::{transport_leds, Bridge, Factory, INTERVAL, TRANSPORT_EVERY};
use crate::defaults::expand;
use crate::profile::{load_profile, Profile};
use crate::state::{default_path, State};
use crate::surfaces;

/// s between reads of the surface.
pub const POLL: f64 = 0.005;
/// s between checks that the surface is still there.
pub const WATCH: f64 = 1.0;
/// s between tries while it is gone.
pub const RETRY: f64 = 1.0;

/// The first port matching the first hint that matches anything. A surface gives several because the same
/// device is named differently depending on how it is connected: `SMC-Mixer-Master` over USB,
/// `SMC-Mixer Bluetooth` over BLE (measured 2026-09-22).
pub fn find_port(hints: &[String], names: &[String]) -> Result<String, String> {
    for hint in hints {
        if let Some(found) = names.iter().find(|n| n.contains(hint.as_str())) {
            return Ok(found.clone());
        }
    }
    Err(format!("no MIDI port matching {hints:?}: {names:?}"))
}

pub trait Input: Send {
    /// Every message that arrived since the last call.
    fn pending(&mut self) -> Vec<Vec<u8>>;
}

pub trait Output: Send {
    fn send(&mut self, bytes: &[u8]);
}

/// CoreMIDI, or a fake of it. A port closes when it is dropped.
pub trait Midi {
    fn input_names(&mut self) -> Result<Vec<String>, String>;
    fn output_names(&mut self) -> Result<Vec<String>, String>;
    fn open_input(&mut self, name: &str) -> Result<Box<dyn Input>, String>;
    fn open_output(&mut self, name: &str) -> Result<Box<dyn Output>, String>;
}

/// midir: the callback only queues what arrives.
pub struct CoreMidi;

struct MidirInput {
    _conn: midir::MidiInputConnection<()>,
    rx: Receiver<Vec<u8>>,
}

impl Input for MidirInput {
    fn pending(&mut self) -> Vec<Vec<u8>> {
        self.rx.try_iter().collect()
    }
}

impl Output for midir::MidiOutputConnection {
    fn send(&mut self, bytes: &[u8]) {
        let _ = midir::MidiOutputConnection::send(self, bytes);
    }
}

fn port_names<T: midir::MidiIO>(io: &T) -> Result<Vec<String>, String> {
    io.ports().iter().map(|p| io.port_name(p).map_err(|e| e.to_string())).collect()
}

fn port<T: midir::MidiIO>(io: &T, name: &str) -> Result<T::Port, String> {
    io.ports()
        .into_iter()
        .find(|p| io.port_name(p).as_deref() == Ok(name))
        .ok_or_else(|| format!("no MIDI port called '{name}'"))
}

impl Midi for CoreMidi {
    fn input_names(&mut self) -> Result<Vec<String>, String> {
        port_names(&midir::MidiInput::new("mackie-control").map_err(|e| e.to_string())?)
    }

    fn output_names(&mut self) -> Result<Vec<String>, String> {
        port_names(&midir::MidiOutput::new("mackie-control").map_err(|e| e.to_string())?)
    }

    fn open_input(&mut self, name: &str) -> Result<Box<dyn Input>, String> {
        let mut io = midir::MidiInput::new("mackie-control").map_err(|e| e.to_string())?;
        io.ignore(midir::Ignore::None);
        let p = port(&io, name)?;
        let (tx, rx) = channel();
        let conn = io
            .connect(&p, "mackie-control-in", move |_, msg, _| drop(tx.send(msg.to_vec())), ())
            .map_err(|e| e.to_string())?;
        Ok(Box::new(MidirInput { _conn: conn, rx }))
    }

    fn open_output(&mut self, name: &str) -> Result<Box<dyn Output>, String> {
        let io = midir::MidiOutput::new("mackie-control").map_err(|e| e.to_string())?;
        let p = port(&io, name)?;
        Ok(Box::new(io.connect(&p, "mackie-control-out").map_err(|e| e.to_string())?))
    }
}

pub type SharedLog = Arc<dyn Fn(&str) + Send + Sync>;
pub type Loader = Box<dyn Fn(&str) -> Result<Profile, String>>;

/// What `run` talks to; the tests replace every piece.
pub struct Deps {
    pub midi: Box<dyn Midi>,
    pub log: SharedLog,
    /// Wait this many seconds; false ends the run.
    pub sleep: Box<dyn FnMut(f64) -> bool>,
    pub watch: f64,
    pub factory: Box<dyn Fn() -> Factory>,
    /// Ask "is it playing?" from a thread of its own.
    pub transport: bool,
    pub load: Loader,
}

impl Deps {
    pub fn real() -> Self {
        Deps {
            midi: Box::new(CoreMidi),
            log: Arc::new(|line| println!("{line}")),
            sleep: Box::new(|s| {
                std::thread::sleep(Duration::from_secs_f64(s));
                true
            }),
            watch: WATCH,
            factory: Box::new(|| Box::new(|name: &str| crate::drivers::build(name))),
            transport: true,
            load: Box::new(load_profile),
        }
    }
}

type Shared = Arc<Mutex<Option<Box<dyn Output>>>>;

fn send_all(out: &Shared, bridge: &mut Bridge) {
    let msgs = bridge.take_out();
    if let Some(port) = out.lock().unwrap().as_mut() {
        for m in msgs {
            port.send(&m.bytes());
        }
    }
}

fn present(hints: &[String], names: Option<Vec<String>>) -> Option<String> {
    find_port(hints, &names?).ok()
}

/// The transport lamps, once a second, with drivers of their own: a 117 ms AppleScript must never delay a
/// button or a fader.
fn spawn_transport(profile: Profile, mut factory: Factory, bank: Arc<AtomicUsize>, out: Shared) {
    std::thread::spawn(move || {
        let mut drivers = HashMap::new();
        loop {
            std::thread::sleep(Duration::from_secs_f64(TRANSPORT_EVERY));
            let leds = transport_leds(&profile, bank.load(Ordering::Relaxed), &mut drivers, &mut factory);
            if let Some(port) = out.lock().unwrap().as_mut() {
                for m in leds {
                    port.send(&m.bytes());
                }
            }
        }
    });
}

pub fn run(profile_path: &str, port: Option<&str>, mut deps: Deps) -> Result<(), String> {
    let profile = expand((deps.load)(profile_path)?)?;
    let surface = surfaces::get(Some(&profile.surface))?;
    let hints: Vec<String> = match port {
        Some(p) => vec![p.to_string()],
        None => surface.port_hints.iter().map(|h| h.to_string()).collect(),
    };
    let log = deps.log.clone();
    let names: Vec<String> = profile.banks.iter().map(|b| b.name.clone()).collect();

    // The output port is replaced on every reconnect, so the bridge sends through whatever is open now
    // instead of holding on to a dead port.
    let out: Shared = Arc::new(Mutex::new(None));
    let bank = Arc::new(AtomicUsize::new(0));
    let bridge_log = log.clone();
    let mut bridge = Bridge::new(
        profile.clone(),
        (deps.factory)(),
        Box::new(move |line| bridge_log(line)),
        Some(State::new(default_path(profile_path))),
    );
    bridge.restore();
    if deps.transport {
        spawn_transport(profile, (deps.factory)(), bank.clone(), out.clone());
    }

    let mut first = true;
    loop {
        let Some(src) = present(&hints, deps.midi.input_names().ok()) else {
            if first {
                log(&format!("waiting for a MIDI port matching {hints:?}"));
            }
            if !(deps.sleep)(crate::run::RETRY) {
                return Ok(());
            }
            continue;
        };
        let dst = present(&hints, deps.midi.output_names().ok()).unwrap_or_else(|| src.clone());
        let (inp, outp) = match (deps.midi.open_input(&src), deps.midi.open_output(&dst)) {
            (Ok(i), Ok(o)) => (i, o),
            (Err(e), _) | (_, Err(e)) => {
                log(&format!("!! cannot open {src}: {e}"));
                if !(deps.sleep)(RETRY) {
                    return Ok(());
                }
                continue;
            }
        };
        *out.lock().unwrap() = Some(outp);
        if first {
            log(&format!("bridge: {} ({src}) -> {}", surface.name, names.join(", ")));
        } else {
            log(&format!("** the surface is back ({src})"));
        }
        bridge.select_bank(bridge.bank_index);
        first = false;
        let going = read_until_gone(&mut bridge, inp, &hints, &mut deps, &out, &bank);
        *out.lock().unwrap() = None;
        if !going {
            return Ok(());
        }
        log(&format!("!! the surface is gone ({src}); waiting for it to come back"));
    }
}

/// Read the surface until its port leaves CoreMIDI. Nothing is raised when it does, so the port list is
/// the only sign. False when the run is over.
fn read_until_gone(
    bridge: &mut Bridge,
    mut inp: Box<dyn Input>,
    hints: &[String],
    deps: &mut Deps,
    out: &Shared,
    bank: &AtomicUsize,
) -> bool {
    let (mut since, mut drained) = (0.0, 0.0);
    loop {
        for msg in inp.pending() {
            // One bad message must not end the bridge: on 2026-10-01 a fader moved while OpenRig was closed
            // raised here, the process died, and every fader went dead with it.
            if let Err(e) = catch_unwind(AssertUnwindSafe(|| bridge.on_midi(&msg))) {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                (deps.log)(&format!("  !! {msg:02x?}: {why}"));
            }
        }
        send_all(out, bridge);
        bank.store(bridge.bank_index, Ordering::Relaxed);
        if !(deps.sleep)(POLL) {
            return false;
        }
        since += POLL;
        drained += POLL;
        if drained >= INTERVAL {
            drained = 0.0;
            bridge.tick();
            send_all(out, bridge);
        }
        if since >= deps.watch {
            since = 0.0;
            let names = deps.midi.input_names().ok();
            if names.is_some() && present(hints, names).is_none() {
                return true;
            }
        }
    }
}
