//! OpenRig, through its MCP server (`http://127.0.0.1:4123`).
//!
//! A fader here is one strip of OpenRig's global mixer -- the level OpenRig sends to an output (FRFR,
//! SYN-5050, ...) or takes from an input. With the HD 8 mixer in Bypass the HD 8's own aux buses ignore
//! writes, so this is where those levels are set (docs/openrig-bank/README.md).
//!
//! Wire format, measured 2026-09-30 against OpenRig's rmcp 1.7.0 server: streamable HTTP. A POST of
//! `initialize` answers `text/event-stream` with a `mcp-session-id` header; every later request carries
//! that header, and a request before `initialize` is refused with 422. Answers come as SSE `data:` lines
//! holding the JSON-RPC response.
//!
//! The strips change with every project and chain opened, so the driver's bank is live: its two scenes,
//! OUT and IN, take their faders from `openrig://mixer` while the bridge runs, in the order OpenRig lists
//! them, and address each strip by its id. Nothing about them is written in a profile.
//!
//! Targets name a strip the way `openrig://mixer` lists it: the full id (`out:24,25@coreaudio:...`), the
//! full name, or the start of the name ("FRFR" matches "FRFR (ADAT out 11/12)"). A name that matches more
//! than one strip is refused rather than guessed. `mute:FRFR` is that strip's mute: it reads 1.0 / 0.0 and
//! toggles, which is how the bridge asks for a mute lamp.
//!
//! Transport: MCP, because it is what OpenRig serves today; its gRPC adapter (`crates/adapter-server`) is
//! still a placeholder (2026-09-30). Everything wire-specific lives behind [`Mcp`], so a gRPC client
//! replaces [`Http`] only.
//!
//! Fader travel is linear in dB over OpenRig's own range, -60..+12 dB (the bottom is silence, as in
//! OpenRig); unity sits at 5/6 of the travel.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::{Driver, LiveFader, Result, Unsupported};

pub const URL: &str = "http://127.0.0.1:4123";
pub const MIN_DB: f64 = -60.0;
pub const MAX_DB: f64 = 12.0;
pub const TIMEOUT: Duration = Duration::from_secs(2);

pub fn to_db(value: f64) -> f64 {
    MIN_DB + value.clamp(0.0, 1.0) * (MAX_DB - MIN_DB)
}

pub fn from_db(db: f64) -> f64 {
    ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpError {
    /// The server answered, and said no.
    Refused(String),
    /// The server could not be reached, or the session is gone.
    Down(String),
}

/// What the driver needs from OpenRig's MCP server.
pub trait Mcp: Send {
    fn connect(&mut self) -> std::result::Result<(), McpError>;
    fn resource(&mut self, uri: &str) -> std::result::Result<Value, McpError>;
    fn call(&mut self, tool: &str, args: Value) -> std::result::Result<Value, McpError>;
}

/// A minimal MCP client over streamable HTTP.
pub struct Http {
    url: String,
    timeout: Duration,
    session: Option<String>,
    id: u64,
}

impl Http {
    pub fn new(url: &str, timeout: Duration) -> Self {
        Http { url: url.into(), timeout, session: None, id: 0 }
    }

    fn post(&mut self, body: &Value) -> std::result::Result<String, McpError> {
        let mut req = ureq::post(&self.url)
            .timeout(self.timeout)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        if let Some(s) = &self.session {
            req = req.set("mcp-session-id", s);
        }
        let r = req.send_string(&body.to_string()).map_err(|e| McpError::Down(e.to_string()))?;
        if let Some(s) = r.header("mcp-session-id") {
            self.session = Some(s.to_string());
        }
        r.into_string().map_err(|e| McpError::Down(e.to_string()))
    }

    fn rpc(&mut self, method: &str, params: Value) -> std::result::Result<Value, McpError> {
        self.id += 1;
        let text = self.post(&json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}))?;
        for line in text.lines() {
            let line = line.strip_prefix("data:").unwrap_or(line).trim();
            if !line.starts_with('{') {
                continue;
            }
            let msg: Value = serde_json::from_str(line).map_err(|e| McpError::Down(e.to_string()))?;
            if msg.get("id").and_then(Value::as_u64) != Some(self.id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let text = err.get("message").and_then(Value::as_str).map_or_else(|| err.to_string(), Into::into);
                return Err(McpError::Refused(text));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(McpError::Down(format!("no answer to {method}")))
    }
}

impl Mcp for Http {
    fn connect(&mut self) -> std::result::Result<(), McpError> {
        self.session = None;
        self.rpc(
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "mackie-control", "version": "0"}}),
        )?;
        self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).map(drop)
    }

    fn resource(&mut self, uri: &str) -> std::result::Result<Value, McpError> {
        let result = self.rpc("resources/read", json!({"uri": uri}))?;
        let text = result["contents"][0]["text"].as_str().ok_or_else(|| McpError::Down(format!("{uri}: no text")))?;
        serde_json::from_str(text).map_err(|e| McpError::Down(e.to_string()))
    }

    fn call(&mut self, tool: &str, args: Value) -> std::result::Result<Value, McpError> {
        let result = self.rpc("tools/call", json!({"name": tool, "arguments": args}))?;
        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            let text: Vec<&str> = result["content"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| c.get("text").and_then(Value::as_str).unwrap_or(""))
                .collect();
            let text = text.join(" ");
            return Err(McpError::Refused(if text.is_empty() { format!("{tool} refused") } else { text }));
        }
        Ok(result)
    }
}

/// Why an operation failed: the server, or the driver itself.
enum Failure {
    Mcp(McpError),
    Driver(Unsupported),
}

impl From<McpError> for Failure {
    fn from(e: McpError) -> Self {
        Failure::Mcp(e)
    }
}

type Op<T> = std::result::Result<T, Failure>;

/// The view the faders show: (scene name, strip direction).
pub const SCENES: [(&str, &str); 2] = [("OUT", "output"), ("IN", "input")];

pub struct OpenRig {
    cli: Box<dyn Mcp>,
    ready: bool,
    /// Which of SCENES the faders show.
    pub view: usize,
}

impl OpenRig {
    pub fn new() -> Self {
        OpenRig { cli: Box::new(Http::new(URL, TIMEOUT)), ready: false, view: 0 }
    }

    /// A client already connected (the tests).
    pub fn with(cli: Box<dyn Mcp>) -> Self {
        OpenRig { cli, ready: true, view: 0 }
    }

    /// Connect on first use -- OpenRig may start after the bridge -- and reconnect once when the session is
    /// gone (OpenRig restarted). A refused command or an unreachable server is Unsupported, never a crash.
    fn retry<T>(&mut self, mut operation: impl FnMut(&mut dyn Mcp) -> Op<T>) -> Result<T> {
        for attempt in 1..=2 {
            let result = if self.ready {
                operation(self.cli.as_mut())
            } else {
                self.cli.connect().map_err(Failure::Mcp).and_then(|()| {
                    self.ready = true;
                    operation(self.cli.as_mut())
                })
            };
            match result {
                Ok(v) => return Ok(v),
                Err(Failure::Driver(u)) => return Err(u),
                Err(Failure::Mcp(McpError::Refused(e))) => return Err(Unsupported(format!("openrig: {e}"))),
                Err(Failure::Mcp(McpError::Down(e))) => {
                    self.ready = false;
                    if attempt == 2 {
                        return Err(Unsupported(format!("openrig: not reachable at {URL} ({e})")));
                    }
                }
            }
        }
        unreachable!("the second attempt always returns")
    }
}

impl Default for OpenRig {
    fn default() -> Self {
        Self::new()
    }
}

fn strips(c: &mut dyn Mcp) -> Op<Vec<Value>> {
    let mixer = c.resource("openrig://mixer")?;
    Ok(mixer.get("strips").and_then(Value::as_array).cloned().unwrap_or_default())
}

fn text<'a>(s: &'a Value, key: &str) -> &'a str {
    s.get(key).and_then(Value::as_str).unwrap_or("")
}

fn strip(c: &mut dyn Mcp, target: &str) -> Op<Value> {
    let strips = strips(c)?;
    if let Some(s) = strips.iter().find(|s| text(s, "id") == target || text(s, "name") == target) {
        return Ok(s.clone());
    }
    let want = target.to_lowercase();
    let found: Vec<&Value> = strips.iter().filter(|s| text(s, "name").to_lowercase().starts_with(&want)).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(Failure::Driver(Unsupported(format!("openrig: no mixer strip called '{target}'")))),
        many => {
            let names: Vec<String> = many.iter().map(|s| format!("'{}'", text(s, "name"))).collect();
            Err(Failure::Driver(Unsupported(format!("openrig: '{target}' matches {}", names.join(", ")))))
        }
    }
}

fn muted(s: &Value) -> bool {
    s.get("muted").and_then(Value::as_bool).unwrap_or(false)
}

impl Driver for OpenRig {
    fn name(&self) -> &str {
        "openrig"
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        Ok(SCENES.iter().map(|(n, _)| n.to_string()).collect())
    }

    fn current_scene(&mut self) -> Result<Option<usize>> {
        Ok(Some(self.view))
    }

    fn load_scene(&mut self, index: usize) -> Result<()> {
        if index >= SCENES.len() {
            return Err(Unsupported(format!("openrig: no scene {}", index + 1)));
        }
        self.view = index;
        Ok(())
    }

    /// The first eight strips of one direction ("output" / "input"), as OpenRig lists them now; "mixer" is
    /// whichever the current scene shows.
    fn live_faders(&mut self, which: &str) -> Result<BTreeMap<u32, LiveFader>> {
        let which = if which == "mixer" { SCENES[self.view].1 } else { which }.to_string();
        let all = self.retry(strips)?;
        let group = if which == "input" { "in" } else { "out" };
        Ok(all
            .iter()
            .filter(|s| text(s, "direction") == which)
            .take(8)
            .zip(1..)
            .map(|(s, n)| {
                let id = text(s, "id");
                let fader = LiveFader {
                    target: id.into(),
                    label: text(s, "name").into(),
                    group: group.into(),
                    mute: Some(format!("mute:{id}")),
                };
                (n, fader)
            })
            .collect())
    }

    fn read(&mut self, target: &str) -> Result<Option<f64>> {
        if let Some(name) = target.strip_prefix("mute:") {
            return self.retry(|c| Ok(if muted(&strip(c, name)?) { 1.0 } else { 0.0 })).map(Some);
        }
        self.retry(|c| Ok(from_db(strip(c, target)?.get("gain_db").and_then(Value::as_f64).unwrap_or(MIN_DB))))
            .map(Some)
    }

    fn write(&mut self, target: &str, value: f64) -> Result<()> {
        self.retry(|c| {
            let id = text(&strip(c, target)?, "id").to_string();
            c.call("set_mixer_fader", json!({"strip": id, "gain_db": to_db(value)}))?;
            Ok(())
        })
    }

    fn toggle(&mut self, target: &str) -> Result<bool> {
        let target = target.strip_prefix("mute:").unwrap_or(target);
        self.retry(|c| {
            let s = strip(c, target)?;
            let now = !muted(&s);
            c.call("set_mixer_mute", json!({"strip": text(&s, "id"), "muted": now}))?;
            Ok(now)
        })
    }
}
