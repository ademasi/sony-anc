use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bluer::rfcomm::{Profile, ProfileHandle, Role, Stream};
use bluer::{Adapter, Device, Session, Uuid};
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use serde::Serialize;
use sony_wf1000xm5::{
    command::{AncMode, BatteryType, Command, EqualizerPreset},
    frame_parser::{FrameParser, FrameParserResult},
    payload::{BatteryLevel, Payload, parse_payload},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time;

const DEFAULT_DEVICE_NAME: &str = "WF-1000XM5";
const SONY_SERVICE_UUID: Uuid = Uuid::from_u128(0x956C7B26_D49A_4BA8_B03F_B17D393CB6E2);
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

struct SonyClient {
    _session: Session,
    _profile: ProfileHandle,
    stream: Stream,
    frame_parser: FrameParser,
    seq_number: u8,
    waiting_for_ack: bool,
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

    if matches!(action, Action::Watch) {
        return run_watch(&target).await;
    }

    let mut client = match connect(&target).await? {
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

/// Long-lived watch loop: hold one connection, stream ANC changes to stdout,
/// react to SIGUSR1 (cycle next) / SIGUSR2 (cycle prev), and reconnect forever.
async fn run_watch(target: &Option<String>) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sig_next = signal(SignalKind::user_defined1())?;
    let mut sig_prev = signal(SignalKind::user_defined2())?;
    let mut last: Option<AncCliMode> = None;
    let mut current_level: u8 = DEFAULT_AMBIENT_LEVEL;

    // Emit the disconnected state immediately on startup. Without this, the
    // `last == None` sentinel collides with the disconnected state (also `None`),
    // so `emit_anc_if_changed(.., None)` would suppress the first line and the
    // bar would stay empty until the buds connect.
    print_output(None);

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

async fn connect(device_hint: &Option<String>) -> Result<Option<SonyClient>> {
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

    let profile = Profile {
        uuid: SONY_SERVICE_UUID,
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
        _session: session,
        _profile: profile_handle,
        stream,
        frame_parser: FrameParser::new(),
        seq_number: 0,
        waiting_for_ack: false,
    };

    client.send_command(Command::Init).await?;
    client
        .wait_for_payload(Duration::from_secs(3), |p| matches!(p, Payload::InitReply))
        .await?
        .ok_or_else(|| anyhow!("no init reply from device"))?;

    Ok(Some(client))
}

impl SonyClient {
    async fn send_command(&mut self, command: Command) -> Result<()> {
        if self.waiting_for_ack {
            return Err(anyhow!("still waiting for ack from previous command"));
        }
        let bytes = sony_wf1000xm5::command::build_command(&command, self.seq_number)
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
                            let payload = match parse_payload(msg.payload, kind) {
                                Ok(p) => p,
                                Err(e) => {
                                    eprintln!("bad payload: {e}");
                                    continue;
                                }
                            };

                            // respond with ACK
                            if let Ok(ack) =
                                sony_wf1000xm5::command::build_command(&Command::Ack, msg.seq_num)
                            {
                                let _ = self.stream.write_all(&ack).await;
                            }

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
    fn should_emit_only_on_change() {
        assert!(should_emit(None, Some(AncCliMode::Anc)));
        assert!(should_emit(Some(AncCliMode::Anc), Some(AncCliMode::Off)));
        assert!(should_emit(Some(AncCliMode::Anc), None));
        assert!(!should_emit(Some(AncCliMode::Anc), Some(AncCliMode::Anc)));
        assert!(!should_emit(None, None));
    }
}
