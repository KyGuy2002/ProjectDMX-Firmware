use embassy_executor::Spawner;

use crate::config::*;
use crate::hardware::*;

mod dimmer;
mod neo;

/**
 * This will eventually use the config for module order but for now its hardcoded, along with the pin assignments and pwm slices and stuff
 */

pub fn init_slot_a(spawner: &Spawner, slot_config: ModuleSlot, r: SlotARelayResources) {
    
}

pub fn init_slot_b(spawner: &Spawner, slot_config: ModuleSlot, r: SlotBUnusedResources) {
    
}

// Neo slot C
pub fn init_slot_c(spawner: &Spawner, slot_config: ModuleSlot, r: SlotCNeoResources) {
    if let ModuleSlot::Neo(settings) = slot_config {
        spawner.spawn(neo::neo_task(settings, r)).unwrap();
    }
}

// Dimmer slot D
pub fn init_slot_d(spawner: &Spawner, slot_config: ModuleSlot, r: SlotDDimmerResources) {
    if let ModuleSlot::Dimmer(settings) = slot_config {
        spawner.spawn(dimmer::dimmer_task(settings, r)).unwrap();
    }
}