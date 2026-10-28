//! Ported from jpfaria/quantum-hd8 tests (test_ucnet, test_client, test_discover, test_scene, test_set): the
//! parts the bridge uses.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quantum_hd8::client::{self, Client, Conn, Connector, Recv, DEVICE_REPLY_CB};
use quantum_hd8::ucnet::{self, Decoder, Message};
use quantum_hd8::Error;
use serde_json::{json, Value};

fn fx(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// probe-device-rx.bin: where ZM+JM end and FD starts.
const FD_OFFSET: usize = 2881;
const CH1_GAIN: f64 = 0.25999999046325684;

fn pv(path: &str, value: f32) -> Vec<u8> {
    ucnet::encode("PV", &ucnet::pv_payload(path, value), DEVICE_REPLY_CB)
}

#[derive(Default)]
struct Fake {
    rx: Mutex<VecDeque<Vec<u8>>>,
    tx: Mutex<Vec<u8>>,
    /// Out of data: a quiet daemon (Timeout) instead of a closed socket.
    quiet: bool,
    /// Every PV written is echoed back, as the daemon does.
    echo: bool,
    /// Writes containing this are swallowed: no echo.
    swallow: Option<&'static [u8]>,
    /// Queued before every write's echo (a stale PV a recall re-sends).
    stale: Option<Vec<u8>>,
    /// Out of data: never quiet, always this.
    chatter: Option<Vec<u8>>,
}

impl Fake {
    fn new(rx: Vec<Vec<u8>>) -> Self {
        Fake { rx: Mutex::new(rx.into()), ..Fake::default() }
    }

    fn tx(&self) -> Vec<u8> {
        self.tx.lock().unwrap().clone()
    }
}

impl Conn for Fake {
    fn send(&self, data: &[u8]) -> io::Result<()> {
        self.tx.lock().unwrap().extend_from_slice(data);
        if self.swallow.is_some_and(|s| data.windows(s.len()).any(|w| w == s)) {
            return Ok(());
        }
        let mut rx = self.rx.lock().unwrap();
        if let Some(stale) = &self.stale {
            rx.push_back(stale.clone());
        }
        if self.echo {
            for m in Decoder::new().feed(data) {
                if m.code == "PV" {
                    let (path, value) = ucnet::parse_pv(&m).unwrap();
                    rx.push_back(pv(&path, value as f32));
                }
            }
        }
        Ok(())
    }

    fn recv(&self, _timeout: Duration) -> io::Result<Recv> {
        if let Some(d) = self.rx.lock().unwrap().pop_front() {
            return Ok(Recv::Data(d));
        }
        if let Some(c) = &self.chatter {
            return Ok(Recv::Data(c.clone()));
        }
        Ok(if self.quiet { Recv::Timeout } else { Recv::Closed })
    }
}

fn connector(fake: Arc<Fake>) -> Connector {
    Box::new(move |_, _, _| Ok(fake.clone() as Arc<dyn Conn>))
}

fn client(fake: &Arc<Fake>) -> Client {
    Client::with(connector(fake.clone()), false, Box::new(Vec::new), None)
}

fn attached(fake: &Arc<Fake>) -> Client {
    let mut c = client(fake);
    c.attach(fake.clone());
    c
}

// --- ucnet -------------------------------------------------------------------------------------------

#[test]
fn split_packet_is_buffered() {
    let raw = ucnet::encode("JM", &ucnet::json_payload(r#"{"id": "X"}"#), ucnet::CB);
    let mut d = Decoder::new();
    assert!(d.feed(&raw[..5]).is_empty());
    let ms = d.feed(&raw[5..]);
    assert_eq!(ucnet::parse_json(&ms[0]), Some(json!({"id": "X"})));
}

#[test]
fn decoder_skips_garbage_before_magic() {
    let mut data = b"\x00garbage\xffbytes".to_vec();
    data.extend(ucnet::encode("KA", b"", ucnet::CB));
    let ms = Decoder::new().feed(&data);
    assert_eq!((ms[0].code.as_str(), ms[0].payload.len()), ("KA", 0));
}

#[test]
fn pv_roundtrip() {
    let ms = Decoder::new().feed(&ucnet::encode("PV", &ucnet::pv_payload("line/ch1/volume", -12.5), ucnet::CB));
    assert_eq!(ucnet::parse_pv(&ms[0]), Some(("line/ch1/volume".into(), -12.5)));
}

#[test]
fn live_subscribe_capture() {
    let ms = Decoder::new().feed(&fx("probe-subscribe-rx.bin"));
    assert_eq!(ms.iter().map(|m| m.code.as_str()).collect::<Vec<_>>(), ["ZM", "JM"]);
    assert_eq!(ucnet::parse_state(&ms[0]), Some(json!({"id": "UpdateMidiEndpoints", "endpointList": []})));
    assert_eq!(ucnet::parse_json(&ms[1]), Some(json!({"id": "SubscriptionReply"})));
    assert_eq!(ms[0].cbytes, [0x65, 0, 0x68, 0]);
}

#[test]
fn parse_state_decompresses_whole_payload_not_sliced_by_u32() {
    use std::io::Write;
    let body = br#"{"id": "State", "tree": {"line/ch1/preampgain": 12.0}}"#;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(body).unwrap();
    let raw = z.finish().unwrap();
    assert_ne!(body.len(), raw.len());
    let mut payload = (body.len() as u32).to_le_bytes().to_vec();
    payload.extend(raw);
    let m = Message { code: "ZM".into(), cbytes: ucnet::CB, payload };
    assert_eq!(ucnet::parse_state(&m), Some(json!({"id": "State", "tree": {"line/ch1/preampgain": 12.0}})));
}

#[test]
fn parse_pv_against_real_write_capture() {
    let ms = Decoder::new().feed(&fx("uc-pvwrite.bin"));
    assert_eq!(ucnet::parse_pv(&ms[0]), Some(("global/mixerMode".into(), 0.0)));
}

#[test]
fn parse_pl_splits_path_value_and_labels() {
    let ms = Decoder::new().feed(&fx("uc-pl.bin"));
    let (path, value, labels) = ucnet::parse_pl(&ms[0]).unwrap();
    assert_eq!((path.as_str(), value), ("global/phones1_src", 0.0));
    assert_eq!(&labels[..2], ["Main L/R", "Out  3/4"]);
    assert_eq!(labels.last().unwrap(), "Loopback  2");
    let link = ms.iter().find(|m| m.payload.starts_with(b"global/mainOutVolumeLink")).unwrap();
    let (_, value, labels) = ucnet::parse_pl(link).unwrap();
    assert_eq!(value, 0.3333);
    assert_eq!(labels, ["None", "All", "1-2", "1-4", "1-6", "1-8", "All + ADAT"]);
}

// --- connect -----------------------------------------------------------------------------------------

#[test]
fn flatten_walks_children_and_values() {
    let tree = json!({"children": {"line": {"children": {"ch1": {"values": {"volume": -3, "preampgain": 12.0}}}}}});
    let flat = client::flatten(&tree);
    assert_eq!(flat.len(), 2);
    assert_eq!(flat["line/ch1/volume"], json!(-3));
    assert_eq!(flat["line/ch1/preampgain"], json!(12.0));
}

#[test]
fn connect_sends_the_captured_bytes_and_reads_state_and_scenes() {
    let fake = Arc::new(Fake::new(vec![fx("probe-device-rx.bin")]));
    let mut c = client(&fake);
    c.connect().unwrap();
    assert_eq!(fake.tx(), fx("probe-device-tx.bin"));
    assert_eq!(c.get("line/ch1/preampgain").unwrap(), CH1_GAIN);
    assert_eq!(c.scenes[0], "ELEMENT.scene");
    assert!(matches!(c.get("line/ch1/does-not-exist"), Err(Error::Unknown(_))));
}

#[test]
fn connect_waits_for_fd_in_a_later_segment() {
    let rx = fx("probe-device-rx.bin");
    let fake = Arc::new(Fake::new(vec![rx[..FD_OFFSET].to_vec(), rx[FD_OFFSET..].to_vec()]));
    let mut c = client(&fake);
    c.connect().unwrap();
    assert_eq!(c.scenes[0], "ELEMENT.scene");
}

#[test]
fn connect_waits_for_fd_split_byte_by_byte() {
    let rx = fx("probe-device-rx.bin");
    let fake = Arc::new(Fake::new(rx.iter().map(|&b| vec![b]).collect()));
    let mut c = client(&fake);
    c.connect().unwrap();
    assert_eq!(c.get("line/ch1/preampgain").unwrap(), CH1_GAIN);
    assert_eq!(c.scenes[0], "ELEMENT.scene");
}

#[test]
fn connect_gives_up_on_fd_instead_of_hanging() {
    let rx = fx("probe-device-rx.bin");
    let fake = Arc::new(Fake { quiet: true, ..Fake::new(vec![rx[..FD_OFFSET].to_vec()]) });
    let mut c = client(&fake);
    c.connect_timeout = Duration::from_millis(50);
    c.connect().unwrap();
    assert_eq!(c.get("line/ch1/preampgain").unwrap(), CH1_GAIN);
    assert!(c.scenes.is_empty());
}

#[test]
fn keepalive_goes_out_once_connected() {
    let fake = Arc::new(Fake::new(vec![fx("probe-device-rx.bin")]));
    let mut c = client(&fake);
    c.connect().unwrap();
    std::thread::sleep(client::KEEPALIVE_INTERVAL + Duration::from_millis(200));
    c.close();
    let ka = ucnet::encode("KA", b"", client::DEVICE_CB);
    assert!(fake.tx().windows(ka.len()).any(|w| w == ka));
}

// --- discovery ---------------------------------------------------------------------------------------

const NETSTAT: &str = "\
tcp4  0 0  127.0.0.1.62586  *.*  LISTEN  0 0 131072 131072  ucdaemon:20298  00180
tcp4  0 0  192.168.15.60.62586  *.*  LISTEN  0 0 131072 131072  ucdaemon:20298  00180
tcp4  0 0  127.0.0.1.4123  *.*  LISTEN  0 0 131072 131072  openrig:13984  00100
tcp4  0 0  127.0.0.1.62586  127.0.0.1.62596  ESTABLISHED 0 0 1 1  ucdaemon:20298 00182
";

#[test]
fn daemon_ports_reads_only_ucdaemon_on_loopback() {
    assert_eq!(client::daemon_ports_from(NETSTAT), [62586]);
}

/// Answers only on `port`, and only to a client addressing `session`.
struct Daemon {
    port: u16,
    session: Option<u8>,
    opened: Arc<Mutex<Vec<u16>>>,
}

struct DaemonConn {
    session: Option<u8>,
    tx: Mutex<Vec<u8>>,
    done: Mutex<bool>,
}

impl Conn for DaemonConn {
    fn send(&self, data: &[u8]) -> io::Result<()> {
        self.tx.lock().unwrap().extend_from_slice(data);
        Ok(())
    }

    fn recv(&self, _: Duration) -> io::Result<Recv> {
        let mut done = self.done.lock().unwrap();
        if let Some(s) = self.session {
            let cb = [0x6a, 0, s, 0];
            if !*done && self.tx.lock().unwrap().windows(4).any(|w| w == cb) {
                *done = true;
                return Ok(Recv::Data(fx("probe-device-rx.bin")));
            }
        }
        Ok(Recv::Timeout)
    }
}

impl Daemon {
    fn new(port: u16, session: Option<u8>) -> Self {
        Daemon { port, session, opened: Arc::default() }
    }

    fn client(&self, ports: Vec<u16>, cache: Option<std::path::PathBuf>) -> Client {
        let (port, session, opened) = (self.port, self.session, self.opened.clone());
        let connector: Connector = Box::new(move |_, p, _| {
            opened.lock().unwrap().push(p);
            if p != port {
                return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused"));
            }
            Ok(Arc::new(DaemonConn { session, tx: Mutex::default(), done: Mutex::default() }) as Arc<dyn Conn>)
        });
        Client::with(connector, true, Box::new(move || ports.clone()), cache)
    }
}

#[test]
fn connect_finds_a_moved_port_and_session() {
    let d = Daemon::new(62586, Some(0x66));
    let mut c = d.client(vec![62586], None);
    c.connect().unwrap();
    assert_eq!((c.port, c.device_cb), (62586, [0x6a, 0, 0x66, 0]));
    assert_eq!(c.get("line/ch1/preampgain").unwrap(), CH1_GAIN);
}

#[test]
fn connect_keeps_the_defaults_when_they_answer() {
    let d = Daemon::new(59791, Some(0x69));
    let mut c = d.client(vec![], None);
    c.connect().unwrap();
    assert_eq!(*d.opened.lock().unwrap(), [59791]);
    assert_eq!(c.device_cb, [0x6a, 0, 0x69, 0]);
}

#[test]
fn no_discovery_unless_asked() {
    let fake_refused: Connector =
        Box::new(|_, _, _| Err(io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused")));
    let mut c = Client::with(fake_refused, false, Box::new(|| vec![62586]), None);
    assert!(matches!(c.connect(), Err(Error::Io(_))));
}

#[test]
fn nothing_answers_is_not_responding_and_keeps_the_defaults() {
    let d = Daemon::new(62586, None);
    let mut c = d.client(vec![62586], None);
    assert!(matches!(c.connect(), Err(Error::NotResponding)));
    assert_eq!((c.port, c.device_cb), (59791, [0x6a, 0, 0x69, 0]));
}

#[test]
fn connect_finds_a_session_far_from_the_default() {
    // Measured 21/10: UC reopened, daemon on 49153, HD 8 session 0x7c.
    let d = Daemon::new(49153, Some(0x7c));
    let mut c = d.client(vec![49153], None);
    c.connect().unwrap();
    assert_eq!((c.port, c.device_cb), (49153, [0x6a, 0, 0x7c, 0]));
}

#[test]
fn found_address_is_remembered_for_the_next_client() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("session.json");
    let d = Daemon::new(49153, Some(0x7c));
    d.client(vec![49153], Some(cache.clone())).connect().unwrap();
    d.opened.lock().unwrap().clear();
    let mut c = d.client(vec![49153], Some(cache));
    c.connect().unwrap();
    assert_eq!(*d.opened.lock().unwrap(), [49153]);
    assert_eq!(c.device_cb, [0x6a, 0, 0x7c, 0]);
}

#[test]
fn a_broken_cache_falls_back_to_the_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("session.json");
    std::fs::write(&cache, "{not json").unwrap();
    let d = Daemon::new(59791, Some(0x69));
    d.client(vec![], Some(cache)).connect().unwrap();
    assert_eq!(*d.opened.lock().unwrap(), [59791]);
}

// --- writes ------------------------------------------------------------------------------------------

fn writer(fake: Fake) -> (Client, Arc<Fake>) {
    let fake = Arc::new(fake);
    let mut c = attached(&fake);
    c.state.insert("line/ch1/preampgain".into(), json!(0.26));
    c.state.insert("global/ledBrightness".into(), json!(0.7475));
    (c, fake)
}

#[test]
fn set_raw_waits_for_the_echo() {
    let (mut c, _) = writer(Fake { echo: true, ..Fake::default() });
    assert!((c.set_raw("line/ch1/preampgain", 0.5).unwrap() - 0.5).abs() < 1e-4);
    assert!((c.get("line/ch1/preampgain").unwrap() - 0.5).abs() < 1e-4);
}

#[test]
fn set_raw_without_echo_is_write_not_confirmed() {
    let (mut c, _) = writer(Fake::default());
    let e = c.set_raw("global/ledBrightness", 0.2).unwrap_err();
    assert!(matches!(e, Error::WriteNotConfirmed(_)));
    assert_eq!(e.to_string(), "global/ledBrightness: the daemon did not confirm the write in 1 s");
}

#[test]
fn set_raw_is_noop_when_state_already_at_value() {
    let (mut c, fake) = writer(Fake::default());
    assert_eq!(c.set_raw("line/ch1/preampgain", 0.26).unwrap(), 0.26);
    assert!(fake.tx().is_empty());
}

#[test]
fn set_raw_ignores_a_stale_pv_and_waits_for_the_echo() {
    let (mut c, _) = writer(Fake { echo: true, stale: Some(pv("line/ch1/preampgain", 0.0)), ..Fake::default() });
    assert!((c.set_raw("line/ch1/preampgain", 0.5).unwrap() - 0.5).abs() < 1e-4);
    assert!((c.get("line/ch1/preampgain").unwrap() - 0.5).abs() < 1e-4);
}

#[test]
fn set_raw_fails_when_only_a_stale_pv_arrives() {
    let (mut c, _) = writer(Fake { stale: Some(pv("line/ch1/preampgain", 0.0)), ..Fake::default() });
    assert!(matches!(c.set_raw("line/ch1/preampgain", 0.5), Err(Error::WriteNotConfirmed(_))));
}

// --- scenes ------------------------------------------------------------------------------------------

#[test]
fn load_scene_sends_byte_identical_restore_preset() {
    for name in ["MK300-FRFR", "MK300-FRFR.scene"] {
        let fake = Arc::new(Fake::new(vec![fx("uc-recalled.bin")]));
        let load = attached(&fake).load_scene(name, false).unwrap();
        assert_eq!(fake.tx(), fx("uc-restore.bin"));
        assert_eq!(load.preset_file, "MK300-FRFR.scene");
    }
}

#[test]
fn load_scene_without_recalled_preset_times_out() {
    let fake = Arc::new(Fake::default());
    assert!(matches!(attached(&fake).load_scene("MK300-FRFR", false), Err(Error::SceneLoadTimeout(_))));
}

fn gain_client(fake: Fake, ch1: f64) -> (Client, Arc<Fake>) {
    let fake = Arc::new(Fake { quiet: true, echo: true, ..fake });
    let mut c = attached(&fake);
    c.scene_quiet = Duration::from_millis(20);
    for n in 1..=8 {
        c.state.insert(format!("line/ch{n}/preampgain"), json!(if n == 1 { ch1 } else { 0.0 }));
    }
    (c, fake)
}

fn joined(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn without_keep_gains_nothing_is_rewritten() {
    let fake = Arc::new(Fake::new(vec![fx("uc-recalled.bin")]));
    let mut c = attached(&fake);
    c.state.insert("line/ch1/preampgain".into(), json!(0.5));
    let load = c.load_scene("MK300-FRFR", false).unwrap();
    assert!(load.gains.is_empty());
    assert_eq!(fake.tx(), fx("uc-restore.bin"));
}

#[test]
fn keep_gains_resets_only_changed_channels() {
    // The ch1 PV arrives in the same read as RecalledPreset, before it.
    let rx = joined(&[pv("line/ch1/preampgain", 0.0), fx("uc-recalled.bin")]);
    let (mut c, _) = gain_client(Fake::new(vec![rx]), 0.5);
    let load = c.load_scene("MK300-FRFR", true).unwrap();
    assert_eq!(load.gains, [("line/ch1/preampgain".to_string(), 0.5, 0.0)]);
}

#[test]
fn keep_gains_restores_gains_zeroed_after_recalled_preset() {
    let rx = vec![fx("uc-recalled.bin"), joined(&[pv("line/ch1/preampgain", 0.0), pv("line/ch2/preampgain", 0.0)])];
    let (mut c, _) = gain_client(Fake::new(rx), 0.5);
    let load = c.load_scene("MK300-FRFR", true).unwrap();
    assert_eq!(load.gains, [("line/ch1/preampgain".to_string(), 0.5, 0.0)]);
    assert!(load.failed.is_empty());
    assert!((c.get("line/ch1/preampgain").unwrap() - 0.5).abs() < 1e-4);
}

#[test]
fn keep_gains_waits_for_recall_pvs_split_across_reads() {
    let zero1 = pv("line/ch1/preampgain", 0.0);
    let rx = vec![fx("uc-recalled.bin"), zero1[..7].to_vec(), zero1[7..].to_vec(), pv("line/ch3/preampgain", 0.0)];
    let (mut c, _) = gain_client(Fake::new(rx), 0.5);
    c.state.insert("line/ch3/preampgain".into(), json!(0.4));
    let load = c.load_scene("MK300-FRFR", true).unwrap();
    let restored: Vec<&str> = load.gains.iter().map(|g| g.0.as_str()).collect();
    assert_eq!(restored, ["line/ch1/preampgain", "line/ch3/preampgain"]);
    assert!((c.get("line/ch3/preampgain").unwrap() - 0.4).abs() < 1e-4);
}

#[test]
fn keep_gains_settle_is_bounded_when_the_daemon_never_goes_quiet() {
    let (mut c, _) =
        gain_client(Fake { chatter: Some(pv("aux/ch1/volume", 0.5)), ..Fake::new(vec![fx("uc-recalled.bin")]) }, 0.0);
    c.scene_settle_max = Duration::from_millis(200);
    let t0 = Instant::now();
    c.load_scene("MK300-FRFR", true).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[test]
fn keep_gains_collects_failed_channels_and_continues() {
    let rx = vec![fx("uc-recalled.bin"), joined(&[pv("line/ch1/preampgain", 0.0), pv("line/ch2/preampgain", 0.0)])];
    let (mut c, _) = gain_client(Fake { swallow: Some(b"line/ch1/preampgain"), ..Fake::new(rx) }, 0.5);
    c.state.insert("line/ch2/preampgain".into(), json!(0.4));
    let load = c.load_scene("MK300-FRFR", true).unwrap();
    assert_eq!(load.failed.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["line/ch1/preampgain"]);
    assert_eq!(load.gains.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(), ["line/ch2/preampgain"]);
}

#[test]
fn state_is_a_json_value_map() {
    let fake = Arc::new(Fake::default());
    let mut c = attached(&fake);
    c.state.insert("line/ch1/username".into(), Value::from("Guitarra"));
    assert!(matches!(c.get("line/ch1/username"), Err(Error::Unknown(_))));
}
