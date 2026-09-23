//! Show logic: what the board does when an input is pressed.
//!
//! Edit this file to program the show. `on_boot` runs once at startup and
//! returns the starting state; `on_button_pressed` runs on every press (buttons
//! are numbered 1..=6, matching the board) and can change the state. The 433
//! MHz remote's A-D buttons press inputs 1-4 (see periphs/ask433.rs).
//!
//! The FPP helpers queue a command and return straight away; commands are sent
//! to FPP in the order they were called. Names are the file name without
//! `.fseq`.
//!
//!   fpp::start_sequence(name, looping)   play a sequence (replaces the current one)
//!   fpp::stop_sequence()                 stop the playing sequence
//!   fpp::start_effect(name, looping)     start an effect on top of the sequence
//!   fpp::stop_effect(name)               stop one effect
//!   fpp::stop_all_effects()              stop every running effect
//!
//!   fpp::wait(Duration::from_millis(n))  delay the FPP commands after this one
//!
//! `press_later(button, Duration::from_millis(n))` acts as if `button` were
//! pressed `n` ms from now. It's dropped if the state changes first.
//!
//! Which level counts as "pressed" is set per input in config.jsonc
//! (`buttons[n].reversed`).

use embassy_time::Duration;

use crate::periphs::fpp;
use crate::periphs::sensors::press_later;

// Sequences
const IDLE: &str = "idle-2026";
const OVERLOAD: &str = "overload-2026";

// Effects
/// Guests should hit button 2 before this runs out; if not, strike anyway.
const STARTUP_MS: u64 = 7500;
const STARTUP: &str = "startup-2026-e";
/// From the strike effect starting to the overload sequence starting.
const STRIKE_MS: u64 = 1800;
const STRIKE: &str = "strike-2026-e";
const FRANK: &str = "frank-2026-e";

/// Add states as needed. Shown on the web status page.
#[derive(Clone, Copy, PartialEq, Debug, defmt::Format)]
pub enum State {
    Idle,
    Running,
    Overload,
}

pub fn on_boot() -> State {
    go_idle()
}

pub fn on_button_pressed(button: u8, state: &mut State) {
    match button {
        // Start the show. Strikes on its own if button 2 isn't pressed in time.
        1 => {
            if *state == State::Idle {
                fpp::start_effect(STARTUP, false);
                press_later(2, Duration::from_millis(STARTUP_MS));
                *state = State::Running;
            }
        }

        // Overload, by a guest or by button 1's timer. The startup effect is
        // left running if it still is.
        2 => {
            if *state == State::Running {
                fpp::start_effect(STRIKE, false);
                fpp::wait(Duration::from_millis(STRIKE_MS));
                fpp::start_sequence(OVERLOAD, true);
                *state = State::Overload;
            }
        }

        // Frank, during overload only. State stays the same.
        3 => {
            if *state == State::Overload {
                fpp::start_effect(FRANK, false);
            }
        }

        // Reset.
        4 => {
            if *state != State::Idle {
                *state = go_idle();
            }
        }

        _ => {}
    }
}

fn go_idle() -> State {
    fpp::stop_all_effects();
    fpp::start_sequence(IDLE, true);
    State::Idle
}
