use serde::{Deserialize, Serialize};
use serde_jsonc;

/**
 * Loads the configuration from the embedded `config.jsonc` file and returns a `BoardInstanceConfig` struct.
 */
fn load_config() -> BoardInstanceConfig {
    let json_data = include_str!("config.jsonc");

    let config: BoardInstanceConfig = serde_jsonc::from_str(json_data)
        .expect("Failed to parse config.jsonc! Check your syntax.");



    // Print bootup information
    //     =======================================
    let input_str = match config.input.source {
        InputProtocol::Dmx => "DMX",
        InputProtocol::Artnet => "Art-Net",
        InputProtocol::Sd => "SD Card",
    };

    info!("         Input: {}", input_str);
    info!("");
    info!("     A        B         C        D     ");
    info!(" [ {} ]  [ {} ]  [ {} ]  [ {} ]", 
        get_module_str(config.modules.slot_a),
        get_module_str(config.modules.slot_b),
        get_module_str(config.modules.slot_c),
        get_module_str(config.modules.slot_d)
    );
    info!("");



    return config;
}

fn get_module_str(module_slot: ModuleSlot) -> &'static str {
    match module_slot {
        ModuleSlot::Neo(_) => "Neo",
        ModuleSlot::Dimmer(_) => "Dimmer",
        ModuleSlot::FogMachine(_) => "Fog",
        ModuleSlot::AudioAmp(_) => "Amp",
        ModuleSlot::Rfid(_) => "RFID",
        ModuleSlot::Disabled { disabled: true } => "X",
        ModuleSlot::Disabled { disabled: false } => "X", // Unused but needed for compiler
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

// =========================================================================
// PORT PORTFOLIOS (UNTYPPED FLAT STRUCTURE HANDLING)
// =========================================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct EnabledPort {
    pub protocol: LedProtocol,
    pub color_order: ColorOrder,
    pub pixel_count: usize,
    pub universe: u16,
    pub start_channel: u16,
}

/// Automatically maps either an enabled port configuration or a "disabled": true flag
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Port {
    Enabled(EnabledPort),
    Disabled { disabled: bool },
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
    // You can easily re-add frequency_hz here later if you need it!
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct FogMachineConfig {
    pub universe: u16,
    pub start_channel: u16,
}

// --- RESERVED FOR YOUR EXTENSIONS ---
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
// MODULE SLOTS (TAGGED HANDLING)
// =========================================================================

/// Matches the `"type": "..."` field or the `"disabled": true` entry in your slots
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModuleSlot {
    Neo(NeoConfig),
    Dimmer(DimmerConfig),
    FogMachine(FogMachineConfig),
    AudioAmp(AudioAmpConfig),
    Rfid(RfidConfig),
    
    // This allows unconfigured slots to use {"disabled": true}
    #[serde(untagged)]
    Disabled { disabled: bool },
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