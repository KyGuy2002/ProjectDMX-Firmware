use embassy_executor::Spawner;

use crate::config::*;
use crate::hardware::SlotPins;

mod dimmer;

pub fn init_slot(spawner: &Spawner, slot_config: ModuleSlot, pins: SlotPins) {
    match slot_config {
        ModuleSlot::Neo(settings) => {
            // spawner.spawn(neopixel_driver_task(settings, pins)).unwrap();
        }
        ModuleSlot::Dimmer(settings) => {
            spawner.spawn(dimmer::dimmer_task(settings, pins)).unwrap();
        }
        ModuleSlot::FogMachine(settings) => {
            // spawner.spawn(fog_machine_task(settings, pins)).unwrap();
        }
        ModuleSlot::AudioAmp(settings) => {
            // spawner.spawn(audio_amp_task(settings, pins)).unwrap();
        }
        ModuleSlot::Rfid(settings) => {
            // spawner.spawn(rfid_task(settings, pins)).unwrap();
        }
        ModuleSlot::Disabled { .. } => {}
    }
}