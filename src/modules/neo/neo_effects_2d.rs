// src/neo_effects.rs

use smart_leds::RGB8;
use crate::pixel_mapping_config::PixelMeta;
use crate::modules::neo::tick_neo_effect::DmxParams;

/// Fixed point interpolation between frames
pub fn blend_rgb(from: RGB8, to: RGB8, alpha: u16) -> RGB8 {
    let inv_alpha = 256u16.saturating_sub(alpha);
    RGB8 {
        r: ((from.r as u16 * inv_alpha + to.r as u16 * alpha) >> 8) as u8,
        g: ((from.g as u16 * inv_alpha + to.g as u16 * alpha) >> 8) as u8,
        b: ((from.b as u16 * inv_alpha + to.b as u16 * alpha) >> 8) as u8,
    }
}

/// Generates base background imagery onto buffers
pub fn render_base_effect(
    id: u8,
    offset: u8,
    params: &DmxParams,
    meta: &PixelMeta,
) -> RGB8 {
    if !meta.is_valid {
        return RGB8::default();
    }

    // Extract the primary and secondary color structs straight from incoming DMX parameters
    let color1 = RGB8::new(params.r, params.g, params.b);
    let color2 = RGB8::new(params.r2, params.g2, params.b2);

    match id {

        // 0: Dual-Color Pinwheel (Smoothly rotates using only the 2 DMX colors)
        0 => {
            // Generates a revolving wave using the pixel's X/Y matrix layout footprint
            let pixel_angle = ((meta.x as u16 * 2) + (meta.y as u16 * 2)) & 255;
            let wave_pos = pixel_angle.wrapping_add(offset as u16) & 255;
            
            // Map the wave cycle position to a clean 0..255 linear blending scalar
            let blend_factor = if wave_pos < 128 {
                (wave_pos * 2) as u16
            } else {
                ((255 - wave_pos) * 2) as u16
            };

            blend_rgb(color1, color2, blend_factor)
        }

        // 1: Original 4th of July continuous layout effect (re-mapped smoothly using physical coordinates)
        1 => {
            // Instead of using an external index lookup, we use your original formula's logic
            // mapped continuously across the individual letter's relative coordinate space.
            let pos = meta.y.wrapping_add(offset);
            patriotic_wheel(pos)
        }

        // 2: Continuous multi-letter Pinwheel layout mapping
        2 => {
            let pos = meta.x.wrapping_add(offset);
            wheel(pos)
        }

        // 3: Random Custom Color Stardust Twinkle (Controlled Speed and Density)
        3 => {
            // --- ADJUSTABLE TUNING VARIABLES ---
            let speed_divisor = 8;  // Higher = slower change (e.g., 4 means change color states every 4 frames)
            let density_cutoff = 238; // Higher = fewer pixels on (238 leaves ~7% of pixels lit concurrently)
            // -----------------------------------

            // Slow down the time steps smoothly by dividing the frame offset
            let slow_offset = (offset / speed_divisor) as u32;

            // Ensure the base is always an odd number so the power cycle doesn't flatline
            let base = (meta.index as u8).wrapping_mul(37) | 1;
            
            // Generate the chaotic exponential state
            let pseudo_rand = base.wrapping_pow(slow_offset);
            
            if pseudo_rand > density_cutoff {
                color1
            } else {
                RGB8::default()
            }
        }

        // 4: Solid static uniform color mapping
        4 => color1,

        // 5: 4th July Static letter assignments (U=Red, S=White, A=Blue, Mirroring to 250)
        5 => match meta.letter_id {
            0 | 5 => RGB8::new(255, 0, 0),     // U and 0 -> Red
            1 | 4 => RGB8::new(255, 255, 255), // S and 5 -> White
            2 | 3 => RGB8::new(0, 0, 255),     // A and 2 -> Blue
            _ => RGB8::default(),
        },

        // 6: USA vs 250 Split (USA gets Color 1, 250 gets Color 2)
        6 => {
            if meta.letter_id <= 2 {
                color1 // Letters 0, 1, 2 ("U", "S", "A")
            } else {
                color2 // Letters 3, 4, 5 ("2", "5", "0")
            }
        }

        // 7: Alternating Zebra Split (U, A, 5 get Color 1 | S, 2, 0 get Color 2)
        7 => {
            if meta.letter_id % 2 == 0 {
                color1 // Letters 0 ("U"), 2 ("A"), 4 ("5")
            } else {
                color2 // Letters 1 ("S"), 3 ("2"), 5 ("0")
            }
        }

        // 8: Electrical Pulse Wave (Pulse Color = Color1, Background = Color2)
        8 => {
            // --- ADJUSTABLE TUNING VARIABLES ---
            let pulse_count = 3;   // Number of electrical pulses visible across the entire sign at once
            let pulse_width = 8;   // Total width of each pulse measured in pixels
            let NUM_LEDS = 100; // TODO dont hardcode
            let speed_slowdown = 3;  // Higher = Slower pulse speed. (1 = default, 3 = three times slower)
            // -----------------------------------

            // 1. Slow down the incoming frame tick safely using a bit-shift
            let slow_offset = (offset >> speed_slowdown) as u16;

            // 2. Continuous movement phase tracking
            let spacing = (NUM_LEDS / pulse_count) as u16;
            let pixel_pos = (meta.index as u16).wrapping_add(slow_offset);
            let cycle_position = pixel_pos % spacing;

            // 3. Draw either the moving blob or the background color
            if cycle_position < pulse_width as u16 {
                color1 // Blob color
            } else {
                color2 // Wire background color
            }
        }

        // 255: Diagnostic Mode - Light a single pixel by raw index using the speed channel value
        255 => {
            // Use the speed parameter directly as the target pixel index
            if meta.index == (params.speed as usize) {
                RGB8::new(255, 255, 255) // Solid white for the identified pixel
            } else {
                RGB8::default() // Turn everything else off
            }
        }

        _ => RGB8::default(),
    }
}

pub fn apply_top_effect(
    id: u8,
    frame_counter: u8, 
    bg_color: RGB8,
    meta: &PixelMeta,
    params: &DmxParams,
) -> RGB8 {
    if !meta.is_valid {
        return RGB8::default();
    }

    // -------------------------------------------------------------
    // SHARED GLOBAL EFFECT CONFIGURATION
    // -------------------------------------------------------------
    let dim_percentage: u16 = 5; // 5 = 5% baseline glow
    
    // Horizontal space constraints
    let horiz_beam_width: i32 = 25; 
    let horiz_fade_range: i32 = 20; 
    let horiz_min_x: i32 = 0-60;   // Where the bar starts (off-screen left)
    let horiz_max_x: i32 = 236+60;   // Where the bar ends (off-screen right)
    let horiz_center_x: i32 = 117;

    // Vertical space constraints
    let vert_beam_width: i32 = 30;  
    let vert_fade_range: i32 = 20;  

    // Unified background dim profile shared across all sweep paths
    let dimmed_bg = RGB8 {
        r: ((bg_color.r as u16 * dim_percentage) / 100) as u8,
        g: ((bg_color.g as u16 * dim_percentage) / 100) as u8,
        b: ((bg_color.b as u16 * dim_percentage) / 100) as u8,
    };

    // Dynamically scale target_pos to map perfectly across your specified min/max span
    let horiz_span = horiz_max_x - horiz_min_x;
    let target_pos = horiz_min_x + ((frame_counter as i32 * horiz_span) / 255);

    match id {
        // 1..4 Horizontal panning sweeps
        1 | 2 | 3 | 4 => {
            let x_val = meta.x as i32;
            let max_horiz_window = horiz_beam_width + horiz_fade_range;

            let dist = match id {
                1 => (x_val - target_pos).abs(), // Left to Right
                2 => ((255 - x_val) - target_pos).abs(), // Right to Left
                
                // Outside-In Sweep (L -> Center)
                3 => {
                    let center_span = horiz_center_x - horiz_min_x;
                    let split_target = horiz_min_x + ((frame_counter as i32 * center_span) / 255);
                    ((x_val - horiz_center_x).abs() - (horiz_center_x - split_target)).abs()
                },
                
                // Inside-Out Sweep (Center -> Outward to min/max edges)
                4 => {
                    let center_span = horiz_center_x - horiz_min_x;
                    let split_target = horiz_min_x + ((frame_counter as i32 * center_span) / 255);
                    ((x_val - horiz_center_x).abs() - (split_target - horiz_min_x)).abs()
                },
                _ => 255,
            };

            if dist <= horiz_beam_width {
                bg_color 
            } else if dist <= max_horiz_window {
                let alpha = ((max_horiz_window - dist) * 256) / horiz_fade_range;
                blend_rgb(dimmed_bg, bg_color, alpha as u16) 
            } else {
                dimmed_bg 
            }
        }

        // 5..6 Vertical panning sweeps
        5 | 6 => {
            let y_val = meta.y as i32;
            let max_vert_window = vert_beam_width + vert_fade_range;

            let dist = match id {
                5 => (y_val - target_pos).abs(),       
                6 => ((255 - y_val) - target_pos).abs(), 
                _ => 255,
            };

            if dist <= vert_beam_width {
                bg_color 
            } else if dist <= max_vert_window {
                let alpha = ((max_vert_window - dist) * 256) / vert_fade_range;
                blend_rgb(dimmed_bg, bg_color, alpha as u16)
            } else {
                dimmed_bg
            }
        }

        // 7: Segmented Letter Flash Strobe (Strict single-letter activation with black gaps)
        7 => {
            let ultra_slow_tick = frame_counter / 16;

            let mut seed = (ultra_slow_tick as u32).wrapping_add(1013904223);
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let random_val = (seed >> 16) as u16;
            let target_letter = (random_val % 6) as u8;

            let is_match = meta.letter_id == target_letter as usize;
            let is_blackout_interval = ((frame_counter / 8) % 2) != 0;

            let force_dark = if params.speed > 120 {
                (ultra_slow_tick % 3) == 0 
            } else {
                false
            };

            if is_match && !is_blackout_interval && !force_dark {
                bg_color
            } else {
                RGB8::default() // Strobe still drops to complete black for crispness
            }
        }

        // 8: Out-to-In Letter Sweep (With end-of-cycle pause)
        // 9: In-to-Out Letter Sweep (With end-of-cycle pause)
        8 | 9 => {
            let fade_threshold = 50u8;
            
            // Slice into 4 parts of 64 frames (Total = 256). 
            // Steps 0, 1, 2 fire letters. Step 3 acts as a full-duration pause.
            let step = frame_counter / 64; 

            let is_targeted = match id {
                8 => match step {
                    0 => meta.letter_id == 0 || meta.letter_id == 5, // Outer (U & 0)
                    1 => meta.letter_id == 1 || meta.letter_id == 4, // Middle (S & 5)
                    2 => meta.letter_id == 2 || meta.letter_id == 3, // Inner (A & 2)
                    _ => false, // Step 3: Complete pause. All letters stay dim.
                },
                9 => match step {
                    0 => meta.letter_id == 2 || meta.letter_id == 3, // Inner (A & 2)
                    1 => meta.letter_id == 1 || meta.letter_id == 4, // Middle (S & 5)
                    2 => meta.letter_id == 0 || meta.letter_id == 5, // Outer (U & 0)
                    _ => false, // Step 3: Complete pause. All letters stay dim.
                },
                _ => false,
            };

            if params.speed < fade_threshold {
                if is_targeted {
                    // Normalize progress inside the compressed 64-tick step window
                    let intra_step = (frame_counter % 64) as i32; 
                    
                    // Keep the 25-tick fade window proportional to the shorter step
                    let time_fade = if intra_step < 25 {
                        (intra_step * 256) / 25 
                    } else if intra_step > 39 {
                        ((64 - intra_step) * 256) / 25 
                    } else {
                        256 // Solid hold for 14 frames in the middle
                    };

                    let speed_scalar = (fade_threshold as i32 - params.speed as i32) * 256 / fade_threshold as i32;
                    let alpha = ((time_fade * speed_scalar) >> 8).clamp(0, 255);

                    blend_rgb(dimmed_bg, bg_color, alpha as u16)
                } else {
                    dimmed_bg
                }
            } else {
                if is_targeted { bg_color } else { dimmed_bg }
            }
        }

        _ => bg_color,
    }
}

fn patriotic_wheel(pos: u8) -> RGB8 {
    let section = pos / 64;
    let step = (pos % 64) as u16;
    match section {
        0 => { let v = fade(step); RGB8::new(255, v, v) }
        1 => { let v = fade(63 - step); RGB8::new(v, v, 255) }
        2 => { let v = fade(step); RGB8::new(v, v, 255) }
        _ => { let v = fade(63 - step); RGB8::new(255, v, v) }
    }
}

fn fade(step: u16) -> u8 {
    ((step * 255) / 63) as u8
}

fn wheel(mut pos: u8) -> RGB8 {
    pos = 255u8.wrapping_sub(pos);
    if pos < 85 {
        RGB8::new(255u8.wrapping_sub(pos * 3), 0, pos * 3)
    } else if pos < 170 {
        let pos = pos - 85;
        RGB8::new(0, pos * 3, 255u8.wrapping_sub(pos * 3))
    } else {
        let pos = pos - 170;
        RGB8::new(pos * 3, 255u8.wrapping_sub(pos * 3), 0)
    }
}