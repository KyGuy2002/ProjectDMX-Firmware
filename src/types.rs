#[derive(Copy, Clone, PartialEq, Debug)]
pub enum ColorOrder {
    Rgb,
    Rgbw,
    Grb,
    Grbw,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum LedProtocol {
    Ws2812,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct PortSettings {
    pub protocol: LedProtocol,
    pub color_order: ColorOrder,
    pub pixel_count: usize,
    pub universe: usize,
    pub start_channel: usize,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Port {
    Disabled,
    Enabled(PortSettings),
}

/// The entire NeoPixel card module, containing 4 distinct port definitions
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct NeoConfig {
    pub ports: [Port; 4],
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct DimmerConfig {
    pub universe: usize,
    pub start_channel: usize,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FogMachineConfig {
    pub universe: usize,
    pub start_channel: usize,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum ModuleType {
    Empty,
    NeoModule(NeoConfig),
    DimmerModule(DimmerConfig),
    FogMachine(FogMachineConfig),
}


/// Configuration parameters used exclusively for physical DMX lines
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct DmxSettings {
}

/// Configuration parameters used exclusively for network Art-Net streams
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ArtNetSettings {
    pub universe: u16,
}


#[derive(Copy, Clone, PartialEq, Debug)]
pub enum InputSource {
    DmxRs485(DmxSettings),
    ArtNet(ArtNetSettings),
}


pub struct BoardInstanceConfig {
    pub active_input: InputSource,
    pub dmx_output_universe: usize,
    pub slot_a: ModuleType,
    pub slot_b: ModuleType,
    pub slot_c: ModuleType,
    pub slot_d: ModuleType,
}