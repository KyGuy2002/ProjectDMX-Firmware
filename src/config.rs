use heapless::{String, Vec};
use serde::{Deserialize, Serialize};
use serde::de::{self, Deserializer};

use defmt::{info, warn};

use crate::{MAX_PIXELS, MAX_UNIVERSES};

const MAX_CONFIG_LEN: usize = 10240;


pub const MAX_AUDIO_FILES: usize = 10;
pub const MAX_FILENAME_LEN: usize = 128;
pub const MAX_HOSTNAME_LEN: usize = 64;

/// Error from parsing or validating a board config document.
#[derive(Clone, Copy, PartialEq, Debug, defmt::Format)]
pub enum ConfigError {
    /// Text (after comment stripping) didn't fit in `MAX_CONFIG_LEN`.
    TooLarge,
    /// A `/* ... */` block comment was never closed.
    BadBlockComment,
    /// `serde_json_core` couldn't deserialize the document.
    Parse,
    /// Deserialized, but a value is out of range. Payload names the field.
    Invalid(&'static str),
}

/// Comment-strips, deserializes, and validates a JSONC document. Source-agnostic
/// so the same path serves the compiled-in string and (later) bytes read from
/// the SD card.
pub fn parse_config(jsonc: &str) -> Result<BoardInstanceConfig, ConfigError> {
    let json = strip_jsonc_comments::<MAX_CONFIG_LEN>(jsonc)?;

    let (config, _): (BoardInstanceConfig, usize) =
        serde_json_core::from_str(&json).map_err(|e| {
            // serde_json_core's error doesn't carry a line/offset, but its kind
            // (e.g. "expected an enum variant") narrows down a bad config fast.
            warn!("Config: JSON parse failed: {}", defmt::Debug2Format(&e));
            ConfigError::Parse
        })?;

    validate(&config)?;

    Ok(config)
}

/// Range checks serde can't express, so a bad config is caught at boot instead
/// of panicking a task later (e.g. an out-of-bounds `pixel_count`).
fn validate(config: &BoardInstanceConfig) -> Result<(), ConfigError> {
    // Universes (1..=MAX_UNIVERSES) and channels (1..=512) are 1-based everywhere
    // in the config, matching fixture / console addressing; `read_channels` and
    // the DMX output loop convert to the 0-based matrix index.
    // The board only speaks IPv4 link-local + mDNS, so a hostname can only be
    // resolved if it's a `.local` name.
    if config.chataigne.host.parse::<core::net::Ipv4Addr>().is_err()
        && !config.chataigne.host.trim_end_matches('.').ends_with(".local")
    {
        return Err(ConfigError::Invalid("chataigne host must be an IPv4 address or a .local name"));
    }
    if !(1..=MAX_UNIVERSES).contains(&(config.dmx_output.universe as usize)) {
        return Err(ConfigError::Invalid("dmx_output universe out of range (expected 1..=MAX_UNIVERSES)"));
    }
    if !(1..=MAX_UNIVERSES).contains(&(config.audio.universe as usize)) {
        return Err(ConfigError::Invalid("audio universe out of range (expected 1..=MAX_UNIVERSES)"));
    }
    if !(1..=512).contains(&config.audio.start_channel) {
        return Err(ConfigError::Invalid("audio start_channel out of range (expected 1..=512)"));
    }
    if config.audio.start_channel > 510 {
        return Err(ConfigError::Invalid("audio right channel is out of range (expected start_channel <= 510)"));
    }

    let slots = [
        &config.modules.slot_a,
        &config.modules.slot_b,
        &config.modules.slot_c,
        &config.modules.slot_d,
    ];

    for slot in slots {
        match slot {
            ModuleSlot::Neo(neo) => {
                for port in &neo.ports {
                    if let Port::Enabled(p) = port {
                        if p.pixel_count > MAX_PIXELS {
                            return Err(ConfigError::Invalid("neo port pixel_count exceeds MAX_PIXELS"));
                        }
                        if !(1..=MAX_UNIVERSES).contains(&(p.universe as usize)) {
                            return Err(ConfigError::Invalid("neo port universe out of range (expected 1..=MAX_UNIVERSES)"));
                        }
                        if !(1..=512).contains(&p.start_channel) {
                            return Err(ConfigError::Invalid("neo port start_channel out of range (expected 1..=512)"));
                        }
                    }
                }
            }
            ModuleSlot::Dimmer(d) => {
                if !(1..=MAX_UNIVERSES).contains(&(d.universe as usize)) {
                    return Err(ConfigError::Invalid("dimmer universe out of range (expected 1..=MAX_UNIVERSES)"));
                }
                if !(1..=512).contains(&d.start_channel) {
                    return Err(ConfigError::Invalid("dimmer start_channel out of range (expected 1..=512)"));
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Loads the board configuration.
///
/// `sd_source` is the text of `config.json` read from the SD card, once that
/// path exists; pass `None` to use the copy compiled in from `config.jsonc`. A
/// rejected SD config falls back to the built-in one so the board always boots.
pub fn load_config(sd_source: Option<&str>) -> BoardInstanceConfig {
    const BUILT_IN: &str = include_str!("config.jsonc");

    let config = match sd_source {
        Some(text) => match parse_config(text) {
            Ok(c) => {
                info!("Config: loaded from SD card");
                c
            }
            Err(e) => {
                warn!("Config: SD card config rejected ({}), using built-in default", e);
                parse_config(BUILT_IN).expect("built-in config.jsonc must be valid")
            }
        },
        None => parse_config(BUILT_IN).expect("built-in config.jsonc must be valid"),
    };

    // Print bootup information
    let input_str = match config.input.source {
        InputProtocol::Dmx => "DMX",
        InputProtocol::Artnet => "Art-Net",
        InputProtocol::sACN => "sACN",
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

fn strip_jsonc_comments<const N: usize>(input: &str) -> Result<String<N>, ConfigError> {
    let mut output = String::<N>::new();

    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch).map_err(|_| ConfigError::TooLarge)?;

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
            output.push(ch).map_err(|_| ConfigError::TooLarge)?;
            continue;
        }

        if ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();

                    while let Some(comment_ch) = chars.next() {
                        if comment_ch == '\n' {
                            output.push('\n').map_err(|_| ConfigError::TooLarge)?;
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
                        return Err(ConfigError::BadBlockComment);
                    }

                    output.push(' ').map_err(|_| ConfigError::TooLarge)?;
                    continue;
                }

                _ => {}
            }
        }

        output.push(ch).map_err(|_| ConfigError::TooLarge)?;
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
    sACN,
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
// AUDIO (DMX-TRIGGERED MP3 PLAYBACK)
// =========================================================================

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AudioConfig {
    pub universe: u16,
    pub start_channel: u16,
    pub bg_files: Vec<String<MAX_FILENAME_LEN>, MAX_AUDIO_FILES>,
    pub left_files: Vec<String<MAX_FILENAME_LEN>, MAX_AUDIO_FILES>,
    pub right_files: Vec<String<MAX_FILENAME_LEN>, MAX_AUDIO_FILES>,
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
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
    Rgbw,
    Grbw,
}

impl ColorOrder {
    /// Wire-order indices into a logical `[r, g, b, w]` source. Length is 3 for
    /// an RGB strip, 4 for RGBW - this is also how the strip's bit width is
    /// decided. Add a variant here plus one line to support another order.
    pub fn perm(self) -> &'static [u8] {
        match self {
            ColorOrder::Rgb => &[0, 1, 2],
            ColorOrder::Rbg => &[0, 2, 1],
            ColorOrder::Grb => &[1, 0, 2],
            ColorOrder::Gbr => &[1, 2, 0],
            ColorOrder::Brg => &[2, 0, 1],
            ColorOrder::Bgr => &[2, 1, 0],
            ColorOrder::Rgbw => &[0, 1, 2, 3],
            ColorOrder::Grbw => &[1, 0, 2, 3],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NeoMode {
    SolidColor,
    Generator2D,
    Raw,
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
    /// Per-output (out0..out3, reading `start_channel + i`): `true` = binary,
    /// full-on above DMX 127 and off below; `false` = linear dim. Defaults to
    /// all-linear when the `binary` key is omitted from the JSON.
    pub binary: [bool; 4],
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
    #[serde(rename = "fog")]
    FogMachine,
    #[serde(rename = "amp")]
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
    binary: Option<[bool; 4]>,
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
                binary: raw.binary.unwrap_or([false; 4]),
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

/// The Game Master PC (or FPP) the board sends switch events to over TCP.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Chataigne {
    /// An IPv4 literal ("169.254.3.7") or an mDNS name ("fpp.local"), resolved
    /// at startup and again on every reconnect.
    pub host: String<MAX_HOSTNAME_LEN>,
    pub port: u16,
}

// =========================================================================
// ROOT CONFIGURATION STRUCT
// =========================================================================

// Not Copy - AudioConfig contains heapless collections. Clone it where the whole
// struct needs to be handed off (e.g. into CONFIG / the audio task); individual Copy
// sub-fields (like config.modules.slot_c) can still be read out normally.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct BoardInstanceConfig {
    pub input: InputConfig,
    pub chataigne: Chataigne,
    pub dmx_output: DmxOutputConfig,
    pub audio: AudioConfig,
    pub modules: ModuleContainer,
}