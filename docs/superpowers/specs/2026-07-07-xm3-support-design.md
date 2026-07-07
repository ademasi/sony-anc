# WF-1000XM3 Support (Sony protocol v1) Design

**Date:** 2026-07-07
**Status:** Approved (pre-implementation)

## Overview

Add support for the Sony **WF-1000XM3** earbuds to the existing tool, which
currently speaks only the WF-1000XM5's "protocol v2". The XM3 uses Sony's older
"protocol v1". The two share the entire framing layer; they differ only in a
handful of command encodings and response payload-type bytes.

Ground truth for every byte in this document is Gadgetbridge's `SonyProtocolImplV1`
and its unit tests (`SonyProtocolImplV1Test.java`, 40+ passing request assertions),
extracted verbatim into `scratchpad/xm3-v1-protocol.md` during design. Byte values
below are copied from that reference.

**Scope (full parity with current XM5 features):** ANC/ambient status + set, battery
(case + L/R), codec, equalizer (presets + custom bands). Out of scope: sound-pressure
/ NC-optimizer on XM3 (different v1 encoding), wind-mode as a distinct CLI subcommand,
DSEE/touch/button-mode configuration.

## What is shared (no changes)

Confirmed byte-identical between v1 and v2, so `frame_parser.rs`, the `lib.rs`
constants, `checksum`, the command envelope, `Command::Init`, and `Command::Ack`
are untouched:

- Framing: header `0x3e`, trailer `0x3c`, escape `0x3d`, mask `0b11101111`,
  1-byte running-sum checksum over bytes `[1..len-2]`.
- MessageType: `Ack=0x01`, `Command1=0x0c`, `Command2=0x0e`.
- Init request payload `00:00` on Command1; Ack = empty payload, seq `1 - seq`.
- EqualizerPreset codes and the "bass + 5 bands, offset +10" custom-band layout.
- Codec value table: `Unknown=0, Sbc=0x1, Aac=0x2, Ldac=0x10, Aptx=0x20, AptxHd=0x21`.

## v1 vs v2 encoding differences (the whole delta)

### Ambient sound control
- **GET**: v1 `[0x66, 0x02]` — v2 `[0x66, 0x17]`.
- **SET**: v1 is always an 8-byte payload
  `[0x68, 0x02, modeFlag, windFlag, modeDetail, 0x01, focus, level]` where (XM3
  declares `WindNoiseReduction`, so it uses the wind-capable encoding):
  - `modeFlag` = `0x00` if OFF else `0x11`
  - `windFlag` = `0x02` (wind-capable)
  - `modeDetail` = NoiseCancelling→`0x02`, WindNoiseReduction→`0x01`, Ambient/Off→`0x00`
  - `focus` = `0x01` if focus-on-voice else `0x00`
  - `level` = ambient level `0..20` for OFF/Ambient, `0x00` for NC/Wind
  - v2 differs: sub-flag `0x17`/`0x15`, `modeFlag` `0x00`/`0x01` (not `0x11`), a
    "dragging slider" byte, and a 7-byte buffer when wind unsupported.
- **Response parse** (v1, exactly 8 bytes): `payload[2]==0x00` → Off; `payload[2]==0x01`
  → enabled, then with `payload[3]==0x02`: `payload[4]` `0`=Ambient / `1`=Wind / `2`=NC.
  `focus = payload[6]==1`, `level = payload[7]`.
  **Note:** response `payload[2]` is a plain 0/1 bool, unlike the request's `0x11`
  sentinel. Wind, if reported, maps to `ambient` for the 3-mode CLI display.

### Battery
- **GET**: v1 `[0x10, typeCode]` — v2 `[0x22, typeCode]`.
- **Type codes**: v1 `SINGLE=0x00, DUAL=0x01, CASE=0x02` — v2 `Headphones=0x1(or 0x9), Case=0xa`.
- **Response**: type at `payload[1]`. DUAL: left=`payload[2]`, right=`payload[4]`
  (same offsets as v2; charging bytes at `[3]`/`[5]` exist but we don't surface them).
  CASE/SINGLE: level=`payload[2]`.
- For XM3 the CLI queries DUAL (`10:01`) + CASE (`10:02`).

### Audio codec
- **GET**: v1 `[0x18, 0x00]` — v2 `[0x12, 0x02]`.
- **Response** (v1, exactly 3 bytes): `[type, 0x00, codecCode]`; codec table identical.

### Equalizer
- **GET**: v1 `[0x56, 0x01]` — v2 `[0x56, 0x00]`.
- **SET preset**: v1 `[0x58, 0x01, presetCode, 0x00]` — v2 `[0x58, 0x00, presetCode, 0x00]`.
- **SET custom bands**: v1 `[0x58, 0x01, 0xff, 0x06, bass+10, b1+10..b5+10]` (10 bytes;
  preset marker is always `0xff`) — v2 uses the actual custom-preset code and sub-flag `0x00`.
  Under v1 the `--preset` custom-slot choice is not distinguished on the wire (always `0xff`).
- **Response parse**: identical to v2 — `preset=payload[2]`, `bass=payload[4]-10`,
  `bands=payload[5..10]-10` (10 bytes).

### Response payload-type table (differs)
`PayloadType::from_byte` must be version-aware. Differences under Command1:

| meaning | v1 reply / notify | v2 reply / notify |
|---|---|---|
| Battery | `0x11` / `0x13` | `0x23` / `0x25` |
| Codec | `0x19` / `0x1b` | `0x13` / `0x15` |
| Equalizer | `0x57` / `0x59` | same |
| Ambient | `0x67` / `0x69` | same |
| InitReply | `0x01` | `0x01` |

### Version detection
Determined from the **init-reply payload length**: **4 bytes → V1, 8 bytes → V2**
(Gadgetbridge's method). Detected during `connect` (the Init handshake is
version-agnostic). `InitReply` parsing is length-based and ignores the passed
version. A `--model xm3|xm5` flag and `SONY_WF1000XM5_MODEL` env override the
auto-detection.

## Architecture

Thread a `ProtocolVersion` through the two encode/parse entry points; keep the
unified `Command`/`Payload` enums so the CLI and watch loop are unaffected.

### `sony-wf1000xm5` (library)
- `lib.rs`: add `pub enum ProtocolVersion { V1, V2 }` (Copy, Eq).
- `command.rs`:
  - `build_command(command: &Command, seq: u8, version: ProtocolVersion)`.
  - `Command::to_bytes(&self, version)` branches only for: `AncSet`, `GetAncStatus`,
    `GetBatteryStatus`, `GetCodec`, `GetEqualizerSettings`, `ChangeEqualizerPreset`,
    `ChangeEqualizerSetting`. `Init`/`Ack` ignore version.
  - Battery-type encoding becomes version-aware (`BatteryType` → byte per version).
  - Sound-pressure commands (`SoundPressureMeasure`, `GetSoundPressure`) return
    `CommandError::UnsupportedForVersion { version }` when `version == V1`.
- `payload.rs`:
  - `parse_payload(bytes, message_type, version)` and
    `PayloadType::from_byte(message_type, byte, version)`.
  - `Payload::InitReply { version }` — carries the length-detected version.
  - AncStatus decode and BatteryType decode branch on version; codec/EQ parse shared.

### `sony-anc` (CLI)
- `SonyClient` gains a `version: ProtocolVersion` field, defaulting from detection.
- `connect`: after the Init handshake, read the `InitReply { version }` and set
  `client.version`; a `--model`/env override wins over the detected value.
- Add a top-level `--model xm3|xm5` arg (mapping to V1/V2) and
  `SONY_WF1000XM5_MODEL` env fallback.
- Every `send_command` / `parse_payload` call site passes `self.version`.
- `battery`: query DUAL+CASE under V1, Headphones+Case under V2 (the `BatteryType`
  enum values already map correctly per version in the library).
- `pressure` on a V1 device surfaces a clean "not supported on WF-1000XM3" error.
- Wind status maps to `ambient` for display.

## Testing

- **Encoding (high confidence):** port all v1 golden request vectors from the
  reference — ambient GET + the 15-row wind-capable SET table, battery
  (`10:00/01/02`), codec (`18:00`), EQ GET (`56:01`), all 11 preset SETs, and the
  5 custom-band SETs. Assert `build_command(.., V1)` produces framed bytes whose
  payload matches.
- **Parsing (source-derived, flagged lower confidence):** construct v1 response
  payloads per the verbatim logic and assert `parse_payload(.., V1)` — ambient (8-byte,
  incl. wind→ambient), battery dual+case, codec (`1b:00:01` is the one upstream-verified
  vector), equalizer. Comment these as logic-derived, not upstream-byte-verified
  (except codec).
- **Regression:** all existing v2 tests stay green unchanged (they call the
  version-aware functions with `ProtocolVersion::V2`).

## Risks / caveats

- Response-side v1 parsing (ambient, battery, EQ) has no upstream byte-level golden
  test in Gadgetbridge — only the request encodings and the codec response are
  test-backed. Logic is transcribed verbatim but should be validated against a real
  XM3 when one is available.
- `INIT_2` (`0x06/0x07`), `SOMETHING` (`0x90/0x91`), and JSON (`0xc4/0xc9`) v1
  payloads are unimplemented even upstream — not needed for our scope.

## Out of scope (deferred)

Sound-pressure on XM3, wind as its own subcommand, DSEE/upsampling toggle, touch/
button-mode config, firmware/power-off. The XM5 socket-daemon idea remains separate.
