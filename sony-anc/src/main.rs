use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bluer::rfcomm::{Profile, ProfileHandle, Role, Stream};
use bluer::{Adapter, Device, Session, Uuid};
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use serde::Serialize;
use sony_wf1000xm5::{
    ProtocolVersion,
    command::{AncMode, BatteryType, Command, EqualizerPreset},
    frame_parser::{FrameParser, FrameParserResult},
    payload::{BatteryLevel, Payload, parse_payload},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time;

const DEFAULT_DEVICE_NAME: &str = "WF-1000XM";
// Sony's RFCOMM service UUIDs. Older protocol-v1 devices (e.g. WF-1000XM3) advertise
// the V1 UUID; newer v2 devices (e.g. WF-1000XM5) advertise the V2 UUID.
const SONY_SERVICE_UUID_V1: Uuid = Uuid::from_u128(0x96CC203E_5068_46AD_B32D_E316F5E069BA);
const SONY_SERVICE_UUID_V2: Uuid = Uuid::from_u128(0x956C7B26_D49A_4BA8_B03F_B17D393CB6E2);
const DEFAULT_AMBIENT_LEVEL: u8 = 10;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Waybar-friendly ANC switcher for Sony WF-1000XM5",
    propagate_version = true
)]
struct Cli {
    /// Device name (substring) or MAC address to target. Defaults to the first WF-1000XM5.
    #[arg(short, long)]
    device: Option<String>,

    /// Force the protocol model instead of auto-detecting (xm3 = v1, xm5 = v2).
    #[arg(long, value_enum)]
    model: Option<CliModel>,

    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Report ANC status (default action)
    Status,
    /// Cycle ANC mode
    Cycle {
        #[arg(value_enum, default_value_t = CycleDirection::Next)]
        direction: CycleDirection,
    },
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
    /// Show battery levels
    Battery,
    /// Show current audio codec
    Codec,
    /// Show or change equalizer settings
    Equalizer {
        #[command(subcommand)]
        action: Option<EqualizerAction>,
    },
    /// Measure and report ambient sound pressure (dB)
    Pressure,
    /// Stream ANC status to stdout on every change (Waybar continuous exec)
    Watch,
}

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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliEqualizerPreset {
    Off,
    Bright,
    Excited,
    Mellow,
    Relaxed,
    Vocal,
    TrebleBoost,
    BassBoost,
    Speech,
}

impl From<CliEqualizerPreset> for EqualizerPreset {
    fn from(p: CliEqualizerPreset) -> Self {
        match p {
            CliEqualizerPreset::Off => EqualizerPreset::Off,
            CliEqualizerPreset::Bright => EqualizerPreset::Bright,
            CliEqualizerPreset::Excited => EqualizerPreset::Excited,
            CliEqualizerPreset::Mellow => EqualizerPreset::Mellow,
            CliEqualizerPreset::Relaxed => EqualizerPreset::Relaxed,
            CliEqualizerPreset::Vocal => EqualizerPreset::Vocal,
            CliEqualizerPreset::TrebleBoost => EqualizerPreset::TrebleBoost,
            CliEqualizerPreset::BassBoost => EqualizerPreset::BassBoost,
            CliEqualizerPreset::Speech => EqualizerPreset::Speech,
        }
    }
}

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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CycleDirection {
    Next,
    Prev,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum AncCliMode {
    Anc,
    Off,
    Ambient,
}

impl From<AncCliMode> for AncMode {
    fn from(value: AncCliMode) -> Self {
        match value {
            AncCliMode::Anc => AncMode::ActiveNoiseCanceling,
            AncCliMode::Off => AncMode::Off,
            AncCliMode::Ambient => AncMode::AmbientSound,
        }
    }
}

impl AncCliMode {
    fn from_anc_mode(mode: AncMode) -> Self {
        match mode {
            AncMode::ActiveNoiseCanceling => AncCliMode::Anc,
            AncMode::Off => AncCliMode::Off,
            AncMode::AmbientSound => AncCliMode::Ambient,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliModel {
    Xm3,
    Xm5,
}

impl From<CliModel> for ProtocolVersion {
    fn from(m: CliModel) -> Self {
        match m {
            CliModel::Xm3 => ProtocolVersion::V1,
            CliModel::Xm5 => ProtocolVersion::V2,
        }
    }
}

/// Parse the SONY_WF1000XM5_MODEL env override (xm3/xm5/v1/v2, case-insensitive).
fn model_from_env() -> Option<ProtocolVersion> {
    match std::env::var("SONY_WF1000XM5_MODEL")
        .ok()?
        .to_ascii_lowercase()
        .as_str()
    {
        "xm3" | "v1" => Some(ProtocolVersion::V1),
        "xm5" | "v2" => Some(ProtocolVersion::V2),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
struct WaybarOutput {
    text: String,
    tooltip: String,
    class: String,
}

#[derive(Debug, Clone)]
struct AncState {
    mode: AncCliMode,
    ambient_level: u8,
}

/// Aggregated state the `watch` daemon renders: ANC mode plus the most recently
/// seen battery levels. Every field is populated purely from notifications the buds
/// push — the daemon never queries them, because any traffic on the control channel
/// audibly disturbs playback on the WF-1000XM3. Fields therefore stay `None` until
/// the buds first report them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WatchState {
    /// Whether the control channel is currently open. Tracked separately from
    /// `mode` because the daemon is passive: it knows it is connected before the
    /// buds have pushed any state.
    connected: bool,
    mode: Option<AncCliMode>,
    ambient_level: u8,
    left: Option<usize>,
    right: Option<usize>,
    case: Option<usize>,
}

struct SonyClient {
    // One-shot commands own the session/profile that produced the stream. The
    // `watch` daemon instead keeps them alive for the whole process, so these are
    // `None` there.
    _session: Option<Session>,
    _profile: Option<ProfileHandle>,
    stream: Stream,
    frame_parser: FrameParser,
    seq_number: u8,
    waiting_for_ack: bool,
    version: ProtocolVersion,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("sony-anc error: {err:?}");
        let out = WaybarOutput {
            text: "ERR".into(),
            tooltip: format!("sony-anc: {err}"),
            class: "error".into(),
        };
        if let Ok(json) = serde_json::to_string(&out) {
            println!("{json}");
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let target = cli
        .device
        .or_else(|| std::env::var("SONY_WF1000XM5_DEVICE").ok());
    let action = cli.action.unwrap_or(Action::Status);
    let version_override = cli.model.map(ProtocolVersion::from).or_else(model_from_env);

    if matches!(action, Action::Watch) {
        return run_watch(&target, version_override).await;
    }

    let mut client = match connect(&target, version_override).await? {
        Some(client) => client,
        None => {
            print_output(None);
            return Ok(());
        }
    };

    match action {
        Action::Status => {
            let status = client.fetch_anc_status().await?;
            print_output(Some(status));
        }
        Action::Cycle { direction } => {
            let status = client.fetch_anc_status().await?;
            let next = cycle_mode(status.mode, direction);
            let updated = client
                .set_mode(next, status.ambient_level, None, None)
                .await?;
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
        Action::Battery => {
            let hp = client.fetch_battery(BatteryType::Headphones).await?;
            let case = client.fetch_battery(BatteryType::Case).await?;
            print_battery(hp, case);
        }
        Action::Codec => {
            let codec = client.fetch_codec().await?;
            print_codec(codec);
        }
        Action::Pressure => {
            let db = client.fetch_sound_pressure().await?;
            print_pressure(db);
        }
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
        Action::Watch => unreachable!("watch is dispatched before connect"),
    }

    Ok(())
}

/// Whether a new ANC mode differs from the last emitted one (dedupes output,
/// including repeated disconnected states represented as `None`).
/// Render the combined watch state (ANC mode + battery) for Waybar. `mode == None`
/// renders the disconnected state.
fn render_watch(state: &WatchState) -> WaybarOutput {
    if !state.connected {
        return WaybarOutput {
            text: String::new(),
            tooltip: "WF-1000XM5 not connected".into(),
            class: "disconnected".into(),
        };
    }
    let (text, class) = match state.mode {
        // Icons chosen from the common Font Awesome set that ships with Nerd Fonts.
        Some(AncCliMode::Anc) => ("\u{f025}".to_string(), "anc".to_string()), // headphones
        Some(AncCliMode::Ambient) => ("\u{f028}".to_string(), "ambient".to_string()), // volume-up
        Some(AncCliMode::Off) => ("\u{f05e}".to_string(), "anc-off".to_string()), // ban
        // Connected, but the buds have not reported their mode yet.
        None => ("\u{f025}".to_string(), "connected".to_string()),
    };
    let mut tooltip = match state.mode {
        Some(mode) => format!("ANC: {mode:?}"),
        None => "ANC: waiting for status".to_string(),
    };
    if let (Some(l), Some(r)) = (state.left, state.right) {
        tooltip.push_str(&format!("\nL: {l}%  R: {r}%"));
    }
    if let Some(c) = state.case {
        tooltip.push_str(&format!("\nCase: {c}%"));
    }
    WaybarOutput {
        text,
        tooltip,
        class,
    }
}

/// Emit a Waybar line only when the watch state changed since the last emission.
fn emit_watch_if_changed(last: &mut WatchState, new: &WatchState) {
    if last != new {
        *last = new.clone();
        emit(render_watch(new));
    }
}

/// Serialize a Waybar output object to stdout, logging serialization failures.
fn emit(output: WaybarOutput) {
    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(err) => eprintln!("failed to serialize output: {err}"),
    }
}

fn print_output(state: Option<AncState>) {
    let output = match state {
        Some(state) => {
            let (text, class) = match state.mode {
                // Icons chosen from the common Font Awesome set that ships with Nerd Fonts.
                AncCliMode::Anc => ("\u{f025}".to_string(), "anc".to_string()), // headphones
                AncCliMode::Ambient => ("\u{f028}".to_string(), "ambient".to_string()), // volume-up
                AncCliMode::Off => ("\u{f05e}".to_string(), "anc-off".to_string()), // ban
            };
            WaybarOutput {
                text,
                tooltip: format!("ANC: {:?}", state.mode),
                class,
            }
        }
        None => WaybarOutput {
            text: String::new(),
            tooltip: "WF-1000XM5 not connected".into(),
            class: "disconnected".into(),
        },
    };

    emit(output);
}

fn print_battery(hp: BatteryLevel, case: BatteryLevel) {
    let output = match (&hp, &case) {
        (BatteryLevel::Headphones { left, right }, BatteryLevel::Case(case_pct)) => WaybarOutput {
            text: format!("L:{left}% R:{right}%"),
            tooltip: format!("Left: {left}%\nRight: {right}%\nCase: {case_pct}%"),
            class: "battery".into(),
        },
        _ => WaybarOutput {
            text: format!("{hp:?}"),
            tooltip: format!("Headphones: {hp:?}\nCase: {case:?}"),
            class: "battery".into(),
        },
    };
    emit(output);
}

fn print_pressure(db: usize) {
    emit(WaybarOutput {
        text: format!("{db} dB"),
        tooltip: format!("Sound pressure: {db} dB"),
        class: "pressure".into(),
    });
}

fn print_codec(codec: sony_wf1000xm5::payload::Codec) {
    let output = WaybarOutput {
        text: codec.as_str().to_string(),
        tooltip: format!("Codec: {}", codec.as_str()),
        class: "codec".into(),
    };
    emit(output);
}

fn print_equalizer(payload: Payload) {
    let output = if let Payload::Equalizer {
        preset,
        clear_bass,
        band_400,
        band_1000,
        band_2500,
        band_6300,
        band_16000,
    } = payload
    {
        WaybarOutput {
            text: format!("{preset}"),
            tooltip: format!(
                "Preset: {preset}\nBass: {clear_bass:+}\n400Hz: {band_400:+}\n1kHz: {band_1000:+}\n2.5kHz: {band_2500:+}\n6.3kHz: {band_6300:+}\n16kHz: {band_16000:+}"
            ),
            class: "equalizer".into(),
        }
    } else {
        WaybarOutput {
            text: "EQ".into(),
            tooltip: "Unknown equalizer state".into(),
            class: "equalizer".into(),
        }
    };
    emit(output);
}

/// Resolve the ambient sound level to use: fall back to the default when the
/// device reports 0, otherwise clamp the current level into the valid range.
fn ambient_level_for(current: u8) -> u8 {
    if current == 0 {
        DEFAULT_AMBIENT_LEVEL
    } else {
        current.min(20)
    }
}

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

fn cycle_mode(current: AncCliMode, direction: CycleDirection) -> AncCliMode {
    use AncCliMode as M;
    match direction {
        CycleDirection::Next => match current {
            M::Anc => M::Ambient,
            M::Ambient => M::Off,
            M::Off => M::Anc,
        },
        CycleDirection::Prev => match current {
            M::Anc => M::Off,
            M::Ambient => M::Anc,
            M::Off => M::Ambient,
        },
    }
}

/// Long-lived watch loop: hold one connection, stream ANC changes to stdout, and
/// react to SIGUSR1 (cycle next) / SIGUSR2 (cycle prev), forever.
///
/// The daemon is deliberately **passive**: it registers Sony's control-channel
/// profiles once and then waits for BlueZ to hand it a connection. It never polls,
/// never forces a connection, and never queries the buds for state — every value in
/// the tooltip arrives from a notification the buds push on their own. On the
/// WF-1000XM3 any traffic on that channel audibly disturbs A2DP playback, and
/// forcing connections makes the controller page the device, which starves other
/// Bluetooth devices. Staying passive avoids both.
async fn run_watch(
    target: &Option<String>,
    version_override: Option<ProtocolVersion>,
) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sig_next = signal(SignalKind::user_defined1())?;
    let mut sig_prev = signal(SignalKind::user_defined2())?;
    let mut last_emitted = WatchState::default();

    // Emit the disconnected state immediately on startup. Without this, the first
    // disconnected state (equal to the default `last_emitted`) would be suppressed
    // by the change check and the bar would stay empty until the buds connect.
    emit(render_watch(&WatchState::default()));

    // Register both Sony control-channel UUIDs once, for the lifetime of the
    // process: v1 devices (WF-1000XM3) and v2 devices (WF-1000XM5) use different
    // ones, and registering costs nothing on the radio.
    let session = Session::new().await?;
    let mut profile_v1 = session
        .register_profile(sony_profile(SONY_SERVICE_UUID_V1))
        .await?;
    let mut profile_v2 = session
        .register_profile(sony_profile(SONY_SERVICE_UUID_V2))
        .await?;

    loop {
        // Block until the buds give us the control channel.
        let stream = wait_for_channel(&session, target, &mut profile_v1, &mut profile_v2).await?;

        let mut client = SonyClient {
            _session: None,
            _profile: None,
            stream,
            frame_parser: FrameParser::new(),
            seq_number: 0,
            waiting_for_ack: false,
            version: version_override.unwrap_or(ProtocolVersion::V2),
        };

        // The Init exchange is the only write the daemon makes on its own; the
        // device will not report anything until it has been greeted.
        if let Err(e) = client.init_handshake(version_override).await {
            eprintln!("sony-anc watch: init failed: {e}");
            continue;
        }

        // Connected, but nothing is known yet — state fills in as the buds push it.
        let mut state = WatchState {
            connected: true,
            ..Default::default()
        };
        emit_watch_if_changed(&mut last_emitted, &state);

        if let Err(e) = client
            .watch_loop(&mut sig_next, &mut sig_prev, &mut state, &mut last_emitted)
            .await
        {
            eprintln!("sony-anc watch: {e}");
        }

        // Channel closed: report disconnected and go back to waiting passively.
        emit_watch_if_changed(&mut last_emitted, &WatchState::default());
    }
}

async fn connect(
    device_hint: &Option<String>,
    version_override: Option<ProtocolVersion>,
) -> Result<Option<SonyClient>> {
    let session = Session::new().await?;
    let adapter = match session.default_adapter().await {
        Ok(adapter) => adapter,
        Err(_) => return Ok(None),
    };

    let Some(device) = find_device(&adapter, device_hint.as_deref()).await? else {
        return Ok(None);
    };

    if !device.is_connected().await.unwrap_or(false) {
        return Ok(None);
    }

    // Pick the Sony RFCOMM service UUID the device actually advertises: v1 devices
    // (WF-1000XM3) and v2 devices (WF-1000XM5) use different UUIDs. Prefer v2, fall
    // back to v1, then default to v2.
    let advertised = device.uuids().await.ok().flatten().unwrap_or_default();
    let service_uuid = if advertised.contains(&SONY_SERVICE_UUID_V2) {
        SONY_SERVICE_UUID_V2
    } else if advertised.contains(&SONY_SERVICE_UUID_V1) {
        SONY_SERVICE_UUID_V1
    } else {
        SONY_SERVICE_UUID_V2
    };

    let profile = Profile {
        uuid: service_uuid,
        role: Some(Role::Client),
        auto_connect: Some(true),
        ..Default::default()
    };

    let mut profile_handle = session.register_profile(profile).await?;

    // Ensure the base BT connection is active before waiting for RFCOMM.
    let _ = device.connect().await;

    let connect_timeout = Duration::from_secs(5);
    let connection_request = time::timeout(connect_timeout, profile_handle.next())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("timed out waiting for WF-1000XM5 service"))?;

    let stream = connection_request
        .accept()
        .context("failed to accept rfcomm connection")?;

    let mut client = SonyClient {
        _session: Some(session),
        _profile: Some(profile_handle),
        stream,
        frame_parser: FrameParser::new(),
        seq_number: 0,
        waiting_for_ack: false,
        version: version_override.unwrap_or(ProtocolVersion::V2),
    };

    client.init_handshake(version_override).await?;

    Ok(Some(client))
}

/// How long the daemon must wait between asking BlueZ to open the control channel.
/// Stops a burst of device property changes from turning into repeated attempts.
const NUDGE_MIN_INTERVAL: Duration = Duration::from_secs(10);
/// Backstop for the (unlikely) case where the buds reconnect without BlueZ emitting
/// any property change we can see.
const NUDGE_FALLBACK: Duration = Duration::from_secs(300);

/// The Sony control-channel UUID a device actually advertises: v1 devices
/// (WF-1000XM3) and v2 devices (WF-1000XM5) use different ones.
async fn sony_service_uuid(device: &Device) -> Uuid {
    let advertised = device.uuids().await.ok().flatten().unwrap_or_default();
    if advertised.contains(&SONY_SERVICE_UUID_V2) {
        SONY_SERVICE_UUID_V2
    } else if advertised.contains(&SONY_SERVICE_UUID_V1) {
        SONY_SERVICE_UUID_V1
    } else {
        SONY_SERVICE_UUID_V2
    }
}

/// Locate the target device. This is a pure DBus lookup — it costs no radio traffic.
async fn lookup_device(session: &Session, device_hint: &Option<String>) -> Option<Device> {
    let adapter = session.default_adapter().await.ok()?;
    find_device(&adapter, device_hint.as_deref()).await.ok()?
}

/// Wait for the buds to give us a control channel.
///
/// BlueZ only auto-connects a registered profile at registration time, not when the
/// device later reconnects, so waiting alone would miss every reconnect. Instead the
/// daemon watches the device's DBus properties — free, no radio — and when the
/// device is present asks BlueZ to connect *just* our profile. That is one targeted
/// action per connection rather than a poll, and because the device is already
/// linked at that point it does not make the controller page anything.
async fn wait_for_channel(
    session: &Session,
    device_hint: &Option<String>,
    profile_v1: &mut ProfileHandle,
    profile_v2: &mut ProfileHandle,
) -> Result<Stream> {
    let mut last_nudge: Option<Instant> = None;

    loop {
        let device = lookup_device(session, device_hint).await;

        if let Some(dev) = &device
            && dev.is_connected().await.unwrap_or(false)
            && last_nudge.is_none_or(|t| t.elapsed() >= NUDGE_MIN_INTERVAL)
        {
            last_nudge = Some(Instant::now());
            let uuid = sony_service_uuid(dev).await;
            let _ = dev.connect_profile(&uuid).await;
        }

        let mut events = match &device {
            Some(dev) => dev.events().await.ok(),
            None => None,
        };

        let request = tokio::select! {
            r = profile_v1.next() => r,
            r = profile_v2.next() => r,
            // Any device property change (connecting, services resolving, ...) is a
            // hint that it is worth trying again.
            _ = async {
                match events.as_mut() {
                    Some(e) => { e.next().await; }
                    None => std::future::pending::<()>().await,
                }
            } => None,
            _ = time::sleep(NUDGE_FALLBACK) => None,
        };

        if let Some(request) = request {
            match request.accept() {
                Ok(stream) => return Ok(stream),
                Err(e) => eprintln!("sony-anc watch: failed to accept connection: {e}"),
            }
        }
    }
}

/// Registration for one of Sony's RFCOMM control-channel UUIDs. Registering is a
/// local DBus operation and costs no radio traffic; with `auto_connect` BlueZ hands
/// us the channel whenever the device brings it up, so we never have to force a
/// connection ourselves.
fn sony_profile(uuid: Uuid) -> Profile {
    Profile {
        uuid,
        role: Some(Role::Client),
        auto_connect: Some(true),
        ..Default::default()
    }
}

impl SonyClient {
    /// Perform the mandatory Init exchange. This is the one write the device
    /// requires before it will talk to us; the protocol version is adopted from the
    /// reply length unless the caller forced one.
    async fn init_handshake(&mut self, version_override: Option<ProtocolVersion>) -> Result<()> {
        self.send_command(Command::Init).await?;
        let reply = self
            .wait_for_payload(Duration::from_secs(3), |p| {
                matches!(p, Payload::InitReply { .. })
            })
            .await?
            .ok_or_else(|| anyhow!("no init reply from device"))?;
        if version_override.is_none()
            && let Payload::InitReply { version } = reply
        {
            self.version = version;
        }
        Ok(())
    }

    async fn send_command(&mut self, command: Command) -> Result<()> {
        if self.waiting_for_ack {
            return Err(anyhow!("still waiting for ack from previous command"));
        }
        let bytes = sony_wf1000xm5::command::build_command(&command, self.seq_number, self.version)
            .context("failed to build command")?;
        self.waiting_for_ack = !matches!(command, Command::Ack);
        self.stream
            .write_all(&bytes)
            .await
            .context("failed to write command")
    }

    async fn wait_for_payload<F>(
        &mut self,
        timeout: Duration,
        mut predicate: F,
    ) -> Result<Option<Payload>>
    where
        F: FnMut(&Payload) -> bool,
    {
        let start = Instant::now();
        let mut buf = [0u8; 64];

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }
            let remaining = timeout - elapsed;
            let read_len = time::timeout(remaining, self.stream.read(&mut buf)).await;
            let n = match read_len {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => return Ok(None),
            };

            if n == 0 {
                return Err(anyhow!("connection closed"));
            }

            for byte in &buf[..n] {
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
                            // Ack first, whether or not we can interpret the payload:
                            // unacked messages are retransmitted indefinitely.
                            if let Ok(ack) = sony_wf1000xm5::command::build_command(
                                &Command::Ack,
                                msg.seq_num,
                                self.version,
                            ) {
                                let _ = self.stream.write_all(&ack).await;
                            }

                            let payload = match parse_payload(msg.payload, kind, self.version) {
                                Ok(p) => p,
                                Err(e) => {
                                    eprintln!("bad payload: {e}");
                                    continue;
                                }
                            };

                            if predicate(&payload) {
                                return Ok(Some(payload));
                            }
                        }
                    }
                    FrameParserResult::Incomplete { .. } => {}
                    FrameParserResult::Error { err, .. } => {
                        return Err(anyhow!("frame parser error: {err}"));
                    }
                }
            }
        }
    }

    async fn fetch_anc_status(&mut self) -> Result<AncState> {
        self.send_command(Command::GetAncStatus).await?;
        let payload = self
            .wait_for_payload(Duration::from_secs(2), |p| {
                matches!(p, Payload::AncStatus { .. })
            })
            .await?
            .ok_or_else(|| anyhow!("no ANC status reply"))?;

        if let Payload::AncStatus {
            mode,
            ambient_sound_level,
            ..
        } = payload
        {
            Ok(AncState {
                mode: AncCliMode::from_anc_mode(mode),
                ambient_level: ambient_sound_level,
            })
        } else {
            Err(anyhow!("unexpected payload while waiting for anc status"))
        }
    }

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

    /// Apply a mode without reading the result back.
    ///
    /// The `watch` daemon uses this instead of [`Self::set_mode`]: the buds push an
    /// ANC-status notification after a change anyway, so asking for it would be a
    /// redundant query on a channel we are trying to keep quiet. The caller updates
    /// its state optimistically and the pushed notification corrects it if needed —
    /// which also makes click-to-cycle feel instant.
    async fn apply_mode(&mut self, target: AncCliMode, current_level: u8) -> Result<()> {
        let (ambient_level, voice_passthrough) = ambient_params(target, current_level, None, None);
        self.send_command(Command::AncSet {
            dragging_ambient_sound_slider: false,
            mode: target.into(),
            ambient_sound_voice_passthrough: voice_passthrough,
            ambient_sound_level: ambient_level as usize,
        })
        .await
    }

    async fn fetch_battery(&mut self, battery_type: BatteryType) -> Result<BatteryLevel> {
        self.send_command(Command::GetBatteryStatus { battery_type })
            .await?;
        let payload = self
            .wait_for_payload(Duration::from_secs(2), |p| {
                matches!(p, Payload::BatteryLevel(..))
            })
            .await?
            .ok_or_else(|| anyhow!("no battery level reply"))?;

        if let Payload::BatteryLevel(level) = payload {
            Ok(level)
        } else {
            Err(anyhow!(
                "unexpected payload while waiting for battery level"
            ))
        }
    }

    async fn fetch_codec(&mut self) -> Result<sony_wf1000xm5::payload::Codec> {
        self.send_command(Command::GetCodec).await?;
        let payload = self
            .wait_for_payload(Duration::from_secs(2), |p| {
                matches!(p, Payload::Codec { .. })
            })
            .await?
            .ok_or_else(|| anyhow!("no codec reply"))?;

        if let Payload::Codec { codec } = payload {
            Ok(codec)
        } else {
            Err(anyhow!("unexpected payload while waiting for codec"))
        }
    }

    async fn fetch_equalizer(&mut self) -> Result<Payload> {
        self.send_command(Command::GetEqualizerSettings).await?;
        self.wait_for_payload(Duration::from_secs(2), |p| {
            matches!(p, Payload::Equalizer { .. })
        })
        .await?
        .ok_or_else(|| anyhow!("no equalizer reply"))
    }

    async fn set_equalizer_preset(&mut self, preset: EqualizerPreset) -> Result<()> {
        self.send_command(Command::ChangeEqualizerPreset { preset })
            .await?;
        let _ = self
            .wait_for_payload(Duration::from_millis(500), |_| false)
            .await;
        Ok(())
    }

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
        let [
            bass_level,
            band_400,
            band_1000,
            band_2500,
            band_6300,
            band_16000,
        ] = merge_bands(base, overrides);

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

    async fn fetch_sound_pressure(&mut self) -> Result<usize> {
        if self.version == ProtocolVersion::V1 {
            return Err(anyhow!("sound pressure is not supported on the WF-1000XM3"));
        }
        self.send_command(Command::SoundPressureMeasure { on: true })
            .await?;
        self.wait_for_payload(Duration::from_secs(2), |p| {
            matches!(p, Payload::SoundPressureMeasureReply { .. })
        })
        .await?
        .ok_or_else(|| anyhow!("no sound-pressure measure reply from device"))?;

        // Read the value, but always stop measuring afterwards regardless of outcome.
        let result = self.read_sound_pressure().await;

        // Best-effort stop; don't mask the real result.
        let _ = self
            .send_command(Command::SoundPressureMeasure { on: false })
            .await;
        let _ = self
            .wait_for_payload(Duration::from_millis(300), |_| false)
            .await;

        result
    }

    /// Feed received bytes through the frame parser, ack any command payloads,
    /// and return the most recent ANC state observed in this chunk (if any).
    /// Feed received bytes through the frame parser, ack command payloads, and fold
    /// any ANC-status or battery payloads into `state`.
    async fn process_bytes(&mut self, bytes: &[u8], state: &mut WatchState) -> Result<()> {
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
                        // Acknowledge receipt BEFORE trying to interpret the
                        // contents. The device retransmits anything it has not seen
                        // acked, so making the ack conditional on us understanding
                        // the payload turns every unknown message type into endless
                        // chatter on the control channel — which is audible on the
                        // WF-1000XM3.
                        if let Ok(ack) = sony_wf1000xm5::command::build_command(
                            &Command::Ack,
                            msg.seq_num,
                            self.version,
                        ) {
                            let _ = self.stream.write_all(&ack).await;
                        }

                        let payload = match parse_payload(msg.payload, kind, self.version) {
                            Ok(p) => p,
                            Err(e) => {
                                eprintln!("bad payload: {e}");
                                continue;
                            }
                        };

                        match payload {
                            Payload::AncStatus {
                                mode,
                                ambient_sound_level,
                                ..
                            } => {
                                state.mode = Some(AncCliMode::from_anc_mode(mode));
                                state.ambient_level = ambient_sound_level;
                            }
                            Payload::BatteryLevel(BatteryLevel::Headphones { left, right }) => {
                                state.left = Some(left);
                                state.right = Some(right);
                            }
                            Payload::BatteryLevel(BatteryLevel::Case(case)) => {
                                state.case = Some(case);
                            }
                            _ => {}
                        }
                    }
                }
                FrameParserResult::Incomplete { .. } => {}
                FrameParserResult::Error { err, .. } => {
                    return Err(anyhow!("frame parser error: {err}"));
                }
            }
        }
        Ok(())
    }

    async fn watch_loop(
        &mut self,
        sig_next: &mut tokio::signal::unix::Signal,
        sig_prev: &mut tokio::signal::unix::Signal,
        state: &mut WatchState,
        last_emitted: &mut WatchState,
    ) -> Result<()> {
        let mut buf = [0u8; 64];
        loop {
            tokio::select! {
                read = self.stream.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        return Err(anyhow!("connection closed"));
                    }
                    self.process_bytes(&buf[..n], state).await?;
                    emit_watch_if_changed(last_emitted, state);
                }
                _ = sig_next.recv() => {
                    let next = cycle_mode(state.mode.unwrap_or(AncCliMode::Anc), CycleDirection::Next);
                    match self.apply_mode(next, state.ambient_level).await {
                        // Optimistic: the buds push the authoritative status shortly.
                        Ok(()) => {
                            state.mode = Some(next);
                            emit_watch_if_changed(last_emitted, state);
                        }
                        Err(e) => eprintln!("sony-anc watch: failed to cycle ANC mode: {e:?}"),
                    }
                }
                _ = sig_prev.recv() => {
                    let prev = cycle_mode(state.mode.unwrap_or(AncCliMode::Anc), CycleDirection::Prev);
                    match self.apply_mode(prev, state.ambient_level).await {
                        Ok(()) => {
                            state.mode = Some(prev);
                            emit_watch_if_changed(last_emitted, state);
                        }
                        Err(e) => eprintln!("sony-anc watch: failed to cycle ANC mode: {e:?}"),
                    }
                }
            }
        }
    }

    async fn read_sound_pressure(&mut self) -> Result<usize> {
        self.send_command(Command::GetSoundPressure).await?;
        let payload = self
            .wait_for_payload(Duration::from_secs(2), |p| {
                matches!(p, Payload::SoundPressure { .. })
            })
            .await?
            .ok_or_else(|| anyhow!("no sound pressure reply"))?;

        if let Payload::SoundPressure { db } = payload {
            Ok(db)
        } else {
            Err(anyhow!(
                "unexpected payload while waiting for sound pressure"
            ))
        }
    }
}

async fn find_device(adapter: &Adapter, hint: Option<&str>) -> Result<Option<Device>> {
    let addresses = adapter.device_addresses().await?;
    let hint_lower = hint.map(|h| h.to_ascii_lowercase());

    for addr in addresses {
        let device = adapter.device(addr)?;
        let name = device.name().await?;
        let addr_str = addr.to_string();
        let matches = if let Some(hint) = hint_lower.as_deref() {
            if hint.contains(":") {
                addr_str.to_ascii_lowercase() == hint
            } else {
                name.as_deref()
                    .map(|n| n.to_ascii_lowercase().contains(hint))
                    .unwrap_or(false)
            }
        } else {
            name.as_deref()
                .map(|n| n.contains(DEFAULT_DEVICE_NAME))
                .unwrap_or(false)
        };

        if matches {
            return Ok(Some(device));
        }
    }

    Ok(None)
}

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
        assert_eq!(
            cycle_mode(AncCliMode::Anc, CycleDirection::Next),
            AncCliMode::Ambient
        );
        assert_eq!(
            cycle_mode(AncCliMode::Ambient, CycleDirection::Next),
            AncCliMode::Off
        );
        assert_eq!(
            cycle_mode(AncCliMode::Off, CycleDirection::Next),
            AncCliMode::Anc
        );
    }

    #[test]
    fn cycle_prev_rotates_reverse() {
        assert_eq!(
            cycle_mode(AncCliMode::Anc, CycleDirection::Prev),
            AncCliMode::Off
        );
        assert_eq!(
            cycle_mode(AncCliMode::Off, CycleDirection::Prev),
            AncCliMode::Ambient
        );
        assert_eq!(
            cycle_mode(AncCliMode::Ambient, CycleDirection::Prev),
            AncCliMode::Anc
        );
    }

    #[test]
    fn ambient_params_defaults() {
        assert_eq!(
            ambient_params(AncCliMode::Ambient, 0, None, None),
            (DEFAULT_AMBIENT_LEVEL, true)
        );
        assert_eq!(
            ambient_params(AncCliMode::Ambient, 15, None, None),
            (15, true)
        );
    }

    #[test]
    fn ambient_params_overrides() {
        assert_eq!(
            ambient_params(AncCliMode::Ambient, 0, Some(7), None),
            (7, true)
        );
        assert_eq!(
            ambient_params(AncCliMode::Ambient, 0, Some(50), None),
            (20, true)
        ); // clamped
        assert_eq!(
            ambient_params(AncCliMode::Ambient, 0, None, Some(false)),
            (DEFAULT_AMBIENT_LEVEL, false)
        );
    }

    #[test]
    fn ambient_params_non_ambient_modes_zeroed() {
        assert_eq!(
            ambient_params(AncCliMode::Anc, 15, Some(7), Some(true)),
            (0, false)
        );
        assert_eq!(
            ambient_params(AncCliMode::Off, 15, Some(7), Some(true)),
            (0, false)
        );
    }

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

    #[test]
    fn render_watch_disconnected() {
        let out = render_watch(&WatchState::default());
        assert_eq!(out.class, "disconnected");
        assert_eq!(out.text, "");
    }

    #[test]
    fn render_watch_anc_with_battery() {
        let state = WatchState {
            connected: true,
            mode: Some(AncCliMode::Anc),
            ambient_level: 0,
            left: Some(70),
            right: Some(50),
            case: Some(30),
        };
        let out = render_watch(&state);
        assert_eq!(out.class, "anc");
        assert!(out.tooltip.contains("ANC: Anc"));
        assert!(out.tooltip.contains("L: 70%"));
        assert!(out.tooltip.contains("R: 50%"));
        assert!(out.tooltip.contains("Case: 30%"));
    }

    #[test]
    fn render_watch_connected_before_any_status() {
        // Passive mode: connected, but the buds have not pushed anything yet.
        let state = WatchState {
            connected: true,
            ..Default::default()
        };
        let out = render_watch(&state);
        assert_eq!(out.class, "connected");
        assert!(out.tooltip.contains("waiting for status"));
        assert!(
            !out.text.is_empty(),
            "module must stay visible when connected"
        );
    }

    #[test]
    fn render_watch_omits_missing_battery() {
        let state = WatchState {
            connected: true,
            mode: Some(AncCliMode::Off),
            ..Default::default()
        };
        let out = render_watch(&state);
        assert_eq!(out.class, "anc-off");
        assert_eq!(out.tooltip, "ANC: Off");
    }
}
