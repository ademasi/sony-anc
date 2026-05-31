# Sony ANC Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add payload parser tests, expose three hidden protocol features (custom EQ, sound pressure, ambient tuning), and add a streaming `watch` daemon to the Sony WF-1000XM5 CLI.

**Architecture:** Two-crate workspace. `sony-wf1000xm5` is a pure protocol library (encode/decode RFCOMM frames); `sony-anc` is the Clap-based CLI that talks BlueZ RFCOMM and emits Waybar JSON. Section A adds characterization tests to the library and pure-helper tests to the binary. Section B exposes existing `Command`/`Payload` variants through new CLI flags. Section C adds a long-lived `watch` connection driven by a `tokio::select!` loop over socket reads + Unix signals.

**Tech Stack:** Rust 2024, tokio (full), bluer, clap (derive), serde_json, thiserror, anyhow.

---

## File structure

- `sony-wf1000xm5/src/payload.rs` — add `PartialEq, Eq` derives + `#[cfg(test)] mod` (Section A).
- `sony-anc/src/main.rs` — all CLI/daemon changes (Sections A helpers, B, C). This file already holds the whole binary; we keep that convention and add focused private helpers + a `#[cfg(test)] mod`.
- `README.md` — Waybar snippet update for `watch` (Section C).

All `cargo test` commands run from the repo root `/home/alex/git/sony`.

> **Note on TDD vs. characterization:** Section A tests cover code that already exists, so the tests pass on first run (characterization tests) rather than failing first. Sections B and C introduce new pure helpers (`ambient_params`, `merge_bands`, `should_emit`) written test-first in the normal red→green order.

---

## Task 1: Make library payload types comparable

**Files:**
- Modify: `sony-wf1000xm5/src/payload.rs` (derives on `BatteryLevel`, `Codec`, `Payload`)

- [ ] **Step 1: Add `PartialEq, Eq` to `BatteryLevel`**

In `sony-wf1000xm5/src/payload.rs`, change the `BatteryLevel` derive (currently `#[derive(Debug)]`) to:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum BatteryLevel {
    Case(usize),
    Headphones { left: usize, right: usize },
}
```

- [ ] **Step 2: Add `PartialEq, Eq` to `Codec`**

Change the `Codec` derive (currently `#[derive(Clone, Copy, Debug)]`) to:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
```

- [ ] **Step 3: Add `PartialEq, Eq` to `Payload`**

Change the `Payload` derive (currently `#[derive(Debug)]`) to:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum Payload {
```

(`AncMode` and `EqualizerPreset` already derive `PartialEq, Eq`, so `Payload` now compiles with these derives.)

- [ ] **Step 4: Verify the crate still builds**

Run: `cargo build -p sony-wf1000xm5`
Expected: builds with no errors.

- [ ] **Step 5: Commit**

```bash
git add sony-wf1000xm5/src/payload.rs
git commit -m "Derive PartialEq/Eq on library payload types for testability"
```

---

## Task 2: End-to-end golden frame tests (parser → payload)

**Files:**
- Modify: `sony-wf1000xm5/src/payload.rs` (append `#[cfg(test)] mod test`)

These feed the real HCI byte frames from the source comments through `FrameParser` then `parse_payload`.

- [ ] **Step 1: Add the test module with golden frame tests**

Append to the end of `sony-wf1000xm5/src/payload.rs`:

```rust
#[cfg(test)]
mod test {
    use super::*;
    use crate::frame_parser::{FrameParser, FrameParserResult};

    /// Run a full HCI frame through the frame parser, then parse its payload.
    fn decode_frame(frame: &[u8]) -> Payload {
        let mut parser = FrameParser::new();
        match parser.parse(frame) {
            FrameParserResult::Ready { msg, .. } => {
                assert!(msg.checksum.is_ok(), "frame checksum invalid: {:?}", msg.checksum);
                let kind = msg.kind.expect("known message type");
                parse_payload(msg.payload, kind).expect("payload parses")
            }
            other => panic!("frame did not complete: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn golden_sound_pressure_measure_on() {
        // from payload.rs comment: device reports measuring turned on
        let frame = [
            0x3e, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x04, 0x59, 0x03, 0x01, 0x00, 0x6f, 0x3c,
        ];
        assert_eq!(
            decode_frame(&frame),
            Payload::SoundPressureMeasureReply { is_on: true }
        );
    }

    #[test]
    fn golden_sound_pressure_measure_off() {
        // from payload.rs comment: device reports measuring turned off
        let frame = [
            0x3e, 0x0e, 0x01, 0x00, 0x00, 0x00, 0x04, 0x59, 0x03, 0x01, 0x01, 0x71, 0x3c,
        ];
        assert_eq!(
            decode_frame(&frame),
            Payload::SoundPressureMeasureReply { is_on: false }
        );
    }

    #[test]
    fn golden_pressure_get() {
        // from payload.rs comment: 3e0e01000000045b034203b63c, value byte 0x42 = 66
        let frame = [
            0x3e, 0x0e, 0x01, 0x00, 0x00, 0x00, 0x04, 0x5b, 0x03, 0x42, 0x03, 0xb6, 0x3c,
        ];
        assert_eq!(decode_frame(&frame), Payload::SoundPressure { db: 66 });
    }
}
```

- [ ] **Step 2: Run the golden tests**

Run: `cargo test -p sony-wf1000xm5 golden`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add sony-wf1000xm5/src/payload.rs
git commit -m "Add golden HCI frame tests for payload parsing"
```

---

## Task 3: Payload slice success-path tests

**Files:**
- Modify: `sony-wf1000xm5/src/payload.rs` (extend `mod test`)

- [ ] **Step 1: Add success-path slice tests**

Inside the `mod test` block (after the golden tests), add:

```rust
    #[test]
    fn init_reply() {
        let payload = [0x01];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::InitReply
        );
    }

    #[test]
    fn battery_case() {
        // [type=0x23, battery=0x0a (case), value=75, _, _]
        let payload = [0x23, 0x0a, 75, 0x00, 0x00];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::BatteryLevel(BatteryLevel::Case(75))
        );
    }

    #[test]
    fn battery_headphones() {
        // left = byte[2], right = byte[4]; byte[3] is intentionally skipped
        let payload = [0x23, 0x01, 80, 0x00, 85];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::BatteryLevel(BatteryLevel::Headphones { left: 80, right: 85 })
        );
    }

    #[test]
    fn equalizer_decodes_band_offset() {
        // preset byte[2]=0x10 (Bright); bands at byte[4..10] are stored +10
        let payload = [0x57, 0x00, 0x10, 0x06, 12, 10, 10, 10, 10, 7];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::Equalizer {
                preset: EqualizerPreset::Bright,
                clear_bass: 2,
                band_400: 0,
                band_1000: 0,
                band_2500: 0,
                band_6300: 0,
                band_16000: -3,
            }
        );
    }

    #[test]
    fn anc_status_off() {
        // byte[3]==0 -> Off
        let payload = [0x67, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::AncStatus {
                mode: AncMode::Off,
                ambient_sound_voice_passthrough: false,
                ambient_sound_level: 0,
            }
        );
    }

    #[test]
    fn anc_status_active_noise_canceling() {
        // byte[3]!=0 && byte[4]==0 -> ANC
        let payload = [0x67, 0, 0, 1, 0, 0, 0];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::AncStatus {
                mode: AncMode::ActiveNoiseCanceling,
                ambient_sound_voice_passthrough: false,
                ambient_sound_level: 0,
            }
        );
    }

    #[test]
    fn anc_status_ambient() {
        // byte[3]!=0 && byte[4]!=0 -> Ambient; voice=byte[5]==1; level=byte[6]
        let payload = [0x67, 0, 0, 1, 1, 1, 10];
        assert_eq!(
            parse_payload(&payload, MessageType::Command1).unwrap(),
            Payload::AncStatus {
                mode: AncMode::AmbientSound,
                ambient_sound_voice_passthrough: true,
                ambient_sound_level: 10,
            }
        );
    }

    #[test]
    fn codec_variants() {
        for (byte, expected) in [
            (0x01u8, Codec::Sbc),
            (0x02, Codec::Aac),
            (0x10, Codec::Ldac),
            (0x20, Codec::Aptx),
            (0x21, Codec::AptxHd),
            (0x00, Codec::Unknown),
        ] {
            let payload = [0x13, 0x00, byte];
            assert_eq!(
                parse_payload(&payload, MessageType::Command1).unwrap(),
                Payload::Codec { codec: expected }
            );
        }
    }
```

- [ ] **Step 2: Run the slice tests**

Run: `cargo test -p sony-wf1000xm5`
Expected: all tests pass (golden + slice).

- [ ] **Step 3: Commit**

```bash
git add sony-wf1000xm5/src/payload.rs
git commit -m "Add payload slice success-path tests"
```

---

## Task 4: Payload error-path tests

**Files:**
- Modify: `sony-wf1000xm5/src/payload.rs` (extend `mod test`)

`ParsePayloadError` does not derive `PartialEq`, so assert with `matches!`.

- [ ] **Step 1: Add error-path tests**

Inside the `mod test` block, add:

```rust
    #[test]
    fn empty_payload_errors() {
        assert!(matches!(
            parse_payload(&[], MessageType::Command1),
            Err(ParsePayloadError::Empty)
        ));
    }

    #[test]
    fn unknown_payload_type_errors() {
        assert!(matches!(
            parse_payload(&[0xff], MessageType::Command1),
            Err(ParsePayloadError::UnknownPayloadType { kind: 0xff })
        ));
    }

    #[test]
    fn battery_too_small_errors() {
        // BatteryLevel needs >= 5 bytes
        assert!(matches!(
            parse_payload(&[0x23, 0x01], MessageType::Command1),
            Err(ParsePayloadError::PayloadTooSmall { .. })
        ));
    }

    #[test]
    fn unknown_battery_type_errors() {
        // 0x05 is not a known battery type
        assert!(matches!(
            parse_payload(&[0x23, 0x05, 0, 0, 0], MessageType::Command1),
            Err(ParsePayloadError::UnknownBatteryType { battery: 0x05 })
        ));
    }

    #[test]
    fn unknown_codec_errors() {
        assert!(matches!(
            parse_payload(&[0x13, 0x00, 0x99], MessageType::Command1),
            Err(ParsePayloadError::UnknownCodec { codec: 0x99 })
        ));
    }

    #[test]
    fn unknown_equalizer_preset_errors() {
        assert!(matches!(
            parse_payload(&[0x57, 0x00, 0x99, 0x06, 10, 10, 10, 10, 10, 10], MessageType::Command1),
            Err(ParsePayloadError::UnknownEqualizerPreset { preset: 0x99 })
        ));
    }
```

- [ ] **Step 2: Run the error-path tests**

Run: `cargo test -p sony-wf1000xm5`
Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add sony-wf1000xm5/src/payload.rs
git commit -m "Add payload error-path tests"
```

---

## Task 5: Extract and test `ambient_level_for` + `cycle_mode`

**Files:**
- Modify: `sony-anc/src/main.rs` (extract helper, add `#[cfg(test)] mod`)

- [ ] **Step 1: Extract `ambient_level_for`**

In `sony-anc/src/main.rs`, find the level clamp inside `set_mode` (currently):

```rust
        let level = if current_level == 0 {
            DEFAULT_AMBIENT_LEVEL
        } else {
            current_level.min(20)
        };
```

Replace that block with a call:

```rust
        let level = ambient_level_for(current_level);
```

Then add this free function near `cycle_mode` (above `connect`):

```rust
/// Resolve the ambient sound level to use: fall back to the default when the
/// device reports 0, otherwise clamp the current level into the valid range.
fn ambient_level_for(current: u8) -> u8 {
    if current == 0 {
        DEFAULT_AMBIENT_LEVEL
    } else {
        current.min(20)
    }
}
```

- [ ] **Step 2: Add the test module**

Append to the end of `sony-anc/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_level_defaults_when_zero() {
        assert_eq!(ambient_level_for(0), DEFAULT_AMBIENT_LEVEL);
    }

    #[test]
    fn ambient_level_clamps_to_twenty() {
        assert_eq!(ambient_level_for(25), 20);
    }

    #[test]
    fn ambient_level_passes_through() {
        assert_eq!(ambient_level_for(15), 15);
    }

    #[test]
    fn cycle_next_rotates_anc_ambient_off() {
        assert_eq!(cycle_mode(AncCliMode::Anc, CycleDirection::Next), AncCliMode::Ambient);
        assert_eq!(cycle_mode(AncCliMode::Ambient, CycleDirection::Next), AncCliMode::Off);
        assert_eq!(cycle_mode(AncCliMode::Off, CycleDirection::Next), AncCliMode::Anc);
    }

    #[test]
    fn cycle_prev_rotates_reverse() {
        assert_eq!(cycle_mode(AncCliMode::Anc, CycleDirection::Prev), AncCliMode::Off);
        assert_eq!(cycle_mode(AncCliMode::Off, CycleDirection::Prev), AncCliMode::Ambient);
        assert_eq!(cycle_mode(AncCliMode::Ambient, CycleDirection::Prev), AncCliMode::Anc);
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p sony-anc`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Extract ambient_level_for and add CLI pure-helper tests"
```

---

## Task 6: Refactor duplicated output into `emit`

**Files:**
- Modify: `sony-anc/src/main.rs` (`print_output`, `print_battery`, `print_codec`, `print_equalizer`)

- [ ] **Step 1: Add the `emit` helper**

In `sony-anc/src/main.rs`, add this free function just above `fn print_output`:

```rust
/// Serialize a Waybar output object to stdout, logging serialization failures.
fn emit(output: WaybarOutput) {
    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(err) => eprintln!("failed to serialize output: {err}"),
    }
}
```

- [ ] **Step 2: Route `print_output` through `emit`**

Replace the trailing `match serde_json::to_string(&output) { ... }` block in `print_output` with:

```rust
    emit(output);
```

(Keep the `let output = match state { ... }` construction above it unchanged.)

- [ ] **Step 3: Route `print_battery` through `emit`**

In `print_battery`, replace the trailing `match serde_json::to_string(&output) { ... }` block with:

```rust
    emit(output);
```

- [ ] **Step 4: Route `print_codec` through `emit`**

In `print_codec`, replace the trailing `match serde_json::to_string(&output) { ... }` block with:

```rust
    emit(output);
```

- [ ] **Step 5: Route `print_equalizer` through `emit`**

In `print_equalizer`, replace the trailing `match serde_json::to_string(&output) { ... }` block with:

```rust
    emit(output);
```

- [ ] **Step 6: Build and test**

Run: `cargo build -p sony-anc && cargo test -p sony-anc`
Expected: builds clean, tests pass.

- [ ] **Step 7: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Collapse duplicated Waybar serialization into emit helper"
```

---

## Task 7: Ambient level + voice passthrough control

**Files:**
- Modify: `sony-anc/src/main.rs` (add `ambient_params`, change `set_mode`, extend `Set` subcommand, update callers, add tests)

- [ ] **Step 1: Write the failing test for `ambient_params`**

In the `#[cfg(test)] mod tests` block in `sony-anc/src/main.rs`, add:

```rust
    #[test]
    fn ambient_params_defaults() {
        assert_eq!(ambient_params(AncCliMode::Ambient, 0, None, None), (DEFAULT_AMBIENT_LEVEL, true));
        assert_eq!(ambient_params(AncCliMode::Ambient, 15, None, None), (15, true));
    }

    #[test]
    fn ambient_params_overrides() {
        assert_eq!(ambient_params(AncCliMode::Ambient, 0, Some(7), None), (7, true));
        assert_eq!(ambient_params(AncCliMode::Ambient, 0, Some(50), None), (20, true)); // clamped
        assert_eq!(ambient_params(AncCliMode::Ambient, 0, None, Some(false)), (DEFAULT_AMBIENT_LEVEL, false));
    }

    #[test]
    fn ambient_params_non_ambient_modes_zeroed() {
        assert_eq!(ambient_params(AncCliMode::Anc, 15, Some(7), Some(true)), (0, false));
        assert_eq!(ambient_params(AncCliMode::Off, 15, Some(7), Some(true)), (0, false));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p sony-anc ambient_params`
Expected: FAIL — `cannot find function ambient_params`.

- [ ] **Step 3: Implement `ambient_params`**

Add this free function next to `ambient_level_for`:

```rust
/// Compute the (ambient_sound_level, voice_passthrough) pair to send for a target
/// mode. Only Ambient uses a level/voice; other modes are always (0, false).
/// `level_override` and `voice_override` come from CLI flags; when absent the
/// level falls back to `ambient_level_for(current_level)` and voice defaults to true.
fn ambient_params(
    target: AncCliMode,
    current_level: u8,
    level_override: Option<u8>,
    voice_override: Option<bool>,
) -> (u8, bool) {
    match target {
        AncCliMode::Ambient => {
            let level = level_override
                .unwrap_or_else(|| ambient_level_for(current_level))
                .min(20);
            (level, voice_override.unwrap_or(true))
        }
        AncCliMode::Anc | AncCliMode::Off => (0, false),
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p sony-anc ambient_params`
Expected: PASS.

- [ ] **Step 5: Rewrite `set_mode` to use `ambient_params`**

Replace the entire body of `set_mode` with:

```rust
    async fn set_mode(
        &mut self,
        target: AncCliMode,
        current_level: u8,
        level_override: Option<u8>,
        voice_override: Option<bool>,
    ) -> Result<AncState> {
        let (ambient_level, voice_passthrough) =
            ambient_params(target, current_level, level_override, voice_override);

        let command = Command::AncSet {
            dragging_ambient_sound_slider: false,
            mode: target.into(),
            ambient_sound_voice_passthrough: voice_passthrough,
            ambient_sound_level: ambient_level as usize,
        };
        self.send_command(command).await?;

        // Wait briefly so the Ack can clear before asking for status.
        let _ = self
            .wait_for_payload(Duration::from_millis(500), |_| false)
            .await;
        self.fetch_anc_status().await
    }
```

(`ambient_level_for` is now only called via `ambient_params`; it remains used, no dead-code warning.)

- [ ] **Step 6: Extend the `Set` subcommand with flags**

In the `Action` enum, replace the `Set` variant with:

```rust
    /// Explicitly set ANC mode
    Set {
        #[arg(value_enum)]
        mode: AncCliMode,
        /// Ambient sound level 0-20 (ambient mode only)
        #[arg(long)]
        level: Option<u8>,
        /// Enable voice passthrough (ambient mode only)
        #[arg(long, conflicts_with = "no_voice")]
        voice: bool,
        /// Disable voice passthrough (ambient mode only)
        #[arg(long)]
        no_voice: bool,
    },
```

- [ ] **Step 7: Update the `Set` and `Cycle` match arms in `run`**

Replace the `Action::Cycle` and `Action::Set` arms with:

```rust
        Action::Cycle { direction } => {
            let status = client.fetch_anc_status().await?;
            let next = cycle_mode(status.mode, direction);
            let updated = client.set_mode(next, status.ambient_level, None, None).await?;
            print_output(Some(updated));
        }
        Action::Set {
            mode,
            level,
            voice,
            no_voice,
        } => {
            let voice_override = if voice {
                Some(true)
            } else if no_voice {
                Some(false)
            } else {
                None
            };
            let status = client.fetch_anc_status().await.unwrap_or(AncState {
                mode,
                ambient_level: DEFAULT_AMBIENT_LEVEL,
            });
            let updated = client
                .set_mode(mode, status.ambient_level, level, voice_override)
                .await?;
            print_output(Some(updated));
        }
```

- [ ] **Step 8: Build and test**

Run: `cargo build -p sony-anc && cargo test -p sony-anc`
Expected: builds clean, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Expose ambient level and voice passthrough control on set"
```

---

## Task 8: Custom equalizer band editing

**Files:**
- Modify: `sony-anc/src/main.rs` (add `merge_bands`, `CliCustomPreset`, extend `EqualizerAction`, add `set_equalizer_bands`, wire `run`, add tests)

- [ ] **Step 1: Write the failing test for `merge_bands`**

In the `#[cfg(test)] mod tests` block, add:

```rust
    #[test]
    fn merge_bands_applies_only_overrides() {
        let base = [1i8, 2, 3, 4, 5, 6];
        let overrides = [Some(9), None, None, None, None, Some(-9)];
        assert_eq!(merge_bands(base, overrides), [9, 2, 3, 4, 5, -9]);
    }

    #[test]
    fn merge_bands_no_overrides_keeps_base() {
        let base = [0i8, 0, 0, 0, 0, 0];
        assert_eq!(merge_bands(base, [None; 6]), base);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p sony-anc merge_bands`
Expected: FAIL — `cannot find function merge_bands`.

- [ ] **Step 3: Implement `merge_bands`**

Add this free function near the other helpers:

```rust
/// Apply per-band overrides over a baseline curve.
/// Index order: [bass, 400Hz, 1kHz, 2.5kHz, 6.3kHz, 16kHz].
fn merge_bands(base: [i8; 6], overrides: [Option<i8>; 6]) -> [i8; 6] {
    let mut out = base;
    for (slot, override_value) in out.iter_mut().zip(overrides) {
        if let Some(v) = override_value {
            *slot = v;
        }
    }
    out
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p sony-anc merge_bands`
Expected: PASS.

- [ ] **Step 5: Add the `CliCustomPreset` enum**

Add near `CliEqualizerPreset`:

```rust
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliCustomPreset {
    Manual,
    Custom1,
    Custom2,
}

impl From<CliCustomPreset> for EqualizerPreset {
    fn from(p: CliCustomPreset) -> Self {
        match p {
            CliCustomPreset::Manual => EqualizerPreset::Manual,
            CliCustomPreset::Custom1 => EqualizerPreset::Custom1,
            CliCustomPreset::Custom2 => EqualizerPreset::Custom2,
        }
    }
}
```

- [ ] **Step 6: Extend `EqualizerAction`**

Replace the `EqualizerAction` enum with:

```rust
#[derive(Debug, Subcommand)]
enum EqualizerAction {
    /// Show current equalizer settings (default)
    Get,
    /// Switch to a preset
    Preset {
        #[arg(value_enum)]
        preset: CliEqualizerPreset,
    },
    /// Set custom band levels (-10..10), keeping current values for unset bands
    Set {
        /// Custom preset slot to write into
        #[arg(long, value_enum, default_value_t = CliCustomPreset::Manual)]
        preset: CliCustomPreset,
        #[arg(long)]
        bass: Option<i8>,
        #[arg(long = "b400")]
        band_400: Option<i8>,
        #[arg(long = "b1k")]
        band_1000: Option<i8>,
        #[arg(long = "b2.5k")]
        band_2500: Option<i8>,
        #[arg(long = "b6.3k")]
        band_6300: Option<i8>,
        #[arg(long = "b16k")]
        band_16000: Option<i8>,
    },
}
```

- [ ] **Step 7: Add `set_equalizer_bands` to `SonyClient`**

Add this method to the `impl SonyClient` block (after `set_equalizer_preset`):

```rust
    async fn set_equalizer_bands(
        &mut self,
        preset: EqualizerPreset,
        overrides: [Option<i8>; 6],
    ) -> Result<()> {
        // Use the device's currently reported curve as the baseline so a single
        // flag nudges one band instead of zeroing the rest.
        let base = match self.fetch_equalizer().await? {
            Payload::Equalizer {
                clear_bass,
                band_400,
                band_1000,
                band_2500,
                band_6300,
                band_16000,
                ..
            } => [
                clear_bass, band_400, band_1000, band_2500, band_6300, band_16000,
            ],
            _ => [0; 6],
        };
        let [bass_level, band_400, band_1000, band_2500, band_6300, band_16000] =
            merge_bands(base, overrides);

        self.send_command(Command::ChangeEqualizerSetting {
            preset,
            bass_level,
            band_400,
            band_1000,
            band_2500,
            band_6300,
            band_16000,
        })
        .await?;
        let _ = self
            .wait_for_payload(Duration::from_millis(500), |_| false)
            .await;
        Ok(())
    }
```

- [ ] **Step 8: Wire the `Set` arm into `run`**

In the `Action::Equalizer { action }` match, replace the inner `match action.unwrap_or(EqualizerAction::Get) { ... }` with one that includes the new arm:

```rust
        Action::Equalizer { action } => match action.unwrap_or(EqualizerAction::Get) {
            EqualizerAction::Get => {
                let eq = client.fetch_equalizer().await?;
                print_equalizer(eq);
            }
            EqualizerAction::Preset { preset } => {
                client.set_equalizer_preset(preset.into()).await?;
                let eq = client.fetch_equalizer().await?;
                print_equalizer(eq);
            }
            EqualizerAction::Set {
                preset,
                bass,
                band_400,
                band_1000,
                band_2500,
                band_6300,
                band_16000,
            } => {
                let overrides = [bass, band_400, band_1000, band_2500, band_6300, band_16000];
                client.set_equalizer_bands(preset.into(), overrides).await?;
                let eq = client.fetch_equalizer().await?;
                print_equalizer(eq);
            }
        },
```

- [ ] **Step 9: Build and test**

Run: `cargo build -p sony-anc && cargo test -p sony-anc`
Expected: builds clean, all tests pass.

- [ ] **Step 10: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Add custom equalizer band editing to the CLI"
```

---

## Task 9: Sound-pressure readout

**Files:**
- Modify: `sony-anc/src/main.rs` (add `pressure` subcommand, `fetch_sound_pressure`, `print_pressure`)

- [ ] **Step 1: Add the `Pressure` subcommand**

In the `Action` enum, add a new variant (after `Equalizer`):

```rust
    /// Measure and report ambient sound pressure (dB)
    Pressure,
```

- [ ] **Step 2: Add `fetch_sound_pressure` to `SonyClient`**

Add this method to `impl SonyClient`:

```rust
    async fn fetch_sound_pressure(&mut self) -> Result<usize> {
        self.send_command(Command::SoundPressureMeasure { on: true })
            .await?;
        self.wait_for_payload(Duration::from_secs(2), |p| {
            matches!(p, Payload::SoundPressureMeasureReply { .. })
        })
        .await?;

        self.send_command(Command::GetSoundPressure).await?;
        let payload = self
            .wait_for_payload(Duration::from_secs(2), |p| {
                matches!(p, Payload::SoundPressure { .. })
            })
            .await?
            .ok_or_else(|| anyhow!("no sound pressure reply"))?;

        // Stop measuring before returning.
        self.send_command(Command::SoundPressureMeasure { on: false })
            .await?;
        let _ = self
            .wait_for_payload(Duration::from_millis(300), |_| false)
            .await;

        if let Payload::SoundPressure { db } = payload {
            Ok(db)
        } else {
            Err(anyhow!("unexpected payload while waiting for sound pressure"))
        }
    }
```

- [ ] **Step 3: Add `print_pressure`**

Add this free function near `print_codec`:

```rust
fn print_pressure(db: usize) {
    emit(WaybarOutput {
        text: format!("{db} dB"),
        tooltip: format!("Sound pressure: {db} dB"),
        class: "pressure".into(),
    });
}
```

- [ ] **Step 4: Wire the `Pressure` arm into `run`**

In the main `match action` block, add:

```rust
        Action::Pressure => {
            let db = client.fetch_sound_pressure().await?;
            print_pressure(db);
        }
```

- [ ] **Step 5: Build and test**

Run: `cargo build -p sony-anc && cargo test -p sony-anc`
Expected: builds clean, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Add sound-pressure readout subcommand"
```

---

## Task 10: `watch` streaming daemon

**Files:**
- Modify: `sony-anc/src/main.rs` (add `Watch` action, `should_emit`, `emit_anc_if_changed`, `process_bytes`, `watch_loop`, `run_watch`, signal imports, test)

- [ ] **Step 1: Write the failing test for `should_emit`**

In the `#[cfg(test)] mod tests` block, add:

```rust
    #[test]
    fn should_emit_only_on_change() {
        assert!(should_emit(None, Some(AncCliMode::Anc)));
        assert!(should_emit(Some(AncCliMode::Anc), Some(AncCliMode::Off)));
        assert!(should_emit(Some(AncCliMode::Anc), None));
        assert!(!should_emit(Some(AncCliMode::Anc), Some(AncCliMode::Anc)));
        assert!(!should_emit(None, None));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p sony-anc should_emit`
Expected: FAIL — `cannot find function should_emit`.

- [ ] **Step 3: Implement `should_emit` and `emit_anc_if_changed`**

Add these free functions near `print_output`:

```rust
/// Whether a new ANC mode differs from the last emitted one (dedupes output,
/// including repeated disconnected states represented as `None`).
fn should_emit(last: Option<AncCliMode>, new: Option<AncCliMode>) -> bool {
    last != new
}

/// Emit a Waybar line only when the ANC mode changed since the last emission.
/// `None` represents the disconnected state.
fn emit_anc_if_changed(last: &mut Option<AncCliMode>, state: Option<&AncState>) {
    let mode = state.map(|s| s.mode);
    if should_emit(*last, mode) {
        *last = mode;
        print_output(state.cloned());
    }
}
```

- [ ] **Step 4: Run to verify the test passes**

Run: `cargo test -p sony-anc should_emit`
Expected: PASS.

- [ ] **Step 5: Add `process_bytes` to `SonyClient`**

Add this method to `impl SonyClient`. It mirrors the frame-handling inside `wait_for_payload` but returns the latest ANC state parsed from the chunk:

```rust
    /// Feed received bytes through the frame parser, ack any command payloads,
    /// and return the most recent ANC state observed in this chunk (if any).
    async fn process_bytes(&mut self, bytes: &[u8]) -> Result<Option<AncState>> {
        let mut latest: Option<AncState> = None;
        for byte in bytes {
            match self.frame_parser.parse(std::slice::from_ref(byte)) {
                FrameParserResult::Ready { msg, .. } => {
                    if let Err(e) = msg.checksum {
                        eprintln!("ignoring bad checksum: {e}");
                        continue;
                    }
                    let Some(kind) = msg.kind.ok() else { continue };

                    if kind == sony_wf1000xm5::MessageType::Ack {
                        self.seq_number = msg.seq_num;
                        self.waiting_for_ack = false;
                        continue;
                    }

                    if kind == sony_wf1000xm5::MessageType::Command1
                        || kind == sony_wf1000xm5::MessageType::Command2
                    {
                        let payload = match parse_payload(msg.payload, kind) {
                            Ok(p) => p,
                            Err(e) => {
                                eprintln!("bad payload: {e}");
                                continue;
                            }
                        };

                        if let Ok(ack) =
                            sony_wf1000xm5::command::build_command(&Command::Ack, msg.seq_num)
                        {
                            let _ = self.stream.write_all(&ack).await;
                        }

                        if let Payload::AncStatus {
                            mode,
                            ambient_sound_level,
                            ..
                        } = payload
                        {
                            latest = Some(AncState {
                                mode: AncCliMode::from_anc_mode(mode),
                                ambient_level: ambient_sound_level,
                            });
                        }
                    }
                }
                FrameParserResult::Incomplete { .. } => {}
                FrameParserResult::Error { err, .. } => {
                    return Err(anyhow!("frame parser error: {err}"));
                }
            }
        }
        Ok(latest)
    }
```

- [ ] **Step 6: Add `watch_loop` to `SonyClient`**

Add this method to `impl SonyClient`:

```rust
    async fn watch_loop(
        &mut self,
        sig_next: &mut tokio::signal::unix::Signal,
        sig_prev: &mut tokio::signal::unix::Signal,
        last: &mut Option<AncCliMode>,
        current_level: &mut u8,
    ) -> Result<()> {
        let mut buf = [0u8; 64];
        loop {
            tokio::select! {
                read = self.stream.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        return Err(anyhow!("connection closed"));
                    }
                    if let Some(state) = self.process_bytes(&buf[..n]).await? {
                        *current_level = state.ambient_level;
                        emit_anc_if_changed(last, Some(&state));
                    }
                }
                _ = sig_next.recv() => {
                    let next = cycle_mode(last.unwrap_or(AncCliMode::Anc), CycleDirection::Next);
                    if let Ok(state) = self.set_mode(next, *current_level, None, None).await {
                        *current_level = state.ambient_level;
                        emit_anc_if_changed(last, Some(&state));
                    }
                }
                _ = sig_prev.recv() => {
                    let prev = cycle_mode(last.unwrap_or(AncCliMode::Anc), CycleDirection::Prev);
                    if let Ok(state) = self.set_mode(prev, *current_level, None, None).await {
                        *current_level = state.ambient_level;
                        emit_anc_if_changed(last, Some(&state));
                    }
                }
            }
        }
    }
```

- [ ] **Step 7: Add `run_watch`**

Add this free function near `connect`:

```rust
/// Long-lived watch loop: hold one connection, stream ANC changes to stdout,
/// react to SIGUSR1 (cycle next) / SIGUSR2 (cycle prev), and reconnect forever.
async fn run_watch(target: &Option<String>) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sig_next = signal(SignalKind::user_defined1())?;
    let mut sig_prev = signal(SignalKind::user_defined2())?;
    let mut last: Option<AncCliMode> = None;
    let mut current_level: u8 = DEFAULT_AMBIENT_LEVEL;

    loop {
        match connect(target).await {
            Ok(Some(mut client)) => {
                if let Ok(state) = client.fetch_anc_status().await {
                    current_level = state.ambient_level;
                    emit_anc_if_changed(&mut last, Some(&state));
                }
                if let Err(e) = client
                    .watch_loop(&mut sig_next, &mut sig_prev, &mut last, &mut current_level)
                    .await
                {
                    eprintln!("sony-anc watch: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("sony-anc watch: connect failed: {e}"),
        }
        // Mark disconnected (emits once) and back off before reconnecting.
        emit_anc_if_changed(&mut last, None);
        time::sleep(Duration::from_secs(5)).await;
    }
}
```

- [ ] **Step 8: Add the `Watch` action and early dispatch**

In the `Action` enum, add:

```rust
    /// Stream ANC status to stdout on every change (Waybar continuous exec)
    Watch,
```

In `run`, immediately after computing `let action = ...;` and before `let mut client = match connect(&target).await? {`, add:

```rust
    if matches!(action, Action::Watch) {
        return run_watch(&target).await;
    }
```

Then add an arm to the main `match action` block so it stays exhaustive:

```rust
        Action::Watch => unreachable!("watch is dispatched before connect"),
```

- [ ] **Step 9: Build and test**

Run: `cargo build -p sony-anc && cargo test -p sony-anc`
Expected: builds clean, all tests pass.

- [ ] **Step 10: Lint and format**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: no warnings, formatting clean. (If `fmt --check` reports diffs, run `cargo fmt` and re-stage.)

- [ ] **Step 11: Commit**

```bash
git add sony-anc/src/main.rs
git commit -m "Add streaming watch daemon with signal-driven cycling"
```

---

## Task 11: Document `watch` in the README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a usage line for `watch` and `pressure`**

In `README.md`, under the `## Usage (CLI)` code block, add after the `set` examples:

```bash
# stream status continuously (one line per change) for Waybar
sony-anc watch

# measure ambient sound pressure
sony-anc pressure

# nudge a single EQ band (writes the Manual preset, keeps other bands)
sony-anc equalizer set --bass 3
```

- [ ] **Step 2: Add a streaming Waybar snippet**

In `README.md`, after the existing polling Waybar snippet, add a new section:

````markdown
### Streaming mode (recommended)

`watch` holds a single Bluetooth connection open and prints a fresh line on every
change, so Waybar never polls. Cycle the mode by signaling the running process —
the buds only accept one app connection at a time, so a second `cycle` process is
not used.

```jsonc
"custom/sony_anc": {
  "format": "{text}",
  "return-type": "json",
  "exec": "$HOME/.local/bin/sony-anc watch",
  "on-click": "pkill -SIGUSR1 -f 'sony-anc watch'",
  "on-scroll-up": "pkill -SIGUSR1 -f 'sony-anc watch'",
  "on-scroll-down": "pkill -SIGUSR2 -f 'sony-anc watch'",
  "tooltip": true
}
```

SIGUSR1 cycles to the next mode; SIGUSR2 cycles to the previous mode. When the buds
disconnect, `watch` emits the `disconnected` class and reconnects automatically.

> A systemd `--user` unit is intentionally **not** provided for `watch`: Waybar must
> own the process so it can read the streamed stdout.
````

- [ ] **Step 2b: Note the deviation from the spec**

(The spec listed a systemd unit example; it is dropped here because the streaming-stdout model requires Waybar to own the process. This is captured in the README note above.)

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document watch streaming mode and new subcommands"
```

---

## Self-review notes

- **Spec coverage:** A1 payload tests → Tasks 2-4; A pure helpers → Task 5; B1 custom EQ → Task 8; B2 pressure → Task 9; B3 ambient tuning → Task 7; B4 emit refactor → Task 6; C watch daemon → Task 10; README → Task 11. Task 1 (derives) is enabling work for Section A assertions.
- **Spec deviation:** systemd unit dropped (incompatible with streaming-stdout ownership); documented in Task 11 README note.
- **Type consistency:** `set_mode(target, current_level, level_override, voice_override)` signature is defined in Task 7 and used identically in Task 7 callers and Task 10 `watch_loop`. `ambient_params`, `merge_bands`, `should_emit`, `emit_anc_if_changed`, `process_bytes`, `watch_loop`, `run_watch` are each defined once and referenced consistently. `AncState` derives `Clone` (existing) — required by `emit_anc_if_changed`'s `state.cloned()`.
- **No placeholders:** every code step contains complete code.
```
