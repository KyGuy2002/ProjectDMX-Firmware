use embassy_rp::i2c::{self, Config};

use embassy_time::{Duration, Instant, Timer};

use core::sync::atomic::Ordering;
use crate::{hardware::{OledIrqs, OledResources}, periphs::sensors::*};
use core::fmt::Write;
use crate::periphs::eth::NET_IDENTITY;
use crate::periphs::fpp::{FppStatus, fpp_status};



use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::{FONT_6X9, FONT_10X20}}, pixelcolor::BinaryColor, prelude::*, primitives::Rectangle,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use embedded_graphics::primitives::PrimitiveStyle;

use ssd1306::{
    prelude::*,
    I2CDisplayInterface,
    Ssd1306,
};

// --- Layout (128x64) ---------------------------------------------------
// A white status bar along the top, the show-logic state centered in the
// middle, a 6-way button strip low on the screen, and a 1px "alive" slider
// along the very bottom row.
//
// All top-bar text uses the same baseline: FONT_6X9 is 9px tall with a 6px
// top-to-baseline offset. 1px of top padding in the 10px bar (0px in the 9px
// icon box) makes both the bar text and the icon glyphs reach exactly to the
// bar's last row - as large as the font can go with no margin left to give.
const TOP_BAR_H: i32 = 10;
const TOP_TEXT_Y: i32 = 7;

const ICON_SIZE: i32 = 9;
const ICON_Y: i32 = 1;
// Right-aligned, D (data) outermost, F (FPP) just left of it.
const D_ICON_X: i32 = 128 - 1 - ICON_SIZE;
const F_ICON_X: i32 = D_ICON_X - 1 - ICON_SIZE;

const RECT_Y: i32 = 56;
// Halfway between the top bar and the button strip.
const STATE_Y: i32 = (TOP_BAR_H + RECT_Y) / 2;
const RECT_H: i32 = 6;
// 128/6 leaves 2px unused at the right edge - cosmetic, not worth the
// complexity of distributing the remainder across segments.
const SEG_W: i32 = 128 / 6;

const SLIDER_Y: i32 = 63;
const SLIDER_WIDTH: i32 = 20;
const SLIDER_TRAVEL: i32 = 128 - SLIDER_WIDTH;
// Pixels per 300ms tick; a full sweep takes SLIDER_TRAVEL / SLIDER_SPEED ticks
// (108/14 ~ 8 ticks = 2.4s one-way, ~4.6s round trip). Kept under
// SLIDER_WIDTH (20) on purpose: each step still overlaps the bar's previous
// position, which is what reads as sliding rather than teeth. Above 20 it
// would visibly teleport between frames no matter the tick rate.
const SLIDER_SPEED: i32 = 14;

#[embassy_executor::task]
pub async fn oled_task(r: OledResources) {

    let mut config = Config::default();
    // 400kHz made each blocking flush() (~1KB framebuffer) take ~23ms, fully stalling
    // the cooperative executor (incl. audio playback) each time. Most SSD1306 modules
    // support Fast Mode Plus (1MHz), which cuts that down to ~5-6ms.
    config.frequency = 1_000_000;

    let i2c = i2c::I2c::new_async(
        r.i2c,
        r.scl,
        r.sda,
        OledIrqs,
        config,
    );

    let interface = I2CDisplayInterface::new(i2c);

    // Tried async flush() (Ssd1306Async): each flush is 64 sequential 16-byte
    // I2C chunks, and at 1MHz a chunk transfer (~150us) is apparently shorter
    // than the async wake/IRQ round-trip overhead - total flush time went up
    // (12ms -> 16-27ms, one spike to 98ms) with no improvement to neo_task's
    // lag. Reverted to the plain blocking flush below.
    let mut display = Ssd1306::new(
        interface,
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();

    display.init().unwrap();
    display.clear_buffer();

    // _on = white ink (for text on a filled-black icon); _off = black ink (for
    // text on the white top bar, and the default elsewhere).
    let text_off = MonoTextStyle::new(&FONT_6X9, BinaryColor::Off);
    let text_on = MonoTextStyle::new(&FONT_6X9, BinaryColor::On);

    let mut slider_x: i32 = 0;
    let mut slider_dir: i32 = 1;

    loop {
        display.clear_buffer();

        draw_top_bar(&mut display, text_off, text_on);
        draw_state(&mut display);
        draw_input_rects(&mut display);

        slider_x += slider_dir * SLIDER_SPEED;
        if slider_x >= SLIDER_TRAVEL {
            slider_x = SLIDER_TRAVEL;
            slider_dir = -1;
        } else if slider_x <= 0 {
            slider_x = 0;
            slider_dir = 1;
        }
        draw_slider(&mut display, slider_x);

        let flush_start = Instant::now(); // DIAG: remove after measuring
        display.flush().unwrap();
        let flush_ms = (Instant::now() - flush_start).as_millis(); // DIAG
        if flush_ms > 3 {
            // defmt::println!("DIAG oled flush: {}ms", flush_ms);
        }

        // Was 90ms; the ~12ms blocking I2C flush every cycle was a continuous
        // ~12% draw on the thread-mode executor shared with audio decode.
        // Slower refresh (still smooth for a status display) frees that budget
        // back for audio without changing anything the OLED shows.
        Timer::after(Duration::from_millis(300)).await;
    }


}


/// White bar across the top: hostname (left), CPU% and the F/D icons (right).
fn draw_top_bar<D>(display: &mut D, text_off: MonoTextStyle<BinaryColor>, text_on: MonoTextStyle<BinaryColor>)
where
    D: DrawTarget<Color = BinaryColor>,
{
    Rectangle::new(Point::new(0, 0), Size::new(128, TOP_BAR_H as u32))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();

    let mut host: heapless::String<24> = heapless::String::new();
    match NET_IDENTITY.try_get() {
        Some(net) => {
            let _ = write!(&mut host, "{}", net.hostname);
        }
        // No eth means the board's input source isn't Art-Net/sACN (eth is
        // only started for those) or the address hasn't been assigned yet.
        None => {
            let _ = host.push_str("no eth");
        }
    }
    Text::new(&host, Point::new(0, TOP_TEXT_Y), text_off).draw(display).ok();

    let mut cpu: heapless::String<8> = heapless::String::new();
    let _ = write!(&mut cpu, "{}%", crate::CPU_STALL_PCT.load(Ordering::Relaxed));
    let cpu_w = 6 * cpu.len() as i32;
    Text::new(&cpu, Point::new(F_ICON_X - cpu_w, TOP_TEXT_Y), text_off).draw(display).ok();

    let fpp_ok = fpp_status() == FppStatus::Online;
    draw_icon(display, F_ICON_X, 'F', fpp_ok, text_off, text_on);

    // Data-received indicator; covers whichever of Art-Net/sACN is enabled.
    // Stays outlined (never lit) when the board isn't listening for network
    // input at all, which is the correct "not applicable" reading.
    draw_icon(display, D_ICON_X, 'D', crate::input_active(), text_off, text_on);
}

/// One status icon: a 9x9 box, filled black with a white letter when `good`,
/// or just an outline with a black letter when not.
fn draw_icon<D>(display: &mut D, x: i32, ch: char, good: bool, text_off: MonoTextStyle<BinaryColor>, text_on: MonoTextStyle<BinaryColor>)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let (box_style, letter_style) = if good {
        (PrimitiveStyle::with_fill(BinaryColor::Off), text_on)
    } else {
        (PrimitiveStyle::with_stroke(BinaryColor::Off, 1), text_off)
    };

    Rectangle::new(Point::new(x, ICON_Y), Size::new(ICON_SIZE as u32, ICON_SIZE as u32))
        .into_styled(box_style)
        .draw(display)
        .ok();

    let mut s: heapless::String<1> = heapless::String::new();
    let _ = s.push(ch);
    Text::new(&s, Point::new(x + 1, TOP_TEXT_Y), letter_style).draw(display).ok();
}

/// The show-logic state (logic.rs), large and centered, e.g. "OVERLOAD".
/// FONT_10X20 fits 12 characters across; longer names are clipped.
fn draw_state<D>(display: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut name: heapless::String<16> = heapless::String::new();
    match logic_state() {
        Some(state) => {
            let _ = write!(&mut name, "{:?}", state);
            name.make_ascii_uppercase();
        }
        None => {
            let _ = name.push_str("...");
        }
    }

    let style = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Middle).build();
    Text::with_text_style(&name, Point::new(64, STATE_Y), MonoTextStyle::new(&FONT_10X20, BinaryColor::On), style)
        .draw(display)
        .ok();
}

/// A 1px bar sliding back and forth along the bottom row, so a glance confirms
/// the render loop (and thus the thread-mode executor) hasn't stalled.
fn draw_slider<D>(display: &mut D, x: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    Rectangle::new(Point::new(x, SLIDER_Y), Size::new(SLIDER_WIDTH as u32, 1))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();
}

/// The 6 inputs as rectangles spanning the full width, right above the slider:
/// filled while triggered (wired OR remote), outlined otherwise. Each segment
/// leaves its rightmost column blank, which is what forms the 1px divider
/// between segments (and before the slider).
fn draw_input_rects<D>(display: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    for i in 0..6 {
        let x0 = i as i32 * SEG_W;
        let w = SEG_W - 1;
        let triggered = button_active(i as u8 + 1);

        let style = if triggered {
            PrimitiveStyle::with_fill(BinaryColor::On)
        } else {
            PrimitiveStyle::with_stroke(BinaryColor::On, 1)
        };

        Rectangle::new(Point::new(x0, RECT_Y), Size::new(w as u32, RECT_H as u32))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}
