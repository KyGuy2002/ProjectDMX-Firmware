use embassy_executor::Spawner;
use embassy_rp::pwm::{Config as PwmConfig, Pwm};

use crate::config::*;
use crate::hardware::{PwmSlices, SlotA, SlotB, SlotC, SlotD, Slots};

pub mod dimmer;

pub fn init_modules(
    spawner: &Spawner,
    slot_a_config: ModuleSlot,
    slot_b_config: ModuleSlot,
    slot_c_config: ModuleSlot,
    slot_d_config: ModuleSlot,
    slots: Slots,
    pwm: PwmSlices,
) {
    match slot_d_config {
        ModuleSlot::Dimmer(settings) => {
            init_slot_d_dimmer(spawner, settings, slots.slot_d, pwm);
            return;
        }
        _ => {}
    }

    match slot_a_config {
        ModuleSlot::Dimmer(settings) => {
            init_slot_a_dimmer(spawner, settings, slots.slot_a, pwm);
            return;
        }
        _ => {}
    }

    match slot_b_config {
        ModuleSlot::Dimmer(settings) => {
            init_slot_b_dimmer(spawner, settings, slots.slot_b, pwm);
            return;
        }
        _ => {}
    }

    match slot_c_config {
        ModuleSlot::Dimmer(settings) => {
            init_slot_c_dimmer(spawner, settings, slots.slot_c, pwm);
            return;
        }
        _ => {}
    }
}

fn pwm_cfg() -> PwmConfig {
    let mut cfg = PwmConfig::default();
    cfg.top = 255;
    cfg.compare_a = 0;
    cfg.compare_b = 0;
    cfg
}

// Slot D:
// pin1 = PIN_19 = PWM_SLICE1 B = DMX channel 1
// pin2 = PIN_18 = PWM_SLICE1 A = DMX channel 2
// pin3 = PIN_17 = PWM_SLICE0 B = DMX channel 3
// pin4 = PIN_16 = PWM_SLICE0 A = DMX channel 4
fn init_slot_d_dimmer(
    spawner: &Spawner,
    settings: DimmerConfig,
    slot: SlotD,
    pwm: PwmSlices,
) {
    let pwm_18_19 = Pwm::new_output_ab(pwm.slice1, slot.pin2, slot.pin1, pwm_cfg());
    let pwm_16_17 = Pwm::new_output_ab(pwm.slice0, slot.pin4, slot.pin3, pwm_cfg());

    dimmer::spawn_pair(spawner, settings.clone(), pwm_18_19, 1, 0);
    dimmer::spawn_pair(spawner, settings, pwm_16_17, 3, 2);
}

// Slot A:
// pin1 = PIN_4  = PWM_SLICE2 A = DMX channel 1
// pin2 = PIN_3  = PWM_SLICE1 B = DMX channel 2
// pin3 = PIN_2  = PWM_SLICE1 A = DMX channel 3
// pin4 = PIN_40 = PWM_SLICE8 A = DMX channel 4
fn init_slot_a_dimmer(
    spawner: &Spawner,
    settings: DimmerConfig,
    slot: SlotA,
    pwm: PwmSlices,
) {
    let pwm_pin1 = Pwm::new_output_a(pwm.slice2, slot.pin1, pwm_cfg());
    let pwm_pin2_pin3 = Pwm::new_output_ab(pwm.slice1, slot.pin3, slot.pin2, pwm_cfg());
    let pwm_pin4 = Pwm::new_output_a(pwm.slice8, slot.pin4, pwm_cfg());

    dimmer::spawn_single_a(spawner, settings.clone(), pwm_pin1, 0);
    dimmer::spawn_pair(spawner, settings.clone(), pwm_pin2_pin3, 2, 1);
    dimmer::spawn_single_a(spawner, settings, pwm_pin4, 3);
}

// Slot B:
// pin1 = PIN_8  = PWM_SLICE4 A = DMX channel 1
// pin2 = PIN_6  = PWM_SLICE3 A = DMX channel 2
// pin3 = PIN_5  = PWM_SLICE2 B = DMX channel 3
// pin4 = PIN_41 = PWM_SLICE8 B = DMX channel 4
fn init_slot_b_dimmer(
    spawner: &Spawner,
    settings: DimmerConfig,
    slot: SlotB,
    pwm: PwmSlices,
) {
    let pwm_pin1 = Pwm::new_output_a(pwm.slice4, slot.pin1, pwm_cfg());
    let pwm_pin2 = Pwm::new_output_a(pwm.slice3, slot.pin2, pwm_cfg());
    let pwm_pin3 = Pwm::new_output_b(pwm.slice2, slot.pin3, pwm_cfg());
    let pwm_pin4 = Pwm::new_output_b(pwm.slice8, slot.pin4, pwm_cfg());

    dimmer::spawn_single_a(spawner, settings.clone(), pwm_pin1, 0);
    dimmer::spawn_single_a(spawner, settings.clone(), pwm_pin2, 1);
    dimmer::spawn_single_b(spawner, settings.clone(), pwm_pin3, 2);
    dimmer::spawn_single_b(spawner, settings, pwm_pin4, 3);
}

// Slot C:
// pin1 = PIN_15 = PWM_SLICE7 B = DMX channel 1
// pin2 = PIN_14 = PWM_SLICE7 A = DMX channel 2
// pin3 = PIN_9  = PWM_SLICE4 B = DMX channel 3
// pin4 = PIN_7  = PWM_SLICE3 B = DMX channel 4
fn init_slot_c_dimmer(
    spawner: &Spawner,
    settings: DimmerConfig,
    slot: SlotC,
    pwm: PwmSlices,
) {
    let pwm_pin1_pin2 = Pwm::new_output_ab(pwm.slice7, slot.pin2, slot.pin1, pwm_cfg());
    let pwm_pin3 = Pwm::new_output_b(pwm.slice4, slot.pin3, pwm_cfg());
    let pwm_pin4 = Pwm::new_output_b(pwm.slice3, slot.pin4, pwm_cfg());

    dimmer::spawn_pair(spawner, settings.clone(), pwm_pin1_pin2, 1, 0);
    dimmer::spawn_single_b(spawner, settings.clone(), pwm_pin3, 2);
    dimmer::spawn_single_b(spawner, settings, pwm_pin4, 3);
}