use smart_leds::RGB8;

use crate::{MAX_PIXELS, config::{EnabledPort}, read_channels};
use crate::modules::neo::neo_effects_2d;
use crate::pixel_mapping_config::PixelMeta;




pub fn tick_wire_effect_rgb(
    port_config: EnabledPort,
    state: &mut NeoEffectState,
    layout_table: &[PixelMeta; MAX_PIXELS],
    leds_output: &mut [RGB8; MAX_PIXELS],
) {


    // Get DMX params
    let ch = read_channels::<9>(port_config.universe as usize, port_config.start_channel as usize);

    let new_dmx_params = DmxParams {
        r: ch[0],
        g: ch[1],
        b: ch[2],
        base_effect_id: ch[3],
        top_effect_id: ch[4],
        speed: ch[5],
        r2: ch[6],
        g2: ch[7],
        b2: ch[8],
    };


    if new_dmx_params.base_effect_id != state.active_params.base_effect_id 
        || new_dmx_params.top_effect_id != state.active_params.top_effect_id 
    {
        state.transition = TransitionState::Crossfading {
            old_params: state.active_params,
            progress: 0,
            duration: 25,
        };
    }

    state.active_params = new_dmx_params;


    // Apply speed increments only if we aren't using the slider as a static index pointer (debug mode 255)
    if state.active_params.base_effect_id != 255 {
        state.base_offset = state.base_offset.wrapping_add(state.active_params.speed.clamp(1, 15));
        state.top_offset = state.top_offset.wrapping_add(state.active_params.speed.clamp(1, 15));
    }


    match state.transition {
        TransitionState::Stable => {
            // Grab master brightness ceiling directly from the active DMX parameter profile
            let master_intensity = state.active_params.r.max(state.active_params.g).max(state.active_params.b) as u32;

            for i in 0..port_config.pixel_count {
                let meta = &layout_table[i];
                let base_color = neo_effects_2d::render_base_effect(state.active_params.base_effect_id, state.base_offset, &state.active_params, meta);
                let mixed_color = neo_effects_2d::apply_top_effect(state.active_params.top_effect_id, state.top_offset, base_color, meta, &state.active_params);

                if master_intensity > 0 {
                    leds_output[i] = RGB8 {
                        r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                        g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                        b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                    };
                } else {
                    leds_output[i] = RGB8::default();
                }
            }
        }
        TransitionState::Crossfading { old_params, ref mut progress, duration } => {
            *progress += 1;
            let alpha = ((*progress as u16) * 256) / (duration as u16);

            // Dynamically interpolate the master intensity ceiling during a crossfade
            let old_intensity = old_params.r.max(old_params.g).max(old_params.b) as u32;
            let new_intensity = state.active_params.r.max(state.active_params.g).max(state.active_params.b) as u32;
            let master_intensity = ((old_intensity * (256 - alpha as u32)) + (new_intensity * alpha as u32)) >> 8;

            for i in 0..port_config.pixel_count {
                let meta = &layout_table[i];

                let old_base = neo_effects_2d::render_base_effect(old_params.base_effect_id, state.base_offset, &old_params, meta);
                let old_composite = neo_effects_2d::apply_top_effect(old_params.top_effect_id, state.top_offset, old_base, meta, &old_params);

                let new_base = neo_effects_2d::render_base_effect(state.active_params.base_effect_id, state.base_offset, &state.active_params, meta);
                let new_composite = neo_effects_2d::apply_top_effect(state.active_params.top_effect_id, state.top_offset, new_base, meta, &state.active_params);

                let mixed_color = neo_effects_2d::blend_rgb(old_composite, new_composite, alpha);

                if master_intensity > 0 {
                    leds_output[i] = RGB8 {
                        r: ((mixed_color.r as u32 * master_intensity) / 255) as u8,
                        g: ((mixed_color.g as u32 * master_intensity) / 255) as u8,
                        b: ((mixed_color.b as u32 * master_intensity) / 255) as u8,
                    };
                } else {
                    leds_output[i] = RGB8::default();
                }
            }

            if *progress >= duration {
                state.transition = TransitionState::Stable;
            }
        }
    }

}






#[derive(Clone, Copy, PartialEq, Debug, defmt::Format)]
pub struct DmxParams {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub base_effect_id: u8,
    pub top_effect_id: u8,
    pub speed: u8,
    pub r2: u8,
    pub g2: u8,
    pub b2: u8,
}

impl Default for DmxParams {
    fn default() -> Self {
        Self {
            r: 255,
            g: 0,
            b: 0,
            base_effect_id: 8,
            top_effect_id: 0,
            speed: 4,
            r2: 0,
            g2: 0,
            b2: 0,
        }
    }
}



/// Tracks running crossfader configurations without dynamic heap allocations.
#[derive(Clone, Copy)]
enum TransitionState {
    Stable,
    Crossfading {
        old_params: DmxParams,
        progress: u8,
        duration: u8,
    },
}



pub struct NeoEffectState {
    active_params: DmxParams,
    transition: TransitionState,
    base_offset: u8,
    top_offset: u8,
}

impl NeoEffectState {
    pub fn new() -> Self {
        Self {
            active_params: DmxParams::default(),
            transition: TransitionState::Stable,
            base_offset: 0,
            top_offset: 0,
        }
    }
}