use crate::types::*;


pub const CURRENT_BOARD: BoardInstanceConfig = BoardInstanceConfig {

    // Lighting data input type
    active_input: InputSource::DmxRs485(DmxSettings {}),
    
    // SLOT A
    slot_a: ModuleType::FogMachine(FogMachineConfig {
        universe: 0,
        start_channel: 1,
    }),

    // SLOT B
    slot_b: ModuleType::Empty,

    // SLOT C
    slot_c: ModuleType::NeoModule(NeoConfig {
        ports: [
            // Port 1 - Unused
            Port::Disabled,

            // Port 2 - Alibaba Uplights
            Port::Enabled(PortSettings {
                protocol: LedProtocol::Ws2812,
                color_order: ColorOrder::Grbw,
                pixel_count: 10,
                universe: 0,
                start_channel: 1,
            }),

            // Port 3 - Neon Tubes
            Port::Enabled(PortSettings {
                protocol: LedProtocol::Ws2812,
                color_order: ColorOrder::Rgb,
                pixel_count: 200,
            }),

            // Port 4 - Broken
            Port::Disabled,
        ],
    }),

    // SLOT D
    slot_d: ModuleType::DimmerModule(DimmerConfig {}),
};