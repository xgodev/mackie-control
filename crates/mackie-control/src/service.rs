//! Keep the bridge running as a macOS LaunchAgent.
//!
//! Run from a terminal, the bridge dies with that terminal: on 2026-09-23 the laptop was moved, the
//! terminal went away, and the surface came back over Bluetooth to a bridge that no longer existed. The
//! bridge already survives the surface going away (`run`); this makes it survive everything else -- it
//! starts at login and launchd starts it again if it exits.
//!
//! The label and the log are the Python agent's, so installing replaces it in place.

use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.github.jpfaria.mackie-control";

/// What a launchctl call answered.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
}

pub type Launchctl<'a> = &'a mut dyn FnMut(&[&str]) -> Ran;

/// The real launchctl.
pub fn launchctl(args: &[&str]) -> Ran {
    match std::process::Command::new("launchctl").args(args).output() {
        Ok(o) => Ran { code: o.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&o.stdout).into() },
        Err(e) => Ran { code: -1, stdout: e.to_string() },
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn agent_path(home: &Path) -> PathBuf {
    home.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn log_path(home: &Path) -> PathBuf {
    home.join("Library").join("Logs").join("mackie-control.log")
}

extern "C" {
    fn getuid() -> u32;
}

fn domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { getuid() })
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

/// The agent runs this very binary, not a `mackie` found on PATH: launchd's PATH is not the shell's.
pub fn plist(profile: &Path, binary: &Path, home: &Path, path: &str) -> plist::Value {
    let log = plist::Value::String(log_path(home).display().to_string());
    let mut env = plist::Dictionary::new();
    env.insert("PATH".into(), path.into());
    let mut d = plist::Dictionary::new();
    d.insert("Label".into(), LABEL.into());
    d.insert(
        "ProgramArguments".into(),
        plist::Value::Array(vec![
            binary.display().to_string().into(),
            "bridge".into(),
            profile.display().to_string().into(),
        ]),
    );
    d.insert("RunAtLoad".into(), true.into());
    d.insert("KeepAlive".into(), true.into());
    d.insert("ThrottleInterval".into(), 5.into());
    d.insert("ProcessType".into(), "Interactive".into());
    d.insert("StandardOutPath".into(), log.clone());
    d.insert("StandardErrorPath".into(), log);
    d.insert("EnvironmentVariables".into(), plist::Value::Dictionary(env));
    plist::Value::Dictionary(d)
}

pub fn install(profile: &str, run: Launchctl, home: &Path, binary: &Path) -> Result<PathBuf, String> {
    let profile = crate::profile::expand_user(profile);
    if !profile.is_file() {
        return Err(format!("no profile at {}", profile.display()));
    }
    let profile = profile.canonicalize().map_err(|e| e.to_string())?;
    let dest = agent_path(home);
    let io = |e: std::io::Error| e.to_string();
    std::fs::create_dir_all(dest.parent().expect("has a parent")).map_err(io)?;
    std::fs::create_dir_all(log_path(home).parent().expect("has a parent")).map_err(io)?;
    // Replacing a loaded agent needs it out first, or launchd keeps the old one.
    run(&["bootout", &service()]);
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    plist(&profile, binary, home, &path).to_file_xml(&dest).map_err(|e| e.to_string())?;
    let r = run(&["bootstrap", &domain(), &dest.display().to_string()]);
    if r.code != 0 {
        return Err(format!("launchctl bootstrap failed ({}): {}", r.code, r.stdout.trim()));
    }
    Ok(dest)
}

pub fn uninstall(run: Launchctl, home: &Path) -> PathBuf {
    run(&["bootout", &service()]);
    let dest = agent_path(home);
    let _ = std::fs::remove_file(&dest);
    dest
}

/// Pick up a freshly installed version: kill and start again at once.
pub fn restart(run: Launchctl) -> Result<(), String> {
    let r = run(&["kickstart", "-k", &service()]);
    if r.code != 0 {
        return Err(format!("launchctl kickstart failed ({}): {}", r.code, r.stdout.trim()));
    }
    Ok(())
}

pub fn status(run: Launchctl, home: &Path) -> String {
    if !agent_path(home).exists() {
        return "not installed".into();
    }
    let r = run(&["print", &service()]);
    if r.code != 0 {
        return "installed, not loaded".into();
    }
    r.stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("pid ="))
        .map_or_else(|| "installed, not running".into(), |l| format!("running ({l})"))
}
