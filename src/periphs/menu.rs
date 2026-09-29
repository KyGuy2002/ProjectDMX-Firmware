//! The on-board menu, drawn by oled_task in place of the status screen.
//!
//!   Menu      (or hold Enter) opens the menu from the status screen; anywhere else, goes
//!             straight back to it
//!   Up/Down   move the cursor (wraps)
//!   Enter     open / pick / toggle the highlighted line
//!
//!   MENU
//!     Mode: <name>  ->  MODE: pick a mask mode (config `modes`)
//!     Inputs        ->  INPUTS: Remote only (all wired inputs off) / each input on-off
//!
//! Changes apply immediately and are saved to flash on the way back to the
//! status screen. No key for MENU_TIMEOUT goes back on its own.

use core::fmt::Write;

use embassy_time::{Duration, Instant};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X9},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use heapless::String;

use crate::periphs::panel::Key;
use crate::settings;

const MENU_TIMEOUT: Duration = Duration::from_secs(30);

// Title bar like the status screen's top bar, then 10px rows. Row 63 is left
// for the alive slider.
const TITLE_H: i32 = 10;
const ROW_Y: i32 = TITLE_H + 2;
const ROW_H: i32 = 10;
const VISIBLE_ROWS: usize = 5;

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Main,
    Modes,
    Inputs,
}

pub struct Menu {
    screen: Screen,
    cursor: usize,
    last_key: Instant,
    /// Settings changed since the menu was opened, so leaving it saves.
    changed: bool,
}

impl Menu {
    pub fn new() -> Self {
        Menu { screen: Screen::Home, cursor: 0, last_key: Instant::now(), changed: false }
    }

    /// `true` = show the status screen, not the menu.
    pub fn is_home(&self) -> bool {
        self.screen == Screen::Home
    }

    pub fn handle(&mut self, key: Key) {
        self.last_key = Instant::now();
        let rows = self.rows();

        match key {
            Key::Menu if self.screen == Screen::Home => self.open(Screen::Main, 0),
            Key::Menu => self.go_home(),
            _ if self.screen == Screen::Home => {}
            Key::Up => self.cursor = if self.cursor == 0 { rows - 1 } else { self.cursor - 1 },
            Key::Down => self.cursor = (self.cursor + 1) % rows,
            Key::Enter => self.enter(),
        }
    }

    /// Goes home after MENU_TIMEOUT without a key. `true` if it just did.
    pub fn check_timeout(&mut self) -> bool {
        if self.screen != Screen::Home && self.last_key.elapsed() >= MENU_TIMEOUT {
            self.go_home();
            return true;
        }
        false
    }

    fn open(&mut self, screen: Screen, cursor: usize) {
        self.screen = screen;
        self.cursor = cursor;
    }

    fn go_home(&mut self) {
        self.screen = Screen::Home;
        if self.changed {
            settings::save();
            self.changed = false;
        }
    }

    fn enter(&mut self) {
        match self.screen {
            Screen::Home => {}
            Screen::Main => match self.cursor {
                0 => self.open(Screen::Modes, settings::mode()),
                _ => self.open(Screen::Inputs, 0),
            },
            Screen::Modes => {
                settings::set_mode(self.cursor);
                self.changed = true;
                self.open(Screen::Main, 0);
            }
            Screen::Inputs => {
                match self.cursor {
                    // Remote only: all off, or all back on if they already are.
                    0 => settings::set_remote_only(!settings::remote_only()),
                    n => settings::set_input_mask(settings::input_mask() ^ (1 << (n - 1))),
                }
                self.changed = true;
            }
        }
    }

    fn rows(&self) -> usize {
        match self.screen {
            Screen::Home => 1,
            Screen::Main => 2,
            Screen::Modes => settings::mode_count(),
            Screen::Inputs => 7,
        }
    }

    fn title(&self) -> &'static str {
        match self.screen {
            Screen::Home | Screen::Main => "MENU",
            Screen::Modes => "MODE",
            Screen::Inputs => "INPUTS",
        }
    }

    fn row_label(&self, row: usize, out: &mut String<24>) {
        let on_off = |on: bool| if on { "ON" } else { "OFF" };
        let _ = match (self.screen, row) {
            (Screen::Main, 0) => write!(out, "Mode: {}", settings::mode_name(settings::mode())),
            (Screen::Main, _) => write!(out, "Inputs >"),
            (Screen::Modes, m) => {
                let mark = if m == settings::mode() { '*' } else { ' ' };
                write!(out, "{} {}", mark, settings::mode_name(m))
            }
            (Screen::Inputs, 0) => write!(out, "Remote only   {}", on_off(settings::remote_only())),
            (Screen::Inputs, n) => write!(out, "Input {}       {}", n, on_off(settings::input_enabled(n as u8))),
            (Screen::Home, _) => Ok(()),
        };
    }

    /// Title bar, then the rows around the cursor, the cursor's row inverted.
    pub fn draw<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let on = MonoTextStyle::new(&FONT_6X9, BinaryColor::On);
        let off = MonoTextStyle::new(&FONT_6X9, BinaryColor::Off);
        let fill = PrimitiveStyle::with_fill(BinaryColor::On);

        Rectangle::new(Point::zero(), Size::new(128, TITLE_H as u32)).into_styled(fill).draw(display).ok();
        Text::with_baseline(self.title(), Point::new(1, 1), off, Baseline::Top).draw(display).ok();

        let rows = self.rows();
        let first = self.cursor.saturating_sub(VISIBLE_ROWS - 1);
        for (slot, row) in (first..rows).take(VISIBLE_ROWS).enumerate() {
            let y = ROW_Y + slot as i32 * ROW_H;
            let mut label = String::new();
            self.row_label(row, &mut label);

            let style = if row == self.cursor {
                Rectangle::new(Point::new(0, y), Size::new(128, ROW_H as u32)).into_styled(fill).draw(display).ok();
                off
            } else {
                on
            };
            Text::with_baseline(&label, Point::new(2, y + 1), style, Baseline::Top).draw(display).ok();
        }
    }
}
