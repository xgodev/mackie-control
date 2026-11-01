# Bridge: a surface driving other gear

`mackie bridge PROFILE.yaml` reads a control surface (today the SMC-Mixer) and
writes wherever the profile says — the surface never knows what it controls.

```
surface (Mackie)  ->  Bridge  ->  driver  ->  gear
```

## Drivers

| Driver | Talks to |
|---|---|
| `hd8` | PreSonus Quantum HD 8, through the `quantum-hd8` library (UCNet). The HD 8 **does not receive MIDI**: a sweep of CC, pitch bend and notes on both of its ports changed none of its 1419 parameters (2026-09-20). |
| `mac` | macOS output volume. Unsupported when the default output is an audio interface — AppleScript answers `missing value`, because the volume lives in the interface. |
| `app` | One application's own volume (`Spotify`, …). |
| `mk300` | M-VAVE MK-300 over USB, through `mvave`. Volume read back from the edit buffer, so takeover works; its 160 presets are its scenes. |
| `ampero2` | Hotone Ampero II Stage over USB, through `ampero2`. The patch volume is **write-only** — the protocol has no query — so its fader applies at once and the value is forgotten when a patch loads; its 300 patches are its scenes. Measured 2026-09-22: 300 names in 0.9 s (read once), and the fader moves the volume on the pedal. |
| | MK-300, measured the same day: volume reads 0.60, writes in 6 ms and reads back; 160 names in 1 s (read once). |

A driver that cannot do something raises `Unsupported`; the bridge logs it and
carries on. One channel the device refuses must never abort a solo — that bug
cost an evening on 2026-09-21.

### Losing the gear and getting it back

Switching the HD 8 off and on again kills the UCNet session, and the library
does not notice: afterwards every write raises `OSError: Bad file descriptor`
while reads keep answering from a **stale cache**, so the bridge looks alive
and moves nothing (measured 2026-09-22). The `hd8` driver therefore reconnects
**once per operation** and retries it; if `ucdaemon` still refuses, that one
operation becomes `Unsupported` and the bridge carries on. Power-cycling the
interface no longer means restarting the bridge.

### When `ucdaemon` itself restarts

Measured 2026-09-23, after the Mac was moved and the mixer reconnected:

- Symptom: every HD 8 read answers, **no write is confirmed** (the FRFR mute
  looked stuck on). Restarting the bridge does not help; the fault is in
  PreSonus' `ucdaemon`.
- `ucdaemon` runs as root (`/Library/LaunchDaemons/com.presonus.ucdaemon.plist`).
  Restarting it takes `sudo launchctl kickstart -k system/com.presonus.ucdaemon`,
  which only João runs — never ask for or accept his password in chat.
- After that restart the daemon listens on a **different TCP port** (59791 →
  62586) and gives the HD 8 a **different session byte**. `quantum-hd8` had
  both fixed; since then `connect` discovers them (its `tests/test_discover.py`).
  Reinstall `quantum-hd8` and `mackie service restart` to pick it up.
- The restart can also drop the HD 8 into **Mixer Bypass** (`global/mixerMode = 0`).
  There only `global/*` exists: MAIN, PHONES 1 and PHONES 2 still move, but a
  bus fader such as the FRFR (`aux/ch10`, ADAT 11/12) has nothing to write.
  Loading any scene leaves Bypass — but it also changes the whole routing, so
  not without João's go-ahead.
- The bridge's own connection can go stale while one-off connections write
  fine: `mackie service restart` fixes it.

### What the HD 8 accepts, per channel

Measured 2026-09-22 by writing a value, reading it back and restoring it:

- `trim` and `preampgain` exist in the table for every channel but only the
  eight analogue inputs honour them. On the ADAT channels measured (`trim` on
  ch16, 17, 19, 25; `trim` and `preampgain` on ch19, 20) the write gets no echo
  and the value does not change — there only `volume` is accepted.
- `line/chN/volume` is the channel's level **inside the HD 8 mixer**, not what
  reaches the computer: with `line/ch19/volume` at 0.0 (−96 dB) the guitar on
  that channel still arrives in OpenRig. A fader on it moves monitoring only.
- A write is confirmed in 31 ms (median of 10, worst 46), so a fader that feels
  late is waiting on takeover or on another driver, not on the interface.
- When checking a fader value by reading it back, use `quantum-hd8` at
  `bb73f94` or later: before that its human-readable value showed 0 dB as
  −18.1 dB (the write was right, the read was wrong) and made a correct write
  look failed.

## Profile

```yaml
surface: smc-mixer
banks:
  - name: HD 8
    faders:
      1: {driver: hd8, target: global/mainOutVolume, label: MAIN, group: out, mute: global/mute}
      5: {driver: hd8, target: line/ch1/volume, label: GUITAR 1, group: in, mute: line/ch1/mute}
    buttons: {rec: scene, select: bank}
```

- `group` (`in`/`out`) is what keeps **solo** honest: soloing an input mutes the
  other inputs and leaves the outputs alone.
- A fader **without** `mute:` is silenced by zeroing its own value, which is
  remembered and handed back — the HD 8 has no mute for its headphone outputs.
- **Banks always page with Channel ◀/▶ and with the arrows ◀/▶.** That is the
  default and needs no configuration.
- `rec: scene` makes R n load the n-th scene of **the same list the arrows
  page** — the bank's `scenes:` when it has one, the device's own order
  otherwise. You rarely
  need to write it: **each driver declares the buttons it can use** (`hd8` →
  R = scene; `app` → play, stop, next, previous), and the bridge falls back to
  that. The profile only has to speak up when it wants something different.
- `select: bank` is optional: it makes the square button n jump straight to bank
  n. Left out (the default), the square buttons do nothing.

## Global faders and transport

A top-level `global:` block is a bank that is always in reach: its faders and
its transport work whichever bank is selected, and a global fader wins over the
bank's own.

```yaml
global:
  encoders:
    1: {driver: app, target: Spotify, label: Spotify}   # endless knob: no takeover
  transport:
    play: {driver: app, target: Spotify, command: playpause}
```

An **encoder** is an endless knob: it nudges its destination by a step per
detent instead of jumping to a position, so it needs no takeover and never
fights with where the control physically sits.

Controlling the music has nothing to do with which set of faders you are on —
without this, play does nothing while you are looking at the mixer bank.

## A device's own banks

The same gear has the same knobs in every rig, so what a device is worth
belongs to its driver. A bank that names a `driver:` and no faders is replaced
by that driver's own banks:

```yaml
banks:
  - driver: hd8        # becomes "HD 8 IN" and "HD 8 OUT", eight faders each
```

`mk300` and `ampero2` ship one bank each, a volume fader, with the presets or
patches on ◀/▶. Both are USB pedals and are often unplugged: the connection is
opened on first use, and a pedal that is not there becomes `Unsupported` for
that operation and is tried again on the next one.

`hd8` ships two: **IN**, the eight analogue channels with their mutes, and
**OUT**, the main output, the two headphone outs and the five ADAT buses. A
bank that writes its own faders is never touched — the default is a starting
point, not a straitjacket, and a rig that needs four of each writes four.

## Devices and scenes

A bank is a device. The arrows move through both:

| Control | Does |
|---|---|
| ◀ / ▶ (notes `62`/`63`) | previous / next **scene** — loads it on the press |
| ▲ / ▼ (notes `60`/`61`) | previous / next **device** |
| Channel ◀ / ▶ | pages devices too, unchanged |

Neither wraps: one press too many must not put the rig somewhere it was
walking away from. **At the end of a list an arrow does nothing** — it does not
reload the scene it is already on, nor reselect the bank: reloading an Ampero
patch throws away any edit not yet saved (seen 2026-09-22 as three
`scene 1/300` in a row). Each device keeps its own position in its list, so
leaving the Ampero on scene 7 does not make ▶ on the HD 8 jump to a scene 8 it
does not have.

Loading on the press means walking the list **changes the rig at every step**:
each HD 8 scene carries its own sends, so paging past `MAIN-FRFR` on the way to
another scene leaves the FRFR bus at that scene's default (2026-09-22, the
FRFR send went from 0.266 to 0.735 and the rig went quiet). That is the cost of
the choice, not a bug.

```yaml
banks:
  - name: HD 8
    driver: hd8                        # the device this bank is
    scenes: [MIXER-ON, SYN2-FRFR]      # optional: which, and in which order
```

Without `scenes:` the device's own list is used, in the order it reports. With
it, the profile chooses — the gear may hold thirty presets of which four matter
tonight, and that choice belongs to the user, never to the code.

**Where am I**: a number up to 64 is a row and a column. The **lamp under the
knob** is the row — it is the fader-position lamp, so the row is shown by a
pitch bend on that channel, and the flash ends by handing that channel back **the
position that fader itself last reported**, which is the only thing that stops
it: the surface compares against the physical fader, so the gear's own value
leaves it blinking for ever. Until that fader has been touched there is no
such position, so the row is still shown (showing nothing said nothing) and
that knob **keeps blinking until the fader is moved** — which João reported as
"never stops". This indicator is not settled; see [`pending.md`](pending.md). The column says which
number it is: **R for the device** (which resource), **S for the scene**. Only what just changed is
shown — the R row cannot carry two numbers at once — and the real mute and solo
LEDs come straight back after the flash.

`mackie devices PROFILE.yaml` prints the same thing in words, and works with
the gear switched off:

```
1  HD 8     (hd8)   8 faders   scenes: 1 ELEMENT  2 MIXER-ON  3 MK300-FRFR
2  Mac      (mac)   2 faders   no scenes
```

## Nothing slow on the MIDI thread

The thread that reads the surface must never wait on a driver. On 2026-09-22
two things did, and a fader on the Spotify bank felt late by most of a second:

- `push_state` read all eight faders on every bank change even with
  `positions` off, and a read of Spotify's volume through AppleScript costs
  ~117 ms. It now reads only what it is going to send.
- An encoder did a read and a write per detent, inline. Detents are now summed
  and applied once per cycle, on the drain thread.

The same rule gave the transport lamps below their shape. A fader sweep of one
second now lands its last write ~120 ms after the hand stops.

## Transport lamps

A transport button lights while the app it is bound to is **open**, and goes
dark when it is closed: a button that does nothing because Spotify is not
running should not look available. The app is not asked again once closed —
asking a closed app about its player would launch it.

Asking costs a round trip (117 ms to Spotify through AppleScript, measured
2026-09-22), so it happens once a second on the drain thread, never on the MIDI
one — and **once per player, not per button**: four buttons bound to Spotify
used to mean eight AppleScript calls a second, which is a second of work per
second, and the faders waited behind it. A driver says what it knows by implementing `playing(target)`: `True`,
`False`, or `None` for "nothing to ask".

## Remembering where the rig was

The bridge is restarted all the time, and the HD 8 cannot say which scene it
has loaded. So it keeps, per profile, the device on screen and the scene each
device was on, in `~/.local/state/mackie-control/<profile>.json` (or under
`$XDG_STATE_HOME`) — never beside the profile, which lives in a git repo.

On start it goes back to that device and uses those scenes as the starting
point of the arrows. **It loads nothing**: remembering where the rig was must
not change the rig. A pedal that can say which scene it has loaded wins over
the file. Scenes are kept by bank name, so adding a bank to the YAML does not
hand one device's scene to another. A missing or broken file is a fresh start.

## Losing the surface and getting it back

Switching the surface off takes its port out of CoreMIDI **without any error
reaching the reader**: nothing is raised, nothing is logged, and the bridge
stays alive and silent forever (measured 2026-09-22). The loop therefore checks
the port list once a second instead of waiting for an exception, closes the dead
ports, and reopens the surface when it comes back — re-arming takeover and
pushing the LEDs, because the faders may have been moved while it was away.

```
** the surface is back (SINCO SMC-Mixer-Master)
```

The same device is named differently depending on how it is connected —
`SINCO SMC-Mixer-Master` over USB, `SMC-Mixer Bluetooth` over BLE — so a surface
carries several `port_hints`, tried in order.

## Takeover

The surface has no motors and cannot report where its faders are, so the bridge
never applies a fader until it **crosses the current value** (or starts within
2% of it). Without that, touching a fader would jump the volume to wherever the
knob happened to be sitting. Changing banks arms takeover again.

### Range

`range: [low, high]` keeps the whole fader travel inside a stretch of the
parameter. A preamp reaches +75 dB, so a fader at the top with no limit is a
blown take; `range: [0.2, 0.5]` makes the fader sweep only that part, and the
value is read back on the fader's own scale.

A destination can waive takeover with `takeover: false` — right for a player's own
volume, where a jump costs nothing, and wrong for a monitor bus.

## Feedback

On every bank change, scene load, mute and solo the bridge pushes state back:
the LEDs for mute and solo, plus three blinks of the number that just changed —
the knob lamp is the row, R (device) or S (scene) the column, so 8×8 addresses
64 of each (see *Where am I* above).

**Fader positions are not sent by default.** Sending one makes the surface blink
that channel's LED until the physical fader matches, which is a useful
out-of-sync sign and an irritation if you do not plan to chase it. Turn it on
with a top-level `positions: true` in the profile.

## Examples

| File | What it shows |
|---|---|
| [`examples/minimal.yaml`](../examples/minimal.yaml) | one fader on the system volume — the smallest useful profile |
| [`examples/quantum-hd8.yaml`](../examples/quantum-hd8.yaml) | a full desk: outputs, inputs, mutes, scenes on the R buttons, a second bank for the computer |

```bash
mackie bridge examples/minimal.yaml
```

## Where things live

| Question | File |
|---|---|
| What does the surface send? | [`protocol.md`](protocol.md), [`smc-mixer.md`](smc-mixer.md) |
| How do I map a new controller? | [`adding-a-surface.md`](adding-a-surface.md) |
| How do I control new gear? | [`adding-a-driver.md`](adding-a-driver.md) |
| What can a profile say? | this file, plus [`../examples/`](../examples/) |
| Why is it built this way? | [`../CLAUDE.md`](../CLAUDE.md) — the rules that came out of real bugs |

Everything in these docs was measured on hardware, with the date. When a
measurement contradicts a manual, the measurement wins and the doc says so.

## One AppleScript process, not one per script

The transport lamps ask an app "is it playing?" once a second. Through one
`osascript` per script that was ~54 processes a minute, and each one spends a
LaunchServices serial: on 2026-10-31, after 21 days of uptime, the 32-bit
counter ran out and every program macOS opened stayed stuck at launch, without
events, until a reboot (xgodev/ghost-mouse#4).

The `mac` and `app` drivers now share the shape of `ScriptHost`: one
`osascript -l JavaScript` per driver, started at its first script, that runs
each script in-process through `NSAppleScript` and leaves on end of stdin, so it
does not outlive the bridge. A host that dies is replaced at most once every
10 s, so a host that cannot stay up does not become a process per call.

Measured 2026-11-01, same profile, 65 s, no surface attached, `osascript`
behind a shim that logs each start:

| | `osascript` starts | check-ins |
|---|---|---|
| one per script | 55 | one each |
| `ScriptHost` | 1 | 1 |

A script costs ~1 ms against ~117 ms. The host's memory settles at ~40 MB
(24 MB at the start, flat from 12 000 to 30 000 scripts).
