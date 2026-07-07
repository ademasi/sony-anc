use crate::{ESCAPE_BYTE, ESCAPE_MASK, MESSAGE_HEADER, MESSAGE_TRAILER, MessageType, ProtocolVersion, checksum};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("ambient sound level {0} exceeds maximum of 20")]
    AmbientSoundLevelOutOfRange(usize),
    #[error("equalizer band value {value} out of range [-10, 10] for {band}")]
    EqualizerBandOutOfRange { band: &'static str, value: i8 },
    #[error(
        "equalizer preset {preset:?} does not support custom settings (use Manual, Custom1, or Custom2)"
    )]
    EqualizerPresetNotCustomizable { preset: EqualizerPreset },
    #[error("command not supported on protocol {version:?}")]
    UnsupportedForVersion { version: ProtocolVersion },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqualizerPreset {
    Off = 0x0,
    Bright = 0x10,
    Excited = 0x11,
    Mellow = 0x12,
    Relaxed = 0x13,
    Vocal = 0x14,
    TrebleBoost = 0x15,
    BassBoost = 0x16,
    Speech = 0x17,
    Manual = 0xa0,
    Custom1 = 0xa1,
    Custom2 = 0xa2,
}

impl EqualizerPreset {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x0 => Self::Off,
            0x10 => Self::Bright,
            0x11 => Self::Excited,
            0x12 => Self::Mellow,
            0x13 => Self::Relaxed,
            0x14 => Self::Vocal,
            0x15 => Self::TrebleBoost,
            0x16 => Self::BassBoost,
            0x17 => Self::Speech,
            0xa0 => Self::Manual,
            0xa1 => Self::Custom1,
            0xa2 => Self::Custom2,
            _ => return None,
        })
    }
}

impl std::fmt::Display for EqualizerPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AncMode {
    Off,
    ActiveNoiseCanceling,
    AmbientSound,
}

#[derive(Clone, Copy, Debug)]
pub enum BatteryType {
    Headphones = 0x1,
    Case = 0xa,
}

impl BatteryType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x1 | 0x9 => Self::Headphones,
            0xa => Self::Case,
            _ => return None,
        })
    }

    /// Wire byte for a battery-status request, which differs between protocol versions.
    pub fn to_byte(self, version: ProtocolVersion) -> u8 {
        match version {
            // v2 discriminants: Headphones = 0x1, Case = 0xa
            ProtocolVersion::V2 => self as u8,
            // v1 codes: DUAL = 0x01, CASE = 0x02
            ProtocolVersion::V1 => match self {
                BatteryType::Headphones => 0x01,
                BatteryType::Case => 0x02,
            },
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Init,
    Ack,
    AncSet {
        dragging_ambient_sound_slider: bool,
        mode: AncMode,
        ambient_sound_voice_passthrough: bool,
        ambient_sound_level: usize,
    },
    GetAncStatus,

    ChangeEqualizerPreset {
        preset: EqualizerPreset,
    },
    ChangeEqualizerSetting {
        // the preset to change the equalizer settings for
        preset: EqualizerPreset,
        bass_level: i8,
        band_400: i8,
        band_1000: i8,
        band_2500: i8,
        band_6300: i8,
        band_16000: i8,
    },
    GetBatteryStatus {
        battery_type: BatteryType,
    },
    GetEqualizerSettings,
    GetCodec,
    SoundPressureMeasure {
        on: bool,
    },
    GetSoundPressure,
}

impl Command {
    const EQUALIZER_SET: u8 = 0x58;
    const ANC_SET: u8 = 0x68;
    const ANC_STATUS_GET: u8 = 0x66;
    const SUPPORTS_AMBIENT_SOUND_CONTROL_2: u8 = 0x17;
    const GET_BATTERY_STATUS: u8 = 0x22;
    const EQUALIZER_GET: u8 = 0x56;
    const CODEC_GET: u8 = 0x12;
    fn to_bytes(&self, version: ProtocolVersion) -> Result<Vec<u8>, CommandError> {
        Ok(match self {
            Self::Init => {
                vec![0, 0]
            }

            Self::Ack => {
                vec![]
            }

            Command::AncSet {
                dragging_ambient_sound_slider,
                mode,
                ambient_sound_voice_passthrough,
                ambient_sound_level,
            } => {
                if *ambient_sound_level > 20 {
                    return Err(CommandError::AmbientSoundLevelOutOfRange(
                        *ambient_sound_level,
                    ));
                }
                match version {
                    ProtocolVersion::V2 => {
                        let mut out = vec![
                            Self::ANC_SET,
                            Self::SUPPORTS_AMBIENT_SOUND_CONTROL_2,
                            if *dragging_ambient_sound_slider { 0 } else { 1 },
                        ];
                        if *mode == AncMode::Off {
                            out.push(0);
                        } else {
                            out.push(1);
                        }
                        if *mode == AncMode::AmbientSound {
                            out.push(1);
                        } else {
                            out.push(0);
                        }
                        out.push(if *ambient_sound_voice_passthrough { 1 } else { 0 });
                        out.push(*ambient_sound_level as u8);
                        out
                    }
                    ProtocolVersion::V1 => {
                        // WF-1000XM3 wind-capable ambient sound control (fixed 8 bytes).
                        let mode_flag = if *mode == AncMode::Off { 0x00 } else { 0x11 };
                        let mode_detail = match mode {
                            AncMode::ActiveNoiseCanceling => 0x02,
                            AncMode::AmbientSound | AncMode::Off => 0x00,
                        };
                        let level = match mode {
                            AncMode::Off | AncMode::AmbientSound => *ambient_sound_level as u8,
                            AncMode::ActiveNoiseCanceling => 0,
                        };
                        vec![
                            Self::ANC_SET,
                            0x02,
                            mode_flag,
                            0x02,
                            mode_detail,
                            0x01,
                            if *ambient_sound_voice_passthrough { 1 } else { 0 },
                            level,
                        ]
                    }
                }
            }

            Self::GetAncStatus => match version {
                ProtocolVersion::V2 => {
                    vec![Self::ANC_STATUS_GET, Self::SUPPORTS_AMBIENT_SOUND_CONTROL_2]
                }
                ProtocolVersion::V1 => vec![Self::ANC_STATUS_GET, 0x02],
            },

            Self::ChangeEqualizerPreset { preset } => {
                let sub_flag = match version {
                    ProtocolVersion::V2 => 0,
                    ProtocolVersion::V1 => 1,
                };
                vec![Self::EQUALIZER_SET, sub_flag, *preset as u8, 0]
            }
            Self::ChangeEqualizerSetting {
                preset,
                bass_level,
                band_400,
                band_1000,
                band_2500,
                band_6300,
                band_16000,
            } => {
                fn check_band(name: &'static str, value: i8) -> Result<(), CommandError> {
                    if value.abs() > 10 {
                        return Err(CommandError::EqualizerBandOutOfRange { band: name, value });
                    }
                    Ok(())
                }
                check_band("bass", *bass_level)?;
                check_band("400Hz", *band_400)?;
                check_band("1kHz", *band_1000)?;
                check_band("2.5kHz", *band_2500)?;
                check_band("6.3kHz", *band_6300)?;
                check_band("16kHz", *band_16000)?;
                if !matches!(
                    preset,
                    EqualizerPreset::Manual | EqualizerPreset::Custom1 | EqualizerPreset::Custom2
                ) {
                    return Err(CommandError::EqualizerPresetNotCustomizable { preset: *preset });
                }

                let data_size = 6; // bass level + 5 bands
                match version {
                    ProtocolVersion::V2 => vec![
                        Self::EQUALIZER_SET,
                        0,
                        *preset as u8,
                        data_size,
                        (bass_level + 10) as u8,
                        (band_400 + 10) as u8,
                        (band_1000 + 10) as u8,
                        (band_2500 + 10) as u8,
                        (band_6300 + 10) as u8,
                        (band_16000 + 10) as u8,
                    ],
                    // v1 always writes the 0xff "custom" marker instead of the preset code.
                    ProtocolVersion::V1 => vec![
                        Self::EQUALIZER_SET,
                        1,
                        0xff,
                        data_size,
                        (bass_level + 10) as u8,
                        (band_400 + 10) as u8,
                        (band_1000 + 10) as u8,
                        (band_2500 + 10) as u8,
                        (band_6300 + 10) as u8,
                        (band_16000 + 10) as u8,
                    ],
                }
            }

            Self::GetBatteryStatus { battery_type } => {
                let cmd_byte = match version {
                    ProtocolVersion::V2 => Self::GET_BATTERY_STATUS,
                    ProtocolVersion::V1 => 0x10,
                };
                vec![cmd_byte, battery_type.to_byte(version)]
            }

            Self::GetEqualizerSettings => {
                let sub_flag = match version {
                    ProtocolVersion::V2 => 0,
                    ProtocolVersion::V1 => 1,
                };
                vec![Self::EQUALIZER_GET, sub_flag]
            }

            Self::GetCodec => match version {
                ProtocolVersion::V2 => vec![Self::CODEC_GET, 2],
                ProtocolVersion::V1 => vec![0x18, 0x00],
            },

            Self::SoundPressureMeasure { on } => match version {
                ProtocolVersion::V2 => {
                    // from HCI logs start: 3e0e0000000004580301006e3c
                    // from HCI logs stop:  3e0e0000000004580301016f3c
                    vec![0x58, 0x03, 0x01, if *on { 0x00 } else { 0x01 }]
                }
                ProtocolVersion::V1 => {
                    return Err(CommandError::UnsupportedForVersion { version });
                }
            },
            Self::GetSoundPressure => match version {
                ProtocolVersion::V2 => {
                    // from HCI logs: 3e0e01000000025a036e3c
                    vec![0x5a, 0x03]
                }
                ProtocolVersion::V1 => {
                    return Err(CommandError::UnsupportedForVersion { version });
                }
            },
        })
    }
}

fn push_escaped(vec: &mut Vec<u8>, byte: u8) {
    if matches!(byte, MESSAGE_HEADER | MESSAGE_TRAILER | ESCAPE_BYTE) {
        vec.push(ESCAPE_BYTE);
        vec.push(byte & ESCAPE_MASK);
    } else {
        vec.push(byte);
    }
}

// this comment is taken from https://github.com/Freeyourgadget/Gadgetbridge/blob/master/app/src/main/java/nodomain/freeyourgadget/gadgetbridge/service/devices/sony/headphones/protocol/Message.java#L79
/**
 * Message format:
 * <p>
 * - MESSAGE_HEADER
 * - Message Type ({@link MessageType})
 * - Sequence Number - needs to be updated with the one sent in the ACK responses
 * - Payload Length - 4-byte big endian int with number of bytes that will follow
 * - N bytes of payload data (first being the PayloadType)
 * - Checksum (1-byte sum, excluding header)
 * - MESSAGE_TRAILER
 * <p>
 * Data between MESSAGE_HEADER and MESSAGE_TRAILER is escaped with MESSAGE_ESCAPE, and the
 * following byte masked with MESSAGE_ESCAPE_MASK.
 */
/// Build a command to send the headphones
pub fn build_command(
    command: &Command,
    seq_number: u8,
    version: ProtocolVersion,
) -> Result<Vec<u8>, CommandError> {
    let cmd = command.to_bytes(version)?;
    let mut buf = Vec::with_capacity(cmd.len() + 7);
    let message_type = match command {
        Command::AncSet { .. }
        | Command::GetCodec
        | Command::GetAncStatus
        | Command::ChangeEqualizerSetting { .. }
        | Command::ChangeEqualizerPreset { .. }
        | Command::Init
        | Command::GetBatteryStatus { .. }
        | Command::GetEqualizerSettings => MessageType::Command1,

        // from hci logs: SoundPressureMeasure: 3e0e0000000004580301006e3c
        // from hci log: GetSoundPressure: 3e0e01000000025a036e3c
        Command::SoundPressureMeasure { .. } | Command::GetSoundPressure => MessageType::Command2,

        Command::Ack => MessageType::Ack,
    };
    buf.push(message_type as u8);
    if matches!(command, Command::Ack) {
        buf.push(1u8.wrapping_sub(seq_number));
    } else {
        buf.push(seq_number);
    }
    buf.extend((cmd.len() as u32).to_be_bytes());
    buf.extend(cmd);
    buf.push(checksum(&buf));
    let mut out = Vec::with_capacity(buf.len() + 9);
    out.push(MESSAGE_HEADER);
    for byte in buf {
        push_escaped(&mut out, byte);
    }
    out.push(MESSAGE_TRAILER);

    Ok(out)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ProtocolVersion;
    use crate::frame_parser::{FrameParser, FrameParserResult};

    /// Build a command as v1 and recover its (unescaped) payload bytes for
    /// comparison against Gadgetbridge's golden request vectors.
    fn v1_payload(cmd: Command) -> Vec<u8> {
        let frame = build_command(&cmd, 0, ProtocolVersion::V1).unwrap();
        let mut parser = FrameParser::new();
        match parser.parse(&frame) {
            FrameParserResult::Ready { msg, .. } => msg.payload.to_vec(),
            _ => panic!("frame did not parse"),
        }
    }

    #[test]
    fn v1_get_anc_status() {
        assert_eq!(v1_payload(Command::GetAncStatus), vec![0x66, 0x02]);
    }

    #[test]
    fn v1_get_codec() {
        assert_eq!(v1_payload(Command::GetCodec), vec![0x18, 0x00]);
    }

    #[test]
    fn v1_get_battery() {
        assert_eq!(
            v1_payload(Command::GetBatteryStatus { battery_type: BatteryType::Headphones }),
            vec![0x10, 0x01]
        );
        assert_eq!(
            v1_payload(Command::GetBatteryStatus { battery_type: BatteryType::Case }),
            vec![0x10, 0x02]
        );
    }

    #[test]
    fn v1_get_equalizer() {
        assert_eq!(v1_payload(Command::GetEqualizerSettings), vec![0x56, 0x01]);
    }

    #[test]
    fn v1_set_equalizer_preset() {
        assert_eq!(
            v1_payload(Command::ChangeEqualizerPreset { preset: EqualizerPreset::Bright }),
            vec![0x58, 0x01, 0x10, 0x00]
        );
        assert_eq!(
            v1_payload(Command::ChangeEqualizerPreset { preset: EqualizerPreset::Off }),
            vec![0x58, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn v1_set_equalizer_custom_bands() {
        // Golden: bands=[0,0,0,0,0] bass=0 -> 58:01:ff:06:0a:0a:0a:0a:0a:0a
        assert_eq!(
            v1_payload(Command::ChangeEqualizerSetting {
                preset: EqualizerPreset::Manual,
                bass_level: 0,
                band_400: 0,
                band_1000: 0,
                band_2500: 0,
                band_6300: 0,
                band_16000: 0,
            }),
            vec![0x58, 0x01, 0xff, 0x06, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a]
        );
        // Golden: bands=[10,0,3,0,5] bass=-2 -> 58:01:ff:06:08:14:0a:0d:0a:0f
        assert_eq!(
            v1_payload(Command::ChangeEqualizerSetting {
                preset: EqualizerPreset::Manual,
                bass_level: -2,
                band_400: 10,
                band_1000: 0,
                band_2500: 3,
                band_6300: 0,
                band_16000: 5,
            }),
            vec![0x58, 0x01, 0xff, 0x06, 0x08, 0x14, 0x0a, 0x0d, 0x0a, 0x0f]
        );
    }

    fn v1_anc_set(mode: AncMode, voice: bool, level: usize) -> Vec<u8> {
        v1_payload(Command::AncSet {
            dragging_ambient_sound_slider: false,
            mode,
            ambient_sound_voice_passthrough: voice,
            ambient_sound_level: level,
        })
    }

    #[test]
    fn v1_set_ambient_off() {
        // 68:02:00:02:00:01:00:00  (OFF, focus=false, level=0)
        assert_eq!(v1_anc_set(AncMode::Off, false, 0), vec![0x68, 0x02, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00]);
        // 68:02:00:02:00:01:01:0A  (OFF, focus=true, level=10)
        assert_eq!(v1_anc_set(AncMode::Off, true, 10), vec![0x68, 0x02, 0x00, 0x02, 0x00, 0x01, 0x01, 0x0a]);
    }

    #[test]
    fn v1_set_ambient_sound() {
        // 68:02:11:02:00:01:00:0A  (AMBIENT, focus=false, level=10)
        assert_eq!(v1_anc_set(AncMode::AmbientSound, false, 10), vec![0x68, 0x02, 0x11, 0x02, 0x00, 0x01, 0x00, 0x0a]);
        // 68:02:11:02:00:01:01:0F  (AMBIENT, focus=true, level=15)
        assert_eq!(v1_anc_set(AncMode::AmbientSound, true, 15), vec![0x68, 0x02, 0x11, 0x02, 0x00, 0x01, 0x01, 0x0f]);
    }

    #[test]
    fn v1_set_noise_cancelling() {
        // 68:02:11:02:02:01:00:00  (NC, focus=false, level=0)
        assert_eq!(v1_anc_set(AncMode::ActiveNoiseCanceling, false, 0), vec![0x68, 0x02, 0x11, 0x02, 0x02, 0x01, 0x00, 0x00]);
        // 68:02:11:02:02:01:01:00  (NC, focus=true, level=15 -> level byte forced to 0)
        assert_eq!(v1_anc_set(AncMode::ActiveNoiseCanceling, true, 15), vec![0x68, 0x02, 0x11, 0x02, 0x02, 0x01, 0x01, 0x00]);
    }

    #[test]
    fn v1_sound_pressure_unsupported() {
        assert!(matches!(
            build_command(&Command::GetSoundPressure, 0, ProtocolVersion::V1),
            Err(CommandError::UnsupportedForVersion { version: ProtocolVersion::V1 })
        ));
        assert!(matches!(
            build_command(&Command::SoundPressureMeasure { on: true }, 0, ProtocolVersion::V1),
            Err(CommandError::UnsupportedForVersion { version: ProtocolVersion::V1 })
        ));
        // v2 still works
        assert!(build_command(&Command::GetSoundPressure, 0, ProtocolVersion::V2).is_ok());
    }

    #[test]
    fn init() {
        // taken from hci logs
        let bytes = [0x3e, 0xc, 0x0, 0x0, 0x0, 0x0, 0x2, 0x0, 0x0, 0xe, 0x3c];
        assert_eq!(
            bytes.as_slice(),
            build_command(&Command::Init, 0, ProtocolVersion::V2)
                .unwrap()
                .as_slice()
        );
    }
    #[test]
    fn init_ack() {
        // taken from hci logs
        let ack = [0x3e, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x3c];
        let init_seq_num = 1;
        let our_ack = build_command(&Command::Ack, init_seq_num, ProtocolVersion::V2).unwrap();
        assert_eq!(ack.as_slice(), our_ack.as_slice());
    }
}
