//! `mackie devices`: what is configured, and what it can load.
//!
//! Answers from the profile first and the gear second, so it works away from the rig: an unreachable
//! device still lists the scenes the profile named, plus one line saying the driver did not answer.

use std::collections::BTreeMap;

use crate::drivers::Driver;
use crate::profile::{Bank, Profile};

/// The driver most of a bank's faders use.
pub fn main_driver(bank: &Bank) -> Option<String> {
    most_common(bank.faders.values().map(|d| d.driver.as_str()))
}

/// The most frequent name; a tie goes to the one seen first.
pub fn most_common<'a>(names: impl Iterator<Item = &'a str>) -> Option<String> {
    let names: Vec<&str> = names.collect();
    let mut best: Option<(&str, usize)> = None;
    for n in &names {
        let count = names.iter().filter(|m| *m == n).count();
        if best.is_none_or(|(_, c)| count > c) {
            best = Some((n, count));
        }
    }
    best.map(|(n, _)| n.to_string())
}

pub type Drivers = BTreeMap<String, Result<Box<dyn Driver>, String>>;

/// The profile's list when it has one, otherwise the device's. The device is asked either way: that is also
/// how we find out it is not there, which is worth a line even when the profile could answer alone.
fn scenes(bank: &Bank, drivers: &mut Drivers, unreachable: &mut Vec<String>) -> Vec<String> {
    let mut from_gear = Vec::new();
    if let Some(name) = bank.driver.clone().or_else(|| main_driver(bank)) {
        let asked = match drivers.get_mut(&name) {
            Some(Ok(d)) => d.scenes().map_err(|e| e.to_string()),
            Some(Err(e)) => Err(e.clone()),
            None => Err(format!("no driver '{name}'")),
        };
        match asked {
            Ok(s) => from_gear = s,
            Err(e) => unreachable.push(format!("   {name}: could not be reached ({e})")),
        }
    }
    if bank.scenes.is_empty() {
        from_gear
    } else {
        bank.scenes.clone()
    }
}

pub fn describe(profile: &Profile, drivers: &mut Drivers) -> Vec<String> {
    let mut lines = Vec::new();
    let mut unreachable = Vec::new();
    for (i, bank) in profile.banks.iter().enumerate() {
        let driver = bank.driver.clone().or_else(|| main_driver(bank)).unwrap_or_else(|| "-".into());
        let scenes = scenes(bank, drivers, &mut unreachable);
        let shown = if scenes.is_empty() {
            "no scenes".to_string()
        } else {
            let each: Vec<String> = scenes.iter().enumerate().map(|(n, s)| format!("{} {s}", n + 1)).collect();
            format!("scenes: {}", each.join("  "))
        };
        let faders = if bank.live.is_some() { "live".to_string() } else { bank.faders.len().to_string() };
        lines.push(format!("{}  {:<8} ({driver})   {faders} faders   {shown}", i + 1, bank.name));
    }
    lines.extend(unreachable);
    lines
}
