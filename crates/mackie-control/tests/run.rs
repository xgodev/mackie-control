//! The run loop against a fake CoreMIDI whose ports come and go on a timeline.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mackie_control::bridge::Factory;
use mackie_control::drivers::{Driver, Result as DriverResult};
use mackie_control::profile::parse_str;
use mackie_control::run::{find_port, run, Deps, Input, Midi, Output};
use mackie_control::surfaces;

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn find_port_matches_by_hint() {
    let found = find_port(&strings(&["SMC-Mixer-Master"]), &strings(&["Quantum HD 8 MIDI", "SINCO SMC-Mixer-Master"]));
    assert_eq!(found.unwrap(), "SINCO SMC-Mixer-Master");
}

#[test]
fn find_port_falls_back_to_the_next_hint() {
    // Over Bluetooth the SMC-Mixer shows up as `SMC-Mixer Bluetooth`, with no -Master port at all (measured
    // 2026-09-22).
    let found = find_port(
        &strings(&["SMC-Mixer-Master", "SMC-Mixer"]),
        &strings(&["Quantum HD 8 MIDI", "SMC-Mixer Bluetooth"]),
    );
    assert_eq!(found.unwrap(), "SMC-Mixer Bluetooth");
}

#[test]
fn find_port_prefers_the_earlier_hint() {
    let found =
        find_port(&strings(&["SMC-Mixer-Master", "SMC-Mixer"]), &strings(&["SMC-Mixer-Private", "SMC-Mixer-Master"]));
    assert_eq!(found.unwrap(), "SMC-Mixer-Master");
}

#[test]
fn find_port_without_a_match_names_every_hint() {
    let e = find_port(&strings(&["SMC-Mixer-Master", "SMC-Mixer"]), &strings(&["Quantum HD 8 MIDI"])).unwrap_err();
    assert!(e.contains("no MIDI port") && e.contains("SMC-Mixer"), "{e}");
}

#[test]
fn the_cli_finds_the_surface_the_same_way_the_bridge_does() {
    // `mackie watch` had its own copy of the port lookup and went on calling surface.port_hint after the
    // surface grew several hints (2026-09-22).
    assert!(include_str!("../src/main.rs").contains("find_port"));
    let hints: Vec<String> = surfaces::get(None).unwrap().port_hints.iter().map(|h| h.to_string()).collect();
    assert_eq!(find_port(&hints, &strings(&["SMC-Mixer Bluetooth"])).unwrap(), "SMC-Mixer Bluetooth");
}

// -- the loop --------------------------------------------------------------------------------------------

/// What CoreMIDI shows at one poll: the port names, or the error listing them raised.
type Names = Result<Vec<String>, String>;

struct World {
    names: Names,
    /// One entry per sleep; the run ends when it is empty.
    timeline: VecDeque<Names>,
    /// Every port opened, input or output, and whether it has been closed since.
    opened: Vec<(String, Arc<AtomicBool>)>,
    /// What a freshly opened input has waiting.
    incoming: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct FakeMidi(Arc<Mutex<World>>);

impl FakeMidi {
    fn new(timeline: Vec<Names>) -> Self {
        let mut timeline: VecDeque<Names> = timeline.into();
        let names = timeline.pop_front().expect("a timeline with at least one entry");
        FakeMidi(Arc::new(Mutex::new(World { names, timeline, opened: Vec::new(), incoming: Vec::new() })))
    }

    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        self.0.lock().unwrap()
    }

    fn tick(&self) -> bool {
        let mut w = self.world();
        match w.timeline.pop_front() {
            Some(names) => {
                w.names = names;
                true
            }
            None => false,
        }
    }

    fn open(&mut self, name: &str) -> Arc<AtomicBool> {
        let closed = Arc::new(AtomicBool::new(false));
        self.world().opened.push((name.into(), closed.clone()));
        closed
    }

    fn opened(&self) -> Vec<String> {
        self.world().opened.iter().map(|(n, _)| n.clone()).collect()
    }
}

struct Port {
    closed: Arc<AtomicBool>,
    incoming: Vec<Vec<u8>>,
}

impl Drop for Port {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl Input for Port {
    fn pending(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.incoming)
    }
}

impl Output for Port {
    fn send(&mut self, _bytes: &[u8]) {}
}

impl Midi for FakeMidi {
    fn input_names(&mut self) -> Result<Vec<String>, String> {
        self.world().names.clone()
    }

    fn output_names(&mut self) -> Result<Vec<String>, String> {
        self.world().names.clone()
    }

    fn open_input(&mut self, name: &str) -> Result<Box<dyn Input>, String> {
        let incoming = self.world().incoming.clone();
        Ok(Box::new(Port { closed: self.open(name), incoming }))
    }

    fn open_output(&mut self, name: &str) -> Result<Box<dyn Output>, String> {
        Ok(Box::new(Port { closed: self.open(name), incoming: Vec::new() }))
    }
}

const SURFACE: &str = "SINCO SMC-Mixer-Master";

fn on() -> Names {
    Ok(strings(&[SURFACE]))
}

fn off() -> Names {
    Ok(strings(&["Quantum HD 8 MIDI"]))
}

/// The state file goes to a temporary directory, never to the real home.
fn temp_state() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        dir
    });
}

struct Panics;

impl Driver for Panics {
    fn name(&self) -> &str {
        "panics"
    }

    fn read(&mut self, _target: &str) -> DriverResult<Option<f64>> {
        panic!("boom")
    }
}

/// Run until the timeline ends; the lines logged.
fn run_on(midi: &FakeMidi, yaml: &'static str) -> Vec<String> {
    temp_state();
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let l = lines.clone();
    let m = midi.clone();
    let deps = Deps {
        midi: Box::new(midi.clone()),
        log: Arc::new(move |s| l.lock().unwrap().push(s.to_string())),
        sleep: Box::new(move |_| m.tick()),
        watch: 0.0,
        factory: Box::new(|| -> Factory {
            Box::new(|name: &str| match name {
                "panics" => Ok(Box::new(Panics) as Box<dyn Driver>),
                _ => Err(format!("no gear for '{name}' in a test")),
            })
        }),
        transport: false,
        load: Box::new(move |_| parse_str(yaml)),
    };
    run("ignored.yaml", None, deps).unwrap();
    let out = lines.lock().unwrap().clone();
    out
}

const RIG: &str = "banks: [{name: rig}]";

#[test]
fn run_reopens_the_surface_after_it_comes_back() {
    // Switching the SMC-Mixer off takes its port out of CoreMIDI without any error reaching the reader: the
    // loop has to watch the port list (measured 2026-09-22).
    let midi = FakeMidi::new(vec![on(), on(), off(), off(), on(), on()]);
    let lines = run_on(&midi, RIG);
    assert_eq!(midi.opened().iter().filter(|n| *n == SURFACE).count(), 4); // in and out, twice
    assert!(lines.iter().any(|l| l.contains("gone")) && lines.iter().any(|l| l.contains("back")));
}

#[test]
fn run_closes_the_ports_of_a_surface_that_went_away() {
    let midi = FakeMidi::new(vec![on(), off(), on()]);
    run_on(&midi, RIG);
    assert!(midi.world().opened[..2].iter().all(|(_, closed)| closed.load(Ordering::SeqCst)));
}

#[test]
fn run_waits_instead_of_giving_up_when_the_surface_is_not_there() {
    let midi = FakeMidi::new(vec![off(), off(), on()]);
    run_on(&midi, RIG);
    assert_eq!(midi.opened(), [SURFACE, SURFACE]);
}

#[test]
fn a_message_that_panics_is_logged_and_the_bridge_keeps_reading() {
    let midi = FakeMidi::new(vec![on(), on(), on()]);
    midi.world().incoming = vec![vec![0xE0, 0, 0x40]]; // fader 1 moves
    let lines = run_on(&midi, "banks: [{name: rig, faders: {1: {driver: panics, target: x}}}]");
    assert!(lines.iter().any(|l| l.contains("boom")), "{lines:?}");
}

#[test]
fn a_port_list_that_fails_does_not_end_the_bridge() {
    // rtmidi raises when a port vanishes while it is listing them; on 2026-10-21 that killed the bridge.
    let midi = FakeMidi::new(vec![on(), on(), Err("portNumber (3) is invalid".into()), on(), on()]);
    let lines = run_on(&midi, RIG);
    assert!(!lines.iter().any(|l| l.contains("gone")));
}

#[test]
fn the_bridge_runs_against_the_expanded_profile() {
    // A profile that says only `driver: hd8` must reach the surface with the device's faders already in
    // place.
    let midi = FakeMidi::new(vec![Ok(strings(&["SMC-Mixer Bluetooth"]))]);
    let lines = run_on(&midi, "banks:\n  - name: HD 8\n    driver: hd8\n");
    assert!(lines.iter().any(|l| l.contains("-> HD 8 IN, HD 8 OUT")), "{lines:?}");
}
