//! macOS volume: the system output, and one application's own volume.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{unsupported, Driver, Result, Unsupported};

/// Runs one AppleScript and answers its trimmed output, or why it failed.
pub type Osascript = Box<dyn Fn(&str) -> std::result::Result<String, String> + Send>;

/// Runs each line of stdin, a JSON string of AppleScript source, and writes one line of JSON back. It leaves
/// on end of input, so it does not outlive the bridge.
const HOST: &str = r#"
ObjC.import("Foundation");
function answer(source) {
  const error = $.NSMutableDictionary.alloc.init;
  const out = $.NSAppleScript.alloc.initWithSource($(source)).executeAndReturnError(error);
  if (out.isNil()) {
    return { err: ObjC.unwrap(error.objectForKey("NSAppleScriptErrorMessage")) || "AppleScript failed" };
  }
  const text = ObjC.unwrap(out.stringValue);
  return { ok: text === undefined ? "missing value" : text };
}
function run() {
  const input = $.NSFileHandle.fileHandleWithStandardInput;
  const output = $.NSFileHandle.fileHandleWithStandardOutput;
  let pending = "";
  for (;;) {
    const data = input.availableData;
    if (Number(data.length) === 0) return;
    pending += ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding));
    let nl;
    while ((nl = pending.indexOf("\n")) >= 0) {
      const line = pending.slice(0, nl);
      pending = pending.slice(nl + 1);
      const reply = $(JSON.stringify(answer(JSON.parse(line))) + "\n");
      output.writeData(reply.dataUsingEncoding($.NSUTF8StringEncoding));
    }
  }
}
"#;

/// s before starting another host after one died: a host that cannot stay up must not become the process per
/// call this type exists to avoid.
const RESPAWN_AFTER: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Reply {
    Ok(String),
    Err(String),
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Running {
    fn ask(&mut self, script: &str) -> std::result::Result<Reply, String> {
        let line = serde_json::to_string(script).map_err(|e| e.to_string())?;
        writeln!(self.stdin, "{line}").map_err(|e| e.to_string())?;
        let mut reply = String::new();
        let read = self.stdout.read_line(&mut reply).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("the AppleScript host left".to_string());
        }
        serde_json::from_str(&reply).map_err(|e| format!("the AppleScript host said {reply:?}: {e}"))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // intentional: it may have left already, and either way there is nobody left to tell.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct HostState {
    running: Option<Running>,
    not_before: Option<Instant>,
}

/// One long-lived process running every script of a driver.
///
/// A new `osascript` per script spends one LaunchServices serial each, and the transport lamps ask once a
/// second: the 32-bit counter ran out after 21 days and every new program on the Mac stuck at launch until a
/// reboot (2026-11-01, docs/bridge.md). One script costs about 1 ms here against 117 ms in a process of its own.
pub struct ScriptHost {
    command: Box<dyn Fn() -> Command + Send>,
    respawn_after: Duration,
    state: Mutex<HostState>,
}

impl ScriptHost {
    pub fn new() -> Self {
        Self::with(
            Box::new(|| {
                let mut c = Command::new("osascript");
                c.args(["-l", "JavaScript", "-e", HOST]);
                c
            }),
            RESPAWN_AFTER,
        )
    }

    pub fn with(command: Box<dyn Fn() -> Command + Send>, respawn_after: Duration) -> Self {
        ScriptHost { command, respawn_after, state: Mutex::new(HostState::default()) }
    }

    fn start(&self) -> std::result::Result<Running, String> {
        let mut child = (self.command)()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("the AppleScript host has no stdin")?;
        let stdout = child.stdout.take().ok_or("the AppleScript host has no stdout")?;
        Ok(Running { child, stdin, stdout: BufReader::new(stdout) })
    }

    pub fn run(&self, script: &str) -> std::result::Result<String, String> {
        let mut state = self.state.lock().unwrap();
        if state.running.is_none() {
            if state.not_before.is_some_and(|t| Instant::now() < t) {
                return Err("the AppleScript host is down".to_string());
            }
            match self.start() {
                Ok(running) => state.running = Some(running),
                Err(e) => {
                    state.not_before = Some(Instant::now() + self.respawn_after);
                    return Err(e);
                }
            }
        }
        match state.running.as_mut().map(|r| r.ask(script)) {
            Some(Ok(Reply::Ok(text))) => Ok(text.trim().to_string()),
            Some(Ok(Reply::Err(e))) => Err(e),
            Some(Err(e)) => {
                state.running = None;
                state.not_before = Some(Instant::now() + self.respawn_after);
                Err(e)
            }
            None => Err("the AppleScript host is down".to_string()),
        }
    }
}

impl Default for ScriptHost {
    fn default() -> Self {
        Self::new()
    }
}

fn script_host() -> Osascript {
    let host = ScriptHost::new();
    Box::new(move |script| host.run(script))
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
        Self::with(script_host())
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
        Self::with(script_host())
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
