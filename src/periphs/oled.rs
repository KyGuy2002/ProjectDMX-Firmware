use embassy_rp::i2c::{self, Config};

use embassy_time::{Duration, Instant, Timer};

use core::sync::atomic::Ordering;
use crate::{hardware::{OledIrqs, OledResources}, periphs::sensors::{self, *}};
use core::fmt::Write;
use crate::periphs::eth::NET_IDENTITY;



use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::{FONT_6X9, FONT_10X20}}, pixelcolor::BinaryColor, prelude::*, primitives::Rectangle,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use embedded_graphics::primitives::{Circle, Line, Polyline, PrimitiveStyle, Sector};

use ssd1306::{
    command::AddrMode,
    prelude::*,
    I2CDisplayInterface,
    Ssd1306,
};

// --- Layout (128x64) ---------------------------------------------------
// A white status bar along the top, the show-logic state centered in the
// middle with a network-data X / checkmark to its left and a CPU pie chart
// to its right, a 6-way button strip
// with the 4 remote buttons above it low on the screen, and a 1px "alive"
// slider along the very bottom row.
// Mounted upside down: the controller flips the image (Rotate180), so all
// drawing below is in normal coordinates.
//
// Top-bar text: FONT_6X9 is 9px tall with a 6px top-to-baseline offset, so
// 1px of top padding in the 10px bar makes it reach exactly to the bar's last
// row - as large as the font can go with no margin left to give.
const TOP_BAR_H: i32 = 10;
const TOP_TEXT_Y: i32 = 7;

// Wired inputs (bottom row) and remote buttons (the row above it).
const RECT_Y: i32 = 56;
const RECT_H: i32 = 6;
const REMOTE_RECT_Y: i32 = RECT_Y - RECT_H - 1;
// Halfway between the top bar and the remote row.
const STATE_Y: i32 = (TOP_BAR_H + REMOTE_RECT_Y) / 2;

// X / checkmark (left edge) and CPU pie (right edge): MARK_SIZE squares,
// centered on the state text. The state text is centered at x=64, so even
// "OVERLOAD" (80px, x=24..104) clears both.
const MARK_X: i32 = 3;
const PIE_X: i32 = 128 - 3 - MARK_SIZE;
const MARK_SIZE: i32 = 16;
const MARK_STROKE: u32 = 3;
// 128/6 leaves 2px unused at the right edge - cosmetic, not worth the
// complexity of distributing the remainder across segments.
const SEG_W: i32 = 128 / 6;

const SLIDER_Y: i32 = 63;
const SLIDER_WIDTH: i32 = 20;
const SLIDER_TRAVEL: i32 = 128 - SLIDER_WIDTH;
// Pixels per frame: 2px / 40ms = 50px/s, ~2.2s one-way.
const SLIDER_SPEED: i32 = 2;

// 25 fps. Cheap because only the 8-row pages that changed are sent (see the
// loop below): normally just the bottom page (slider + input boxes).
const FRAME_TIME: Duration = Duration::from_millis(40);

/// Frame layout the SSD1306 uses: 8 pages of 8 rows, one byte per column per
/// page, LSB = top row of the page.
const PAGES: usize = 8;
const PAGE_BYTES: usize = 128;

#[embassy_executor::task]
pub async fn oled_task(r: OledResources) {

    let mut config = Config::default();
    // 1MHz Fast Mode Plus: a 128-byte page takes ~1.3ms instead of ~3.3ms at
    // 400kHz. Most SSD1306 modules support it.
    config.frequency = 1_000_000;

    let i2c = i2c::I2c::new_async(
        r.i2c,
        r.scl,
        r.sda,
        OledIrqs,
        config,
    );

    let interface = I2CDisplayInterface::new(i2c);

    // The I2C writes below are blocking - an async flush was tried and was
    // slower (the ssd1306 crate sends 16-byte chunks, each shorter than the
    // async wake round trip). What keeps this from starving audio decode and
    // the rest of thread mode is sending only changed pages, one per executor
    // turn, so no single stretch blocks for longer than one page (~1.3ms).
    let mut display = Ssd1306::new(
        interface,
        DisplaySize128x64,
        DisplayRotation::Rotate180,
    );
    display.init_with_addr_mode(AddrMode::Horizontal).unwrap();

    // Black ink, for text on the white top bar.
    let text_off = MonoTextStyle::new(&FONT_6X9, BinaryColor::Off);

    let mut frame = Frame::new();
    // What the panel currently shows. `None` forces every page out, e.g. at
    // power-up when the panel's RAM is garbage.
    let mut sent: Option<Frame> = None;

    let mut slider_x: i32 = 0;
    let mut slider_dir: i32 = 1;

    loop {
        let frame_start = Instant::now();

        frame.clear();

        draw_top_bar(&mut frame, text_off);
        draw_data_mark(&mut frame, crate::input_active());
        draw_cpu_pie(&mut frame, crate::CPU_STALL_PCT.load(Ordering::Relaxed));
        draw_state(&mut frame);
        draw_input_rects(&mut frame);
        draw_remote_rects(&mut frame);

        slider_x += slider_dir * SLIDER_SPEED;
        if slider_x >= SLIDER_TRAVEL {
            slider_x = SLIDER_TRAVEL;
            slider_dir = -1;
        } else if slider_x <= 0 {
            slider_x = 0;
            slider_dir = 1;
        }
        draw_slider(&mut frame, slider_x);

        for page in 0..PAGES {
            if sent.as_ref().is_some_and(|s| s.page(page) == frame.page(page)) {
                continue;
            }

            let y = (page * 8) as u8;
            let ok = display.set_draw_area((0, y), (128, y + 8)).is_ok()
                && display.draw(frame.page(page)).is_ok();
            if ok {
                sent.get_or_insert_with(Frame::new).page_mut(page).copy_from_slice(frame.page(page));
            } else {
                // Resend everything next frame rather than guess what landed.
                sent = None;
            }

            // Let every other thread-mode task run between pages.
            embassy_futures::yield_now().await;
        }

        Timer::at(frame_start + FRAME_TIME).await;
    }
}

/// 128x64 1bpp framebuffer in the SSD1306's own page layout, so each page can
/// be compared and sent as-is.
struct Frame([u8; PAGES * PAGE_BYTES]);

impl Frame {
    fn new() -> Self {
        Frame([0; PAGES * PAGE_BYTES])
    }

    fn clear(&mut self) {
        self.0.fill(0);
    }

    fn page(&self, page: usize) -> &[u8] {
        &self.0[page * PAGE_BYTES..][..PAGE_BYTES]
    }

    fn page_mut(&mut self, page: usize) -> &mut [u8] {
        &mut self.0[page * PAGE_BYTES..][..PAGE_BYTES]
    }
}

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(128, 64)
    }
}

impl DrawTarget for Frame {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(Point { x, y }, color) in pixels {
            if !(0..128).contains(&x) || !(0..64).contains(&y) {
                continue;
            }
            let idx = (y as usize / 8) * PAGE_BYTES + x as usize;
            let bit = 1 << (y % 8);
            if color.is_on() {
                self.0[idx] |= bit;
            } else {
                self.0[idx] &= !bit;
            }
        }
        Ok(())
    }
}


/// Big X (no network data) or checkmark (receiving) at the left of the state
/// text. Covers whichever of Art-Net/sACN is enabled; never a checkmark when
/// the board isn't listening for network input at all.
fn draw_data_mark<D>(display: &mut D, receiving: bool)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = PrimitiveStyle::with_stroke(BinaryColor::On, MARK_STROKE);
    let (left, right) = (MARK_X, MARK_X + MARK_SIZE - 1);
    let (top, bottom) = (STATE_Y - MARK_SIZE / 2, STATE_Y + MARK_SIZE / 2 - 1);

    if receiving {
        let points = [
            Point::new(left + 1, STATE_Y),
            Point::new(left + MARK_SIZE / 3, bottom - 1),
            Point::new(right - 1, top + 1),
        ];
        Polyline::new(&points).into_styled(style).draw(display).ok();
    } else {
        Line::new(Point::new(left + 1, top + 1), Point::new(right - 1, bottom - 1))
            .into_styled(style)
            .draw(display)
            .ok();
        Line::new(Point::new(left + 1, bottom - 1), Point::new(right - 1, top + 1))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}


/// White bar across the top: hostname (left).
fn draw_top_bar<D>(display: &mut D, text_off: MonoTextStyle<BinaryColor>)
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
}

/// CPU_STALL_PCT as a pie: an outline circle, filled clockwise from 12
/// o'clock by the percentage.
fn draw_cpu_pie<D>(display: &mut D, pct: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let top_left = Point::new(PIE_X, STATE_Y - MARK_SIZE / 2);
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);

    if pct >= 100 {
        Circle::new(top_left, MARK_SIZE as u32).into_styled(fill).draw(display).ok();
        return;
    }

    Circle::new(top_left, MARK_SIZE as u32)
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();

    if pct > 0 {
        Sector::new(top_left, MARK_SIZE as u32, (-90.0).deg(), (pct as f32 * 3.6).deg())
            .into_styled(fill)
            .draw(display)
            .ok();
    }
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

/// The 6 wired inputs as rectangles spanning the full width, right above the
/// slider: filled while triggered, outlined otherwise. Each segment
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

/// The 4 remote buttons as a second strip right above the inputs: filled
/// while held, outlined otherwise. Same 1px dividers as the input strip.
fn draw_remote_rects<D>(display: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let seg_w = 128 / sensors::REMOTE_BUTTONS.len() as i32;

    for (i, &button) in sensors::REMOTE_BUTTONS.iter().enumerate() {
        let style = if remote_active(button) {
            PrimitiveStyle::with_fill(BinaryColor::On)
        } else {
            PrimitiveStyle::with_stroke(BinaryColor::On, 1)
        };

        Rectangle::new(Point::new(i as i32 * seg_w, REMOTE_RECT_Y), Size::new((seg_w - 1) as u32, RECT_H as u32))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}
