//! `mackie describe`: what each device is, its faders and its scenes, even away from the rig.

use mackie_control::describe::{describe, Drivers};
use mackie_control::drivers::{unsupported, Driver, Result};
use mackie_control::profile::parse_str;

struct Fake;

impl Driver for Fake {
    fn name(&self) -> &str {
        "fake"
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        Ok(vec!["ONE".into(), "TWO".into()])
    }
}

struct Unreachable;

impl Driver for Unreachable {
    fn name(&self) -> &str {
        "gone"
    }

    fn scenes(&mut self) -> Result<Vec<String>> {
        unsupported("ucdaemon is not answering")
    }
}

const HD8_BANK: &str =
    "{name: HD 8, driver: fake, faders: {1: {driver: fake, target: main}, 2: {driver: fake, target: phones}}";

fn profile() -> String {
    format!("banks: [{HD8_BANK}}}, {{name: Mac, faders: {{1: {{driver: mac}}}}}}]")
}

fn lines(yaml: &str) -> Vec<String> {
    let mut drivers = Drivers::new();
    drivers.insert("fake".into(), Ok(Box::new(Fake) as Box<dyn Driver>));
    drivers.insert("gone".into(), Ok(Box::new(Unreachable) as Box<dyn Driver>));
    drivers.insert("mac".into(), Err("not here".into()));
    describe(&parse_str(yaml).unwrap(), &mut drivers)
}

#[test]
fn every_device_is_listed_with_its_driver_and_faders() {
    let l = lines(&profile());
    assert!(l[0].starts_with("1  HD 8"));
    assert!(l[0].contains("fake") && l[0].contains("2 faders"));
}

#[test]
fn the_scenes_of_a_device_are_listed_in_order() {
    let l = lines(&profile());
    assert!(l[0].contains("1 ONE") && l[0].contains("2 TWO"));
}

#[test]
fn a_device_without_scenes_says_so() {
    assert!(lines(&profile())[1].contains("no scenes"));
}

#[test]
fn the_profile_list_wins_over_the_device_list() {
    let l = lines(&format!("banks: [{HD8_BANK}, scenes: [TWO]}}]"));
    assert!(l[0].contains("1 TWO") && !l[0].contains("ONE"));
}

#[test]
fn gear_that_cannot_be_reached_still_lists_the_profile() {
    // It has to be usable away from the rig.
    let l = lines("banks: [{name: HD 8, driver: gone, scenes: [MIXER-ON], faders: {1: {driver: gone, target: main}}}]");
    assert!(l[0].contains("1 MIXER-ON"));
    assert!(l.iter().any(|x| x.contains("could not be reached")));
}
