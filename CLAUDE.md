# mackie-control

## Every change is committed AND pushed
Any change here ends with `git commit` + `git push` to `origin/main` in the same
turn. João installs this with `cargo install --git https://github.com/xgodev/mackie-control`;
an unpushed commit does not exist for him or for his other sessions. Run
`cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
and check the exit codes before committing.

## What this repo is — and is not
It is the **surface** (Mackie) and the **bridge**. It is not a repo about one
piece of gear, and **nobody's rig belongs in the code**: fader destinations,
channel names and mixer paths live in the user's **YAML profile**, outside this
repo.
**Why:** this bridge was born inside João's rig repo with `line/ch1/volume`
hardcoded in the middle of it, and on 2026-09-21 it had to move house twice
because of that.

## Protocol is measured, never remembered
Notes, CCs and device behaviour land here **measured**, with the date, and the
measurement goes into `docs/`. A manufacturer's manual is a starting point, not
proof — the SMC-Mixer, for one, does not answer the Mackie Device Query, which
no manual mentions.

## A failing driver brings nothing down
A parameter the device refuses becomes `Unsupported` plus a line in the log, and
the rest of the operation continues. On 2026-09-21 one refused write
(`line/ch20/mute`) aborted a whole solo halfway through, leaving the mixer in a
broken state and the button unable to undo it.

## Tests run without hardware, and without the real home
Drivers and clients go in by injection (`Hd8::with(client, ...)`), and the profile is
data. No test in this repo may require a device to be plugged in.
Nor may a test write outside a temp dir: tests that reach the state file point
`XDG_STATE_HOME` at a `tempfile` dir first, because on 2026-09-22 a test
written without that left `~/.local/state/mackie-control/ignored.json`
in João's home.

## Nothing slow on the MIDI thread
The thread reading the surface never waits on a driver: reads happen only
when their value is sent, encoder detents are summed and applied on the drain,
and anything polled (transport lamps) is asked once per target, once a second,
off the MIDI thread. A single 117 ms AppleScript call per fader made the
Spotify bank feel a second late (2026-09-22, `docs/bridge.md`).

## When João names a physical thing, light it — do not argue the name
The indicator lamp took a dozen rounds on 2026-09-22 because "the LED under
the knob", "M", "R" and "the row" were argued as words while each fix was
pushed on a guess. What ended it: stop the bridge, light **one** lamp at a time
with a number, and ask which number is the one he means (answer: the knob lamp
is the fader-position lamp, reachable only by Pitch Bend). Do that first,
before changing code, and never with the bridge running — it clears the lamps.

## Stop the bridge only with João's go-ahead
A `pkill -f "mackie bridge"` for a measurement also kills the one he just
started in his terminal (it happened twice on 2026-09-22). Say it will stop,
and tell him the command to bring it back when done.

## Language
Code, comments, docs and log messages are in **English**. João reads Portuguese
in chat, not in a public repo.

## Answers are short
Four lines is the ceiling for an answer in chat; a decision question is one
line of fact plus one line per option. Findings, measurements and reasoning go
into `docs/`, never into the reply. João stops reading long answers, so a long
answer buries the one line that mattered (2026-09-22: "seja simples na
resposta, nao vou ler um texto do tamanho de um livro").

## A bank is one subject
Outputs in one bank, guitars in another, the computer in a third. Eight faders
of mixed subjects on one screen is what makes a profile unreadable, and paging
is a keypress.

**L and R are two faders when the gear has two parameters** — do not decide for
the user that a pair should be linked (2026-09-22: "eu quero poder controlar L
e R separadamente"). On the HD 8 there is no such pair to split: MAIN and both
headphone outs have one volume parameter each, and an aux send is a whole
stereo bus (`aux/ch(4+k)` is the ADAT k/k+1 pair, so `aux/ch10` is all of ADAT
11/12). Splitting L and R there has to happen further down the chain.

That last fact came from reading `quantum-hd8/docs/` **after** telling João his
FRFR fader was moving one side of a pair, which was wrong: `aux/ch9` is the ADAT
9/10 bus, a different destination. The rig's own repos answer these questions —
read them before making a claim about his rig, not after.

## After every push, update João's machine and restart the service
The bridge runs as a LaunchAgent (`mackie service install PROFILE`), not in a
terminal. A pushed change is not live until it is installed and the service
restarted, so every push here is followed by:
`CARGO_TARGET_DIR="/Volumes/MACBOOK EXTERNO/workspaces/.cache/cargo-target-mackie" cargo install -q --locked --force --path crates/mackie-control && ~/.cargo/bin/mackie service restart`,
then `~/.cargo/bin/mackie service status` and a look at `~/Library/Logs/mackie-control.log`.
The LaunchAgent runs `~/.cargo/bin/mackie` (the Python version was retired on
2026-10-27).
(2026-09-23: "qdo a gente alterar alguma coisa aqui, vc precisa atualizar a
app na minha maquina e reiniciar o servico".)
