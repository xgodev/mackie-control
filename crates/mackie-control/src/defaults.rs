//! A device's own banks.
//!
//! The same gear has the same knobs in every rig, so the map of what a device is worth belongs to its
//! driver, not to each user's profile. A bank that names a `driver:` and no faders is replaced by that
//! driver's default banks; a bank that writes its own faders is never touched.

use crate::drivers::default_banks;
use crate::profile::{Bank, Profile};

fn banks_of(bank: Bank) -> Result<Vec<Bank>, String> {
    let Some(driver) = bank.driver.clone() else { return Ok(vec![bank]) };
    if !bank.faders.is_empty() || !bank.encoders.is_empty() {
        return Ok(vec![bank]);
    }
    let all = default_banks(&driver);
    if all.is_empty() {
        return Ok(vec![bank]);
    }
    let chosen: Vec<_> = match &bank.default {
        None => all.clone(),
        Some(want) => {
            let chosen: Vec<_> =
                all.iter().filter(|b| b.name.to_uppercase().ends_with(&want.to_uppercase())).cloned().collect();
            if chosen.is_empty() {
                let names: Vec<&str> = all.iter().map(|b| b.name.as_str()).collect();
                return Err(format!("{driver} has no default bank '{want}'; it has: {}", names.join(", ")));
            }
            chosen
        }
    };
    Ok(chosen
        .into_iter()
        .map(|b| Bank {
            name: if b.name.is_empty() { bank.name.clone() } else { b.name },
            live: b.live,
            faders: b.faders,
            ..bank.clone()
        })
        .collect())
}

/// The profile as the bridge should see it, with every device's default banks in place.
pub fn expand(profile: Profile) -> Result<Profile, String> {
    let mut banks = Vec::new();
    for bank in profile.banks.iter().cloned() {
        banks.extend(banks_of(bank)?);
    }
    Ok(Profile { banks, ..profile })
}
