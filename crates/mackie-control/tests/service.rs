//! The LaunchAgent, against a fake launchctl and a temporary home.

use std::path::Path;

use mackie_control::service::{self, agent_path, Ran};

struct Launchctl {
    calls: Vec<Vec<String>>,
    code: i32,
    stdout: String,
}

impl Launchctl {
    fn new(code: i32, stdout: &str) -> Self {
        Launchctl { calls: Vec::new(), code, stdout: stdout.into() }
    }

    fn run(&mut self) -> impl FnMut(&[&str]) -> Ran + '_ {
        move |args| {
            self.calls.push(args.iter().map(|s| s.to_string()).collect());
            Ran { code: self.code, stdout: self.stdout.clone() }
        }
    }
}

fn plant_agent(home: &Path) {
    let dest = agent_path(home);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(dest, "x").unwrap();
}

#[test]
fn install_writes_an_agent_that_restarts_the_bridge() {
    let home = tempfile::tempdir().unwrap();
    let profile = home.path().join("rig.yaml");
    std::fs::write(&profile, "surface: smc-mixer\n").unwrap();
    let mut l = Launchctl::new(0, "");
    let dest =
        service::install(profile.to_str().unwrap(), &mut l.run(), home.path(), Path::new("/bin/mackie")).unwrap();
    let data = plist::Value::from_file(&dest).unwrap().into_dictionary().unwrap();
    let args: Vec<&str> = data["ProgramArguments"].as_array().unwrap().iter().map(|v| v.as_string().unwrap()).collect();
    let canonical = profile.canonicalize().unwrap();
    assert_eq!(args, ["/bin/mackie", "bridge", canonical.to_str().unwrap()]);
    assert_eq!(data["KeepAlive"].as_boolean(), Some(true));
    assert_eq!(data["RunAtLoad"].as_boolean(), Some(true));
    let log = home.path().join("Library/Logs/mackie-control.log");
    assert_eq!(data["StandardOutPath"].as_string(), log.to_str());
    assert_eq!(l.calls[0][0], "bootout"); // old one out first
    assert_eq!(l.calls[1][0], "bootstrap");
}

#[test]
fn install_refuses_a_missing_profile() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("nope.yaml");
    let mut l = Launchctl::new(0, "");
    assert!(service::install(missing.to_str().unwrap(), &mut l.run(), home.path(), Path::new("/bin/mackie")).is_err());
}

#[test]
fn uninstall_removes_the_agent() {
    let home = tempfile::tempdir().unwrap();
    plant_agent(home.path());
    service::uninstall(&mut Launchctl::new(0, "").run(), home.path());
    assert!(!agent_path(home.path()).exists());
}

#[test]
fn status() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(service::status(&mut Launchctl::new(0, "").run(), home.path()), "not installed");
    plant_agent(home.path());
    assert_eq!(service::status(&mut Launchctl::new(0, "\tpid = 42\n").run(), home.path()), "running (pid = 42)");
    assert_eq!(service::status(&mut Launchctl::new(113, "").run(), home.path()), "installed, not loaded");
}

#[test]
fn restart_kicks_the_running_agent() {
    let mut l = Launchctl::new(0, "");
    service::restart(&mut l.run()).unwrap();
    assert_eq!(l.calls[0][..2], ["kickstart", "-k"]);
}
