# OpenRig bank

With the HD 8 mixer in Bypass, the HD 8 only exposes MAIN and the two phones
volumes; the aux buses (FRFR = `aux/ch10`, SYN-5050 = `aux/ch2`) ignore writes.
The rig profile's HD 8 bank still holds MAIN, FRFR, SYN 5050, FONE 1, FONE 2:
FRFR and SYN 5050 are for the analog scenes (mixer on), where they work (João,
2026-09-29).

## The `openrig` driver (2026-09-30)

`crates/mackie-control/src/drivers/openrig.rs`. A fader drives one strip of OpenRig's
global mixer (`openrig://mixer`), i.e. the level OpenRig sends to that output.

- Target: strip id, full name, or a unique name prefix (`FRFR`, `SYN-5050`).
  An ambiguous prefix (`SYN-2` matches four strips) is refused.
- `mute:<strip>` is the strip's mute (reads 1.0/0.0, toggles).
- Travel: linear dB over -60..+12 dB, OpenRig's own clamp; unity at 5/6.
- Nothing is connected until first use; OpenRig down = `Unsupported`, one
  reconnect when the session dies (OpenRig restarted).

### Transport, measured 2026-09-30

MCP over streamable HTTP at `127.0.0.1:4123` (rmcp 1.7.0): `initialize` first
(anything else before it gets HTTP 422), answer is `text/event-stream` with a
`mcp-session-id` header that every later request carries. Measured latency:
read 17–26 ms, read + write 49 ms.

gRPC would be the natural transport, but OpenRig's `adapter-server` crate is
still a placeholder; the wire code is isolated in the `Http` class so it can
be swapped when gRPC exists.

## The banks are live (2026-09-30)

OpenRig's mixer strips change with every project and chain opened, so a
profile that names them goes stale the moment another project is loaded. The
driver's default banks, `OpenRig OUT` and `OpenRig IN`, carry `live: output` /
`live: input` instead of faders: the bridge asks `live_faders()` on the drain
thread right after the bank is selected and once a second after that, maps the
first eight strips of that direction in OpenRig's order, and addresses each
strip by its id. When the list changes, takeover starts over for that bank. A
profile only writes `- {driver: openrig}`.

**2026-10-01:** OpenRig is one device, not two banks. OUT and IN are its
scenes (`scenes()` / `load_scene()`), paged with the arrows like any other
device's; the bank carries `live: mixer` and the faders follow the scene.
