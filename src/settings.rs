//! Show settings changed from the on-board menu: the active mask mode (config
//! `modes`, which outputs are held off) and which wired inputs are disabled.
//! Both survive a power cycle - they're kept in the last flash sector, which
//! memory.x leaves out of the program's FLASH region.
//!
//! Everything else reads them through `output_enabled` / `input_enabled`,
//! which are plain atomic loads and safe from any executor.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, AtomicU16, Ordering};

use defmt::{info, warn};
use embassy_rp::flash::{Blocking, ERASE_SIZE, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;

use crate::CONFIG;
use crate::config::{MAX_MODE_NAME_LEN, ModeConfig, mode_mask};
use crate::hardware::FlashResources;

/// Must match memory.x (FLASH LENGTH + the 4K reserved here).
const FLASH_SIZE: usize = 4 * 1024 * 1024;
const SETTINGS_OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;

/// Saved record: magic, input mask, mode name length, mode name (padded),
/// checksum. The mode is saved by name so reordering `modes` in the config
/// can't silently switch to a different one.
const MAGIC: [u8; 4] = *b"DMXS";
const RECORD_LEN: usize = 4 + 1 + 1 + MAX_MODE_NAME_LEN + 1;

/// Module slot, for `output_enabled`.
#[derive(Clone, Copy)]
pub enum Slot {
    A,
    B,
    C,
    D,
}

/// Index into `mode_name`: 0 = Normal, n = config `modes[n - 1]`.
static MODE: AtomicU8 = AtomicU8::new(0);
/// The active mode's disabled outputs, bit slot * 4 + output (see config::mode_mask).
static OUTPUT_MASK: AtomicU16 = AtomicU16::new(0);
/// Disabled wired inputs, bit n - 1 for input n.
static INPUT_MASK: AtomicU8 = AtomicU8::new(0);

type SettingsFlash = Flash<'static, FLASH, Blocking, FLASH_SIZE>;
static STORE: BlockingMutex<ThreadModeRawMutex, RefCell<Option<SettingsFlash>>> =
    BlockingMutex::new(RefCell::new(None));

/// Whether physical output `output` (0-based) of `slot` is allowed to turn on
/// in the active mode.
pub fn output_enabled(slot: Slot, output: usize) -> bool {
    OUTPUT_MASK.load(Ordering::Relaxed) & (1 << (slot as usize * 4 + output)) == 0
}

/// Whether wired input `input` (1..=6) is allowed to reach the show logic.
pub fn input_enabled(input: u8) -> bool {
    !(1..=6).contains(&input) || INPUT_MASK.load(Ordering::Relaxed) & (1 << (input - 1)) == 0
}

pub fn input_mask() -> u8 {
    INPUT_MASK.load(Ordering::Relaxed)
}

pub fn set_input_mask(mask: u8) {
    INPUT_MASK.store(mask & 0x3f, Ordering::Relaxed);
}

fn modes() -> &'static [ModeConfig] {
    CONFIG.try_get().map(|c| c.modes.as_slice()).unwrap_or(&[])
}

/// Normal plus the config's modes.
pub fn mode_count() -> usize {
    1 + modes().len()
}

pub fn mode() -> usize {
    MODE.load(Ordering::Relaxed) as usize
}

pub fn mode_name(mode: usize) -> &'static str {
    match mode {
        0 => "Normal",
        n => modes().get(n - 1).map(|m| m.name.as_str()).unwrap_or("?"),
    }
}

pub fn set_mode(mode: usize) {
    let mode = if mode < mode_count() { mode } else { 0 };
    // Validated at config load, so the mask can't fail here.
    let mask = match mode {
        0 => 0,
        n => mode_mask(&modes()[n - 1]).unwrap_or(0),
    };
    OUTPUT_MASK.store(mask, Ordering::Relaxed);
    MODE.store(mode as u8, Ordering::Relaxed);
}

/// Loads the saved settings. Call once, after CONFIG is set and before the
/// modules and sensors start. A blank or unreadable sector, or a saved mode
/// no longer in the config, leaves Normal with every input enabled.
pub fn init(r: FlashResources) {
    let mut flash = SettingsFlash::new_blocking(r.flash);

    let mut record = [0u8; RECORD_LEN];
    if flash.blocking_read(SETTINGS_OFFSET, &mut record).is_ok() {
        if let Some((name, inputs)) = decode(&record) {
            match (0..mode_count()).find(|&m| mode_name(m) == name) {
                Some(m) => set_mode(m),
                None => warn!("Settings: saved mode \"{}\" not in config, using Normal", name.as_str()),
            }
            set_input_mask(inputs);
        }
    }
    info!("Settings: mode {}, disabled inputs {=u8:06b}", mode_name(mode()), input_mask());

    STORE.lock(|s| s.replace(Some(flash)));
}

/// Writes the current settings to flash if they differ from what's saved.
///
/// The sector erase runs with interrupts off and blocks everything, audio
/// included, for tens of ms, so this is only called when leaving the menu.
pub fn save() {
    let record = encode(mode_name(mode()), input_mask());

    STORE.lock(|s| {
        let mut s = s.borrow_mut();
        let Some(flash) = s.as_mut() else { return };

        let mut saved = [0u8; RECORD_LEN];
        if flash.blocking_read(SETTINGS_OFFSET, &mut saved).is_ok() && saved == record {
            return;
        }

        let ok = flash.blocking_erase(SETTINGS_OFFSET, SETTINGS_OFFSET + ERASE_SIZE as u32).is_ok()
            && flash.blocking_write(SETTINGS_OFFSET, &record).is_ok();
        if ok {
            info!("Settings: saved");
        } else {
            warn!("Settings: flash write failed");
        }
    });
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0xA5u8, |sum, &b| sum.wrapping_add(b))
}

fn encode(mode: &str, inputs: u8) -> [u8; RECORD_LEN] {
    let name = &mode.as_bytes()[..mode.len().min(MAX_MODE_NAME_LEN)];

    let mut record = [0u8; RECORD_LEN];
    record[..4].copy_from_slice(&MAGIC);
    record[4] = inputs;
    record[5] = name.len() as u8;
    record[6..6 + name.len()].copy_from_slice(name);
    record[RECORD_LEN - 1] = checksum(&record[..RECORD_LEN - 1]);
    record
}

fn decode(record: &[u8; RECORD_LEN]) -> Option<(heapless::String<MAX_MODE_NAME_LEN>, u8)> {
    if record[..4] != MAGIC || record[RECORD_LEN - 1] != checksum(&record[..RECORD_LEN - 1]) {
        return None;
    }
    let len = (record[5] as usize).min(MAX_MODE_NAME_LEN);
    let name = core::str::from_utf8(&record[6..6 + len]).ok()?;
    Some((heapless::String::try_from(name).ok()?, record[4]))
}
