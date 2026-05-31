use thiserror::Error;

use crate::{
    MessageType,
    command::{AncMode, BatteryType, EqualizerPreset},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadType {
    InitReply,
    BatteryLevel,
    BatteryLevelNotify,
    Equalizer,
    EqualizerNotify,
    AncStatus,
    AncStatusNotify,
    CodecGet,
    CodecNotify,
    SoundPressureMeasureReply,
    PressureGet,
}

impl PayloadType {
    pub fn from_byte(msg_type: MessageType, byte: u8) -> Option<Self> {
        Some(match msg_type {
            MessageType::Ack => return None,
            MessageType::Command1 => match byte {
                0x1 => Self::InitReply,
                0x13 => Self::CodecGet,
                0x15 => Self::CodecNotify,
                0x23 => Self::BatteryLevel,
                0x25 => Self::BatteryLevelNotify,
                0x57 => Self::Equalizer,
                0x59 => Self::EqualizerNotify,
                0x67 => Self::AncStatus,
                0x69 => Self::AncStatusNotify,
                _ => return None,
            },
            MessageType::Command2 => {
                match byte {
                    // from hci log: 3e0e0000000004590301006f3c
                    0x59 => Self::SoundPressureMeasureReply,
                    // from hci logs: 3e0e01000000045b034203b63c
                    0x5b => Self::PressureGet,
                    _ => return None,
                }
            }
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatteryLevel {
    Case(usize),
    Headphones { left: usize, right: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Unknown = 0,
    Sbc = 0x1,
    Aac = 0x2,
    Ldac = 0x10,
    Aptx = 0x20,
    AptxHd = 0x21,
}

impl Codec {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Unknown,
            0x1 => Self::Sbc,
            0x2 => Self::Aac,
            0x10 => Self::Ldac,
            0x20 => Self::Aptx,
            0x21 => Self::AptxHd,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Sbc => "SBC",
            Self::Aac => "AAC",
            Self::Ldac => "LDAC",
            Self::Aptx => "APTX",
            Self::AptxHd => "APTX HD",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Payload {
    InitReply,
    BatteryLevel(BatteryLevel),
    Equalizer {
        preset: EqualizerPreset,
        clear_bass: i8,
        band_400: i8,
        band_1000: i8,
        band_2500: i8,
        band_6300: i8,
        band_16000: i8,
    },
    AncStatus {
        mode: AncMode,
        ambient_sound_voice_passthrough: bool,
        ambient_sound_level: u8,
    },
    Codec {
        codec: Codec,
    },
    SoundPressureMeasureReply {
        is_on: bool,
    },
    SoundPressure {
        db: usize,
    },
}

#[derive(Debug, Error)]
pub enum ParsePayloadError {
    #[error("The given payload is empty")]
    Empty,
    #[error("Unknown payload type: 0x{kind:x}")]
    UnknownPayloadType { kind: u8 },
    #[error("Unknown battery type: 0x{battery:x}")]
    UnknownBatteryType { battery: u8 },
    #[error("Unknown equalizer preset: 0x{preset:x}")]
    UnknownEqualizerPreset { preset: u8 },
    #[error("Unknown codec: 0x{codec:x}")]
    UnknownCodec { codec: u8 },
    #[error("Payload is too small for payload of type {payload_type:?}")]
    PayloadTooSmall { payload_type: PayloadType },
}

pub fn parse_payload(
    payload: &[u8],
    message_type: MessageType,
) -> std::result::Result<Payload, ParsePayloadError> {
    if payload.is_empty() {
        return Err(ParsePayloadError::Empty);
    }

    let payload_type = PayloadType::from_byte(message_type, payload[0])
        .ok_or(ParsePayloadError::UnknownPayloadType { kind: payload[0] })?;

    Ok(match payload_type {
        PayloadType::InitReply => Payload::InitReply,
        PayloadType::BatteryLevel | PayloadType::BatteryLevelNotify => {
            if payload.len() < 5 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }
            let battery_type = BatteryType::from_byte(payload[1]).ok_or(
                ParsePayloadError::UnknownBatteryType {
                    battery: payload[1],
                },
            )?;
            match battery_type {
                BatteryType::Case => Payload::BatteryLevel(BatteryLevel::Case(payload[2] as usize)),

                BatteryType::Headphones => Payload::BatteryLevel(BatteryLevel::Headphones {
                    left: payload[2] as usize,
                    right: payload[4] as usize,
                }),
            }
        }

        PayloadType::Equalizer | PayloadType::EqualizerNotify => {
            if payload.len() < 10 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }
            let clear_bass = payload[4] as i8 - 10;
            let band_400 = payload[5] as i8 - 10;
            let band_1000 = payload[6] as i8 - 10;
            let band_2500 = payload[7] as i8 - 10;
            let band_6300 = payload[8] as i8 - 10;
            let band_16000 = payload[9] as i8 - 10;
            Payload::Equalizer {
                preset: EqualizerPreset::from_byte(payload[2])
                    .ok_or(ParsePayloadError::UnknownEqualizerPreset { preset: payload[2] })?,
                clear_bass,
                band_400,
                band_1000,
                band_2500,
                band_6300,
                band_16000,
            }
        }

        PayloadType::AncStatus | PayloadType::AncStatusNotify => {
            if payload.len() < 7 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }
            let mode = if payload[3] == 0 {
                AncMode::Off
            } else if payload[4] == 0 {
                AncMode::ActiveNoiseCanceling
            } else {
                AncMode::AmbientSound
            };
            let ambient_sound_voice_passthrough = payload[5] == 1;

            let ambient_sound_level = payload[6];

            Payload::AncStatus {
                mode,
                ambient_sound_voice_passthrough,
                ambient_sound_level,
            }
        }

        PayloadType::CodecGet | PayloadType::CodecNotify => {
            if payload.len() < 3 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }

            let codec = Codec::from_byte(payload[2])
                .ok_or(ParsePayloadError::UnknownCodec { codec: payload[2] })?;
            Payload::Codec { codec }
        }

        PayloadType::PressureGet => {
            if payload.len() < 3 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }
            // PressureGet logs:
            // hci log 1: 3e0e01000000045b034203b63c
            // hci log 2: 3e0e00000000045b034003b33c
            // payload[2] (0x42 top 0x40 bottom) seems to be the value as it changes between different logs.
            // Unsure what the 03 which wrap it signal.
            Payload::SoundPressure {
                db: payload[2] as usize,
            }
        }

        // when it turns on sends: 3e0e0000000004590301006f3c
        // when it turns off: 3e0e010000000459030101713c
        PayloadType::SoundPressureMeasureReply => {
            if payload.len() < 4 {
                return Err(ParsePayloadError::PayloadTooSmall { payload_type });
            }
            Payload::SoundPressureMeasureReply {
                is_on: payload[3] == 0,
            }
        }
    })
}

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
}
