//! A bank that names only a device gets that device's own banks.

use mackie_control::defaults::expand;
use mackie_control::profile::{parse_str, Profile};

fn expanded(yaml: &str) -> Profile {
    expand(parse_str(yaml).unwrap()).unwrap()
}

fn names(p: &Profile) -> Vec<&str> {
    p.banks.iter().map(|b| b.name.as_str()).collect()
}

#[test]
fn a_bank_with_only_a_driver_becomes_that_devices_default_banks() {
    assert_eq!(names(&expanded("banks: [{name: HD 8, driver: hd8}]")), ["HD 8 IN", "HD 8 OUT"]);
}

#[test]
fn the_input_bank_is_the_eight_analog_channels() {
    let p = expanded("banks: [{driver: hd8}]");
    let ins = &p.banks[0];
    assert_eq!(ins.faders.len(), 8);
    assert_eq!(ins.faders[&1].target(), "line/ch1/volume");
    assert_eq!(ins.faders[&1].mute.as_deref(), Some("line/ch1/mute"));
    assert_eq!(ins.faders[&8].target(), "line/ch8/volume");
    assert!(ins.faders.values().all(|d| d.group == "in"));
}

#[test]
fn the_output_bank_starts_at_main_and_the_two_headphones() {
    let p = expanded("banks: [{driver: hd8}]");
    let outs = &p.banks[1];
    assert_eq!(outs.faders[&1].target(), "global/mainOutVolume");
    assert_eq!(outs.faders[&1].mute.as_deref(), Some("global/mute"));
    assert_eq!(outs.faders[&2].target(), "global/phones1_volume");
    assert_eq!(outs.faders[&3].target(), "global/phones2_volume");
    assert_eq!(outs.faders.len(), 8);
    assert!(outs.faders.values().all(|d| d.group == "out"));
}

#[test]
fn the_scenes_and_the_driver_travel_to_both_banks() {
    let p = expanded("banks: [{driver: hd8, scenes: [MIXER-ON]}]");
    assert!(p.banks.iter().all(|b| b.driver.as_deref() == Some("hd8") && b.scenes == ["MIXER-ON"]));
}

#[test]
fn a_bank_that_names_its_own_faders_is_left_alone() {
    let p = expanded("banks: [{name: meu, driver: hd8, faders: {1: {driver: hd8, target: global/mainOutVolume}}}]");
    assert_eq!(names(&p), ["meu"]);
    assert_eq!(p.banks[0].faders.len(), 1);
}

#[test]
fn a_driver_with_no_default_is_left_alone() {
    let p = expanded("banks: [{name: Mac, driver: mac}]");
    assert_eq!(names(&p), ["Mac"]);
    assert!(p.banks[0].faders.is_empty());
}

#[test]
fn a_bank_can_ask_for_one_of_the_devices_defaults() {
    let p = expanded("banks: [{driver: hd8, default: IN}]");
    assert_eq!(names(&p), ["HD 8 IN"]);
    assert_eq!(p.banks[0].faders[&1].target(), "line/ch1/volume");
}

#[test]
fn the_three_shapes_live_side_by_side() {
    // A rig's own bank, then the device's whole input side, then its output side -- what João asked for
    // (2026-09-22).
    let p = expanded(
        "banks:
  - {name: RIG, driver: hd8, faders: {1: {driver: hd8, target: global/mainOutVolume}}}
  - {driver: hd8, default: IN}
  - {driver: hd8, default: OUT}",
    );
    assert_eq!(names(&p), ["RIG", "HD 8 IN", "HD 8 OUT"]);
}

#[test]
fn a_default_that_does_not_exist_is_refused_by_name() {
    let e = expand(parse_str("banks: [{driver: hd8, default: MIDDLE}]").unwrap()).unwrap_err();
    assert!(e.contains("MIDDLE") && e.contains("IN"), "{e}");
}
