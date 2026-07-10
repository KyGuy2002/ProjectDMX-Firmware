#![no_std]
#![no_main]

mod config;
mod dmx_state;
mod neo_effects;

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};

use embassy_rp::gpio::{Level, Output};
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::{PIO0, UART1};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{Rgb, Rgbw, Grb, Grbw, PioWs2812, RgbwPioWs2812, PioWs2812Program};
use smart_leds::{RGB8, RGBW, White};
use static_cell::StaticCell;

enum ActiveDriver<'d> {
    Rgb(PioWs2812<'d, PIO0, 0, NUM_LEDS, Rgb>),
    Rgbw(RgbwPioWs2812<'d, PIO0, 0, NUM_LEDS, Grbw>),
}

use embassy_rp::uart::{
    Async, Config as UartConfig, DataBits, Parity, StopBits, Uart, UartRx,
};

use config::{get_layout_map, PixelMeta, NUM_LEDS};
use dmx_state::{DmxParams, DMX_SIGNAL};

// ==========================================
// CONFIGURATION FLAG
// ==========================================
const IS_RGBW: bool = true; // Set to true for RGBW strips, false for RGB strips
// ==========================================

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    UART1_IRQ => embassy_rp::uart::InterruptHandler<UART1>;
});

/// Tracks running crossfader configurations without dynamic heap allocations.
enum TransitionState {
    Stable,
    Crossfading {
        old_params: DmxParams,
        progress: u8,
        duration: u8,
    },
}

/// Dynamic dual-buffer storage wrapping the two potential pixel types
enum LedBuffer {
    Rgb([RGB8; NUM_LEDS]),
    Rgbw([RGBW<u8>; NUM_LEDS]),
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // 1. Create a default hardware config
    let mut config = embassy_rp::config::Config::default();
    
    // 2. Force the system clock to 125MHz for timing accuracy with standard PIO code
    config.clocks = embassy_rp::clocks::ClockConfig::system_freq(125_000_000).unwrap();     
    // 3. Initialize the RP2350 with the custom config
    let p = embassy_rp::init(config);

    // RS-485 transceiver enable, active-low.
    let _rs485_enable = Output::new(p.PIN_23, Level::Low);

    // DMX UART Peripheral initialization: 250000 baud, 8N2.
    let mut dmx_uart_cfg = UartConfig::default();
    dmx_uart_cfg.baudrate = 250_000;
    dmx_uart_cfg.data_bits = DataBits::DataBits8;
    dmx_uart_cfg.stop_bits = StopBits::STOP2;
    dmx_uart_cfg.parity = Parity::ParityNone;

    let dmx_uart = Uart::new(
        p.UART1,
        p.PIN_24, // Unused TX Pin allocation
        p.PIN_25, // RX Pin input
        Irqs,
        p.DMA_CH1,
        p.DMA_CH2,
        dmx_uart_cfg,
    );

    let (_dmx_tx, dmx_rx) = dmx_uart.split();
    spawner.spawn(dmx_rx_task(dmx_rx)).unwrap();

    // WS2812 PIO Motor Driver instantiation on GPIO9
    let Pio { mut common, sm0, .. } = Pio::new(p.PIO0, Irqs);

    static PROGRAM: StaticCell<PioWs2812Program<PIO0>> = StaticCell::new();
    let program = PROGRAM.init(PioWs2812Program::new(&mut common));

    // Global pre-calculated coordinate space mapping allocation
    let layout_table: [PixelMeta; NUM_LEDS] = get_layout_map();

    // Initialize ONLY the driver needed, consuming the peripherals exactly once
    let mut led_driver = if !IS_RGBW {
        ActiveDriver::Rgb(PioWs2812::<PIO0, 0, NUM_LEDS, Rgb>::with_color_order(
            &mut common,
            sm0,
            p.DMA_CH0,
            p.PIN_15,
            program,
        ))
    } else {
        ActiveDriver::Rgbw(RgbwPioWs2812::<PIO0, 0, NUM_LEDS, Grbw>::with_color_order(
            &mut common,
            sm0,
            p.DMA_CH0,
            p.PIN_15,
            program,
        ))
    };

    // Initialize our safe buffer matching our active configuration
    let mut leds_output = if IS_RGBW {
        LedBuffer::Rgbw([RGBW::<u8>::default(); NUM_LEDS])
    } else {
        LedBuffer::Rgb([RGB8::default(); NUM_LEDS])
    };

    let mut active_params = DmxParams::default();
    let mut transition = TransitionState::Stable;

    let mut base_offset: u8 = 0;
    let mut top_offset: u8 = 0;

    let mut ticker = Ticker::every(Duration::from_millis(20)); // Clean ~50FPS Refresh rate

    loop {
        // Look for a non-blocking incoming DMX update
        if let Some(new_dmx) = DMX_SIGNAL.try_take() {
            if new_dmx.base_effect_id != active_params.base_effect_id 
                || new_dmx.top_effect_id != active_params.top_effect_id 
            {
                // Trigger a smooth crossfade over 25 frame updates
                transition = TransitionState::Crossfading {
                    old_params: active_params,
                    progress: 0,
                    duration: 25,
                };
            }
            active_params = new_dmx;
        }

        // Apply speed increments only if we aren't using the slider as a static index pointer (debug mode 255)
        if active_params.base_effect_id != 255 {
            base_offset = base_offset.wrapping_add(active_params.speed.clamp(1, 15));
            top_offset = top_offset.wrapping_add(active_params.speed.clamp(1, 15));
        }

        match transition {
            TransitionState::Stable => {
                // Grab master brightness ceiling directly from the active DMX parameter profile
                let master_intensity = active_params.r.max(active_params.g).max(active_params.b) as u32;

                for i in 0..NUM_LEDS {
                    let meta = &layout_table[i];
                    let base_color = neo_effects::render_base_effect(active_params.base_effect_id, base_offset, &active_params, meta);
                    let mixed_color = neo_effects::apply_top_effect(active_params.top_effect_id, top_offset, base_color, meta, &active_params);

                    match &mut leds_output {
                        LedBuffer::Rgb(buf) => {
                            if master_intensity > 0 {
                                buf[i] = RGB8 {
                                    r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                                    g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                                    b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                                };
                            } else {
                                buf[i] = RGB8::default();
                            }
                        }
                        LedBuffer::Rgbw(buf) => {
                            if master_intensity > 0 {
                                buf[i] = RGBW {
                                    r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                                    g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                                    b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                                    a: White(0), // Hardcoded 0 White element since effects engine is 3-channel
                                };
                            } else {
                                buf[i] = RGBW::default();
                            }
                        }
                    }
                }
            }
            TransitionState::Crossfading { old_params, ref mut progress, duration } => {
                *progress += 1;
                let alpha = ((*progress as u16) * 256) / (duration as u16);

                // Dynamically interpolate the master intensity ceiling during a crossfade
                let old_intensity = old_params.r.max(old_params.g).max(old_params.b) as u32;
                let new_intensity = active_params.r.max(active_params.g).max(active_params.b) as u32;
                let master_intensity = ((old_intensity * (256 - alpha as u32)) + (new_intensity * alpha as u32)) >> 8;

                for i in 0..NUM_LEDS {
                    let meta = &layout_table[i];

                    let old_base = neo_effects::render_base_effect(old_params.base_effect_id, base_offset, &old_params, meta);
                    let old_composite = neo_effects::apply_top_effect(old_params.top_effect_id, top_offset, old_base, meta, &old_params);

                    let new_base = neo_effects::render_base_effect(active_params.base_effect_id, base_offset, &active_params, meta);
                    let new_composite = neo_effects::apply_top_effect(active_params.top_effect_id, top_offset, new_base, meta, &active_params);

                    let mixed_color = neo_effects::blend_rgb(old_composite, new_composite, alpha);

                    match &mut leds_output {
                        LedBuffer::Rgb(buf) => {
                            if master_intensity > 0 {
                                buf[i] = RGB8 {
                                    r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                                    g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                                    b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                                };
                            } else {
                                buf[i] = RGB8::default();
                            }
                        }
                        LedBuffer::Rgbw(buf) => {
                            if master_intensity > 0 {
                                buf[i] = RGBW {
                                    r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                                    g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                                    b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                                    a: White(0), // Hardcoded 0 White element since effects engine is 3-channel
                                };
                            } else {
                                buf[i] = RGBW::default();
                            }
                        }
                    }
                }

                if *progress >= duration {
                    transition = TransitionState::Stable;
                }
            }
        }

        // Stream frame via DMA using whichever driver flavor is active
        match &mut led_driver {
            ActiveDriver::Rgb(driver) => {
                if let LedBuffer::Rgb(buf) = &leds_output {
                    driver.write(buf).await;
                }
            }
            ActiveDriver::Rgbw(driver) => {
                if let LedBuffer::Rgbw(buf) = &leds_output {
                    driver.write(buf).await;
                }
            }
        }
        
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn dmx_rx_task(mut rx: UartRx<'static, Async>) {
    let mut frame = [0u8; 513];
    loop {
        match rx.read(&mut frame).await {
            Ok(_) => {
                const START_CH: usize = 97;

                let extracted = DmxParams {
                    r: frame[START_CH + 0],
                    g: frame[START_CH + 1],
                    b: frame[START_CH + 2],
                    base_effect_id: frame[START_CH + 3],
                    top_effect_id: frame[START_CH + 4],
                    speed: frame[START_CH + 5],
                    r2: frame[START_CH + 6],
                    g2: frame[START_CH + 7],
                    b2: frame[START_CH + 8],
                };
                DMX_SIGNAL.signal(extracted);
            }
            Err(embassy_rp::uart::Error::Break) => {}
            Err(embassy_rp::uart::Error::Framing) => {}
            Err(e) => {
                warn!("DMX connection drop or frame issue: {:?}", e);
            }
        }
    }
}