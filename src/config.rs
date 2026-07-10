pub const NUM_LEDS: usize = 200;

#[derive(Clone, Copy, Debug)]
pub struct PixelMeta {
    pub index: usize,
    pub letter_id: usize, // 0='U', 1='S', 2='A', 3='2', 4='5', 5='0'
    pub x: u8,            // 0 (Far Left) to 255 (Far Right)
    pub y: u8,            // 0 (Top) to 255 (Bottom)
    pub is_valid: bool,   // true = lit, false = unmapped/skipped spacer wire
}

pub struct SegmentRule {
    pub start_idx: usize,
    pub length: usize,
    pub letter_id: usize,
    pub min_x: u8,
    pub max_x: u8,
    pub min_y: u8,
    pub max_y: u8,
    pub force_invalid: bool,
    pub invert_y: bool, // Explicit flag instead of guessing by letter_id
}

impl SegmentRule {
    /// Standard vertical or horizontal line where the wiring goes forward (top-to-bottom / left-to-right)
    pub const fn line(start_idx: usize, length: usize, letter_id: usize, min_x: u8, max_x: u8, min_y: u8, max_y: u8) -> Self {
        Self {
            start_idx,
            length,
            letter_id,
            min_x,
            max_x,
            min_y,
            max_y,
            force_invalid: false,
            invert_y: false,
        }
    }

    /// Standard line or segment where the physical wire is traveling backward (bottom-to-top)
    pub const fn line_inverted(start_idx: usize, length: usize, letter_id: usize, min_x: u8, max_x: u8, min_y: u8, max_y: u8) -> Self {
        Self {
            start_idx,
            length,
            letter_id,
            min_x,
            max_x,
            min_y,
            max_y,
            force_invalid: false,
            invert_y: true,
        }
    }

    /// A diagonal slash. Maps explicitly from (x1, y1) to (x2, y2) in absolute spatial coordinates
    pub const fn diagonal(start_idx: usize, length: usize, letter_id: usize, x1: u8, y1: u8, x2: u8, y2: u8) -> Self {
        Self {
            start_idx,
            length,
            letter_id,
            min_x: x1,
            max_x: x2,
            min_y: y1,
            max_y: y2,
            force_invalid: false,
            invert_y: false, // Keep spatial direction absolute
        }
    }

    pub const fn single_pixel(idx: usize, letter_id: usize, x: u8, y: u8) -> Self {
        Self {
            start_idx: idx,
            length: 1,
            letter_id,
            min_x: x,
            max_x: x,
            min_y: y,
            max_y: y,
            force_invalid: false,
            invert_y: false,
        }
    }

    pub const fn skip(start_idx: usize, length: usize) -> Self {
        Self {
            start_idx,
            length,
            letter_id: 0,
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
            force_invalid: true,
            invert_y: false,
        }
    }
}

// -------------------------------------------------------------
// SETTINGS FILE CONFIGURATION BLOCK
// -------------------------------------------------------------
// Define your physical straight line blocks here. 
// Any index NOT covered here automatically drops into the skip/blank pool.

// U: 0   S: 1   A: 2   L2: 3   L5: 4   L0: 5

const CONFIG_SEGMENTS: &[SegmentRule] = &[


    SegmentRule::line(0, 400, 0, 0,0, 0, 0), // Top left flat 2


];

pub const fn get_layout_map() -> [PixelMeta; NUM_LEDS] {
    let mut map = [PixelMeta {
        index: 0,
        letter_id: 0,
        x: 0,
        y: 0,
        is_valid: false,
    }; NUM_LEDS];

    let mut i = 0;
    while i < NUM_LEDS {
        map[i].index = i;
        let mut matched = false;
        let mut seg_idx = 0;

        while seg_idx < CONFIG_SEGMENTS.len() {
            let seg = &CONFIG_SEGMENTS[seg_idx];
            
            if i >= seg.start_idx && i < (seg.start_idx + seg.length) {
                if seg.force_invalid {
                    break;
                }

                let local_offset = i - seg.start_idx;
                
                let step = if seg.length > 1 {
                    (local_offset * 255) / (seg.length - 1)
                } else {
                    0
                };
                
                let delta_x = (seg.max_x as i16 - seg.min_x as i16) as i32;
                let delta_y = (seg.max_y as i16 - seg.min_y as i16) as i32;

                map[i].letter_id = seg.letter_id;
                map[i].x = (seg.min_x as i32 + ((step as i32 * delta_x) / 255)) as u8;
                
                if seg.length == 1 || delta_y == 0 {
                    map[i].y = seg.min_y;
                } else if !seg.invert_y {
                    // Coordinates map exactly forward down physical space
                    map[i].y = (seg.min_y as i32 + ((step as i32 * delta_y) / 255)) as u8;
                } else {
                    // Physical wire goes upward, map coordinates reversed
                    map[i].y = (seg.max_y as i32 - ((step as i32 * delta_y) / 255)) as u8;
                }
                
                map[i].is_valid = true;
                matched = true;
                break;
            }
            seg_idx += 1;
        }

        if !matched {
            map[i].is_valid = false;
            map[i].x = 0;
            map[i].y = 0;
            map[i].letter_id = 0;
        }

        i += 1;
    }

    map
}