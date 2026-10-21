import pytest

from mackie.bridge import run


def test_find_port_matches_by_hint():
    assert run.find_port(("SMC-Mixer-Master",),
                         ["Quantum HD 8 MIDI", "SINCO SMC-Mixer-Master"]) \
        == "SINCO SMC-Mixer-Master"


def test_find_port_falls_back_to_the_next_hint():
    """Over Bluetooth the SMC-Mixer shows up as `SMC-Mixer Bluetooth`, with no
    -Master port at all (measured 2026-09-22)."""
    assert run.find_port(("SMC-Mixer-Master", "SMC-Mixer"),
                         ["Quantum HD 8 MIDI", "SMC-Mixer Bluetooth"]) \
        == "SMC-Mixer Bluetooth"


def test_find_port_prefers_the_earlier_hint():
    assert run.find_port(("SMC-Mixer-Master", "SMC-Mixer"),
                         ["SMC-Mixer-Private", "SMC-Mixer-Master"]) \
        == "SMC-Mixer-Master"


def test_find_port_without_a_match_names_every_hint():
    with pytest.raises(SystemExit) as e:
        run.find_port(("SMC-Mixer-Master", "SMC-Mixer"), ["Quantum HD 8 MIDI"])
    assert "no MIDI port" in str(e.value) and "SMC-Mixer" in str(e.value)


class FakePort:
    """A mido port that answers iter_pending() and remembers what it was sent."""

    def __init__(self, name):
        self.name = name
        self.incoming = []
        self.sent = []
        self.closed = False

    def iter_pending(self):
        pending, self.incoming = self.incoming, []
        return iter(pending)

    def send(self, msg):
        self.sent.append(msg)

    def close(self):
        self.closed = True


class FakeMidi:
    """mido, with the surface appearing and disappearing on a timeline: one
    entry per poll, each the list of port names CoreMIDI shows at that moment."""

    def __init__(self, timeline):
        self.timeline = list(timeline)
        self.names = self.timeline.pop(0)
        self.opened = []

    def tick(self):
        if not self.timeline:
            raise KeyboardInterrupt
        self.names = self.timeline.pop(0)

    def get_input_names(self):
        if isinstance(self.names, Exception):
            raise self.names
        return list(self.names)

    get_output_names = get_input_names

    def open_input(self, name):
        p = FakePort(name)
        self.opened.append(p)
        return p

    open_output = open_input

    def Message(self, **kwargs):
        return kwargs


ON = ["SINCO SMC-Mixer-Master"]
OFF = ["Quantum HD 8 MIDI"]


def _profile():
    from mackie.bridge.profile import Bank, Profile
    return Profile(surface="smc-mixer", banks=[Bank(name="rig")])


def test_run_reopens_the_surface_after_it_comes_back(monkeypatch):
    """Switching the SMC-Mixer off takes its port out of CoreMIDI without any
    error reaching the reader: the loop has to watch the port list (measured
    2026-09-22)."""
    monkeypatch.setattr(run, "load_profile", lambda p: _profile())
    midi = FakeMidi([ON, ON, OFF, OFF, ON, ON])
    lines = []
    with pytest.raises(KeyboardInterrupt):
        run.run("ignored.yaml", midi=midi, log=lines.append, watch=0,
                sleep=lambda s: midi.tick())

    inputs = [p.name for p in midi.opened]
    assert inputs.count("SINCO SMC-Mixer-Master") == 4      # in and out, twice
    assert any("gone" in l for l in lines) and any("back" in l for l in lines)


def test_run_closes_the_ports_of_a_surface_that_went_away(monkeypatch):
    monkeypatch.setattr(run, "load_profile", lambda p: _profile())
    midi = FakeMidi([ON, OFF, ON])
    with pytest.raises(KeyboardInterrupt):
        run.run("ignored.yaml", midi=midi, log=lambda *a: None, watch=0,
                sleep=lambda s: midi.tick())
    assert all(p.closed for p in midi.opened[:2])


def test_run_waits_instead_of_giving_up_when_the_surface_is_not_there(monkeypatch):
    monkeypatch.setattr(run, "load_profile", lambda p: _profile())
    midi = FakeMidi([OFF, OFF, ON])
    with pytest.raises(KeyboardInterrupt):
        run.run("ignored.yaml", midi=midi, log=lambda *a: None, watch=0,
                sleep=lambda s: midi.tick())
    assert [p.name for p in midi.opened] == ["SINCO SMC-Mixer-Master"] * 2


def test_the_cli_finds_the_surface_the_same_way_the_bridge_does():
    """`mackie watch` had its own copy of the port lookup and went on calling
    surface.port_hint after the surface grew several hints (2026-09-22)."""
    import inspect

    from mackie import cli, surfaces

    assert "find_port" in inspect.getsource(cli.cmd_watch)
    assert run.find_port(surfaces.get().port_hints,
                         ["SMC-Mixer Bluetooth"]) == "SMC-Mixer Bluetooth"


def test_a_message_that_raises_is_logged_and_the_bridge_keeps_reading(monkeypatch):
    monkeypatch.setattr(run, "load_profile", lambda p: _profile())
    monkeypatch.setattr(run.Bridge, "on_midi",
                        lambda self, msg: (_ for _ in ()).throw(TypeError("boom")))
    midi = FakeMidi([ON, ON, ON])
    lines = []
    real_open = midi.open_input

    def open_with_message(name):
        p = real_open(name)
        p.incoming = ["a fader"]
        return p
    midi.open_input = open_with_message
    with pytest.raises(KeyboardInterrupt):
        run.run("ignored.yaml", midi=midi, log=lines.append, watch=0,
                sleep=lambda s: midi.tick())
    assert any("boom" in l for l in lines)


def test_a_port_list_that_raises_does_not_end_the_bridge(monkeypatch):
    """rtmidi raises InvalidPortError when a port vanishes while it is listing
    them; on 2026-10-21 that killed the bridge."""
    monkeypatch.setattr(run, "load_profile", lambda p: _profile())
    midi = FakeMidi([ON, ON, RuntimeError("portNumber (3) is invalid"), ON, ON])
    lines = []
    with pytest.raises(KeyboardInterrupt):
        run.run("ignored.yaml", midi=midi, log=lines.append, watch=0,
                sleep=lambda s: midi.tick())
    assert not any("gone" in l for l in lines)
