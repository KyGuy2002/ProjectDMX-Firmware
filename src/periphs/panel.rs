//! The 4 buttons on the board (Menu / Up / Down / Enter). Each press is queued
//! for the OLED menu (periphs/menu.rs).
//!
//! The Menu button doesn't register on this board (GPIO 33 never goes low), so
//! holding Enter for ENTER_HOLD_MS acts as Menu instead. Enter's own press is
//! therefore sent on release, and only if it wasn't held that long.

use embassy_rp::gpio::{Input, Pull};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Timer;

use crate::hardware::PanelResources;

#[derive(Clone, Copy, PartialEq, defmt::Format)]
pub enum Key {
    Menu,
    Up,
    Down,
    Enter,
}

/// Presses, oldest first. Read by oled_task once per frame.
pub static KEYS: Channel<ThreadModeRawMutex, Key, 8> = Channel::new();

/// A level has to hold for this many polls in a row to count (debounce).
const STABLE_POLLS: u8 = 2;
const POLL_MS: u32 = 10;
/// Holding Enter this long sends Menu instead of Enter.
const ENTER_HOLD_MS: u32 = 700;

/// Polled rather than edge-triggered: 4 pin reads every 10ms is nothing, and a
/// press only needs to reach the screen within a frame (40ms).
#[embassy_executor::task]
pub async fn panel_task(r: PanelResources) {
    let buttons = [
        (Input::new(r.menu, Pull::Up), Key::Menu),
        (Input::new(r.up, Pull::Up), Key::Up),
        (Input::new(r.down, Pull::Up), Key::Down),
        (Input::new(r.enter, Pull::Up), Key::Enter),
    ];
    let mut pressed = [false; 4];
    let mut polls = [0u8; 4];

    // Enter: ms held so far, and whether that hold has already sent Menu.
    let mut enter_held_ms: u32 = 0;
    let mut enter_was_menu = false;

    loop {
        for (i, (pin, key)) in buttons.iter().enumerate() {
            let low = pin.is_low();
            if low == pressed[i] {
                polls[i] = 0;
                continue;
            }

            polls[i] += 1;
            if polls[i] >= STABLE_POLLS {
                pressed[i] = low;
                polls[i] = 0;
                match (*key, low) {
                    (Key::Enter, true) => {
                        enter_held_ms = 0;
                        enter_was_menu = false;
                    }
                    (Key::Enter, false) if !enter_was_menu => {
                        let _ = KEYS.try_send(Key::Enter);
                    }
                    (_, true) => {
                        let _ = KEYS.try_send(*key);
                    }
                    _ => {}
                }
            }
        }

        if pressed[3] && !enter_was_menu {
            enter_held_ms += POLL_MS;
            if enter_held_ms >= ENTER_HOLD_MS {
                enter_was_menu = true;
                let _ = KEYS.try_send(Key::Menu);
            }
        }

        Timer::after_millis(POLL_MS as u64).await;
    }
}
