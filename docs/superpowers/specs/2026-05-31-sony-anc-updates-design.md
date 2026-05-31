# Sony ANC — Three Updates Design

**Date:** 2026-05-31
**Status:** Approved (pre-implementation)

## Overview

Three independent updates to the `sony` workspace (a Rust CLI controlling Sony
WF-1000XM5 earbuds over Bluetooth RFCOMM, for Waybar integration):

- **A — Payload parser tests** in the `sony-wf1000xm5` library (no current tests
  exist for `parse_payload`).
- **B — Expose protocol features** already implemented in the library but
  unreachable from the `sony-anc` CLI: custom EQ band editing, sound-pressure
  readout, and ambient level / voice-passthrough control.
- **C — A `watch` daemon** that holds one connection open and streams Waybar JSON
  on every change, replacing the heavy per-poll connect/init/teardown cycle.

Build order: **A → B → C** (safety net first, largest change last). Existing
one-shot commands (`status`, `cycle`, `set`, `battery`, `codec`, `equalizer`) are
kept unchanged — no breaking changes.

## Section A — Payload parser tests (`sony-wf1000xm5`)

Add a `#[cfg(test)] mod` to `sony-wf1000xm5/src/payload.rs` with two layers.

### End-to-end golden tests
Feed the real HCI byte frames already documented in source comments through
`FrameParser` → `parse_payload`, asserting the decoded `Payload`. This exercises
the parser↔payload seam, not just the payload decoder. Frames available in
comments include sound-pressure measure on/off and pressure-get sequences.

### Targeted slice tests
Hand-built payload slices for each `parse_payload` branch:

- `InitReply`
- `BatteryLevel` — both `Case` and `Headphones { left, right }` (note left=`[2]`,
  right=`[4]`; byte `[3]` is intentionally skipped — document this in the test).
- `Equalizer` — verify the `−10` band offset decoding.
- `AncStatus` — all three mode decodes derived from bytes `[3]`, `[4]`, `[5]`, `[6]`
  (Off / ActiveNoiseCanceling / AmbientSound).
- `Codec` — every variant (`Sbc`, `Aac`, `Ldac`, `Aptx`, `AptxHd`, `Unknown`).
- Error paths: `Empty`, `UnknownPayloadType`, `PayloadTooSmall`, `UnknownCodec`,
  `UnknownEqualizerPreset`, `UnknownBatteryType`.

### Pure-helper refactor in `main.rs`
- Extract the ambient-level clamp currently inline in `set_mode`
  (`main.rs:511-515`) into a pure `ambient_level_for(current: u8) -> u8` and test
  it (0 → `DEFAULT_AMBIENT_LEVEL`, else `min(20)`).
- Add `cycle_mode` tests (already a pure function — covers next/prev across all
  three modes).

## Section B — Expose hidden commands (`sony-anc`)

### B1 — Custom EQ band editing
Extend `EqualizerAction` with a `Set` variant:

```
sony-anc equalizer set [--preset manual|custom1|custom2]
                       [--bass N] [--b400 N] [--b1k N]
                       [--b2.5k N] [--b6.3k N] [--b16k N]
```

- `--preset` defaults to `manual`. Only Manual/Custom1/Custom2 are accepted (the
  library already rejects others via `EqualizerPresetNotCustomizable`).
- Each band flag is an optional `i8`; range `[-10, 10]` is enforced by the library
  (`EqualizerBandOutOfRange`).
- Flow: fetch current EQ → use its reported 6-band curve as the baseline → apply
  only the flags the user passed → send `Command::ChangeEqualizerSetting` →
  re-fetch → print. So `equalizer set --bass 3` nudges one band from the current
  sound rather than zeroing the rest.
- New `SonyClient::set_equalizer_bands(...)` method.

### B2 — Sound-pressure readout
New `pressure` subcommand. Flow:
1. `Command::SoundPressureMeasure { on: true }` → await `SoundPressureMeasureReply`.
2. `Command::GetSoundPressure` → await `Payload::SoundPressure { db }`.
3. Print Waybar JSON (`class: "pressure"`, text/tooltip with the dB value).
4. `Command::SoundPressureMeasure { on: false }` to stop measurement before exit.

New `SonyClient::fetch_sound_pressure()` method. These use `MessageType::Command2`;
ack handling follows the existing pattern in `wait_for_payload`.

### B3 — Ambient level + voice passthrough
Extend the `Set` subcommand:

```
sony-anc set ambient [--level 0..20] [--voice | --no-voice]
```

- `--level` and `--voice/--no-voice` are only meaningful for `ambient`; ignored for
  `anc`/`off`.
- `set_mode` gains explicit `level: Option<u8>` and `voice: Option<bool>` params
  instead of hardcoding `voice_passthrough = true` and reusing the current level.
  Defaults preserve today's behavior when flags are omitted.

### B4 — Refactor
Collapse the five duplicated `print_*` serialize-or-eprintln bodies
(`main.rs:231-322`) into one `emit(WaybarOutput)` helper. The `print_*` functions
become thin builders that call `emit`.

## Section C — `watch` daemon (`sony-anc`)

New `watch` subcommand holding a single long-lived RFCOMM connection.

### Behavior
- On connect: `Init`, then one `GetAncStatus`; emit an initial Waybar JSON line.
- Main loop: `tokio::select!` over three sources:
  - **Incoming frames** — parse incrementally; handle `Ack` (clear the
    waiting-for-ack flag / update seq); on `AncStatus` or `AncStatusNotify`,
    re-render and print **only when the state changed**.
  - **`SIGUSR1`** — cycle ANC mode next (send `AncSet` for the next mode).
  - **`SIGUSR2`** — cycle ANC mode previous.
- On disconnect / read-EOF / device-absent: print the `disconnected` line, back off
  (bounded retry interval), and reconnect in a loop. `watch` never exits on its own.

### Output & integration
- Output is the **same ANC JSON as `status`** — drop-in for the existing Waybar
  module.
- Waybar runs it as a continuous `exec` (no `interval`).
- `on-click` / `on-scroll` become `pkill -SIGUSR1 -f 'sony-anc watch'` (next) and
  `pkill -SIGUSR2 -f 'sony-anc watch'` (prev). One connection, no contention — the
  buds only accept one app RFCOMM connection at a time, which is why signals (not a
  second `cycle` process) drive cycling.
- Update the README Waybar snippet and add a `systemd --user` unit example.

### Concurrency note
`watch` cannot reuse `wait_for_payload`'s blocking read loop because it must select
across reads and signals. It gets a dedicated loop that feeds bytes into the same
`FrameParser` and handles ack/seq inline, mirroring the existing logic.

## Testing strategy

- **A**: unit tests are the deliverable.
- **B**: TDD on pure helpers — EQ band-merge (overrides applied over baseline),
  ambient level/voice defaulting.
- **C**: unit-test the pure parts (state-change detection / line rendering, cycle
  logic). Device-dependent paths (live RFCOMM, signal handling, reconnect/backoff)
  are verified manually against hardware — they can't be unit-tested without a
  device.

## Out of scope

- Socket-daemon IPC model (chose streaming stdout).
- Removing/deprecating one-shot polling commands.
- Combined `status --full` JSON, CI workflow, `tmp-controller` cleanup, crate
  metadata — deferred (proposed earlier, not selected for this round).
