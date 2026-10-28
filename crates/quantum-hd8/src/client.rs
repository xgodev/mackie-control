//! The ucdaemon client for the Quantum HD 8's device session.
//!
//! Two UCNet sessions matter: the root session (endpoints only) and the device session, which carries the
//! HD 8's parameter tree, scene list and live events. This client only ever opens the device session.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::ucnet::{self, Decoder, Message, Py};

/// cbytes for the UM (UDP-port announce) message: the root session's pair.
pub const UM_CB: [u8; 4] = [0x00, 0x00, 0x69, 0x00];
/// cbytes for the HD 8 device session (client -> daemon).
pub const DEVICE_CB: [u8; 4] = [0x6a, 0x00, 0x69, 0x00];
/// What the daemon replies with on the device session (the pair swapped).
pub const DEVICE_REPLY_CB: [u8; 4] = [0x69, 0x00, 0x6a, 0x00];

pub const DEFAULT_PORT: u16 = 59791;
pub const UDP_DISCOVERY_PORT: u16 = 47809;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// s per (port, session) probe; the right one answers in ~0.06 s.
pub const DISCOVER_TIMEOUT: Duration = Duration::from_millis(250);
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
/// After RecalledPreset the daemon re-sends every PV: a scene load reads until the link is quiet this
/// long, bounded by SCENE_SETTLE_MAX, before comparing gains.
pub const SCENE_QUIET: Duration = Duration::from_millis(300);
pub const SCENE_SETTLE_MAX: Duration = Duration::from_secs(3);
pub const SCENE_LOAD_TIMEOUT: Duration = Duration::from_secs(3);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// Echo tolerance: the daemon answers a write with a PV rounded to four places.
pub const ECHO_TOLERANCE: f64 = 1e-3;
/// A write equal to the current value is not echoed, so it is not sent.
pub const NOOP_TOLERANCE: f64 = 1e-4;
const INITIAL_FR_COUNTER: u16 = 1;

pub const NOT_RESPONDING: &str = "ucdaemon did not answer: is Universal Control installed and its daemon running?";

#[derive(Debug)]
pub enum Error {
    /// The socket is gone (the interface was switched off, ucdaemon restarted): reconnect.
    Io(io::Error),
    /// Nothing answered on any port and session.
    NotResponding,
    /// A PV write was sent but no echo of it arrived in time.
    WriteNotConfirmed(String),
    /// RestorePreset was sent but no RecalledPreset arrived.
    SceneLoadTimeout(String),
    /// A parameter the state does not hold, or holds as something other than a number.
    Unknown(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::NotResponding => f.write_str(NOT_RESPONDING),
            Error::WriteNotConfirmed(p) => write!(f, "{p}: the daemon did not confirm the write in 1 s"),
            Error::SceneLoadTimeout(s) => write!(f, "scene {s}: no RecalledPreset in 3 s"),
            Error::Unknown(p) => write!(f, "unknown parameter: {p}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What one read off the socket gave.
pub enum Recv {
    Data(Vec<u8>),
    Closed,
    Timeout,
}

/// The daemon's TCP session. `send` is called from the keepalive thread too, so both take `&self`.
pub trait Conn: Send + Sync {
    fn send(&self, data: &[u8]) -> io::Result<()>;
    fn recv(&self, timeout: Duration) -> io::Result<Recv>;
}

/// Opens a session to (host, port) within a timeout.
pub type Connector = Box<dyn Fn(&str, u16, Duration) -> io::Result<Arc<dyn Conn>> + Send>;

struct Tcp {
    read: Mutex<TcpStream>,
    write: Mutex<TcpStream>,
}

impl Conn for Tcp {
    fn send(&self, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        self.write.lock().unwrap_or_else(|e| e.into_inner()).write_all(data)
    }

    fn recv(&self, timeout: Duration) -> io::Result<Recv> {
        use std::io::Read;
        let mut s = self.read.lock().unwrap_or_else(|e| e.into_inner());
        s.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut buf = [0u8; 4096];
        match s.read(&mut buf) {
            Ok(0) => Ok(Recv::Closed),
            Ok(n) => Ok(Recv::Data(buf[..n].to_vec())),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(Recv::Timeout),
            Err(e) => Err(e),
        }
    }
}

pub fn tcp_connector() -> Connector {
    Box::new(|host, port, timeout| {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{host}:{port}")))?;
        let s = TcpStream::connect_timeout(&addr, timeout)?;
        s.set_nodelay(true)?;
        Ok(Arc::new(Tcp { write: Mutex::new(s.try_clone()?), read: Mutex::new(s) }) as Arc<dyn Conn>)
    })
}

/// TCP ports ucdaemon listens on at 127.0.0.1, from `netstat -anv -p tcp` output.
pub fn daemon_ports_from(netstat: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in netstat.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.contains(&"LISTEN") && line.contains("ucdaemon") && cols.len() > 3 && cols[3].starts_with("127.0.0.1.")
        {
            if let Some(port) = cols[3].rsplit('.').next().and_then(|p| p.parse().ok()) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    ports
}

pub fn daemon_ports() -> Vec<u16> {
    std::process::Command::new("netstat")
        .args(["-anv", "-p", "tcp"])
        .output()
        .map(|o| daemon_ports_from(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Where the last discovery found the daemon, so the full scan is paid once per UC reopen, not per call.
pub fn default_session_cache() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".quantum-hd8").join("session.json"))
}

/// Flatten a Synchronize-shaped tree (`values` + `children`, recursively) into path -> value.
pub fn flatten(tree: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    walk(tree, "", &mut out);
    out
}

fn walk(node: &Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
    let join = |k: &str| if prefix.is_empty() { k.to_owned() } else { format!("{prefix}/{k}") };
    if let Some(values) = node.get("values").and_then(Value::as_object) {
        for (k, v) in values {
            out.insert(join(k), v.clone());
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_object) {
        for (k, child) in children {
            walk(child, &join(k), out);
        }
    }
}

fn parse_scene_list(m: &Message) -> Vec<String> {
    // FD payload: 14-byte binary header, then a JSON body (measured).
    m.payload
        .get(14..)
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .and_then(|v| v.get("files").and_then(Value::as_array).cloned())
        .map(|files| files.iter().filter_map(|f| f.get("name")?.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn subscribe_json() -> String {
    // Same client fields as a real, working capture (probe-device-tx.bin), spelled the way Python wrote it.
    ucnet::py_object(
        &[
            ("id", Py::Str("Subscribe")),
            ("clientName", Py::Str("quantum-hd8")),
            ("clientInternalName", Py::Str("ucapp")),
            ("clientType", Py::Str("Mac")),
            ("clientDescription", Py::Str("quantum-hd8")),
            ("clientIdentifier", Py::Str("quantum-hd8")),
            ("clientOptions", Py::Str("")),
            ("clientEncoding", Py::Int(23117)),
        ],
        ", ",
        ": ",
    )
}

fn list_scenes(counter: u16) -> Vec<u8> {
    let mut p = counter.to_le_bytes().to_vec();
    p.extend_from_slice(b"Listscene\x00\x00");
    p
}

/// What a scene load did to the preamp gains it was asked to keep.
#[derive(Debug, Default, PartialEq)]
pub struct SceneLoad {
    pub preset_file: String,
    /// (path, before, after) of every gain put back.
    pub gains: Vec<(String, f64, f64)>,
    /// (path, before, after, error) of every gain that could not be put back.
    pub failed: Vec<(String, f64, f64, String)>,
}

struct Keepalive {
    stop: mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

pub struct Client {
    pub host: String,
    pub port: u16,
    pub device_cb: [u8; 4],
    pub state: BTreeMap<String, Value>,
    pub scenes: Vec<String>,
    pub lists: BTreeMap<String, Vec<String>>,
    pub connect_timeout: Duration,
    pub scene_quiet: Duration,
    pub scene_settle_max: Duration,
    connector: Connector,
    discover: bool,
    ports_finder: Box<dyn Fn() -> Vec<u16> + Send>,
    session_cache: Option<PathBuf>,
    conn: Option<Arc<dyn Conn>>,
    decoder: Decoder,
    keepalive: Option<Keepalive>,
}

impl Client {
    /// A client for the real daemon: TCP, with discovery and the session cache in the home directory.
    pub fn new() -> Self {
        Self::with(tcp_connector(), true, Box::new(daemon_ports), default_session_cache())
    }

    /// Everything injected (the tests): discovery only when asked for, and no cache unless given one.
    pub fn with(
        connector: Connector,
        discover: bool,
        ports_finder: Box<dyn Fn() -> Vec<u16> + Send>,
        session_cache: Option<PathBuf>,
    ) -> Self {
        let mut c = Client {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            device_cb: DEVICE_CB,
            state: BTreeMap::new(),
            scenes: Vec::new(),
            lists: BTreeMap::new(),
            connect_timeout: CONNECT_TIMEOUT,
            scene_quiet: SCENE_QUIET,
            scene_settle_max: SCENE_SETTLE_MAX,
            connector,
            discover,
            ports_finder,
            session_cache,
            conn: None,
            decoder: Decoder::new(),
            keepalive: None,
        };
        c.load_session_cache();
        c
    }

    /// A client already holding a session (the tests drive one without the connect handshake).
    pub fn attach(&mut self, conn: Arc<dyn Conn>) {
        self.conn = Some(conn);
    }

    fn conn(&self) -> Result<&Arc<dyn Conn>> {
        self.conn.as_ref().ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotConnected, "not connected")))
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        Ok(self.conn()?.send(data)?)
    }

    pub fn connect(&mut self) -> Result<()> {
        match self.connect_once() {
            Ok(()) => Ok(()),
            Err(e @ (Error::Io(_) | Error::NotResponding)) => {
                if !self.discover {
                    return Err(e);
                }
                self.close();
                self.rediscover()
            }
            Err(e) => Err(e),
        }
    }

    /// Find the (port, session) the daemon answers on now. The port and the session byte move (measured
    /// 2026-09-23 and 2026-10-21), so every session byte is tried, nearest to the last known one first.
    pub fn rediscover(&mut self) -> Result<()> {
        let current = (self.port, self.device_cb);
        self.close();
        let mut ports = vec![self.port];
        ports.extend((self.ports_finder)().into_iter().filter(|&p| p != self.port));
        let last = self.device_cb[2] as i32;
        let mut sessions: Vec<u8> = (0..=255).collect();
        sessions.sort_by_key(|&b| ((b as i32 - last).abs(), b));
        let timeout = std::mem::replace(&mut self.connect_timeout, DISCOVER_TIMEOUT);
        for port in ports {
            for &b in &sessions {
                self.port = port;
                self.device_cb = [DEVICE_CB[0], 0, b, 0];
                if self.connect_once().is_ok() {
                    self.connect_timeout = timeout;
                    self.save_session_cache();
                    return Ok(());
                }
                self.close();
            }
        }
        self.connect_timeout = timeout;
        (self.port, self.device_cb) = current;
        Err(Error::NotResponding)
    }

    fn load_session_cache(&mut self) {
        let Some(path) = &self.session_cache else { return };
        let Some(saved) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return;
        };
        let port = saved.get("port").and_then(Value::as_u64);
        let session = saved.get("session").and_then(Value::as_u64);
        if let (Some(port), Some(session)) = (port, session) {
            if (1..65536).contains(&port) && session < 256 {
                self.port = port as u16;
                self.device_cb = [DEVICE_CB[0], 0, session as u8, 0];
            }
        }
    }

    fn save_session_cache(&self) {
        let Some(path) = &self.session_cache else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // A cache that cannot be written only costs a rescan.
        let _ = std::fs::write(path, format!("{{\"port\": {}, \"session\": {}}}", self.port, self.device_cb[2]));
    }

    fn connect_once(&mut self) -> Result<()> {
        let conn = (self.connector)(&self.host, self.port, self.connect_timeout)?;
        self.conn = Some(conn);
        self.decoder = Decoder::new();
        self.send(&ucnet::encode("UM", &UDP_DISCOVERY_PORT.to_le_bytes(), UM_CB))?;
        self.send(&ucnet::encode("JM", &ucnet::json_payload(&subscribe_json()), self.device_cb))?;
        self.send(&ucnet::encode("FR", &list_scenes(INITIAL_FR_COUNTER), self.device_cb))?;

        // State (ZM/ZB) and the scene list (FD) can arrive split across reads at any byte. Wait for both
        // under one deadline, so a daemon that answers state but never sends FD does not hang connect().
        let (mut got_state, mut got_scenes) = (false, false);
        let deadline = Instant::now() + self.connect_timeout;
        while !(got_state && got_scenes) {
            let left = deadline.saturating_duration_since(Instant::now());
            let data = if left.is_zero() { Recv::Timeout } else { self.conn()?.recv(left)? };
            let Recv::Data(data) = data else {
                if !got_state {
                    return Err(Error::NotResponding);
                }
                break; // scenes never arrived: carry on with none
            };
            for m in self.decoder.feed(&data) {
                match self.handle(&m) {
                    Some("state") => got_state = true,
                    Some("scenes") => got_scenes = true,
                    _ => {}
                }
            }
        }
        self.start_keepalive();
        Ok(())
    }

    fn handle(&mut self, m: &Message) -> Option<&'static str> {
        match m.code.as_str() {
            "ZM" | "ZB" => {
                // A wrong session answers junk: it must not count as state.
                let tree = ucnet::parse_state(m)?;
                self.state = flatten(&tree);
                Some("state")
            }
            "FD" => {
                self.scenes = parse_scene_list(m);
                Some("scenes")
            }
            "PV" => {
                let (path, value) = ucnet::parse_pv(m)?;
                self.state.insert(path, value.into());
                Some("pv")
            }
            "PL" => {
                let (path, value, labels) = ucnet::parse_pl(m)?;
                self.lists.insert(path.clone(), labels);
                self.state.insert(path, value.into());
                Some("pl")
            }
            _ => None,
        }
    }

    /// A parameter's normalized value, from the state the daemon keeps sending.
    pub fn get(&self, path: &str) -> Result<f64> {
        self.state.get(path).and_then(Value::as_f64).ok_or_else(|| Error::Unknown(path.to_owned()))
    }

    /// Write a normalized value and wait for the daemon's echo of it. A value the state already holds is
    /// not sent: the daemon would not echo it, and the write would look refused.
    pub fn set_raw(&mut self, path: &str, normalized: f64) -> Result<f64> {
        if let Some(current) = self.state.get(path).and_then(Value::as_f64) {
            if (normalized - current).abs() < NOOP_TOLERANCE {
                return Ok(current);
            }
        }
        self.send(&ucnet::encode("PV", &ucnet::pv_payload(path, normalized as f32), self.device_cb))?;
        // A stale PV for the same path (re-sent by a recall in flight) updates the state but is not the echo.
        let echo = self.drain_until(
            |m| {
                m.code == "PV"
                    && ucnet::parse_pv(m).is_some_and(|(p, v)| p == path && (v - normalized).abs() < ECHO_TOLERANCE)
            },
            WRITE_TIMEOUT,
        )?;
        echo.and_then(|m| ucnet::parse_pv(&m)).map(|(_, v)| v).ok_or_else(|| Error::WriteNotConfirmed(path.into()))
    }

    /// Load a stored scene and wait for RecalledPreset. With `keep_gains`, every analogue preamp gain the
    /// scene changed is put back: loading a scene resets the gains on the rig.
    pub fn load_scene(&mut self, name: &str, keep_gains: bool) -> Result<SceneLoad> {
        let preset_file = if name.ends_with(".scene") { name.to_owned() } else { format!("{name}.scene") };
        let gain_paths: Vec<String> = (1..=8).map(|ch| format!("line/ch{ch}/preampgain")).collect();
        let before: Vec<(String, f64)> = if keep_gains {
            gain_paths.iter().filter_map(|p| Some((p.clone(), self.get(p).ok()?))).collect()
        } else {
            Vec::new()
        };

        let target = format!("scene/{preset_file}");
        let json = ucnet::py_object(
            &[
                ("id", Py::Str("RestorePreset")),
                ("url", Py::Str("presets")),
                ("presetTarget", Py::Str("")),
                ("presetTargetSlave", Py::Int(0)),
                ("presetFile", Py::Str(&target)),
            ],
            ",",
            ": ",
        );
        self.send(&ucnet::encode("JM", &ucnet::json_payload(&json), self.device_cb))?;
        let recalled = self.drain_until(
            |m| {
                m.code == "JM"
                    && ucnet::parse_json(m).and_then(|j| j.get("id").cloned()) == Some(Value::from("RecalledPreset"))
            },
            SCENE_LOAD_TIMEOUT,
        )?;
        if recalled.is_none() {
            return Err(Error::SceneLoadTimeout(name.into()));
        }
        // The daemon re-sends every PV after RecalledPreset: read until the link is quiet before comparing.
        self.drain_quiet(self.scene_quiet, self.scene_settle_max)?;

        let mut load = SceneLoad { preset_file, ..SceneLoad::default() };
        for (path, b) in before {
            let Ok(a) = self.get(&path) else { continue };
            if a == b {
                continue;
            }
            match self.set_raw(&path, b) {
                Ok(_) => load.gains.push((path, b, a)),
                Err(e @ Error::WriteNotConfirmed(_)) => load.failed.push((path, b, a, e.to_string())),
                Err(e) => return Err(e),
            }
        }
        Ok(load)
    }

    fn drain_quiet(&mut self, quiet: Duration, max_total: Duration) -> Result<()> {
        let deadline = Instant::now() + max_total;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            let Recv::Data(data) = self.conn()?.recv(quiet.min(left))? else { return Ok(()) };
            for m in self.decoder.feed(&data) {
                self.handle(&m);
            }
        }
    }

    /// Read, keeping the state current, until `wanted` matches a message (returned) or the time is up.
    fn drain_until(&mut self, wanted: impl Fn(&Message) -> bool, timeout: Duration) -> Result<Option<Message>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let Recv::Data(data) = self.conn()?.recv(left)? else { return Ok(None) };
            // Every message of the read updates the state, even those after the wanted one.
            let mut found = None;
            for m in self.decoder.feed(&data) {
                self.handle(&m);
                if found.is_none() && wanted(&m) {
                    found = Some(m);
                }
            }
            if found.is_some() {
                return Ok(found);
            }
        }
    }

    fn start_keepalive(&mut self) {
        let Some(conn) = self.conn.clone() else { return };
        let cb = self.device_cb;
        let (stop, wait) = mpsc::channel::<()>();
        let thread = thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(KEEPALIVE_INTERVAL) {
                if conn.send(&ucnet::encode("KA", b"", cb)).is_err() {
                    return;
                }
            }
        });
        self.keepalive = Some(Keepalive { stop, thread });
    }

    pub fn close(&mut self) {
        if let Some(k) = self.keepalive.take() {
            drop(k.stop);
            let _ = k.thread.join();
        }
        self.conn = None;
        self.decoder = Decoder::new();
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}
