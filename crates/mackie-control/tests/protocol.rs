//! The wire format, the surfaces, the profile parser and the shipped examples.

use std::path::PathBuf;

use mackie_control::profile::{load_profile, parse_str};
use mackie_control::protocol::{self as p, decode, fader_position, led, Button, Event};
use mackie_control::surfaces;

fn button(msg: &[u8]) -> Button {
    match decode(msg) {
        Some(Event::Button(b)) => b,
        other => panic!("not a button: {other:?}"),
    }
}

#[test]
fn fader_is_unsigned_14_bit() {
    // mido's pitch 8191 is raw 16383; -8192 is raw 0
    assert_eq!(decode(&[0xE2, 0x7F, 0x7F]), Some(Event::Fader { channel: 2, value: 1.0 }));
    assert_eq!(decode(&[0xE0, 0, 0]), Some(Event::Fader { channel: 0, value: 0.0 }));
}

#[test]
fn encoder_is_relative() {
    assert_eq!(decode(&[0xB0, 0x10, 1]), Some(Event::Encoder { channel: 0, delta: 1 }));
    assert_eq!(decode(&[0xB0, 0x17, 0x41]), Some(Event::Encoder { channel: 7, delta: -1 }));
}

#[test]
fn button_kinds_and_channel() {
    let b = button(&[0x90, p::MUTE + 4, 127]);
    assert_eq!((b.kind(), b.channel(), b.pressed), ("mute", Some(4), true));
    let bank = button(&[0x90, p::BANK_RIGHT, 127]);
    assert_eq!((bank.kind(), bank.channel()), ("bank_right", None));
    assert!(!button(&[0x80, p::SOLO, 0]).pressed);
}

#[test]
fn round_trip_of_a_fader_position() {
    match decode(&fader_position(3, 0.5).bytes()) {
        Some(Event::Fader { channel, value }) => assert!(channel == 3 && (value - 0.5).abs() < 1e-4),
        other => panic!("{other:?}"),
    }
}

#[test]
fn led_builds_a_valid_message() {
    assert_eq!(led(p::SELECT + 1, true).bytes()[2], 127);
    assert_eq!(led(p::SELECT + 1, false).bytes()[2], 0);
}

#[test]
fn the_up_and_down_arrows_decode() {
    // Measured on the SMC-Mixer 2026-09-22: notes 60 and 61.
    assert_eq!(button(&[0x90, 0x60, 127]).kind(), "arrow_up");
    assert_eq!(button(&[0x90, 0x61, 127]).kind(), "arrow_down");
}

// -- surfaces --------------------------------------------------------------------------------------------

#[test]
fn default_surface_is_the_smc_mixer() {
    let s = surfaces::get(None).unwrap();
    assert!(s.name == "SMC-Mixer" && s.faders == 8 && s.encoders == 8);
    assert_eq!(s.port_hints, ["SMC-Mixer-Master", "SMC-Mixer"]);
}

#[test]
fn name_is_normalised() {
    assert!(std::ptr::eq(surfaces::get(Some("smc_mixer")).unwrap(), surfaces::get(Some("SMC-MIXER")).unwrap()));
}

#[test]
fn unknown_surface_is_refused() {
    assert!(surfaces::get(Some("nope")).unwrap_err().contains("unknown surface"));
}

// -- profile ---------------------------------------------------------------------------------------------

const PROFILE: &str = "surface: smc-mixer
banks:
  - name: HD 8
    faders:
      1: {driver: hd8, target: global/mainOutVolume, label: MAIN, group: out, mute: global/mute}
      5: {driver: hd8, target: line/ch1/volume, label: GUITAR 1, group: in}
    buttons: {rec: scene, select: bank}
  - {name: Mac, faders: {1: {driver: mac}}}
";

#[test]
fn parses_banks_faders_and_buttons() {
    let p = parse_str(PROFILE).unwrap();
    let names: Vec<&str> = p.banks.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["HD 8", "Mac"]);
    let main = &p.banks[0].faders[&1];
    assert_eq!(
        (main.driver.as_str(), main.label.as_str(), main.group.as_str(), main.mute.as_deref()),
        ("hd8", "MAIN", "out", Some("global/mute"))
    );
    assert_eq!(p.banks[0].buttons["rec"], "scene");
}

#[test]
fn label_falls_back_to_the_target() {
    let p = parse_str("banks: [{faders: {2: {driver: hd8, target: aux/ch10/volume}}}]").unwrap();
    assert_eq!(p.banks[0].faders[&2].label, "aux/ch10/volume");
}

#[test]
fn bank_index_wraps_around() {
    let p = parse_str(PROFILE).unwrap();
    assert!(p.bank(2).name == "HD 8" && p.bank(-1).name == "Mac");
}

#[test]
fn a_destination_without_driver_is_refused() {
    assert!(parse_str("banks: [{faders: {1: {target: x}}}]").is_err());
}

#[test]
fn a_profile_without_banks_is_refused() {
    assert!(parse_str("{}").is_err());
}

#[test]
fn missing_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_profile(dir.path().join("nao-existe.yaml").to_str().unwrap()).is_err());
}

// -- examples: a broken example is a broken doc ----------------------------------------------------------

fn examples() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn there_are_examples() {
    assert!(!examples().is_empty(), "examples/ should ship at least one profile");
}

#[test]
fn every_example_parses_and_has_a_known_shape() {
    for path in examples() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let p = load_profile(path.to_str().unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!p.banks.is_empty());
        for bank in &p.banks {
            for (fader, dest) in &bank.faders {
                assert!((1..=8).contains(fader), "{name}: fader {fader} is out of range");
                assert!(
                    ["hd8", "mac", "app"].contains(&dest.driver.as_str()),
                    "{name}: unknown driver {}",
                    dest.driver
                );
                assert!(["in", "out"].contains(&dest.group.as_str()), "{name}: unknown group {}", dest.group);
            }
        }
    }
}
