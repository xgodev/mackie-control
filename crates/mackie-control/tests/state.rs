//! Where the rig was survives a restart, and remembering it changes nothing on the gear.

mod common;

use std::path::Path;

use common::*;
use mackie_control::bridge::Bridge;
use mackie_control::state::State;

fn at(dir: &Path, yaml: &str) -> (Bridge, Fake) {
    let fake = Fake::new();
    let (b, _) = with_state(yaml, &fake, Some(State::new(dir.join("state.json"))));
    (b, fake)
}

#[test]
fn the_device_and_scene_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut b, _) = at(dir.path(), SCENES_PROFILE);
    b.step_bank(1); // device 2
    b.step_bank(-1); // back to device 1
    b.step_scene(1); // scene 1 there
    b.step_scene(1); // scene 2
    b.step_bank(1); // end on device 2

    let (mut novo, _) = at(dir.path(), SCENES_PROFILE);
    novo.restore();
    assert_eq!(novo.bank_index, 1);
    novo.step_bank(-1);
    assert_eq!(novo.scene_index(), Some(1)); // device 1 remembered its scene 2
}

#[test]
fn restoring_reloads_nothing() {
    // Remembering where the rig was must not change the rig: no scene is loaded on start, the position is
    // only the starting point of the arrows.
    let dir = tempfile::tempdir().unwrap();
    let (mut b, _) = at(dir.path(), SCENES_PROFILE);
    b.step_scene(1);
    let (mut novo, fake) = at(dir.path(), SCENES_PROFILE);
    novo.restore();
    assert_eq!(fake.get().loaded, None);
}

#[test]
fn scenes_are_remembered_by_bank_name_not_position() {
    // Adding a bank at the top of the YAML must not hand device 1's scene to whatever is now first.
    let dir = tempfile::tempdir().unwrap();
    let (mut b, _) = at(dir.path(), SCENES_PROFILE);
    b.step_scene(1);
    b.step_scene(1); // "HD 8" on scene 2
    let novo_raw = SCENES_PROFILE.replace("banks:\n", "banks:\n  - {name: NEW, faders: {}}\n");
    let (mut novo, _) = at(dir.path(), &novo_raw);
    novo.restore();
    assert_eq!(novo.profile.banks[novo.bank_index].name, "HD 8");
    assert_eq!(novo.scene_index(), Some(1));
}

#[test]
fn a_missing_or_broken_state_file_is_a_fresh_start() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), "{ not json").unwrap();
    let (mut b, _) = at(dir.path(), SCENES_PROFILE);
    b.restore();
    assert!(b.bank_index == 0 && b.scene_index().is_none());
}

#[test]
fn the_file_is_plain_json_a_person_can_read() {
    let dir = tempfile::tempdir().unwrap();
    let (mut b, _) = at(dir.path(), SCENES_PROFILE);
    b.step_scene(1);
    let text = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
    let dados: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(dados["device"], "HD 8");
    assert_eq!(dados["scenes"], serde_json::json!({"HD 8": 0}));
}
