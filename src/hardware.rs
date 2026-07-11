use embassy_rp::gpio::AnyPin;
use embassy_rp::peripherals::{
    DMA_CH1, DMA_CH2, DMA_CH3, DMA_CH4,
    PIN_2, PIN_3, PIN_4, PIN_5, PIN_6, PIN_7, PIN_8, PIN_9,
    PIN_10, PIN_11, PIN_12, PIN_13,
    PIN_14, PIN_15, PIN_16, PIN_17, PIN_18, PIN_19,
    PIN_24, PIN_25,
    PIN_40, PIN_41,
    PWM_SLICE0, PWM_SLICE1, PWM_SLICE2, PWM_SLICE3, PWM_SLICE4, PWM_SLICE7, PWM_SLICE8,
    SPI1, UART1,
};
use embassy_rp::Peri;

pub struct SlotA {
    pub pin1: Peri<'static, PIN_4>,
    pub pin2: Peri<'static, PIN_3>,
    pub pin3: Peri<'static, PIN_2>,
    pub pin4: Peri<'static, PIN_40>,
}

pub struct SlotB {
    pub pin1: Peri<'static, PIN_8>,
    pub pin2: Peri<'static, PIN_6>,
    pub pin3: Peri<'static, PIN_5>,
    pub pin4: Peri<'static, PIN_41>,
}

pub struct SlotC {
    pub pin1: Peri<'static, PIN_15>,
    pub pin2: Peri<'static, PIN_14>,
    pub pin3: Peri<'static, PIN_9>,
    pub pin4: Peri<'static, PIN_7>,
}

pub struct SlotD {
    pub pin1: Peri<'static, PIN_19>,
    pub pin2: Peri<'static, PIN_18>,
    pub pin3: Peri<'static, PIN_17>,
    pub pin4: Peri<'static, PIN_16>,
}

pub struct Slots {
    pub slot_a: SlotA,
    pub slot_b: SlotB,
    pub slot_c: SlotC,
    pub slot_d: SlotD,
}

pub struct PwmSlices {
    pub slice0: Peri<'static, PWM_SLICE0>,
    pub slice1: Peri<'static, PWM_SLICE1>,
    pub slice2: Peri<'static, PWM_SLICE2>,
    pub slice3: Peri<'static, PWM_SLICE3>,
    pub slice4: Peri<'static, PWM_SLICE4>,
    pub slice7: Peri<'static, PWM_SLICE7>,
    pub slice8: Peri<'static, PWM_SLICE8>,
}

pub struct SdCardPins {
    pub sck: Peri<'static, AnyPin>,
    pub mosi: Peri<'static, AnyPin>,
    pub miso: Peri<'static, AnyPin>,
    pub cs: Peri<'static, AnyPin>,
}

pub struct ButtonPins {
    pub menu: Peri<'static, AnyPin>,
    pub down: Peri<'static, AnyPin>,
    pub up: Peri<'static, AnyPin>,
    pub enter: Peri<'static, AnyPin>,
}

pub struct EthernetPins {
    pub sck: Peri<'static, PIN_10>,
    pub mosi: Peri<'static, PIN_11>,
    pub miso: Peri<'static, PIN_12>,
    pub cs: Peri<'static, PIN_13>,
}

pub struct OledPins {
    pub sda: Peri<'static, AnyPin>,
    pub scl: Peri<'static, AnyPin>,
}

pub struct DmxPins {
    pub mode: Peri<'static, AnyPin>,
}

pub struct AudioPins {
    pub din: Peri<'static, AnyPin>,
    pub bck: Peri<'static, AnyPin>,
    pub lck: Peri<'static, AnyPin>,
}

pub struct UnusedPins {
    pub pin1: Peri<'static, AnyPin>,
    pub pin2: Peri<'static, AnyPin>,
    pub pin3: Peri<'static, AnyPin>,
}

pub struct InputPins {
    pub input1: Peri<'static, AnyPin>,
    pub input2: Peri<'static, AnyPin>,
    pub input3: Peri<'static, AnyPin>,
    pub input4: Peri<'static, AnyPin>,
    pub input5: Peri<'static, AnyPin>,
    pub input6: Peri<'static, AnyPin>,
}

pub struct PcbLayout {
    pub sd_card: SdCardPins,
    pub buttons: ButtonPins,
    pub ethernet: EthernetPins,
    pub oled: OledPins,
    pub dmx: DmxPins,
    pub audio: AudioPins,
    pub unused: UnusedPins,
    pub slots: Slots,
    pub pwm: PwmSlices,
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

                slots: Slots {
                    slot_a: SlotA {
                        pin1: p.PIN_4,
                        pin2: p.PIN_3,
                        pin3: p.PIN_2,
                        pin4: p.PIN_40,
                    },

                    slot_b: SlotB {
                        pin1: p.PIN_8,
                        pin2: p.PIN_6,
                        pin3: p.PIN_5,
                        pin4: p.PIN_41,
                    },

                    slot_c: SlotC {
                        pin1: p.PIN_15,
                        pin2: p.PIN_14,
                        pin3: p.PIN_9,
                        pin4: p.PIN_7,
                    },

                    slot_d: SlotD {
                        pin1: p.PIN_19,
                        pin2: p.PIN_18,
                        pin3: p.PIN_17,
                        pin4: p.PIN_16,
                    },
                },

                pwm: PwmSlices {
                    slice0: p.PWM_SLICE0,
                    slice1: p.PWM_SLICE1,
                    slice2: p.PWM_SLICE2,
                    slice3: p.PWM_SLICE3,
                    slice4: p.PWM_SLICE4,
                    slice7: p.PWM_SLICE7,
                    slice8: p.PWM_SLICE8,
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