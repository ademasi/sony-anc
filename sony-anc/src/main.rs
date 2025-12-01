use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bluer::rfcomm::{Profile, ProfileHandle, Role, Stream};
use bluer::{Adapter, Device, Session, Uuid};
use clap::{Parser, Subcommand, ValueEnum};
use futures::StreamExt;
use serde::Serialize;
use sony_wf1000xm5::{
    command::{AncMode, Command},
    frame_parser::{FrameParser, FrameParserResult},
    payload::{Payload, parse_payload},
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
    },
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
            let updated = client.set_mode(next, status.ambient_level).await?;
            print_output(Some(updated));
        }
        Action::Set { mode } => {
            let status = client
                .fetch_anc_status()
                .await
                .unwrap_or_else(|_| AncState {
                    mode,
                    ambient_level: DEFAULT_AMBIENT_LEVEL,
                });
            let updated = client.set_mode(mode, status.ambient_level).await?;
            print_output(Some(updated));
        }
    }

    Ok(())
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

    match serde_json::to_string(&output) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("failed to serialize output: {err}");
            println!("--");
        }
    }
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
        let bytes = sony_wf1000xm5::command::build_command(&command, self.seq_number);
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
                            let ack =
                                sony_wf1000xm5::command::build_command(&Command::Ack, msg.seq_num);
                            let _ = self.stream.write_all(&ack).await;

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

    async fn set_mode(&mut self, target: AncCliMode, current_level: u8) -> Result<AncState> {
        let level = if current_level == 0 {
            DEFAULT_AMBIENT_LEVEL
        } else {
            current_level.min(20)
        };
        let (ambient_level, voice_passthrough) = match target {
            AncCliMode::Anc => (0, false),
            AncCliMode::Ambient => (level, true),
            AncCliMode::Off => (0, false),
        };

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
