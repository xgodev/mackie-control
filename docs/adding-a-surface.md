# Adding a control surface

1. **Plug it in and watch it.**

   ```bash
   mackie ports
   mackie watch 40 --port "<a porta dele>"
   ```

   Press one control at a time, in a known order: fader 1, then M, S, R and the
   square of channel 1, then the transport. Write down what each one sends.

2. **Check it against [`docs/protocol.md`](protocol.md).** If the notes and CCs
   match the table, the surface is a standard Mackie one and `protocol.rs`
   already decodes it — you only need step 3. If it differs, extend
   `protocol.rs` (and its tests) with what you measured, never with what a
   manual claims.

3. **Write the surface** in `crates/mackie-control/src/surfaces.rs`, following
   `SMC_MIXER`: name, `port_hints` (a substring of the CoreMIDI port), how
   many faders and encoders, which buttons each channel has, and a `notes` line
   for anything a user would trip on (mode switch, battery warning, a twin
   port). Register it in `SURFACES`.

4. **Test it** in `crates/mackie-control/tests/protocol.rs`: the profile is data, so the test is
   cheap and needs no hardware.

5. **Document what you measured** in `docs/<surface>.md` — including what the
   surface *cannot* do. That section saves the next person a whole evening.
