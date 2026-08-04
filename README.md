# Sony WF-1000XM5 ANC helper (Waybar-ready)

A tiny Rust CLI to flip the ANC mode of Sony WF-1000XM5 earbuds and surface the state to Waybar.  
All low-level protocol code is reused from [`usering-around/sony-wf1000xm5-controller`](https://github.com/usering-around/sony-wf1000xm5-controller); this project only strips the GUI away and exposes a JSON-friendly interface for bars/scripts.

## Build & install

```bash
cargo build --release
install -Dm755 target/release/sony-anc ~/.local/bin/sony-anc
```

## Usage (CLI)

```bash
# status (default)
sony-anc

# cycle ANC -> Ambient -> Off
sony-anc cycle next

# set a specific mode
sony-anc set anc|ambient|off

# target a specific device (name substring or MAC)
sony-anc --device "WF-1000XM5" status

# stream status continuously (one line per change) for Waybar
sony-anc watch

# measure ambient sound pressure
sony-anc pressure

# nudge a single EQ band (writes the Manual preset, keeps other bands)
sony-anc equalizer set --bass 3
```

Environment override: `SONY_WF1000XM5_DEVICE` can be set to a name substring or MAC to pick the device.

## Waybar snippet

```jsonc
"custom/sony_anc": {
  "format": "{text}",
  "return-type": "json",
  "interval": 3,
  "signal": 12,
  "exec": "$HOME/.local/bin/sony-anc status",
  "on-click": "sh -c '$HOME/.local/bin/sony-anc cycle next; pkill -RTMIN+12 waybar'",
  "on-scroll-up": "sh -c '$HOME/.local/bin/sony-anc cycle next; pkill -RTMIN+12 waybar'",
  "on-scroll-down": "sh -c '$HOME/.local/bin/sony-anc cycle prev; pkill -RTMIN+12 waybar'",
  "tooltip": true
}
```

The module shows icons for ANC/Ambient/Off when connected and hides itself when the buds are disconnected (via the `disconnected` class styling). Click or scroll to cycle modes; the signal refresh forces an immediate redraw.

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

## Notes on protocol

The binary links against the `sony-wf1000xm5` crate in this repo, which is a direct copy of the upstream protocol library from `usering-around/sony-wf1000xm5-controller`. The RFCOMM UUID, frame parser, and ANC command payloads come from that source; only the surrounding CLI/Waybar glue was added here.
