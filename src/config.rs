use heapless::String;
use serde::{Deserialize, Serialize};
use serde::de::{self, Deserializer};

use defmt::info;

const MAX_CONFIG_LEN: usize = 8192;

/**
 * Loads the configuration from the embedded `config.jsonc` file and returns a `BoardInstanceConfig` struct.
 */
pub fn load_config() -> BoardInstanceConfig {
    let jsonc_data = include_str!("config.jsonc");

    let json_data = strip_jsonc_comments::<MAX_CONFIG_LEN>(jsonc_data)
        .expect("config.jsonc is too large or has a bad block comment");

    let (config, _): (BoardInstanceConfig, usize) = serde_json_core::from_str(&json_data)
        .expect("Failed to parse config.jsonc! Check your syntax.");

    // Print bootup information
    let input_str = match config.input.source {
        InputProtocol::Dmx => "DMX",
        InputProtocol::Artnet => "Art-Net",
        InputProtocol::Sd => "SD Card",
    };

    info!("         Input: {}", input_str);
    info!("");
    info!("     A        B         C        D     ");
    info!(
        " [ {} ]  [ {} ]  [ {} ]  [ {} ]",
        get_module_str(config.modules.slot_a),
        get_module_str(config.modules.slot_b),
        get_module_str(config.modules.slot_c),
        get_module_str(config.modules.slot_d)
    );
    info!("");

    config
}

/**
 * For later SD card loading:
 *
 * Read the SD card config.jsonc into a &str, then call this.
 */
pub fn parse_config_jsonc(jsonc_data: &str) -> Result<BoardInstanceConfig, ()> {
    let json_data = strip_jsonc_comments::<MAX_CONFIG_LEN>(jsonc_data)?;

    let (config, _): (BoardInstanceConfig, usize) =
        serde_json_core::from_str(&json_data).map_err(|_| ())?;

    Ok(config)
}

fn strip_jsonc_comments<const N: usize>(input: &str) -> Result<String<N>, ()> {
    let mut output = String::<N>::new();

    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch).map_err(|_| ())?;

            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }

            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch).map_err(|_| ())?;
            continue;
        }

        if ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();

                    while let Some(comment_ch) = chars.next() {
                        if comment_ch == '\n' {
                            output.push('\n').map_err(|_| ())?;
                            break;
                        }
                    }

                    continue;
                }

                Some('*') => {
                    chars.next();

                    let mut last = '\0';
                    let mut found_end = false;

                    while let Some(comment_ch) = chars.next() {
                        if last == '*' && comment_ch == '/' {
                            found_end = true;
                            break;
                        }

                        last = comment_ch;
                    }

                    if !found_end {
                        return Err(());
                    }

                    output.push(' ').map_err(|_| ())?;
                    continue;
                }

                _ => {}
            }
        }

        output.push(ch).map_err(|_| ())?;
    }

    Ok(output)
}

fn get_module_str(module_slot: ModuleSlot) -> &'static str {
    match module_slot {
        ModuleSlot::Neo(_) => "Neo",
        ModuleSlot::Dimmer(_) => "Dimmer",
        ModuleSlot::FogMachine(_) => "Fog",
        ModuleSlot::AudioAmp(_) => "Amp",
        ModuleSlot::Rfid(_) => "RFID",
        ModuleSlot::Disabled { .. } => "X",
    }
}

// =========================================================================
// INPUT & MASTER OUTPUT SETTINGS
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputProtocol {
    Dmx,
    Artnet,
    Sd,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct InputConfig {
    pub source: InputProtocol,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DmxOutputConfig {
    pub universe: u16,
}

// =========================================================================
// NEOPIXEL PROTOCOLS & ORDERS
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedProtocol {
    Ws2812,
    Sk6812,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorOrder {
    Rgb,
    Rgbw,
    Grb,
    Grbw,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NeoMode {
    SolidColor,
    Generator2D,
}

// =========================================================================
// PORT PORTFOLIOS
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct EnabledPort {
    pub protocol: LedProtocol,
    pub color_order: ColorOrder,
    pub pixel_count: usize,
    pub universe: u16,
    pub start_channel: u16,
    pub mode: NeoMode,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub enum Port {
    Enabled(EnabledPort),
    Disabled { disabled: bool },
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
struct RawPort {
    disabled: Option<bool>,

    protocol: Option<LedProtocol>,
    color_order: Option<ColorOrder>,
    pixel_count: Option<usize>,
    universe: Option<u16>,
    start_channel: Option<u16>,
    mode: Option<NeoMode>,
}

impl<'de> Deserialize<'de> for Port {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawPort::deserialize(deserializer)?;

        if raw.disabled.unwrap_or(false) {
            return Ok(Port::Disabled { disabled: true });
        }

        Ok(Port::Enabled(EnabledPort {
            protocol: raw.protocol.ok_or_else(|| de::Error::missing_field("protocol"))?,
            color_order: raw.color_order.ok_or_else(|| de::Error::missing_field("color_order"))?,
            pixel_count: raw.pixel_count.ok_or_else(|| de::Error::missing_field("pixel_count"))?,
            universe: raw.universe.ok_or_else(|| de::Error::missing_field("universe"))?,
            start_channel: raw.start_channel.ok_or_else(|| de::Error::missing_field("start_channel"))?,
            mode: raw.mode.ok_or_else(|| de::Error::missing_field("mode"))?,
        }))
    }
}

// =========================================================================
// SPECIFIC CARD MODULE SETTINGS
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct NeoConfig {
    pub ports: [Port; 4],
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DimmerConfig {
    pub universe: u16,
    pub start_channel: u16,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct FogMachineConfig {
    pub universe: u16,
    pub start_channel: u16,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct AudioAmpConfig {
    pub universe: u16,
    pub start_channel: u16,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct RfidConfig {
    pub universe: u16,
    pub start_channel: u16,
}

// =========================================================================
// MODULE SLOTS
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub enum ModuleSlot {
    Neo(NeoConfig),
    Dimmer(DimmerConfig),
    FogMachine(FogMachineConfig),
    AudioAmp(AudioAmpConfig),
    Rfid(RfidConfig),
    Disabled { disabled: bool },
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModuleType {
    Neo,
    Dimmer,
    FogMachine,
    AudioAmp,
    Rfid,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
struct RawModuleSlot {
    #[serde(rename = "type")]
    module_type: Option<ModuleType>,

    disabled: Option<bool>,

    ports: Option<[Port; 4]>,

    universe: Option<u16>,
    start_channel: Option<u16>,
}

impl<'de> Deserialize<'de> for ModuleSlot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawModuleSlot::deserialize(deserializer)?;

        if raw.disabled.unwrap_or(false) {
            return Ok(ModuleSlot::Disabled { disabled: true });
        }

        match raw.module_type.ok_or_else(|| de::Error::missing_field("type"))? {
            ModuleType::Neo => Ok(ModuleSlot::Neo(NeoConfig {
                ports: raw.ports.ok_or_else(|| de::Error::missing_field("ports"))?,
            })),

            ModuleType::Dimmer => Ok(ModuleSlot::Dimmer(DimmerConfig {
                universe: raw.universe.ok_or_else(|| de::Error::missing_field("universe"))?,
                start_channel: raw.start_channel.ok_or_else(|| de::Error::missing_field("start_channel"))?,
            })),

            ModuleType::FogMachine => Ok(ModuleSlot::FogMachine(FogMachineConfig {
                universe: raw.universe.ok_or_else(|| de::Error::missing_field("universe"))?,
                start_channel: raw.start_channel.ok_or_else(|| de::Error::missing_field("start_channel"))?,
            })),

            ModuleType::AudioAmp => Ok(ModuleSlot::AudioAmp(AudioAmpConfig {
                universe: raw.universe.ok_or_else(|| de::Error::missing_field("universe"))?,
                start_channel: raw.start_channel.ok_or_else(|| de::Error::missing_field("start_channel"))?,
            })),

            ModuleType::Rfid => Ok(ModuleSlot::Rfid(RfidConfig {
                universe: raw.universe.ok_or_else(|| de::Error::missing_field("universe"))?,
                start_channel: raw.start_channel.ok_or_else(|| de::Error::missing_field("start_channel"))?,
            })),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ModuleContainer {
    pub slot_a: ModuleSlot,
    pub slot_b: ModuleSlot,
    pub slot_c: ModuleSlot,
    pub slot_d: ModuleSlot,
}

// =========================================================================
// ROOT CONFIGURATION STRUCT
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct BoardInstanceConfig {
    pub input: InputConfig,
    pub dmx_output: DmxOutputConfig,
    pub modules: ModuleContainer,
}