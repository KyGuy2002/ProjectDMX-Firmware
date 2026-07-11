use embassy_executor::Spawner;

use crate::config::*;
use crate::hardware::SlotPwm;

pub mod dimmer;

pub fn init_slot<S>(
    spawner: &Spawner,
    slot_config: ModuleSlot,
    slot: S,
) where
    S: SlotPwm + 'static,
{
    match slot_config {
        ModuleSlot::Dimmer(settings) => {
            dimmer::spawn_dimmer(spawner, settings, slot);
        }

        ModuleSlot::Neo(_settings) => {}

        ModuleSlot::FogMachine(_settings) => {}

        ModuleSlot::AudioAmp(_settings) => {}

        ModuleSlot::Rfid(_settings) => {}

        ModuleSlot::Disabled { .. } => {}
    }
}