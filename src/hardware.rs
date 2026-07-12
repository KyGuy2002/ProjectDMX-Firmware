use assign_resources::assign_resources;
use embassy_rp::pio_programs::ws2812::PioWs2812Program;
use embassy_rp::{peripherals, Peri};

use embassy_rp::bind_interrupts;
use embassy_rp::pio::InterruptHandler as PioInterruptHandler;
use embassy_rp::uart::InterruptHandler as UartInterruptHandler;
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
    slot_a_relay: SlotARelayResources {
        pin1: PIN_4,
        pin2: PIN_3,
        pin3: PIN_2,
        pin4: PIN_40, // Analog
    },
    slot_b_unused: SlotBUnusedResources {
        pin1: PIN_8,
        pin2: PIN_6,
        pin3: PIN_5,
        pin4: PIN_41, // Analog
    },
    slot_c_neo: SlotCNeoResources {
        pin1: PIN_15,
        pin2: PIN_14,
        pin3: PIN_9,
        pin4: PIN_7,
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

pub static NEO_PROGRAM: StaticCell<PioWs2812Program<peripherals::PIO0>> = StaticCell::new();