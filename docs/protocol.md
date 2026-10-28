# Mackie Control, as this repo uses it

Plain MIDI, no SysEx. `crates/mackie-control/src/protocol.rs` decodes messages into `Fader`,
`Encoder` and `Button`, and builds the two messages that go back to the surface.

## Surface → host

| Control | Message | Detail |
|---|---|---|
| Fader n | Pitch Bend, channel n−1 | unsigned 14-bit position. `mido` centres pitch bend on zero, so the decoder shifts by +8192 |
| Encoder n | CC `0x10`+n−1 | **relative**: `0x01`–`0x3F` = right, `0x41`–`0x7F` = left, value is the number of detents |
| Rec / Solo / Mute / Select of channel n | Note On `0x00` / `0x08` / `0x10` / `0x18` + n−1 | velocity 127 pressed, 0 released |
| Channel ◀ / ▶ | Note `2E` / `2F` | pages the 8-channel window on a real DAW |
| Arrows ◀ / ▶ | Note `62` / `63` | |
| Arrows ▲ / ▼ | Note `60` / `61` | measured on the SMC-Mixer 2026-09-22 |
| Rewind / Forward / Stop / Play / Record | Note `5B` / `5C` / `5D` / `5E` / `5F` | |

## Host → surface

| To do | Message |
|---|---|
| Light a button's LED | the same Note On, velocity 127 (0 clears) |
| Say where a fader should be | Pitch Bend on that channel |

A surface without motors cannot move to the value it receives. It **blinks that
channel's LED** until the physical position matches — useful, and the reason
`mackie watch` shows a lot of blinking the first time you plug one in.

## What the protocol does not give you

There is no way to ask a surface where its faders are: the Mackie Device Query
(`F0 00 00 66 14 00 F7`) brought no position report back from the SMC-Mixer
(measured 2026-09-21). A host that starts mid-session therefore cannot know the
physical positions — hence soft takeover in the bridge.
