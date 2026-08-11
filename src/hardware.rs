use assign_resources::assign_resources;
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_rp::{peripherals, Peri};

use embassy_rp::bind_interrupts;
use embassy_rp::pio::InterruptHandler as PioInterruptHandler;
use embassy_rp::uart::InterruptHandler as UartInterruptHandler;
use embassy_rp::i2c::InterruptHandler as I2cInterruptHandler;
use static_cell::StaticCell;


assign_resources! {
    eth: EthResources {
        sck: PIN_10,
        mosi: PIN_11,
        miso: PIN_12,
        cs: PIN_13,
        spi: SPI1,
        tx_dma: DMA_CH3,
        rx_dma: DMA_CH4,
    },
    dmx: DmxResources {
        tx: PIN_24,
        rx: PIN_25,
        mode: PIN_23,
        uart: UART1,
        tx_dma: DMA_CH1,
        rx_dma: DMA_CH2, 
    },
    oled: OledResources {
        sda: PIN_38,
        scl: PIN_27, // Used to be 39 but pcb routing error
        i2c: I2C1,
    },
    sensors: SensorResources {
        in1: PIN_42,
        in2: PIN_43,
        in3: PIN_44,
        in4: PIN_46,
        in5: PIN_45,
        in6: PIN_47,
    },
    audio: AudioResources {
        // NOTE: LCK/WS MUST BE BCK+1 (one more than BCK) [lck > bck]
        din: PIN_20,
        bck: PIN_21,
        lck: PIN_22,
        pio: PIO1,
        dma: DMA_CH8,
    },
    sd: SdResources {
        sck: PIN_34,
        mosi: PIN_35,
        miso: PIN_36,
        cs: PIN_37,
        spi: SPI0,
    },
    slot_a_relay: SlotARelayResources {
        pin1: PIN_4,
        pin2: PIN_3,
        pin3: PIN_2,
        pin4: PIN_40, // Analog
    },
    slot_b_ask433: SlotBAsk433Resources {
        pin1: PIN_8, // Receiver DO pin
        pin2: PIN_6,
        pin3: PIN_5,
        pin4: PIN_41, // Analog
        pio: PIO2,
    },
    slot_c_neo: SlotCNeoResources {
        pin1: PIN_15, // 4th Output (Dead on JST and Term Modules)
        pin2: PIN_14, // 3rd Output
        pin3: PIN_9, // 1st Output
        pin4: PIN_7, // 2nd Output
        pio: PIO0,
        dma1: DMA_CH0,
        dma2: DMA_CH5,
        dma3: DMA_CH6,
        dma4: DMA_CH7,
    },
    slot_d_dimmer: SlotDDimmerResources {
        pin1: PIN_19, // 1B
        pin2: PIN_18, // 1A
        pin3: PIN_17, // 0B
        pin4: PIN_16, // 0A
        pwm0: PWM_SLICE0,
        pwm1: PWM_SLICE1,
    },
}


pub type EthSpi = peripherals::SPI1;

bind_interrupts!(pub struct DmxIrqs {
    UART1_IRQ => UartInterruptHandler<peripherals::UART1>;
});

bind_interrupts!(pub struct NeoIrqs {
    PIO0_IRQ_0 => PioInterruptHandler<peripherals::PIO0>;
});

bind_interrupts!(pub struct AudioIrqs {
    PIO1_IRQ_0 => PioInterruptHandler<peripherals::PIO1>;
});

bind_interrupts!(pub struct Ask433Irqs {
    PIO2_IRQ_0 => PioInterruptHandler<peripherals::PIO2>;
});

bind_interrupts!(pub struct OledIrqs {
    I2C1_IRQ => I2cInterruptHandler<peripherals::I2C1>;
});


pub static NEO_PROGRAM: StaticCell<PioWs2812Program<peripherals::PIO0>> = StaticCell::new();