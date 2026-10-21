"""Runs the bridge against real MIDI ports (mido), until Ctrl-C.

The surface can go away under the loop: switching the SMC-Mixer off takes its
port out of CoreMIDI **without any error reaching the reader** -- the bridge
stays alive and silent forever (measured 2026-09-22). So the loop watches the
port list instead of waiting for an exception, and reopens the surface when it
comes back."""
from __future__ import annotations

import threading
import time

import mido

from ..surfaces import get as get_surface
from .daemon import Bridge
from .defaults import expand
from .profile import load_profile
from .state import State, default_path

POLL = 0.005        # s between reads of the surface
WATCH = 1.0         # s between checks that the surface is still there
RETRY = 1.0         # s between tries while it is gone


def find_port(hints, names: list[str]) -> str:
    """The first port matching the first hint that matches anything. A surface
    gives several because the same device is named differently depending on how
    it is connected: `SMC-Mixer-Master` over USB, `SMC-Mixer Bluetooth` over
    BLE (measured 2026-09-22)."""
    for hint in hints:
        found = [n for n in names if hint in n]
        if found:
            return found[0]
    raise SystemExit(f"no MIDI port matching {list(hints)}: {names}")


def _present(hints, names) -> str | None:
    try:
        return find_port(hints, names)
    except SystemExit:
        return None


def _ports(list_names) -> list[str] | None:
    """The port names CoreMIDI shows now, or None when listing them fails.
    A port can vanish between rtmidi counting the ports and naming them, which
    raises InvalidPortError; on 2026-10-21 that one call killed the bridge."""
    try:
        return list_names()
    except Exception:
        return None


def run(profile_path: str, port: str | None = None, log=print,
        midi=mido, sleep=time.sleep, watch: float = WATCH) -> None:
    profile = expand(load_profile(profile_path))
    surface = get_surface(profile.surface)
    hints = (port,) if port else surface.port_hints

    # The output port is replaced on every reconnect, so the bridge sends
    # through whatever is open now instead of holding on to a dead port.
    live = {"out": None}
    bridge = Bridge(profile,
                    send=lambda **k: live["out"] and live["out"].send(midi.Message(**k)),
                    log=log, state=State(default_path(profile_path)))
    bridge.restore()
    threading.Thread(target=bridge.drain_forever, daemon=True).start()

    first = True
    try:
        while True:
            src = _wait_for_surface(hints, midi, log, sleep, first)
            if src is None:
                continue
            dst = _present(hints, _ports(midi.get_output_names) or []) or src
            inp, out = midi.open_input(src), midi.open_output(dst)
            live["out"] = out
            if first:
                log(f"bridge: {surface.name} ({src}) -> "
                    + ", ".join(b.name for b in profile.banks))
            else:
                log(f"** the surface is back ({src})")
            bridge.select_bank(bridge.bank_index)
            first = False
            _read_until_gone(bridge, inp, hints, midi, sleep, watch)
            live["out"] = None
            for p in (inp, out):
                p.close()
            log(f"!! the surface is gone ({src}); waiting for it to come back")
    except KeyboardInterrupt:
        log("stopped")
        raise


def _wait_for_surface(hints, midi, log, sleep, first):
    src = _present(hints, _ports(midi.get_input_names) or [])
    if src is None:
        if first:
            log(f"waiting for a MIDI port matching {list(hints)}")
        sleep(RETRY)
    return src


def _read_until_gone(bridge, inp, hints, midi, sleep, watch):
    """Read the surface until its port leaves CoreMIDI. Nothing is raised when
    it does, so the port list is the only sign."""
    since = 0.0
    while True:
        for msg in inp.iter_pending():
            # One bad message must not end the bridge: on 2026-10-01 a fader
            # moved while OpenRig was closed raised here, the process died,
            # and every fader went dead with it.
            try:
                bridge.on_midi(msg)
            except Exception as e:
                bridge.log(f"  !! {msg}: {e!r}")
        sleep(POLL)
        since += POLL
        if since >= watch:
            since = 0.0
            names = _ports(midi.get_input_names)
            if names is not None and _present(hints, names) is None:
                return
