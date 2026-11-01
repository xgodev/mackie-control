//! A running bridge must not create a process per script: each `osascript` spends one LaunchServices serial,
//! and at ~54 a minute the 32-bit counter ran out after 21 days, leaving every new program on the Mac stuck
//! at launch until a reboot (2026-11-01, xgodev/mackie-control#2).

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use mackie_control::drivers::mac::ScriptHost;

/// A host that logs each start, then runs `body`.
fn fake(log: &Path, body: &str, respawn_after: Duration) -> ScriptHost {
    let script = format!("echo start >> '{}'; {body}", log.display());
    ScriptHost::with(
        Box::new(move || {
            let mut c = Command::new("sh");
            c.args(["-c", &script]);
            c
        }),
        respawn_after,
    )
}

fn starts(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |s| s.lines().count())
}

const ANSWERS: &str = r#"while read -r line; do echo '{"ok":"42"}'; done"#;
const REFUSES: &str = r#"while read -r line; do echo '{"err":"nope"}'; done"#;

#[test]
fn many_scripts_start_one_process() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let host = fake(&log, ANSWERS, Duration::ZERO);

    for _ in 0..5 {
        assert_eq!(host.run("anything"), Ok("42".to_string()));
    }

    assert_eq!(starts(&log), 1);
}

#[test]
fn nothing_starts_before_the_first_script() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let _host = fake(&log, ANSWERS, Duration::ZERO);

    assert_eq!(starts(&log), 0);
}

#[test]
fn a_refused_script_keeps_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let host = fake(&log, REFUSES, Duration::ZERO);

    assert_eq!(host.run("one"), Err("nope".to_string()));
    assert_eq!(host.run("two"), Err("nope".to_string()));

    assert_eq!(starts(&log), 1);
}

#[test]
fn a_host_that_keeps_dying_does_not_become_a_process_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let host = fake(&log, "exit 1", Duration::from_secs(3600));

    for _ in 0..5 {
        assert!(host.run("anything").is_err());
    }

    assert_eq!(starts(&log), 1);
}

#[test]
fn a_dead_host_is_replaced_once_the_wait_is_over() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let host = fake(&log, "exit 1", Duration::ZERO);

    assert!(host.run("first").is_err());
    assert!(host.run("second").is_err());

    assert_eq!(starts(&log), 2);
}

/// The real host answers exactly what `osascript -e` printed, which is what the drivers parse.
#[cfg(target_os = "macos")]
#[test]
fn the_real_host_answers_like_osascript() {
    let host = ScriptHost::new();

    assert_eq!(host.run("return 42"), Ok("42".to_string()));
    assert_eq!(host.run("return true"), Ok("true".to_string()));
    assert_eq!(host.run("return missing value"), Ok("missing value".to_string()));
    assert_eq!(host.run("if 1 is 1 then\n  return \"olá\"\nend if"), Ok("olá".to_string()));
    assert_eq!(host.run("error \"nope\""), Err("nope".to_string()));
    assert_eq!(host.run("return 7"), Ok("7".to_string()));
}
