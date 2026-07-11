use defmt::*;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{Rgb, Grbw, PioWs2812, RgbwPioWs2812, PioWs2812Program};
use smart_leds::{RGB8, RGBW, White};
use static_cell::StaticCell;

use crate::config::{get_layout_map, PixelMeta, NUM_LEDS};
use crate::dmx_state::{DmxParams, DMX_SIGNAL};
use crate::neo_effects;

/// Defines how the pixels interpret the current DMX state data at runtime
#[derive(Copy, Clone, Format, PartialEq)]
pub enum OutputMode {
    /// Render dynamic generative effects based on mathematical functions and single DMX parameters.
    GeneratedEffects,
    /// Direct 1:1 mapping where incoming channel strings stream directly into pixel arrays.
    RawMultiUniverse,
}

/// The system configuration state containing operational configurations.
/// Can be updated safely via an embassy channel or global atomic variable.
pub struct LedEngineConfig {
    pub mode: OutputMode,
    pub is_rgbw: bool,
}

/// Wrapper variant containing initialized hardware driver pins.
pub enum ActiveDriver<'d> {
    Rgb(PioWs2812<'d, PIO0, 0, NUM_LEDS, Rgb>),
    Rgbw(RgbwPioWs2812<'d, PIO0, 0, NUM_LEDS, Grbw>),
}

/// Buffer structures maintaining localized copy of state prior to DMA write.
pub enum LedBuffer {
    Rgb([RGB8; NUM_LEDS]),
    Rgbw([RGBW<u8>; NUM_LEDS]),
}

/// Tracks smooth transitions between generative states.
enum TransitionState {
    Stable,
    Crossfading {
        old_params: DmxParams,
        progress: u8,
        duration: u8,
    },
}

#[embassy_executor::task]
pub async fn led_render_task(
    mut driver: ActiveDriver<'static>,
    mut config: LedEngineConfig,
    raw_universe_buffer: &'static [u8; 2048], // Holds bulk raw data for multiple universes (e.g. 4 universes * 512)
) {
    let layout_table: [PixelMeta; NUM_LEDS] = get_layout_map();
    
    // Allocate dual output buffers matching our compilation constraints
    let mut leds_output = if config.is_rgbw {
        LedBuffer::Rgbw([RGBW::<u8>::default(); NUM_LEDS])
    } else {
        LedBuffer::Rgb([RGB8::default(); NUM_LEDS])
    };

    let mut active_params = DmxParams::default();
    let mut transition = TransitionState::Stable;
    let mut base_offset: u8 = 0;
    let mut top_offset: u8 = 0;
    
    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_millis(20)); // ~50FPS Refresh rate

    loop {
        // 1. Runtime Input Parsing: Fetch parameters intended for Generative Mode
        if let Some(new_dmx) = DMX_SIGNAL.try_take() {
            if new_dmx.base_effect_id != active_params.base_effect_id 
                || new_dmx.top_effect_id != active_params.top_effect_id 
            {
                transition = TransitionState::Crossfading {
                    old_params: active_params,
                    progress: 0,
                    duration: 25,
                };
            }
            active_params = new_dmx;
        }

        // 2. Runtime Execution Strategy Selection
        match config.mode {
            OutputMode::GeneratedEffects => {
                // Procedural generation loop (your original animation logic)
                if active_params.base_effect_id != 255 {
                    base_offset = base_offset.wrapping_add(active_params.speed.clamp(1, 15));
                    top_offset = top_offset.wrapping_add(active_params.speed.clamp(1, 15));
                }

                match transition {
                    TransitionState::Stable => {
                        let master_intensity = active_params.r.max(active_params.g).max(active_params.b) as u32;
                        for i in 0..NUM_LEDS {
                            let meta = &layout_table[i];
                            let base_color = neo_effects::render_base_effect(active_params.base_effect_id, base_offset, &active_params, meta);
                            let mixed_color = neo_effects::apply_top_effect(active_params.top_effect_id, top_offset, base_color, meta, &active_params);

                            write_to_buffer(&mut leds_output, i, mixed_color, master_intensity);
                        }
                    }
                    TransitionState::Crossfading { old_params, ref mut progress, duration } => {
                        *progress += 1;
                        let alpha = ((*progress as u16) * 256) / (duration as u16);
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
                            write_to_buffer(&mut leds_output, i, mixed_color, master_intensity);
                        }

                        if *progress >= duration {
                            transition = TransitionState::Stable;
                        }
                    }
                }
            }

            OutputMode::RawMultiUniverse => {
                // Raw mapping loop: Each pixel consumes 3 elements out of bulk universe memory array
                // Maps arrays sequentially over multiple universes transparently
                for i in 0..NUM_LEDS {
                    let src_idx = i * 3;
                    if src_idx + 2 < raw_universe_buffer.len() {
                        let raw_color = RGB8 {
                            r: raw_universe_buffer[src_idx],
                            g: raw_universe_buffer[src_idx + 1],
                            b: raw_universe_buffer[src_idx + 2],
                        };
                        // Raw mode honors its own pixel intensity scaling directly from frame values
                        write_to_buffer(&mut leds_output, i, raw_color, 255);
                    }
                }
            }
        }

        // 3. Write Frame directly to Hardware
        match &mut driver {
            ActiveDriver::Rgb(d) => {
                if let LedBuffer::Rgb(buf) = &leds_output { d.write(buf).await; }
            }
            ActiveDriver::Rgbw(d) => {
                if let LedBuffer::Rgbw(buf) = &leds_output { d.write(buf).await; }
            }
        }

        ticker.next().await;
    }
}

/// Helper abstraction layer to parse formatting elements directly down to structural frames safely.
fn write_to_buffer(buffer: &mut LedBuffer, index: usize, color: RGB8, intensity: u32) {
    if index >= NUM_LEDS { return; }
    match buffer {
        LedBuffer::Rgb(buf) => {
            if intensity > 0 {
                buf[index] = RGB8 {
                    r: ((color.r as u32 * intensity) / 255) as u8,
                    g: ((color.g as u32 * intensity) / 255) as u8,
                    b: ((color.b as u32 * intensity) / 255) as u8,
                };
            } else {
                buf[index] = RGB8::default();
            }
        }
        LedBuffer::Rgbw(buf) => {
            if intensity > 0 {
                buf[index] = RGBW {
                    r: ((color.r as u32 * intensity) / 255) as u8,
                    g: ((color.g as u32 * intensity) / 255) as u8,
                    b: ((color.b as u32 * intensity) / 255) as u8,
                    a: White(0),
                };
            } else {
                buf[index] = RGBW::default();
            }
        }
    }
}