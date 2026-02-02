# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust CLI for controlling Sony WF-1000XM5 earbuds ANC (Active Noise Cancellation) modes via Bluetooth RFCOMM. Designed for Waybar integration on Linux.

## Build Commands

```bash
cargo build --release          # Build optimized binary
cargo test                     # Run all tests
cargo clippy                   # Lint
cargo fmt                      # Format
```

Install to local bin:
```bash
install -Dm755 target/release/sony-anc ~/.local/bin/sony-anc
```

## Architecture

Two-crate workspace:

- **sony-anc** (`sony-anc/src/main.rs`): CLI binary using Clap for argument parsing. Handles Bluetooth connection via bluer (BlueZ DBus), outputs JSON for Waybar consumption.

- **sony-wf1000xm5** (`sony-wf1000xm5/src/`): Protocol library implementing the binary RFCOMM protocol:
  - `command.rs` - Command encoding (Init, GetAncStatus, AncSet, Ack)
  - `frame_parser.rs` - Message framing with escape sequences (header 0x3e, trailer 0x3c)
  - `payload.rs` - Response parsing (ANC status, battery, equalizer)

## Protocol Flow

1. Client registers RFCOMM profile with Sony service UUID
2. Sends Init command, waits for InitReply
3. Commands use sequence numbers; server responds with Ack + payload
4. Client must Ack command payloads from server

## Key Constants

- Service UUID: `956C7B26-D49A-4BA8-B03F-B17D393CB6E2`
- Default device name: `WF-1000XM5`
- Environment override: `SONY_WF1000XM5_DEVICE`

## Waybar Output Format

JSON object with `text` (icon), `tooltip`, and `class` (anc/ambient/anc-off/disconnected/error) for CSS styling.
