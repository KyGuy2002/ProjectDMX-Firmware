// src/hardware.rs
use embassy_rp::gpio::{AnyPin, Pin};
use embassy_rp::peripherals;

/// Holds the 4 hardware pins belonging to a modular PCB slot.
pub struct SlotPins {
    pub pin1: AnyPin,
    pub pin2: AnyPin,
    pub pin3: AnyPin,
    pub pin4: AnyPin,
}

/// Dedicated SPI Pin mapping for the SD Card.
pub struct SdCardPins {
    pub sck: AnyPin,
    pub mosi: AnyPin,
    pub miso: AnyPin,
    pub cs: AnyPin,
}

/// Physical buttons mapped on the board.
pub struct ButtonPins {
    pub menu: AnyPin,
    pub down: AnyPin,
    pub up: AnyPin,
    pub enter: AnyPin,
}

/// Dedicated SPI Pin mapping for the Ethernet Controller.
pub struct EthernetPins {
    pub sck: AnyPin,
    pub mosi: AnyPin,
    pub miso: AnyPin,
    pub cs: AnyPin,
}

/// Dedicated I2C Pin mapping for the OLED Screen.
pub struct OledPins {
    pub sda: AnyPin,
    pub scl: AnyPin,
}

/// Dedicated RS485 / DMX Interface Pin mapping.
pub struct DmxPins {
    pub tx: AnyPin,
    pub rx: AnyPin,
    pub mode: AnyPin,
}

/// Dedicated Inter-IC Sound (I2S) Audio Interface Pin mapping.
pub struct AudioPins {
    pub din: AnyPin,
    pub bck: AnyPin,
    pub lck: AnyPin,
}

/// Loose extra pins left over on the board schematic.
pub struct UnusedPins {
    pub pin1: AnyPin,
    pub pin2: AnyPin,
    pub pin3: AnyPin,
    pub pin4: AnyPin,
}

/// Inputs 1 through 6 mapped on the board.
pub struct InputPins {
    pub input1: AnyPin,
    pub input2: AnyPin,
    pub input3: AnyPin,
    pub input4: AnyPin,
    pub input5: AnyPin,
    pub input6: AnyPin,
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
    /// Consumes the raw RP2350 peripherals and neatly sorts them into domain-specific structures.
    pub fn new(p: peripherals::Peripherals) -> Self {
        Self {
            sd_card: SdCardPins {
                sck: p.PIN_34.degrade(),
                mosi: p.PIN_35.degrade(),
                miso: p.PIN_36.degrade(),
                cs: p.PIN_37.degrade(),
            },
            buttons: ButtonPins {
                menu: p.PIN_33.degrade(),
                down: p.PIN_31.degrade(),
                up: p.PIN_32.degrade(),
                enter: p.PIN_30.degrade(),
            },
            ethernet: EthernetPins {
                sck: p.PIN_10.degrade(),
                mosi: p.PIN_11.degrade(),
                miso: p.PIN_12.degrade(),
                cs: p.PIN_13.degrade(),
            },
            oled: OledPins {
                sda: p.PIN_38.degrade(),
                scl: p.PIN_27.degrade(), // Note: used to be 39 but pcb errors
            },
            dmx: DmxPins {
                tx: p.PIN_24.degrade(),
                rx: p.PIN_25.degrade(),
                mode: p.PIN_23.degrade(),
            },
            audio: AudioPins {
                din: p.PIN_20.degrade(),
                bck: p.PIN_21.degrade(),
                lck: p.PIN_22.degrade(),
            },
            unused: UnusedPins {
                pin1: p.PIN_27.degrade(),
                pin2: p.PIN_26.degrade(),
                pin3: p.PIN_29.degrade(),
                pin4: p.PIN_28.degrade(),
            },
            slot_a: SlotPins {
                pin1: p.PIN_4.degrade(),
                pin2: p.PIN_3.degrade(),
                pin3: p.PIN_2.degrade(),
                pin4: p.PIN_40.degrade(), // (Analog)
            },
            slot_b: SlotPins {
                pin1: p.PIN_8.degrade(),
                pin2: p.PIN_6.degrade(),
                pin3: p.PIN_5.degrade(),
                pin4: p.PIN_41.degrade(), // (Analog)
            },
            slot_c: SlotPins {
                pin1: p.PIN_15.degrade(),
                pin2: p.PIN_14.degrade(),
                pin3: p.PIN_9.degrade(),
                pin4: p.PIN_7.degrade(),
            },
            slot_d: SlotPins {
                pin1: p.PIN_19.degrade(),
                pin2: p.PIN_18.degrade(),
                pin3: p.PIN_17.degrade(),
                pin4: p.PIN_16.degrade(),
            },
            digital_inputs: InputPins {
                input1: p.PIN_42.degrade(),
                input2: p.PIN_43.degrade(),
                input3: p.PIN_44.degrade(),
                input4: p.PIN_46.degrade(),
                input5: p.PIN_45.degrade(),
                input6: p.PIN_47.degrade(),
            },
        }
    }
}