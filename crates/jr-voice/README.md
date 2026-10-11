# jr_conventional_voice 1.2.0

English | [日本語](README.ja.md)

Pure Rust RL-CELP synthesis. The receiver uses this
crate's predictor, correction model, and CELP synthesizer with candidate selection
as its default synthesis path.

## Decoder behavior

The fixed acoustic target model estimates periodicity, pitch, spectral shape, and
level from the received CELP payload and predictor output. The planner synthesizes
three current-frame candidates and nine next-frame candidates, scores the actual
PCM, and commits the chosen synthesis state. A margin retains the original choice
when the estimated improvement is small. The first frame retains the original
choice. Chime mixing and gap fades are included in candidate scoring.

All weights are compiled into Rust. The channel decoder controls FEC/CRC acceptance;
this crate accepts the resulting 158-bit payloads. Input features used by the model
do not establish normative RL-CELP bit assignments.

Continuous input has **160 ms of lookahead** (four 40 ms frames). The first output
arrives on the fifth accepted contiguous frame. Output is 320 mono PCM16 samples at
8 kHz with its original slot index. Missing slots advance queued work without
inventing replacement speech. Gaps reset synthesis state, and `flush` emits a short
call or the buffered tail exactly once. The receiver's monitor jitter buffer is
440 ms; this is separate from synthesis lookahead and does not change saved samples.

Use the safe `receiver::Voice` API or C ABI **2**. Drain `read()` after every `push()`
/ `flush()` (C: `process` / `advance` / `flush`). Pass `None` for a rejected or nonvoice
slot. Indices increase across both accepted and missing slots. Start a fresh `Voice`
after EOF or reception restart. The C status layout is 28 bytes;
`jr_voice_version()` reports the crate version, 1.2.0.

## Maintenance and tests

- `plan_features.rs`: 220-element feature extraction and state history.
- `plan_target.rs`: quantized fixed model inference and pitch risk tables.
- `plan_acoustics.rs`: PCM pitch, periodicity, spectrum, and level scoring.
- `planner.rs`: trial synthesis, state selection, and one-frame lookahead.
- `stream.rs`: bounded indexed input, gaps, chime mixing, and flushing.
- `src/models/planner_data.rs`: fixed weights and measurement constants.

From the repository root:

```sh
cargo test -p jr_conventional_voice --locked
cargo clippy -p jr_conventional_voice --all-targets --locked -- -D warnings
```

`tests/planner.rs` generates synthetic payloads in memory to check lookahead,
short calls, missing slots, gaps, maximum uint32 indices, and idempotent flushing.
Unit tests also check C ABI backpressure, invalid input, reset, failure retirement,
numeric bounds, and deterministic noise generation. These tests check API and
stream behavior; they do not measure speech fidelity.
