use embassy_rp::gpio::AnyPin;
use embassy_rp::peripherals::{
    DMA_CH1, DMA_CH2, DMA_CH3, DMA_CH4,
    PIN_10, PIN_11, PIN_12, PIN_13,
    PIN_24, PIN_25,
    SPI1, UART1,
};
use embassy_rp::Peri;

/// Holds the 4 hardware pins belonging to a modular PCB slot.
pub struct SlotPins {
    pub pin1: Peri<'static, AnyPin>,
    pub pin2: Peri<'static, AnyPin>,
    pub pin3: Peri<'static, AnyPin>,
    pub pin4: Peri<'static, AnyPin>,
}

/// Dedicated SPI Pin mapping for the SD Card.
pub struct SdCardPins {
    pub sck: Peri<'static, AnyPin>,
    pub mosi: Peri<'static, AnyPin>,
    pub miso: Peri<'static, AnyPin>,
    pub cs: Peri<'static, AnyPin>,
}

/// Physical buttons mapped on the board.
pub struct ButtonPins {
    pub menu: Peri<'static, AnyPin>,
    pub down: Peri<'static, AnyPin>,
    pub up: Peri<'static, AnyPin>,
    pub enter: Peri<'static, AnyPin>,
}

/// Dedicated SPI Pin mapping for the Ethernet Controller.
pub struct EthernetPins {
    pub sck: Peri<'static, PIN_10>,
    pub mosi: Peri<'static, PIN_11>,
    pub miso: Peri<'static, PIN_12>,
    pub cs: Peri<'static, PIN_13>,
}

/// Dedicated I2C Pin mapping for the OLED Screen.
pub struct OledPins {
    pub sda: Peri<'static, AnyPin>,
    pub scl: Peri<'static, AnyPin>,
}

/// Dedicated RS485 / DMX Interface Pin mapping.
///
/// UART TX/RX are not stored here because Embassy UART needs concrete pin types.
/// DMX TX = PIN_24
/// DMX RX = PIN_25
pub struct DmxPins {
    pub mode: Peri<'static, AnyPin>,
}

/// Dedicated Inter-IC Sound Audio Interface Pin mapping.
pub struct AudioPins {
    pub din: Peri<'static, AnyPin>,
    pub bck: Peri<'static, AnyPin>,
    pub lck: Peri<'static, AnyPin>,
}

/// Loose extra pins left over on the board schematic.
///
/// PIN_27 is not listed here because it is used as OLED SCL.
pub struct UnusedPins {
    pub pin1: Peri<'static, AnyPin>,
    pub pin2: Peri<'static, AnyPin>,
    pub pin3: Peri<'static, AnyPin>,
}

/// Inputs 1 through 6 mapped on the board.
pub struct InputPins {
    pub input1: Peri<'static, AnyPin>,
    pub input2: Peri<'static, AnyPin>,
    pub input3: Peri<'static, AnyPin>,
    pub input4: Peri<'static, AnyPin>,
    pub input5: Peri<'static, AnyPin>,
    pub input6: Peri<'static, AnyPin>,
}

/// The master physical hardware mapping representation of your complete PCB layout.
pub struct PcbLayout {
    pub sd_card: SdCardPins,
    pub buttons: ButtonPins,
    pub ethernet: EthernetPins,
    pub oled: OledPins,
    pub dmx: DmxPins,
    pub audio: AudioPins,
    pub unused: UnusedPins,
    pub slot_a: SlotPins,
    pub slot_b: SlotPins,
    pub slot_c: SlotPins,
    pub slot_d: SlotPins,
    pub digital_inputs: InputPins,
}

impl PcbLayout {
    pub fn new(
        p: embassy_rp::Peripherals,
    ) -> (
        Self,
        Peri<'static, UART1>,
        Peri<'static, PIN_24>,
        Peri<'static, PIN_25>,
        Peri<'static, DMA_CH1>,
        Peri<'static, DMA_CH2>,
        Peri<'static, SPI1>,
        Peri<'static, DMA_CH3>,
        Peri<'static, DMA_CH4>,
    ) {
        (
            Self {
                sd_card: SdCardPins {
                    sck: p.PIN_34.into(),
                    mosi: p.PIN_35.into(),
                    miso: p.PIN_36.into(),
                    cs: p.PIN_37.into(),
                },

                buttons: ButtonPins {
                    menu: p.PIN_33.into(),
                    down: p.PIN_31.into(),
                    up: p.PIN_32.into(),
                    enter: p.PIN_30.into(),
                },

                ethernet: EthernetPins {
                    sck: p.PIN_10,
                    mosi: p.PIN_11,
                    miso: p.PIN_12,
                    cs: p.PIN_13,
                },

                oled: OledPins {
                    sda: p.PIN_38.into(),
                    scl: p.PIN_27.into(),
                },

                dmx: DmxPins {
                    mode: p.PIN_23.into(),
                },

                audio: AudioPins {
                    din: p.PIN_20.into(),
                    bck: p.PIN_21.into(),
                    lck: p.PIN_22.into(),
                },

                unused: UnusedPins {
                    pin1: p.PIN_26.into(),
                    pin2: p.PIN_29.into(),
                    pin3: p.PIN_28.into(),
                },

                slot_a: SlotPins {
                    pin1: p.PIN_4.into(),
                    pin2: p.PIN_3.into(),
                    pin3: p.PIN_2.into(),
                    pin4: p.PIN_40.into(),
                },

                slot_b: SlotPins {
                    pin1: p.PIN_8.into(),
                    pin2: p.PIN_6.into(),
                    pin3: p.PIN_5.into(),
                    pin4: p.PIN_41.into(),
                },

                slot_c: SlotPins {
                    pin1: p.PIN_15.into(),
                    pin2: p.PIN_14.into(),
                    pin3: p.PIN_9.into(),
                    pin4: p.PIN_7.into(),
                },

                slot_d: SlotPins {
                    pin1: p.PIN_19.into(),
                    pin2: p.PIN_18.into(),
                    pin3: p.PIN_17.into(),
                    pin4: p.PIN_16.into(),
                },

                digital_inputs: InputPins {
                    input1: p.PIN_42.into(),
                    input2: p.PIN_43.into(),
                    input3: p.PIN_44.into(),
                    input4: p.PIN_46.into(),
                    input5: p.PIN_45.into(),
                    input6: p.PIN_47.into(),
                },
            },
            p.UART1,
            p.PIN_24,
            p.PIN_25,
            p.DMA_CH1,
            p.DMA_CH2,
            p.SPI1,
            p.DMA_CH3,
            p.DMA_CH4,
        )
    }
}