//! Safe structured extraction of incoming DMX commands.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

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

// Thread-safe mechanism passing atomic updates between frames
pub static DMX_SIGNAL: Signal<CriticalSectionRawMutex, DmxParams> = Signal::new();