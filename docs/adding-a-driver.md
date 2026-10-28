# Adding a driver

A driver is how the bridge talks to one kind of gear. The interface is small
(`crates/mackie-control/src/drivers/mod.rs`); every method but `name` has a
default that returns `Unsupported`:

```rust
fn read(&mut self, target: &str) -> Result<Option<f64>>   // 0..1
fn write(&mut self, target: &str, value: f64) -> Result<()>
fn toggle(&mut self, target: &str) -> Result<bool>        // the new state
fn scenes(&mut self) -> Result<Vec<String>>
fn load_scene(&mut self, index: usize) -> Result<()>
```

A driver also declares **which buttons it knows what to do with**:

```rust
// Hd8: R n loads the n-th scene of the bank's scene list
fn buttons(&self) -> &'static [(&'static str, &'static str)] { &[("rec", "scene")] }

// AppVolume
fn buttons(&self) -> &'static [(&'static str, &'static str)] {
    &[("play", "playpause"), ("stop", "pause"), ("forward", "next track"), ("rewind", "previous track")]
}
```

The bridge uses those whenever the profile says nothing, so a profile never has
to repeat what a driver already knows — and a bank made of gear that has no
transport simply has dead transport buttons. `"scene"` is the one special
value: it means `load_scene()`. Anything else is passed to `command()`.

Rules that came out of real bugs:

- **Return `Unsupported`, never panic.** The bridge logs it and carries on. One
  parameter the device refuses must not abort a whole solo.
- **Say *why* it is unsupported.** `mac` answers "the Mac's default output has no
  system-controlled volume (an audio interface is selected)" instead of a
  stack trace — that message is the diagnosis.
- **Take the connection by injection.** `Hd8::with(client, ...)` is what makes the
  tests run with no hardware; build the real one only when nothing is passed.
- **Clamp what you write.** The bridge sends 0..1; the device decides what that
  means.

Register it in `drivers.build()` and add a test with a fake client, like
`crates/mackie-control/tests/drivers.rs`.

## Banks that cannot be written down (`live_faders`)

Some gear has no fixed channel list: OpenRig's mixer strips change with the
project and the chains that are open, so any bank naming them in the YAML goes
stale the moment another project is loaded. A driver for gear like that
implements

```rust
fn live_faders(&mut self, which: &str) -> Result<BTreeMap<u32, LiveFader>>
```

and its default-bank entry carries `live: <which>` instead of `faders:`
(`openrig` uses `out` and `in`, giving the banks *OpenRig OUT* and
*OpenRig IN*). The daemon then asks the driver on entering the bank and once a
second (`refresh_live`), and a failure is logged, not fatal — the bank just
keeps the faders it had. `describe` prints `live` where a static bank prints a
fader count.

The default is `Unsupported`: a driver whose channels *are* stable should keep
listing them in its default banks, which costs nothing per second.
