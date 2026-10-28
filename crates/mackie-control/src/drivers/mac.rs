//! macOS volume: the system output, and one application's own volume.

use std::process::Command;

use super::{unsupported, Driver, Result, Unsupported};

/// Runs one AppleScript and answers its trimmed output, or why it failed.
pub type Osascript = Box<dyn Fn(&str) -> std::result::Result<String, String> + Send>;

pub fn osascript(script: &str) -> std::result::Result<String, String> {
    let out = Command::new("osascript").args(["-e", script]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("osascript exited with {}", out.status) } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

const NO_SYSTEM_VOLUME: &str =
    "the Mac's default output has no system-controlled volume (an audio interface is selected)";

/// System output volume.
///
/// Only works when the default output is a device macOS controls. With an interface like the Quantum HD 8
/// selected, AppleScript answers "missing value" for output volume -- the volume lives in the interface,
/// not in the OS -- and this driver says so instead of guessing.
pub struct MacVolume {
    run: Osascript,
}

impl MacVolume {
    pub fn new() -> Self {
        Self::with(Box::new(osascript))
    }

    pub fn with(run: Osascript) -> Self {
        MacVolume { run }
    }

    fn script(&self, s: &str) -> Result<String> {
        (self.run)(s).map_err(Unsupported)
    }
}

impl Default for MacVolume {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for MacVolume {
    fn name(&self) -> &str {
        "mac"
    }

    fn read(&mut self, _target: &str) -> Result<Option<f64>> {
        let raw = self.script("output volume of (get volume settings)")?;
        if raw == "missing value" {
            return unsupported(NO_SYSTEM_VOLUME);
        }
        let v: f64 = raw.parse().map_err(|_| Unsupported(format!("mac: output volume is {raw:?}")))?;
        Ok(Some(v / 100.0))
    }

    fn write(&mut self, target: &str, value: f64) -> Result<()> {
        self.read(target)?; // fail early, with the right reason
        self.script(&format!("set volume output volume {}", (value * 100.0).round() as i64)).map(drop)
    }

    fn toggle(&mut self, _target: &str) -> Result<bool> {
        let raw = self.script("output muted of (get volume settings)")?;
        if raw == "missing value" {
            return unsupported(NO_SYSTEM_VOLUME);
        }
        let muted = raw == "true";
        self.script(&format!("set volume {} output muted", if muted { "without" } else { "with" }))?;
        Ok(!muted)
    }
}

/// target = the application name, e.g. "Spotify".
pub struct AppVolume {
    run: Osascript,
}

impl AppVolume {
    pub fn new() -> Self {
        Self::with(Box::new(osascript))
    }

    pub fn with(run: Osascript) -> Self {
        AppVolume { run }
    }
}

impl Default for AppVolume {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver for AppVolume {
    fn name(&self) -> &str {
        "app"
    }

    fn read(&mut self, target: &str) -> Result<Option<f64>> {
        let raw = (self.run)(&format!("tell application \"{target}\" to sound volume"))
            .map_err(|e| Unsupported(format!("{target} did not answer: {e}")))?;
        let v: f64 = raw.parse().map_err(|_| Unsupported(format!("{target} did not answer: {raw:?}")))?;
        Ok(Some(v / 100.0))
    }

    fn write(&mut self, target: &str, value: f64) -> Result<()> {
        let v = (value * 100.0).round() as i64;
        (self.run)(&format!("tell application \"{target}\" to set sound volume to {v}")).map(drop).map_err(Unsupported)
    }

    fn toggle(&mut self, target: &str) -> Result<bool> {
        self.command(target, "playpause")?;
        Ok(true)
    }

    /// None while the app is closed -- asking a closed app about its player would launch it, which is not
    /// what a lamp is worth.
    fn playing(&mut self, target: &str) -> Result<Option<bool>> {
        // One AppleScript, not two: each call costs about 117 ms, and this runs beside the fader writes
        // (measured 2026-09-22).
        let script = format!(
            "if application \"{target}\" is running then\n  tell application \"{target}\" to return player state as text\nelse\n  return \"closed\"\nend if"
        );
        Ok(match (self.run)(&script) {
            Err(_) => None,
            Ok(state) if state.trim() == "closed" => None,
            Ok(state) => Some(state.trim() == "playing"),
        })
    }

    /// Any AppleScript command the app understands: playpause, pause, next track, previous track...
    fn command(&mut self, target: &str, name: &str) -> Result<()> {
        (self.run)(&format!("tell application \"{target}\" to {name}"))
            .map(drop)
            .map_err(|e| Unsupported(format!("{target} refused '{name}': {e}")))
    }
}
