# M-VAVE SMC-Mixer

Control surface, not a pedal: 8 faders, 8 endless encoders, 43 buttons, USB-C
and BLE, 780 mAh battery. It does **not** speak M-EFCS — in DAW mode it speaks
**Mackie Control**, which is `crates/mackie-control/src/protocol.rs`.

Everything below was measured on 2026-09-21 with the device on Bluetooth.

## Ports

CoreMIDI shows `SMC-Mixer-Master` (used here) and a `SMC-Mixer-Private` twin.
Over USB-C it is class-compliant and charges at the same time; the manual and
the Mixxx docs both recommend the cable for live use.

## Messages

| Control | Message |
|---|---|
| Fader n | Pitch Bend on channel n — unsigned 14-bit position |
| Encoder n | CC `0x10`+n−1, **relative**: `01` = +1, `41` = −1 |
| Mute / Solo / Rec / Select of channel n | Note On `0x10` / `0x08` / `0x00` / `0x18` + n−1 |
| Channel ◀ / ▶ | Note `2E` / `2F` |
| Arrows ◀ / ▶ | Note `62` / `63` |
| Arrows ▲ / ▼ | Note `60` / `61` |
| Rewind / Forward / Stop / Play / Record | Note `5B` / `5C` / `5D` / `5E` / `5F` |

The four buttons beside each fader, top to bottom as they sit on the
device: **M is the one under the knob**, then S, R and the square
(measured 2026-09-22 by pressing them in order: `10`, `08`, `00`, `18`).

**The lamps** (measured 2026-09-22, one at a time, asking João which lit):
notes `00`-`07` are the R row, `08`-`0F` the S, `10`-`17` the M, `18`-`1F` the
square. **The lamp under each knob is the fader-position lamp** — no note or CC
reaches it, only a **Pitch Bend** on that channel, and it then *blinks* until
the physical fader matches. CC `30`-`37` (the Mackie V-Pot ring) and notes
`20`-`27` do nothing; notes `50`-`5F` light the transport.

**The arrows and Channel have lamps the host cannot reach.** They light when
they are pressed — the device does that itself — but nothing sent to them lights
them: notes `60`-`63` and `2E`/`2F` do nothing, measured 2026-09-22 with the
bridge stopped so nothing could clear them. So a bridge cannot show on those
buttons which way there is still somewhere to go; only the player pressing one
lights it.

So the lamp a player reads first cannot be held steady: showing a row on it
means blinking that channel, and the only value that stops the blink is the
position that fader itself last sent. Sending what the *gear* holds does not
stop it — the comparison is against the physical fader, not against the rig.

Every LED is lit by sending the same note back with velocity 127 (0 clears it).
Sending a Pitch Bend makes that channel's LED **blink until the physical fader
matches** — the fader has no motor.

## What it cannot do

- **No internal preset.** Shift + the two bottom-right buttons switch DAW mode ↔
  User/CC mode; the lit button under Shift is the current mode. Channel ◀/▶ only
  pages the host's 8-track window.
- **No position report.** The Mackie Device Query (`F0 00 00 66 14 00 F7`) gets
  no fader positions back, so a host cannot ask where the faders are. That is
  why the bridge uses soft takeover: a fader only starts writing once it
  crosses the current value, otherwise touching it would jump the volume.
- Shift blinking on its own is the **low-battery** warning (manual).
- The battery level is **not readable from the Mac** (2026-09-27): over BLE the
  SMC-Mixer reports only its address (`system_profiler SPBluetoothDataType`,
  `ioreg` have no percentage). Shift blinking is the only battery signal.
- The BLE port can **drop out of CoreMIDI on its own** and only come back after
  power-cycling the surface (seen 2026-09-27, Shift not blinking, so not the
  battery; cause unknown). The bridge reopens it by itself once it is back.
- The configuration app (MidiSuite) is Windows-only, and as of 21/09 the
  official download page lists only NAM A2, MK-300 and TANK-PRO — nothing for
  the SMC-Mixer.
