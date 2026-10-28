# Rust rewrite (xgodev/mackie-control#1)

Asked 2026-10-27: the bridge moves from Python to Rust, and the repo from
`jpfaria/mackie-control` to `xgodev/mackie-control` (done the same day, by
transfer, so history and the old URL survive).

## What stays the same

- The YAML profile format, word for word: every profile that runs on the
  Python bridge runs on the Rust one.
- The state file (`$XDG_STATE_HOME/mackie-control/<profile>.json`).
- The commands: `mackie bridge|devices|surfaces|ports|watch|service`.
- The LaunchAgent label and log file, so `service install` replaces the
  Python agent in place.
- Every behaviour the Python tests pin down. The tests are ported, not
  rewritten from memory.

## Layout

    crates/quantum-hd8     UCNet: framing and the ucdaemon client, the part
                           of jpfaria/quantum-hd8 the bridge uses (connect,
                           session discovery and cache, read, write with echo,
                           scenes, scene load keeping preamp gains).
    crates/mackie-control  protocol, surfaces, profile, state, drivers,
                           bridge, run loop, service, CLI (binary `mackie`).

The MK-300 and Ampero II drivers use `xgodev/pedal-drivers` (`mvave-mk300`,
`hotone-ampero2`) instead of the Python `mvave` / `ampero2` libraries.

## Threads

The MIDI callback only decodes and queues; it never touches a driver.

One **bridge thread** owns the `Bridge` and every driver: it handles the
queued surface events, applies the last position of each fader every 30 ms,
refreshes a live bank once a second, and plays the bank/scene flashes as
timed steps instead of sleeping in a thread of their own.

The **transport thread** asks "is it playing?" once a second with drivers of
its own, so a 117 ms AppleScript never delays a button or a fader.

## Cut-over

The Python package stays on `main` and installed on João's machine until the
Rust bridge does everything it does. Then `mackie service install` from the
Rust binary replaces the LaunchAgent, and `mackie/`, `tests/*.py` and
`pyproject.toml` leave the repo.
