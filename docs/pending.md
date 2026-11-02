# Pending

What has been asked for and is not done yet, so it survives the session that
heard it.

- **An `openrig` driver** (asked 2026-09-22). OpenRig exposes its whole
  command set as MCP tools over Streamable HTTP at `127.0.0.1:4123`
  (`OpenRig/docs/mcp.md`), the same surface Claude drives — so the driver is
  an MCP client, not a new protocol. Before writing it: list the live tools
  (`tools/list`) and resources, and measure a chain-volume write and read back
  on the running app.
- **MK-300 and Ampero II over Bluetooth** (asked 2026-09-22). Both pedals take
  USB or BLE, and the port name changes with the transport, as on the
  SMC-Mixer. The drivers only look for the USB name today (`USB Composite
  Device`, `Ampero II Stage MIDI`). Measure each pedal's BLE port name, and
  whether its SysEx survives BLE at all, before adding it.
- **Starting the arrows from the device's own scene, on the hardware.** The
  MK-300 (`current_preset_index`) and the Ampero (global page 9) can say which
  preset or patch is loaded, and the bridge now starts from it. Not yet
  measured on the pedals: the first try hit the MK-300 unplugged and the
  Ampero port already held by the running bridge ("input overrun"). The HD 8
  cannot say at all — no scene parameter in its state — so there the arrows
  start from the scene the bridge itself last loaded, kept across restarts.
- **The "where am I" indicator** (2026-09-22, not settled). João's design:
  the lamp under the knob is the row, R the device's column, S the scene's.
  The knob lamp is the fader-position lamp (Pitch Bend only) and blinks until
  the physical fader matches, so a row whose fader has not been touched since
  the bridge started blinks until it is moved — João: "ele nao para de piscar
  NUNCA". A steady alternative was proposed (row on the square row, R and S as
  columns, no Pitch Bend) and an attempt at it was discarded unfinished; the
  installed code is `a9745da`. Decide with João before touching it again.
- **The HD 8 OUT default leaves out ADAT 11/12** (`aux/ch10`): it takes
  `aux/ch5`-`ch9` (ADAT 1/2 to 9/10), eight faders in all, and in João's rig
  ADAT 11/12 is the FRFR. Offered twice (swap 1/2 for 11/12), no answer yet.
- **A destination that never confirms is retried on every move.** Partly done
  (2026-11-02): each unconfirmed write blocked the drain 1 s, so a fader on an
  aux bus in Mixer Bypass froze every other fader. The hd8 driver now refuses
  that path without sending it for `STALE_RETRY` seconds. Still open: say so
  once in the log instead of on every move.
