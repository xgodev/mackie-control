//! Each driver against a fake of its gear: no device, no daemon, no AppleScript.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

use mackie_control::defaults::expand;
use mackie_control::drivers::hd8::{self, Client, Hd8};
use mackie_control::drivers::mac::{AppVolume, MacVolume};
use mackie_control::drivers::openrig::{from_db, to_db, Mcp, McpError, OpenRig};
use mackie_control::drivers::pedal::{Ampero2, AmperoClient, Mk300, Mk300Client};
use mackie_control::drivers::{self, Driver, LiveFader};
use mackie_control::profile::parse_str;
use quantum_hd8::Error;
use serde_json::{json, Value};

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// -- HD 8 ------------------------------------------------------------------------------------------------

/// How a fake client's writes fail.
#[derive(Clone, Copy, PartialEq)]
enum Writes {
    Land,
    /// The socket died: what the HD 8 does after the interface is power-cycled (measured 2026-09-22).
    Dying,
    /// A session the daemon stopped confirming: reads answer, every write times out (measured 2026-09-23
    /// and 2026-09-27).
    Stale,
}

#[derive(Default)]
struct Ucd {
    values: HashMap<String, f64>,
    loaded: Option<(String, bool)>,
    closed: bool,
}

#[derive(Clone)]
struct FakeClient {
    ucd: Arc<Mutex<Ucd>>,
    writes: Writes,
}

impl FakeClient {
    fn new(writes: Writes) -> Self {
        let values = [("global/mainOutVolume", 0.5), ("global/mute", 0.0)];
        let ucd = Ucd { values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(), ..Ucd::default() };
        FakeClient { ucd: Arc::new(Mutex::new(ucd)), writes }
    }

    fn value(&self, path: &str) -> f64 {
        self.ucd.lock().unwrap().values[path]
    }
}

impl Client for FakeClient {
    fn get(&mut self, path: &str) -> Result<f64, Error> {
        self.ucd.lock().unwrap().values.get(path).copied().ok_or_else(|| Error::Unknown(path.into()))
    }

    fn set_raw(&mut self, path: &str, value: f64) -> Result<(), Error> {
        match self.writes {
            Writes::Land => {
                self.ucd.lock().unwrap().values.insert(path.into(), value);
                Ok(())
            }
            Writes::Dying => Err(Error::Io(io::Error::from_raw_os_error(9))),
            Writes::Stale => Err(Error::WriteNotConfirmed(format!("{path}: not confirmed"))),
        }
    }

    fn scenes(&mut self) -> Vec<String> {
        vec!["A.scene".into(), "B.scene".into()]
    }

    fn load_scene(&mut self, name: &str, keep_gains: bool) -> Result<(), Error> {
        self.ucd.lock().unwrap().loaded = Some((name.into(), keep_gains));
        Ok(())
    }

    fn close(&mut self) {
        self.ucd.lock().unwrap().closed = true;
    }
}

fn no_clock() -> Box<dyn Fn() -> f64 + Send> {
    Box::new(|| 0.0)
}

fn hd8_on(c: &FakeClient) -> Hd8 {
    Hd8::with(Box::new(c.clone()), None, no_clock())
}

/// A driver whose first client is `first` and whose reconnects hand over `fresh`, counting the tries.
fn reconnecting(first: Writes, fresh: &FakeClient) -> (Hd8, Arc<Mutex<u32>>) {
    let tries = Arc::new(Mutex::new(0));
    let (t, f) = (tries.clone(), fresh.clone());
    let connect: hd8::Connect = Box::new(move || {
        *t.lock().unwrap() += 1;
        Ok(Box::new(f.clone()) as Box<dyn Client>)
    });
    (Hd8::with(Box::new(FakeClient::new(first)), Some(connect), no_clock()), tries)
}

const MAIN: &str = "global/mainOutVolume";

#[test]
fn hd8_write_is_clamped_and_read_back() {
    let mut d = hd8_on(&FakeClient::new(Writes::Land));
    d.write(MAIN, 2.0).unwrap();
    assert_eq!(d.read(MAIN).unwrap(), Some(1.0));
    d.write(MAIN, -1.0).unwrap();
    assert_eq!(d.read(MAIN).unwrap(), Some(0.0));
}

#[test]
fn hd8_toggle_flips_and_reports() {
    let mut d = hd8_on(&FakeClient::new(Writes::Land));
    assert!(d.toggle("global/mute").unwrap());
    assert_eq!(d.read("global/mute").unwrap(), Some(1.0));
    assert!(!d.toggle("global/mute").unwrap());
}

#[test]
fn hd8_scenes_lose_the_extension_and_keep_gains() {
    let c = FakeClient::new(Writes::Land);
    let mut d = hd8_on(&c);
    assert_eq!(d.scenes().unwrap(), ["A", "B"]);
    d.load_scene(1).unwrap();
    assert_eq!(c.ucd.lock().unwrap().loaded, Some(("B".into(), true)));
}

#[test]
fn hd8_refuses_a_scene_that_does_not_exist() {
    assert!(hd8_on(&FakeClient::new(Writes::Land)).load_scene(9).is_err());
}

#[test]
fn hd8_reconnects_when_the_socket_died_and_the_write_lands() {
    let fresh = FakeClient::new(Writes::Land);
    let (mut d, _) = reconnecting(Writes::Dying, &fresh);
    d.write(MAIN, 0.25).unwrap();
    assert_eq!(fresh.value(MAIN), 0.25);
}

#[test]
fn hd8_reconnects_before_a_toggle() {
    let fresh = FakeClient::new(Writes::Land);
    let (mut d, _) = reconnecting(Writes::Dying, &fresh);
    assert!(d.toggle("global/mute").unwrap());
    assert_eq!(fresh.value("global/mute"), 1.0);
}

#[test]
fn hd8_reconnects_only_once_per_operation() {
    let (mut d, tries) = reconnecting(Writes::Dying, &FakeClient::new(Writes::Dying));
    assert!(d.write(MAIN, 0.25).is_err());
    assert_eq!(*tries.lock().unwrap(), 1);
}

#[test]
fn hd8_says_why_when_it_cannot_reconnect() {
    let connect: hd8::Connect = Box::new(|| Err("ucdaemon is not answering".into()));
    let mut d = Hd8::with(Box::new(FakeClient::new(Writes::Dying)), Some(connect), no_clock());
    assert!(d.write(MAIN, 0.25).unwrap_err().0.contains("ucdaemon is not answering"));
}

#[test]
fn hd8_without_a_way_to_reconnect_refuses_instead_of_crashing() {
    assert!(hd8_on(&FakeClient::new(Writes::Dying)).write(MAIN, 0.25).is_err());
}

#[test]
fn hd8_closes_the_dead_client_before_reconnecting() {
    let dead = FakeClient::new(Writes::Dying);
    let connect: hd8::Connect = Box::new(|| Ok(Box::new(FakeClient::new(Writes::Land)) as Box<dyn Client>));
    let mut d = Hd8::with(Box::new(dead.clone()), Some(connect), no_clock());
    d.write(MAIN, 0.25).unwrap();
    assert!(dead.ucd.lock().unwrap().closed);
}

#[test]
fn hd8_reconnects_when_a_stale_session_stops_confirming() {
    let fresh = FakeClient::new(Writes::Land);
    let (mut d, _) = reconnecting(Writes::Stale, &fresh);
    d.write(MAIN, 0.25).unwrap();
    assert_eq!(fresh.value(MAIN), 0.25);
}

#[test]
fn hd8_does_not_reconnect_again_and_again_for_a_refused_write() {
    // A write the mixer genuinely refuses (an aux bus in Mixer Bypass) must not reconnect on every fader
    // message.
    let tries = Arc::new(Mutex::new(0));
    let t = tries.clone();
    let connect: hd8::Connect = Box::new(move || {
        *t.lock().unwrap() += 1;
        Ok(Box::new(FakeClient::new(Writes::Stale)) as Box<dyn Client>)
    });
    let now = Arc::new(Mutex::new(100.0));
    let n = now.clone();
    let mut d =
        Hd8::with(Box::new(FakeClient::new(Writes::Stale)), Some(connect), Box::new(move || *n.lock().unwrap()));
    for _ in 0..3 {
        assert!(d.write("aux/ch10/volume", 0.5).is_err());
    }
    assert_eq!(*tries.lock().unwrap(), 1);
    *now.lock().unwrap() += hd8::STALE_RETRY + 1.0;
    assert!(d.write("aux/ch10/volume", 0.5).is_err());
    assert_eq!(*tries.lock().unwrap(), 2);
}

#[test]
fn unknown_driver_is_refused() {
    assert!(drivers::build("nope").is_err());
}

// -- Mac -------------------------------------------------------------------------------------------------

#[test]
fn mac_says_why_when_the_output_has_no_system_volume() {
    let mut d = MacVolume::with(Box::new(|_| Ok("missing value".into())));
    assert!(d.read("").unwrap_err().0.contains("audio interface"));
}

#[test]
fn app_reports_nothing_when_the_app_is_not_open() {
    // A closed app must not be asked about its player: AppleScript would launch it (2026-09-22).
    let mut d = AppVolume::with(Box::new(|_| Ok("closed".into())));
    assert_eq!(d.playing("Spotify").unwrap(), None);
}

#[test]
fn app_reports_whether_it_is_playing() {
    let answer = Arc::new(Mutex::new("playing"));
    let a = answer.clone();
    let mut d = AppVolume::with(Box::new(move |_| Ok(a.lock().unwrap().to_string())));
    assert_eq!(d.playing("Spotify").unwrap(), Some(true));
    *answer.lock().unwrap() = "paused";
    assert_eq!(d.playing("Spotify").unwrap(), Some(false));
}

#[test]
fn app_asks_the_player_in_a_single_call() {
    // Two calls cost 234 ms beside the fader writes; one costs 117.
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let c = calls.clone();
    let mut d = AppVolume::with(Box::new(move |s| {
        c.lock().unwrap().push(s.into());
        Ok("playing".into())
    }));
    d.playing("Spotify").unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls.len() == 1 && calls[0].contains("is running"));
}

// -- MK-300 ----------------------------------------------------------------------------------------------

#[derive(Default)]
struct Pedal {
    vol: Option<u16>,
    loaded: Option<usize>,
    names: Vec<String>,
    name_reads: u32,
    current: usize,
}

#[derive(Clone)]
struct FakePedal(Arc<Mutex<Pedal>>);

impl FakePedal {
    fn new(vol: Option<u16>, names: &[&str]) -> Self {
        FakePedal(Arc::new(Mutex::new(Pedal {
            vol,
            names: names.iter().map(|s| s.to_string()).collect(),
            ..Pedal::default()
        })))
    }

    fn get(&self) -> std::sync::MutexGuard<'_, Pedal> {
        self.0.lock().unwrap()
    }
}

impl Mk300Client for FakePedal {
    fn volume(&mut self) -> Result<f64, String> {
        Ok(f64::from(self.get().vol.unwrap_or(0)))
    }
    fn set_volume(&mut self, v: u16) -> Result<(), String> {
        self.get().vol = Some(v);
        Ok(())
    }
    fn preset_names(&mut self) -> Result<Vec<String>, String> {
        let mut p = self.get();
        p.name_reads += 1;
        Ok(p.names.clone())
    }
    fn load_preset(&mut self, index: usize) -> Result<(), String> {
        self.get().loaded = Some(index);
        Ok(())
    }
    fn current(&mut self) -> Result<usize, String> {
        Ok(self.get().current)
    }
}

impl AmperoClient for FakePedal {
    fn set_volume(&mut self, v: u16) -> Result<(), String> {
        self.get().vol = Some(v);
        Ok(())
    }
    fn patch_names(&mut self) -> Result<Vec<String>, String> {
        Ok(self.get().names.clone())
    }
    fn load_patch(&mut self, index: usize) -> Result<(), String> {
        self.get().loaded = Some(index);
        Ok(())
    }
    fn current(&mut self) -> Result<usize, String> {
        Ok(self.get().current)
    }
}

fn mk300(fake: &FakePedal) -> Mk300 {
    let f = fake.clone();
    Mk300::with(Box::new(move || Ok(Box::new(f.clone()) as Box<dyn Mk300Client>)))
}

fn mk300_fake() -> FakePedal {
    FakePedal::new(Some(70), &["CLEAN", "CRUNCH", "LEAD"])
}

fn ampero(fake: &FakePedal) -> Ampero2 {
    let f = fake.clone();
    Ampero2::with(Box::new(move || Ok(Box::new(f.clone()) as Box<dyn AmperoClient>)))
}

fn ampero_fake() -> FakePedal {
    FakePedal::new(None, &["DET-LUGA", "AMBIENT"])
}

#[test]
fn mk300_volume_reads_and_writes_as_0_to_1() {
    let fake = mk300_fake();
    let mut d = mk300(&fake);
    assert!(approx(d.read("volume").unwrap().unwrap(), 0.70));
    d.write("volume", 0.42).unwrap();
    assert_eq!(fake.get().vol, Some(42));
}

#[test]
fn mk300_scenes_are_its_presets() {
    let fake = mk300_fake();
    let mut d = mk300(&fake);
    assert_eq!(d.scenes().unwrap(), ["CLEAN", "CRUNCH", "LEAD"]);
    d.load_scene(2).unwrap();
    assert_eq!(fake.get().loaded, Some(2));
}

#[test]
fn mk300_unplugged_is_unsupported_not_a_crash() {
    let mut d = Mk300::with(Box::new(|| Err("no MIDI port containing 'USB Composite Device'".into())));
    assert!(d.read("volume").unwrap_err().0.contains("MK-300"));
}

#[test]
fn mk300_reconnects_once_it_is_plugged_in() {
    let mut tries = 0;
    let mut d = Mk300::with(Box::new(move || {
        tries += 1;
        if tries == 1 {
            return Err("not there yet".into());
        }
        Ok(Box::new(mk300_fake()) as Box<dyn Mk300Client>)
    }));
    assert!(d.read("volume").is_err());
    assert!(approx(d.read("volume").unwrap().unwrap(), 0.70));
}

#[test]
fn mk300_reads_the_preset_names_once() {
    // Reading the 160 names takes a second on the real pedal (measured 2026-09-22); doing it on every arrow
    // press would stall the surface.
    let fake = mk300_fake();
    let mut d = mk300(&fake);
    d.scenes().unwrap();
    d.scenes().unwrap();
    d.load_scene(1).unwrap();
    assert_eq!(fake.get().name_reads, 1);
}

#[test]
fn mk300_names_lose_their_padding() {
    let fake = FakePedal::new(Some(70), &["UK Clean ", "UK OD "]);
    assert_eq!(mk300(&fake).scenes().unwrap(), ["UK Clean", "UK OD"]);
}

#[test]
fn mk300_says_which_preset_is_loaded() {
    let fake = mk300_fake();
    fake.get().current = 2;
    assert_eq!(mk300(&fake).current_scene().unwrap(), Some(2));
}

// -- Ampero II -------------------------------------------------------------------------------------------

#[test]
fn ampero_volume_is_write_only_so_read_is_none_until_written() {
    // The protocol has no query for the patch volume: a fader on it cannot take over, so read answers None
    // (the bridge then applies at once) and, after a write, the value it wrote.
    let fake = ampero_fake();
    let mut d = ampero(&fake);
    assert_eq!(d.read("volume").unwrap(), None);
    d.write("volume", 0.30).unwrap();
    assert_eq!(fake.get().vol, Some(30));
    assert!(approx(d.read("volume").unwrap().unwrap(), 0.30));
}

#[test]
fn ampero_forgets_the_volume_when_a_patch_loads() {
    // The volume belongs to the patch: a new patch brings its own.
    let mut d = ampero(&ampero_fake());
    d.write("volume", 0.30).unwrap();
    d.load_scene(1).unwrap();
    assert_eq!(d.read("volume").unwrap(), None);
}

#[test]
fn ampero_scenes_are_its_patches() {
    let fake = ampero_fake();
    let mut d = ampero(&fake);
    assert_eq!(d.scenes().unwrap(), ["DET-LUGA", "AMBIENT"]);
    d.load_scene(1).unwrap();
    assert_eq!(fake.get().loaded, Some(1));
}

#[test]
fn ampero_says_which_patch_is_loaded() {
    let fake = ampero_fake();
    fake.get().current = 1;
    assert_eq!(ampero(&fake).current_scene().unwrap(), Some(1));
}

#[test]
fn both_pedals_are_known_and_ship_a_bank() {
    for name in ["mk300", "ampero2"] {
        assert!(drivers::KNOWN.contains(&name));
        assert!(!drivers::default_banks(name).is_empty());
    }
}

// -- OpenRig ---------------------------------------------------------------------------------------------

/// How a fake MCP server behaves.
#[derive(Clone, Copy, PartialEq)]
enum Server {
    Up,
    /// OpenRig restarted: the old session is gone until connect() runs again.
    Restarted,
    Down,
    /// Every tool call is refused.
    Refusing,
}

struct Rig {
    strips: Vec<Value>,
    connects: u32,
    alive: bool,
    server: Server,
}

#[derive(Clone)]
struct FakeMcp(Arc<Mutex<Rig>>);

impl FakeMcp {
    fn new(server: Server) -> Self {
        let strips = vec![
            json!({"id": "out:24,25@hd8", "name": "FRFR (ADAT out 11/12)", "gain_db": -16.3, "muted": true}),
            json!({"id": "out:4,5@hd8", "name": "SYN-5050 (Out 5/6)", "gain_db": -29.5, "muted": false}),
            json!({"id": "out:8,9@hd8", "name": "SYN-2 FX RETURN L/R (Out 9/10)", "gain_db": 0.0, "muted": false}),
            json!({"id": "out:2@hd8", "name": "SYN-2 INST IN (Out 3)", "gain_db": 0.0, "muted": false}),
        ];
        FakeMcp(Arc::new(Mutex::new(Rig { strips, connects: 0, alive: server != Server::Restarted, server })))
    }

    /// Every strip an output, plus one input.
    fn with_directions() -> Self {
        let mcp = FakeMcp::new(Server::Up);
        {
            let mut r = mcp.rig();
            for s in r.strips.iter_mut() {
                s["direction"] = json!("output");
            }
            r.strips.push(
                json!({"id": "in:0@hd8", "name": "GUITARRA 1", "gain_db": 0.0, "muted": false, "direction": "input"}),
            );
        }
        mcp
    }

    fn rig(&self) -> std::sync::MutexGuard<'_, Rig> {
        self.0.lock().unwrap()
    }

    fn driver(&self) -> OpenRig {
        OpenRig::with(Box::new(self.clone()))
    }
}

impl Mcp for FakeMcp {
    fn connect(&mut self) -> Result<(), McpError> {
        let mut r = self.rig();
        if r.server == Server::Down {
            return Err(McpError::Down("connection refused".into()));
        }
        r.connects += 1;
        r.alive = true;
        Ok(())
    }

    fn resource(&mut self, uri: &str) -> Result<Value, McpError> {
        assert_eq!(uri, "openrig://mixer");
        let r = self.rig();
        match r.server {
            Server::Down => Err(McpError::Down("connection refused".into())),
            _ if !r.alive => Err(McpError::Down("session expired".into())),
            _ => Ok(json!({"strips": r.strips})),
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, McpError> {
        let mut r = self.rig();
        if r.server == Server::Refusing {
            return Err(McpError::Refused("strip not found".into()));
        }
        let s = r.strips.iter_mut().find(|s| s["id"] == args["strip"]).expect("a known strip");
        match tool {
            "set_mixer_fader" => s["gain_db"] = json!(args["gain_db"].as_f64().unwrap().clamp(-60.0, 12.0)),
            "set_mixer_mute" => s["muted"] = args["muted"].clone(),
            _ => return Err(McpError::Refused(format!("unknown tool {tool}"))),
        }
        Ok(Value::Null)
    }
}

fn live(target: &str, label: &str, group: &str) -> LiveFader {
    LiveFader { target: target.into(), label: label.into(), group: group.into(), mute: Some(format!("mute:{target}")) }
}

#[test]
fn fader_travel_is_linear_in_db_over_openrigs_range() {
    assert_eq!(to_db(0.0), -60.0);
    assert_eq!(to_db(1.0), 12.0);
    assert!(approx(to_db(5.0 / 6.0), 0.0));
    assert_eq!(to_db(7.0), 12.0);
    assert!(approx(from_db(to_db(0.42)), 0.42));
}

#[test]
fn write_reaches_the_strip_named_by_its_prefix_and_reads_back() {
    let mcp = FakeMcp::new(Server::Up);
    let mut d = mcp.driver();
    d.write("FRFR", 5.0 / 6.0).unwrap();
    assert!(approx(mcp.rig().strips[0]["gain_db"].as_f64().unwrap(), 0.0));
    assert!(approx(d.read("FRFR").unwrap().unwrap(), 5.0 / 6.0));
}

#[test]
fn a_strip_can_be_named_by_its_full_id() {
    let mut d = FakeMcp::new(Server::Up).driver();
    assert!(approx(d.read("out:4,5@hd8").unwrap().unwrap(), from_db(-29.5)));
}

#[test]
fn toggle_flips_the_strip_mute_and_reports_it() {
    let mcp = FakeMcp::new(Server::Up);
    let mut d = mcp.driver();
    assert!(!d.toggle("FRFR").unwrap());
    assert_eq!(mcp.rig().strips[0]["muted"], json!(false));
    assert!(d.toggle("FRFR").unwrap());
}

#[test]
fn an_ambiguous_name_is_refused_not_guessed() {
    let mut d = FakeMcp::new(Server::Up).driver();
    assert!(d.write("SYN-2", 0.5).unwrap_err().0.contains("FX RETURN"));
}

#[test]
fn an_unknown_strip_is_unsupported() {
    assert!(FakeMcp::new(Server::Up).driver().read("NOPE").is_err());
}

#[test]
fn a_lost_session_reconnects_once_and_the_read_lands() {
    let mcp = FakeMcp::new(Server::Restarted);
    let mut d = mcp.driver();
    assert!(approx(d.read("SYN-5050").unwrap().unwrap(), from_db(-29.5)));
    assert_eq!(mcp.rig().connects, 1);
}

#[test]
fn openrig_not_running_is_unsupported_not_a_crash() {
    let mut d = FakeMcp::new(Server::Down).driver();
    assert!(d.read("FRFR").unwrap_err().0.contains("not reachable"));
}

#[test]
fn a_refused_command_is_unsupported() {
    assert!(FakeMcp::new(Server::Refusing).driver().write("FRFR", 0.5).is_err());
}

#[test]
fn the_driver_is_registered() {
    assert_eq!(drivers::build("openrig").unwrap().name(), "openrig");
}

#[test]
fn mute_target_reads_the_mute_the_lamp_needs() {
    let mut d = FakeMcp::new(Server::Up).driver();
    assert_eq!(d.read("mute:FRFR").unwrap(), Some(1.0));
    assert!(!d.toggle("mute:FRFR").unwrap());
    assert_eq!(d.read("mute:FRFR").unwrap(), Some(0.0));
}

#[test]
fn live_faders_follow_the_strips_openrig_has_now() {
    let mcp = FakeMcp::with_directions();
    let mut d = mcp.driver();
    let out = d.live_faders("output").unwrap();
    let labels: Vec<&str> = out.values().map(|f| f.label.as_str()).take(2).collect();
    assert_eq!(labels, ["FRFR (ADAT out 11/12)", "SYN-5050 (Out 5/6)"]);
    assert_eq!(out[&1], live("out:24,25@hd8", "FRFR (ADAT out 11/12)", "out"));
    assert_eq!(d.live_faders("input").unwrap(), BTreeMap::from([(1, live("in:0@hd8", "GUITARRA 1", "in"))]));
    {
        let mut r = mcp.rig();
        r.strips = r.strips[1..2].to_vec(); // another project opened
    }
    let now = d.live_faders("output").unwrap();
    assert_eq!(now.keys().copied().collect::<Vec<_>>(), [1]);
    assert_eq!(now[&1].target, "out:4,5@hd8");
}

#[test]
fn openrig_banks_are_live_and_expand_from_a_bare_bank() {
    let p = expand(parse_str("banks: [{driver: openrig}]").unwrap()).unwrap();
    let banks: Vec<(&str, Option<&str>, usize)> =
        p.banks.iter().map(|b| (b.name.as_str(), b.live.as_deref(), b.faders.len())).collect();
    assert_eq!(banks, [("OpenRig", Some("mixer"), 0)]);
}

#[test]
fn out_and_in_are_scenes_of_one_device() {
    let mut d = FakeMcp::with_directions().driver();
    assert_eq!(d.scenes().unwrap(), ["OUT", "IN"]);
    assert_eq!(d.current_scene().unwrap(), Some(0));
    assert_eq!(d.live_faders("mixer").unwrap()[&1].target, "out:24,25@hd8");
    d.load_scene(1).unwrap();
    assert_eq!(d.live_faders("mixer").unwrap(), BTreeMap::from([(1, live("in:0@hd8", "GUITARRA 1", "in"))]));
    assert!(d.load_scene(2).is_err());
}
