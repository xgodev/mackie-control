//! The bridge against a fake driver: takeover, solo, scenes, banks, lamps.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use mackie_control::bridge::{Bridge, BLINKS, STEP};
use mackie_control::profile::parse_str;
use mackie_control::protocol::{self as p, fader_position, Button, Out};

fn rig_bridge() -> (Bridge, Fake) {
    let fake = Fake::new();
    let (b, _) = bridge(&profile(), &fake);
    (b, fake)
}

fn rig() -> (Bridge, Fake) {
    let fake = Fake::new();
    let (b, _) = bridge(SCENES_PROFILE, &fake);
    (b, fake)
}

fn press(b: &mut Bridge, note: u8) {
    b.button(Button { note, pressed: true });
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_fader_far_from_the_value_writes_nothing() {
    let (mut b, fake) = rig_bridge();
    b.fader(0, 0.9); // main sits at 0.5
    b.drain();
    assert_eq!(fake.value("main"), 0.5);
}

#[test]
fn a_fader_takes_over_when_it_crosses_the_value() {
    let (mut b, fake) = rig_bridge();
    b.fader(0, 0.9);
    b.fader(0, 0.1); // crossed 0.5
    b.drain();
    assert_eq!(fake.value("main"), 0.1);
}

#[test]
fn a_fader_already_near_takes_over_at_once() {
    let (mut b, fake) = rig_bridge();
    b.fader(0, 0.51);
    b.drain();
    assert_eq!(fake.value("main"), 0.51);
}

#[test]
fn drain_applies_only_the_last_position() {
    let (mut b, fake) = rig_bridge();
    b.fader(0, 0.5);
    for v in [0.6, 0.7, 0.8] {
        b.fader(0, v);
    }
    b.drain();
    assert_eq!(fake.value("main"), 0.8);
}

#[test]
fn solo_mutes_its_group_and_gives_it_back() {
    let (mut b, fake) = rig_bridge();
    b.solo(5);
    assert_eq!((fake.value("g1/mute"), fake.value("g2/mute")), (0.0, 1.0));
    assert_eq!(fake.value("main/mute"), 0.0); // outputs stay out of an input solo
    b.solo(5);
    assert_eq!(fake.value("g2/mute"), 0.0);
}

#[test]
fn solo_on_an_output_zeroes_whoever_has_no_mute() {
    let (mut b, fake) = rig_bridge();
    b.solo(1);
    assert_eq!(fake.value("phones"), 0.0);
    b.solo(1);
    assert_eq!(fake.value("phones"), 0.4); // handed back as it was
}

#[test]
fn mute_without_a_parameter_zeroes_and_gives_back() {
    let (mut b, fake) = rig_bridge();
    b.mute(2);
    assert_eq!(fake.value("phones"), 0.0);
    b.mute(2);
    assert_eq!(fake.value("phones"), 0.4);
}

#[test]
fn bank_buttons() {
    let (mut b, _) = rig_bridge();
    b.on_midi(&note_on(p::BANK_RIGHT));
    assert_eq!(b.base().name, "mac");
    // Since 2026-09-22 the side arrows are scenes; the device moves on the vertical pair and on Channel.
    b.on_midi(&note_on(p::ARROW_UP));
    assert_eq!(b.base().name, "rig");
    b.on_midi(&note_on(p::SELECT + 1));
    assert_eq!(b.base().name, "rig"); // square does nothing without select: bank
}

#[test]
fn square_jumps_to_a_bank_only_when_the_profile_asks() {
    let (mut b, _) = bridge(&banks(", buttons: {select: bank}"), &Fake::new());
    b.on_midi(&note_on(p::SELECT + 1));
    assert_eq!(b.base().name, "mac");
}

#[test]
fn changing_bank_arms_takeover_again() {
    let (mut b, fake) = rig_bridge();
    b.fader(0, 0.51);
    b.drain();
    b.step_bank(1);
    b.step_bank(-1);
    b.fader(0, 0.95);
    b.drain();
    assert_eq!(fake.value("main"), 0.51);
}

#[test]
fn rec_loads_a_scene_and_marks_it() {
    let (mut b, fake) = rig_bridge();
    b.on_midi(&note_on(p::REC + 1));
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO"));
    assert_eq!(b.scene_loaded, Some(2));
}

#[test]
fn a_missing_scene_does_not_bring_it_down() {
    let (mut b, _) = rig_bridge();
    b.on_midi(&note_on(p::REC + 5));
    assert_eq!(b.scene_loaded, None);
}

#[test]
fn a_fader_with_no_destination_is_ignored() {
    let (mut b, _) = rig_bridge();
    b.fader(7, 0.5);
    b.drain(); // must not panic
}

#[test]
fn a_refused_write_does_not_abort_the_solo() {
    let fake = Fake::new();
    fake.get().refuse_toggle = Some("g2/mute".into());
    let (mut b, _) = bridge(&profile(), &fake);
    b.solo(5);
    assert_eq!(b.soloed, Some(5)); // carried on despite the failure
}

#[test]
fn push_state_sends_positions_and_leds() {
    let (mut b, _) = rig_bridge();
    b.push_state();
    let out = b.take_out();
    assert!(!bends(&out).is_empty());
    assert_eq!(led(&out, p::SELECT).first(), Some(&false)); // no job, no lamp
}

#[test]
fn the_square_lamp_only_lights_when_it_selects_banks() {
    let (mut b, _) = bridge(&banks(", buttons: {select: bank}"), &Fake::new());
    b.push_state();
    assert_eq!(led(&b.take_out(), p::SELECT).first(), Some(&true));
}

const TRANSPORT: &str = "banks: [{name: rig, faders: {}, transport: {
  play: {driver: fake, target: Spotify, command: playpause},
  forward: {driver: fake, target: Spotify, command: next track}}}]";

fn commandable() -> Fake {
    let fake = Fake::new();
    fake.get().commands = true;
    fake
}

fn ran(fake: &Fake) -> Vec<(String, String)> {
    fake.get().ran.clone()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.into(), b.into())
}

#[test]
fn transport_buttons_run_the_profiles_command() {
    let fake = commandable();
    let (mut b, _) = bridge(TRANSPORT, &fake);
    b.on_midi(&note_on(p::PLAY));
    b.on_midi(&note_on(p::FORWARD));
    assert_eq!(ran(&fake), vec![pair("Spotify", "playpause"), pair("Spotify", "next track")]);
}

#[test]
fn a_transport_button_with_no_entry_is_ignored() {
    let (mut b, _) = rig_bridge();
    b.on_midi(&note_on(p::PLAY)); // must not panic
}

#[test]
fn a_driver_that_cannot_run_the_command_only_warns() {
    let (mut b, lines) = bridge(TRANSPORT, &Fake::new());
    b.on_midi(&note_on(p::PLAY));
    assert!(logged(&lines).iter().any(|l| l.contains("play")));
}

#[test]
fn push_state_does_not_send_positions_for_unmapped_faders() {
    let (mut b, _) = rig_bridge();
    b.push_state();
    let mut channels: Vec<u8> = bends(&b.take_out()).iter().map(|(c, _)| *c).collect();
    channels.sort();
    channels.dedup();
    assert_eq!(channels, vec![0, 1, 4, 5]); // the profile's four faders, only those
}

#[test]
fn push_state_skips_a_destination_it_cannot_read() {
    let fake = Fake::new();
    fake.get().blind = true;
    let (mut b, _) = bridge(&profile(), &fake);
    b.push_state();
    assert!(bends(&b.take_out()).is_empty());
}

const SIMPLE_BANK: &str = "banks: [{name: app, faders: {1: {driver: fake, target: Spotify, label: SPOTIFY}}}]";

fn with_buttons() -> Fake {
    let fake = commandable();
    fake.get().buttons = &[("play", "playpause"), ("rec", "scene")];
    fake
}

#[test]
fn driver_declares_its_own_buttons() {
    let fake = with_buttons();
    let (mut b, _) = bridge(SIMPLE_BANK, &fake);
    b.on_midi(&note_on(p::PLAY));
    assert_eq!(ran(&fake), vec![pair("Spotify", "playpause")]);
}

#[test]
fn a_driver_button_of_kind_scene_loads_a_scene() {
    let fake = with_buttons();
    let (mut b, _) = bridge(SIMPLE_BANK, &fake);
    b.on_midi(&note_on(p::REC));
    assert_eq!(fake.get().loaded.as_deref(), Some("ONE"));
}

#[test]
fn the_profile_wins_over_the_driver_default() {
    let fake = with_buttons();
    let yaml = "banks: [{name: app, faders: {1: {driver: fake, target: Spotify, label: SPOTIFY}},
      transport: {play: {driver: fake, target: Outro, command: pause}}}]";
    let (mut b, _) = bridge(yaml, &fake);
    b.on_midi(&note_on(p::PLAY));
    assert_eq!(ran(&fake), vec![pair("Outro", "pause")]);
}

#[test]
fn a_driver_without_that_button_does_nothing() {
    let fake = commandable();
    let (mut b, _) = bridge(&profile(), &fake);
    b.on_midi(&note_on(p::PLAY)); // the fake's buttons are only rec
    assert!(ran(&fake).is_empty());
}

fn shared_clock() -> (Arc<Mutex<f64>>, Box<dyn Fn() -> f64 + Send>) {
    let now = Arc::new(Mutex::new(0.0));
    let n = now.clone();
    (now, Box::new(move || *n.lock().unwrap()))
}

#[test]
fn bank_change_blinks_row_and_column_three_times() {
    let (now, clock) = shared_clock();
    let (b, _) = bridge(&many(20), &Fake::new());
    let mut b = b.with_clock(clock);
    b.bank_index = 11; // row 1, column 3
    b.flash_bank();
    for t in 1..=10 {
        *now.lock().unwrap() = f64::from(t) * 0.1;
        b.tick();
    }
    assert_eq!(lit(&b.take_out(), p::REC), vec![3; BLINKS as usize]);
}

#[test]
fn the_blink_touches_only_its_own_cell() {
    let (mut b, _) = bridge(&many(20), &Fake::new());
    b.bank_index = 11;
    b.flash_number_now(b.bank_index, p::REC);
    let blink: Vec<u8> = b
        .take_out()
        .iter()
        .filter_map(|o| match *o {
            Out::Led { note, .. } => Some(note),
            _ => None,
        })
        .take(2 * BLINKS as usize)
        .collect();
    assert!(blink.iter().all(|&n| n == p::REC + 3)); // only its column
}

#[test]
fn beyond_64_banks_there_is_nothing_to_flash() {
    let (mut b, _) = bridge(&many(70), &Fake::new());
    b.bank_index = 64;
    b.flash_number_now(b.bank_index, p::REC);
    assert!(b.take_out().is_empty());
}

#[test]
fn the_flash_puts_the_real_leds_back_on_its_own() {
    let (mut b, _) = rig_bridge();
    b.flash_number_now(b.bank_index, p::REC);
    // push_state runs after the flash: the last LED messages are the real state, not the grid.
    let out = b.take_out();
    assert_eq!(led(&out, p::MUTE).last(), Some(&false));
    assert_eq!(led(&out, p::SELECT).last(), Some(&false));
}

fn global() -> String {
    format!(
        "{}\nglobal: {{faders: {{8: {{driver: fake, target: g2, label: SEMPRE}}}}, \
         transport: {{play: {{driver: fake, target: Spotify, command: playpause}}}}}}",
        banks("")
    )
}

#[test]
fn a_global_fader_works_in_every_bank() {
    let fake = commandable();
    let (mut b, _) = bridge(&global(), &fake);
    for _ in 0..2 {
        b.fader(7, 0.8); // g2 is at 0.8: takes over at once
        b.fader(7, 0.3);
        b.drain();
        assert_eq!(fake.value("g2"), 0.3);
        fake.set("g2", 0.8);
        b.step_bank(1);
    }
}

#[test]
fn global_transport_works_in_every_bank() {
    let fake = commandable();
    let (mut b, _) = bridge(&global(), &fake);
    b.on_midi(&note_on(p::PLAY));
    b.step_bank(1);
    b.on_midi(&note_on(p::PLAY));
    assert_eq!(ran(&fake), vec![pair("Spotify", "playpause"); 2]);
}

#[test]
fn a_global_fader_wins_over_the_banks() {
    let yaml = "banks: [{name: x, faders: {8: {driver: fake, target: main, label: FROM THE BANK}}}]
global: {faders: {8: {driver: fake, target: g2, label: GLOBAL}}}";
    let (b, _) = bridge(yaml, &commandable());
    assert_eq!(b.destination(8).unwrap().label, "GLOBAL");
}

fn encoders() -> String {
    format!("{}\nglobal: {{encoders: {{1: {{driver: fake, target: g2, label: SPOTIFY}}}}}}", banks(""))
}

#[test]
fn an_encoder_nudges_its_destination_up_and_down() {
    let fake = Fake::new();
    let (mut b, _) = bridge(&encoders(), &fake);
    let before = fake.value("g2");
    b.on_midi(&[0xB0, p::VPOT, 1]);
    b.drain();
    assert!(approx(fake.value("g2"), before + STEP));
    b.on_midi(&[0xB0, p::VPOT, 0x41]);
    b.drain();
    assert!(approx(fake.value("g2"), before));
}

#[test]
fn an_encoder_never_leaves_the_range() {
    let fake = Fake::new();
    fake.set("g2", 0.99);
    let (mut b, _) = bridge(&encoders(), &fake);
    for _ in 0..10 {
        b.on_midi(&[0xB0, p::VPOT, 1]);
        b.drain();
    }
    assert_eq!(fake.value("g2"), 1.0);
}

#[test]
fn a_global_encoder_works_in_every_bank() {
    let fake = Fake::new();
    let (mut b, _) = bridge(&encoders(), &fake);
    b.step_bank(1);
    let before = fake.value("g2");
    b.on_midi(&[0xB0, p::VPOT, 1]);
    b.drain();
    assert!(fake.value("g2") > before);
}

#[test]
fn an_unmapped_encoder_is_ignored() {
    let (mut b, _) = rig_bridge();
    b.on_midi(&[0xB0, p::VPOT + 3, 1]); // must not panic
    b.drain();
}

#[test]
fn a_destination_can_waive_takeover() {
    let fake = Fake::new();
    let yaml = "banks: [{name: app, faders: {1: {driver: fake, target: main, label: SPOTIFY, takeover: false}}}]";
    let (mut b, _) = bridge(yaml, &fake);
    b.fader(0, 0.05); // main is at 0.5: no takeover, writes at once
    b.drain();
    assert_eq!(fake.value("main"), 0.05);
}

const RANGE: &str =
    "banks: [{name: r, faders: {1: {driver: fake, target: main, label: GAIN, range: [0.2, 0.6], takeover: false}}}]";

#[test]
fn a_range_keeps_the_whole_travel_inside_it() {
    let fake = Fake::new();
    let (mut b, _) = bridge(RANGE, &fake);
    b.fader(0, 1.0);
    b.drain();
    assert!(approx(fake.value("main"), 0.6)); // top of the fader = top of the range
    b.fader(0, 0.0);
    b.drain();
    assert!(approx(fake.value("main"), 0.2));
    b.fader(0, 0.5);
    b.drain();
    assert!(approx(fake.value("main"), 0.4));
}

#[test]
fn a_ranged_value_is_read_back_on_the_faders_scale() {
    let fake = Fake::new();
    fake.set("main", 0.4);
    let (mut b, _) = bridge(&format!("positions: true\n{RANGE}"), &fake);
    b.push_state();
    let ch0: Vec<u16> = bends(&b.take_out()).into_iter().filter(|(c, _)| *c == 0).map(|(_, r)| r).collect();
    assert_eq!(Out::Position { channel: 0, raw: ch0[0] }, fader_position(0, 0.5));
}

#[test]
fn a_bad_range_is_refused() {
    for bad in ["[0.5, 0.5]", "[0.8, 0.2]", "[-1, 0.5]", "[a, b]"] {
        let yaml = format!("banks: [{{faders: {{1: {{driver: fake, target: x, range: {bad}}}}}}}]");
        assert!(parse_str(&yaml).is_err(), "{bad} was accepted");
    }
}

// -- devices and scenes on the surface (spec 2026-09-22) ------------------------------------------------

#[test]
fn the_right_arrow_loads_the_next_scene() {
    let (mut b, fake) = rig();
    press(&mut b, p::ARROW_RIGHT);
    assert_eq!(fake.get().loaded.as_deref(), Some("ONE"));
    press(&mut b, p::ARROW_RIGHT);
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO"));
}

#[test]
fn the_left_arrow_loads_the_previous_scene() {
    let (mut b, fake) = rig();
    press(&mut b, p::ARROW_RIGHT);
    press(&mut b, p::ARROW_RIGHT);
    press(&mut b, p::ARROW_LEFT);
    assert_eq!(fake.get().loaded.as_deref(), Some("ONE"));
}

#[test]
fn the_scenes_do_not_wrap_around() {
    let (mut b, fake) = rig();
    for _ in 0..5 {
        press(&mut b, p::ARROW_RIGHT);
    }
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO")); // the driver has two, and it stops there
    for _ in 0..5 {
        press(&mut b, p::ARROW_LEFT);
    }
    assert_eq!(fake.get().loaded.as_deref(), Some("ONE"));
}

#[test]
fn the_up_and_down_arrows_change_device() {
    let (mut b, _) = rig();
    press(&mut b, p::ARROW_DOWN);
    assert_eq!(b.bank_index, 1);
    press(&mut b, p::ARROW_UP);
    assert_eq!(b.bank_index, 0);
}

#[test]
fn the_devices_do_not_wrap_around() {
    let (mut b, _) = rig();
    for _ in 0..5 {
        press(&mut b, p::ARROW_UP);
    }
    assert_eq!(b.bank_index, 0);
    for _ in 0..5 {
        press(&mut b, p::ARROW_DOWN);
    }
    assert_eq!(b.bank_index, 2);
}

#[test]
fn a_device_with_no_scenes_ignores_the_side_arrows() {
    let (mut b, _) = bridge("banks: [{name: empty, faders: {}}]", &Fake::new());
    press(&mut b, p::ARROW_RIGHT);
    assert_eq!(b.scene_index(), None);
}

#[test]
fn the_profile_chooses_which_scenes_and_in_which_order() {
    let fake = Fake::new();
    let yaml = "banks: [{name: HD 8, driver: fake, scenes: [TWO], faders: {1: {driver: fake, target: main}}}]";
    let (mut b, _) = bridge(yaml, &fake);
    assert_eq!(b.scene_names(), vec!["TWO".to_string()]);
    press(&mut b, p::ARROW_RIGHT);
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO"));
}

#[test]
fn changing_device_shows_it_under_the_knobs_and_on_the_square() {
    let (mut b, _) = bridge(&many(20), &Fake::new());
    b.bank_index = 11; // row 1, column 3
    b.flash_number_now(b.bank_index, p::REC);
    assert_eq!(lit(&b.take_out(), p::REC), vec![3; BLINKS as usize]);
}

#[test]
fn changing_scene_shows_it_under_the_knobs_and_on_the_s_row() {
    let (mut b, _) = rig_bridge();
    b.last_seen.insert((0, 1), 0.4); // fader 2 = row 1, position known
    b.flash_number_now(9, p::SOLO); // row 1, column 1
    let out = b.take_out();
    assert_eq!(bends(&out)[0].0, 1);
    assert_eq!(lit(&out, p::SOLO), vec![1; BLINKS as usize]);
    // and hands the gear's value back, or that knob blinks for ever
    let last = bends(&out).into_iter().rfind(|(c, _)| *c == 1).unwrap();
    assert_eq!(Out::Position { channel: last.0, raw: last.1 }, fader_position(1, 0.4));
}

#[test]
fn the_r_button_and_the_arrows_page_the_same_list() {
    // R n loaded the n-th scene of the device while the arrows paged the profile's list, so the two
    // disagreed on any bank with `scenes:`.
    let fake = Fake::new();
    let yaml = "banks: [{name: HD 8, driver: fake, scenes: [TWO, ONE],
      faders: {1: {driver: fake, target: main}, 2: {driver: fake, target: phones}}}]";
    let (mut b, _) = bridge(yaml, &fake);
    b.scene(1);
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO")); // the first of the profile's list
    b.scene(2);
    assert_eq!(fake.get().loaded.as_deref(), Some("ONE"));
    assert_eq!(b.scene_index(), Some(1)); // the arrows now carry on from here
}

#[test]
fn the_knob_lamp_is_handed_back_where_the_fader_really_is() {
    // The surface compares against the physical fader, so only the position that fader itself reported
    // stops the blink. Sending the gear's value left it blinking for ever (measured 2026-09-22).
    let (mut b, fake) = rig_bridge();
    b.last_seen.insert((0, 1), 0.4); // where fader 2 physically sits
    fake.set("phones", 0.9); // the gear disagrees; irrelevant
    b.flash_number_now(9, p::SOLO);
    let ch1: Vec<Out> = bends(&b.take_out())
        .into_iter()
        .filter(|(c, _)| *c == 1)
        .map(|(channel, raw)| Out::Position { channel, raw })
        .collect();
    assert_eq!(ch1[0], fader_position(1, 0.0)); // lights the row
    assert_eq!(ch1[1], fader_position(1, 0.4)); // and hands the fader back
}

#[test]
fn a_row_is_shown_even_before_that_fader_has_been_touched() {
    // It blinks until someone touches that fader, and a blinking lamp on the right channel still says which
    // row you are on. Showing nothing did not.
    let (mut b, _) = bridge(&many(20), &Fake::new());
    b.bank_index = 11; // row 1
    b.flash_number_now(b.bank_index, p::REC);
    assert_eq!(bends(&b.take_out()).first().map(|(c, _)| *c), Some(1));
}

// -- encoders do not block the MIDI thread (2026-09-22) -------------------------------------------------

const ONE_ENCODER: &str =
    "banks: [{name: rig, faders: {}, encoders: {1: {driver: fake, target: main, label: SPOTIFY}}}]";

fn with_encoder() -> (Bridge, Fake) {
    let fake = Fake::new();
    let (b, _) = bridge(ONE_ENCODER, &fake);
    (b, fake)
}

#[test]
fn an_encoder_detent_writes_nothing_until_the_drain() {
    // A write can take a tenth of a second (AppleScript to Spotify: 117 ms measured). Doing it on the MIDI
    // thread backs up every other message.
    let (mut b, fake) = with_encoder();
    b.encoder(0, 1);
    assert_eq!(fake.value("main"), 0.5); // untouched so far
    b.drain();
    assert!(fake.value("main") > 0.5);
}

#[test]
fn detents_are_added_up_and_applied_once() {
    let (mut b, fake) = with_encoder();
    for _ in 0..4 {
        b.encoder(0, 1);
    }
    b.drain();
    assert!(approx(fake.value("main"), 0.5 + 4.0 * STEP));
}

#[test]
fn a_detent_the_other_way_cancels_one() {
    let (mut b, fake) = with_encoder();
    b.encoder(0, 3);
    b.encoder(0, -1);
    b.drain();
    assert!(approx(fake.value("main"), 0.5 + 2.0 * STEP));
}

#[test]
fn push_state_reads_nothing_when_positions_are_off() {
    // Reading a fader costs a round trip -- 117 ms to Spotify through AppleScript -- and the value is only
    // used to send a position back. With positions off, reading all eight on every bank change blocked the
    // surface for the best part of a second (2026-09-22).
    let fake = Fake::new();
    let (mut b, _) = bridge(&banks(""), &fake);
    b.push_state();
    assert!(fake.get().reads.iter().all(|t| t.ends_with("/mute")));
}

// -- transport lamps follow the player (2026-09-22) -----------------------------------------------------

const TRANSPORT_GLOBAL: &str = "banks: [{name: rig, faders: {}}]
global: {transport: {play: {driver: fake, target: Spotify, command: playpause}}}";

fn with_player() -> (Bridge, Fake) {
    let fake = Fake::new();
    fake.get().buttons = &[("play", "playpause")];
    let (b, _) = bridge(TRANSPORT_GLOBAL, &fake);
    (b, fake)
}

#[test]
fn the_play_lamp_is_dark_while_the_app_is_closed() {
    let (mut b, _) = with_player();
    b.push_transport();
    assert_eq!(led(&b.take_out(), p::PLAY), vec![false]);
}

#[test]
fn the_play_lamp_goes_dark_when_the_app_is_closed() {
    let (mut b, fake) = with_player();
    fake.get().playing = Some(true);
    b.push_transport();
    assert_eq!(led(&b.take_out(), p::PLAY), vec![true]);
    fake.get().playing = None; // the app was closed
    b.push_transport();
    assert_eq!(led(&b.take_out(), p::PLAY), vec![false]);
}

#[test]
fn the_player_is_asked_only_when_something_is_bound_to_it() {
    // No transport in the profile, no round trip to any app.
    let (mut b, _) = rig_bridge();
    b.push_transport();
    assert!(b.take_out().is_empty());
}

#[test]
fn every_bound_transport_button_lights_while_the_app_is_open() {
    let (mut b, fake) = with_player();
    fake.get().playing = Some(false); // open, paused
    b.push_transport();
    assert_eq!(led(&b.take_out(), p::PLAY), vec![true]);
}

#[test]
fn nothing_is_sent_to_lamps_the_surface_does_not_have() {
    // Measured 2026-09-22, with the bridge stopped: notes 60-63 (the four arrows) and 2E/2F (Channel) light
    // nothing on the SMC-Mixer. Sending to them is noise on the wire and a lie in the docs.
    let (mut b, _) = rig();
    b.push_state();
    let dead = [p::ARROW_UP, p::ARROW_DOWN, p::ARROW_LEFT, p::ARROW_RIGHT, p::BANK_LEFT, p::BANK_RIGHT];
    assert!(b.take_out().iter().all(|o| !matches!(o, Out::Led { note, .. } if dead.contains(note))));
}

#[test]
fn the_player_is_asked_once_per_target_not_once_per_button() {
    // Four buttons bound to Spotify used to mean four round trips a second, each two AppleScript calls: the
    // drain thread had no time left to write faders (2026-09-22).
    let fake = Fake::new();
    fake.get().playing = Some(true);
    let four: Vec<String> = ["play", "stop", "forward", "rewind"]
        .iter()
        .map(|k| format!("{k}: {{driver: fake, target: Spotify, command: {k}}}"))
        .collect();
    let yaml = format!("banks: [{{name: rig, faders: {{}}}}]\nglobal: {{transport: {{{}}}}}", four.join(", "));
    let (mut b, _) = bridge(&yaml, &fake);
    b.push_transport();
    assert_eq!(fake.get().asked, vec!["Spotify".to_string()]);
}

#[test]
fn pressing_past_the_first_scene_does_not_reload_it() {
    // At the end of the list the arrow does nothing. Reloading the same scene is not harmless: on a pedal
    // it throws away every unsaved tweak (seen on the Ampero 2026-09-22: three presses of left at scene 1,
    // three reloads).
    let (mut b, fake) = rig();
    press(&mut b, p::ARROW_RIGHT); // scene 1
    fake.get().loads.clear();
    press(&mut b, p::ARROW_LEFT);
    press(&mut b, p::ARROW_LEFT);
    assert!(fake.get().loads.is_empty());
}

#[test]
fn pressing_past_the_last_device_does_not_reselect_it() {
    let fake = Fake::new();
    let (mut b, lines) = bridge(SCENES_PROFILE, &fake);
    press(&mut b, p::ARROW_UP); // already on the first
    assert!(!logged(&lines).iter().any(|l| l.contains("bank")));
}

#[test]
fn each_device_remembers_its_own_scene() {
    // Scene 2 on the first device must not make the next device think it is on scene 2 too: the position
    // is per device (2026-09-22).
    let (mut b, _) = rig();
    press(&mut b, p::ARROW_RIGHT);
    press(&mut b, p::ARROW_RIGHT); // device 1, scene 2
    press(&mut b, p::ARROW_DOWN); // device 2
    assert_eq!(b.scene_index(), None);
    press(&mut b, p::ARROW_UP); // back to device 1
    assert_eq!(b.scene_index(), Some(1));
}

#[test]
fn the_first_arrow_starts_from_the_scene_the_device_is_on() {
    // With the scene loaded on the device itself (its own footswitch, its own editor), the first arrow press
    // must go one step from there -- not load the first scene over it (2026-09-22: ELEMENT loaded over the
    // scene João had just picked).
    let (mut b, fake) = rig();
    fake.get().current = Some(0); // the device is on ONE
    press(&mut b, p::ARROW_RIGHT);
    assert_eq!(fake.get().loaded.as_deref(), Some("TWO"));
}

// -- live banks ------------------------------------------------------------------------------------------

const LIVE: &str = "banks: [{name: live, driver: live, live: output}]";

#[test]
fn a_live_bank_takes_its_faders_from_the_driver_while_running() {
    let fake = Fake::new();
    let (mut b, _) = bridge(LIVE, &fake);
    assert!(b.faders().is_empty()); // nothing until the driver is asked
    b.refresh_live();
    let targets: Vec<&str> = b.faders().values().map(|d| d.target()).collect();
    assert_eq!(targets, vec!["main", "phones"]);
    b.fader(1, fake.value("phones"));
    b.drain();
    fake.get().strips = vec!["g1".into()]; // another project opened
    b.refresh_live();
    let now: Vec<(u32, &str)> = b.faders().iter().map(|(n, d)| (*n, d.target())).collect();
    assert_eq!(now, vec![(1, "g1")]);
    assert!(!b.took_over.contains(&(0, 1))); // takeover starts over
}

#[test]
fn a_live_bank_whose_driver_is_down_has_no_faders_and_no_panic() {
    let fake = Fake::new();
    fake.get().live_down = true;
    let (mut b, lines) = bridge(LIVE, &fake);
    b.refresh_live();
    b.refresh_live();
    assert!(b.faders().is_empty());
    assert_eq!(logged(&lines).iter().filter(|l| l.contains("not reachable")).count(), 1);
}

#[test]
fn a_fader_whose_gear_stops_answering_does_not_panic() {
    let fake = Fake::new();
    fake.get().blind = true;
    let (mut b, _) = bridge(&profile(), &fake);
    b.last_seen.insert((0, 0), 0.2);
    b.fader(0, 0.9); // used to raise TypeError
    b.drain();
}
