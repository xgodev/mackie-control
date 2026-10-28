//! PreSonus Quantum HD 8, through the `quantum-hd8` crate (UCNet).
//!
//! The HD 8 does not receive MIDI: a full sweep of CC, pitch bend and notes on both of its CoreMIDI ports
//! changed none of its 1419 parameters (2026-09-20). Every write here goes to `ucdaemon` over TCP instead.
//!
//! That TCP session does not survive the interface being switched off: afterwards every write fails while
//! reads keep answering from a stale cache, so the bridge looks alive and moves nothing (measured
//! 2026-09-22). The driver therefore reconnects once per operation and retries.
//!
//! A session can also go stale with the socket still open: after hours running, the daemon stops
//! confirming the bridge's writes while a fresh connection writes fine (measured 2026-09-23 and
//! 2026-09-27). An unconfirmed write is therefore also retried on a new connection -- but at most once
//! every STALE_RETRY seconds, because the mixer also leaves genuinely refused writes unconfirmed (an aux bus
//! in Mixer Bypass) and a fader sends dozens a second.

use std::collections::BTreeMap;
use std::time::Instant;

use quantum_hd8::Error;

use super::{unsupported, DefaultBank, Driver, Result, Unsupported};
use crate::profile::Destination;

/// Seconds between reconnects caused by unconfirmed writes.
pub const STALE_RETRY: f64 = 10.0;

/// The part of `quantum_hd8::Client` the driver uses, so the tests can hand it a fake.
pub trait Client: Send {
    fn get(&mut self, path: &str) -> std::result::Result<f64, Error>;
    fn set_raw(&mut self, path: &str, value: f64) -> std::result::Result<(), Error>;
    fn scenes(&mut self) -> Vec<String>;
    fn load_scene(&mut self, name: &str, keep_gains: bool) -> std::result::Result<(), Error>;
    fn close(&mut self) {}
}

impl Client for quantum_hd8::Client {
    fn get(&mut self, path: &str) -> std::result::Result<f64, Error> {
        quantum_hd8::Client::get(self, path)
    }

    fn set_raw(&mut self, path: &str, value: f64) -> std::result::Result<(), Error> {
        quantum_hd8::Client::set_raw(self, path, value).map(drop)
    }

    fn scenes(&mut self) -> Vec<String> {
        self.scenes.clone()
    }

    fn load_scene(&mut self, name: &str, keep_gains: bool) -> std::result::Result<(), Error> {
        quantum_hd8::Client::load_scene(self, name, keep_gains).map(drop)
    }

    fn close(&mut self) {
        quantum_hd8::Client::close(self)
    }
}

pub type Connect = Box<dyn FnMut() -> std::result::Result<Box<dyn Client>, String> + Send>;

fn connect_real() -> std::result::Result<Box<dyn Client>, String> {
    let mut c = quantum_hd8::Client::new();
    c.connect().map_err(|e| e.to_string())?;
    Ok(Box::new(c))
}

pub struct Hd8 {
    cli: Option<Box<dyn Client>>,
    connect: Option<Connect>,
    clock: Box<dyn Fn() -> f64 + Send>,
    /// When the last stale reconnect happened.
    stale_at: Option<f64>,
}

impl Hd8 {
    /// The real interface, connected on first use: the bridge may start before ucdaemon.
    pub fn new() -> Self {
        let start = Instant::now();
        Hd8 {
            cli: None,
            connect: Some(Box::new(connect_real)),
            clock: Box::new(move || start.elapsed().as_secs_f64()),
            stale_at: None,
        }
    }

    /// A client, a way to build a fresh one (None: it cannot be reconnected, and the driver then refuses
    /// instead of crashing), and a clock in seconds.
    pub fn with(cli: Box<dyn Client>, connect: Option<Connect>, clock: Box<dyn Fn() -> f64 + Send>) -> Self {
        Hd8 { cli: Some(cli), connect, clock, stale_at: None }
    }

    fn client(&mut self) -> Result<&mut Box<dyn Client>> {
        if self.cli.is_none() {
            let Some(connect) = self.connect.as_mut() else {
                return unsupported("hd8: not connected to ucdaemon");
            };
            self.cli = Some(connect().map_err(|e| Unsupported(format!("hd8: cannot connect to ucdaemon ({e})")))?);
        }
        Ok(self.cli.as_mut().expect("connected above"))
    }

    /// Run an operation, and if the socket is gone, reconnect once and run it again. A second failure is
    /// Unsupported, not a crash.
    fn retry<T>(&mut self, mut operation: impl FnMut(&mut dyn Client) -> std::result::Result<T, Error>) -> Result<T> {
        let cause = match operation(self.client()?.as_mut()) {
            Ok(v) => return Ok(v),
            Err(e @ Error::Io(_)) => e,
            Err(e @ Error::WriteNotConfirmed(_)) => {
                let now = (self.clock)();
                if self.stale_at.is_some_and(|at| now - at < STALE_RETRY) {
                    return unsupported(format!("hd8: {e}"));
                }
                self.stale_at = Some(now);
                e
            }
            Err(e) => return unsupported(format!("hd8: {e}")),
        };
        self.reconnect(&cause)?;
        operation(self.client()?.as_mut()).map_err(|e| Unsupported(format!("hd8: {e}")))
    }

    fn reconnect(&mut self, cause: &Error) -> Result<()> {
        let Some(connect) = self.connect.as_mut() else {
            return unsupported(format!("hd8: lost the connection to ucdaemon ({cause})"));
        };
        if let Some(mut old) = self.cli.take() {
            old.close();
        }
        self.cli = Some(connect().map_err(|e| Unsupported(format!("hd8: cannot reconnect to ucdaemon ({e})")))?);
        Ok(())
    }
}

impl Default for Hd8 {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for Hd8 {
    fn name(&self) -> &str {
        "hd8"
    }

    fn read(&mut self, target: &str) -> Result<Option<f64>> {
        self.retry(|c| c.get(target)).map(Some)
    }

    fn write(&mut self, target: &str, value: f64) -> Result<()> {
        let v = value.clamp(0.0, 1.0);
        self.retry(|c| c.set_raw(target, v))
    }

    fn toggle(&mut self, target: &str) -> Result<bool> {
        let on = self.retry(|c| c.get(target))? >= 0.5;
        self.retry(|c| c.set_raw(target, if on { 0.0 } else { 1.0 }))?;
        Ok(!on)
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        let names = self.client()?.scenes();
        Ok(names.into_iter().map(|n| n.strip_suffix(".scene").map(Into::into).unwrap_or(n)).collect())
    }

    fn load_scene(&mut self, index: usize) -> Result<()> {
        let names = self.scenes()?;
        let Some(name) = names.get(index) else {
            return unsupported(format!("scene {} does not exist (there are {})", index + 1, names.len()));
        };
        self.retry(|c| c.load_scene(name, true))
    }
}

fn fader(target: &str, label: &str, group: &str, mute: Option<&str>) -> Destination {
    Destination {
        label: label.into(),
        group: group.into(),
        mute: mute.map(Into::into),
        ..Destination::new("hd8", Some(target))
    }
}

/// Two banks, because that is how the desk is read: what comes in, and where it goes. The eight analogue
/// inputs are the eight the HD 8 has; `aux/ch(4+k)` is the ADAT k/k+1 bus (measured, quantum-hd8 docs).
pub fn default_banks() -> Vec<DefaultBank> {
    let inputs = (1..=8)
        .map(|n| {
            let mute = format!("line/ch{n}/mute");
            (n, fader(&format!("line/ch{n}/volume"), &format!("IN {n}"), "in", Some(&mute)))
        })
        .collect();
    let mut outputs: BTreeMap<u32, Destination> = BTreeMap::from([
        (1, fader("global/mainOutVolume", "MAIN", "out", Some("global/mute"))),
        (2, fader("global/phones1_volume", "PHONES 1", "out", None)),
        (3, fader("global/phones2_volume", "PHONES 2", "out", None)),
    ]);
    for k in 1..=5u32 {
        let mute = format!("aux/ch{}/mute", 4 + k);
        let label = format!("ADAT {}/{}", 2 * k - 1, 2 * k);
        outputs.insert(3 + k, fader(&format!("aux/ch{}/volume", 4 + k), &label, "out", Some(&mute)));
    }
    vec![
        DefaultBank { name: "HD 8 IN".into(), live: None, faders: inputs },
        DefaultBank { name: "HD 8 OUT".into(), live: None, faders: outputs },
    ]
}
