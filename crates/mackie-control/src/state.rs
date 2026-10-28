//! Where the rig was when the bridge last stopped.
//!
//! The bridge is restarted all the time, and the HD 8 cannot say which scene it has loaded (measured
//! 2026-09-22), so without this every restart forgot the device on screen and made the first arrow start
//! from scene 1. What is kept is only a *position*: restoring it loads nothing and changes nothing on the
//! gear.
//!
//! Scenes are keyed by bank name, not by position, so adding a bank to the YAML does not hand one device's
//! scene to another. The file is plain JSON.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// One state file per profile, under the user's state directory -- never beside the profile, which lives
/// in a git repo.
pub fn default_path(profile_path: &str) -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local").join("state"));
    let stem = Path::new(profile_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    base.join("mackie-control").join(format!("{stem}.json"))
}

pub struct State {
    pub path: Option<PathBuf>,
}

impl State {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        State { path: Some(path.into()) }
    }

    /// The saved object, or an empty one when the file is missing or broken: a fresh start.
    pub fn load(&self) -> Map<String, Value> {
        let Some(path) = &self.path else { return Map::new() };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub fn save(&self, device: &str, scenes: &BTreeMap<String, usize>) {
        let Some(path) = &self.path else { return };
        // Losing a position must not stop the rig.
        let _ = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("tmp");
            let text = serde_json::to_string_pretty(&json!({"device": device, "scenes": scenes}))?;
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        })();
    }
}
