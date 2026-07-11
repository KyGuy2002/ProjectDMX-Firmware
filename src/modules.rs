pub mod dimmer;

pub fn init_slot(spawner: &Spawner, slot_config: ModuleSlot, pins: SlotPins) {
    match slot_config {
        ModuleSlot::Neo(settings) => {
            // spawner.spawn(neopixel_driver_task(settings, pins)).unwrap();
        }
        ModuleSlot::Dimmer(settings) => {
            spawner.spawn(dimmer_task(settings, pins)).unwrap();
        }
        ModuleSlot::FogMachine(settings) => {
            // spawner.spawn(fog_machine_task(settings, pins)).unwrap();
        }
        ModuleSlot::Disabled { .. } => {}
    }
}