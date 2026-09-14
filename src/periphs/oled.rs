use embassy_rp::i2c::{self, Config};

use embassy_time::{Duration, Instant, Timer};

use core::sync::atomic::{AtomicBool, Ordering};
use crate::{hardware::{OledIrqs, OledResources}, periphs::sensors::*};
use core::fmt::Write;
use embassy_net::Ipv4Address;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex as AsyncMutex;



use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_6X10}, pixelcolor::BinaryColor, prelude::*, primitives::Circle, text::Text,
};
use embedded_graphics::primitives::{Line, PrimitiveStyle};

use ssd1306::{
    prelude::*,
    I2CDisplayInterface,
    Ssd1306Async,
};



#[embassy_executor::task]
pub async fn oled_task(r: OledResources, ip_state: &'static AsyncMutex<CriticalSectionRawMutex, Option<Ipv4Address>>) {
    
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

    // Async mode: flush() transfers each 16-byte I2C chunk via DMA/IRQ and
    // awaits it instead of blocking the executor for the whole ~12ms buffer,
    // so sibling thread-mode tasks (neo_task, audio decode) can run in between.
    let mut display = Ssd1306Async::new(
        interface,
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();

    display.init().await.unwrap();
    display.clear_buffer();

    let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);


    let mut frame: usize = 0;

    loop {
        display.clear_buffer();





        let current_ip = {
            let ip = ip_state.lock().await;
            *ip
        };

        if let Some(ip) = current_ip {
            let mut ip_text: heapless::String<32> = heapless::String::new();
            core::write!(&mut ip_text, "{}", ip).unwrap();

            Text::new(&ip_text, Point::new(28, 58), text_style)
                .draw(&mut display)
                .ok();
        } else {
            Text::new("Connecting...", Point::new(28, 58), text_style)
                .draw(&mut display)
                .ok();
        }




        render_input_state(&mut display);


        



        let mut cpu_text: heapless::String<16> = heapless::String::new();
        core::write!(&mut cpu_text, "CPU:{}%", crate::CPU_STALL_PCT.load(Ordering::Relaxed)).unwrap();
        Text::new(&cpu_text, Point::new(0, 20), text_style)
            .draw(&mut display)
            .ok();

        draw_spinner(&mut display, 64, 28, frame);

        let flush_start = Instant::now(); // DIAG: remove after measuring
        display.flush().await.unwrap();
        let flush_ms = (Instant::now() - flush_start).as_millis(); // DIAG
        if flush_ms > 3 {
            defmt::println!("DIAG oled flush: {}ms", flush_ms);
        }

        frame += 1;
        if frame >= 12 {
            frame = 0;
        }

        // Was 90ms; the ~12ms blocking I2C flush every cycle was a continuous
        // ~12% draw on the thread-mode executor shared with audio decode.
        // Slower refresh (still smooth for a status display) frees that budget
        // back for audio without changing anything the OLED shows.
        Timer::after(Duration::from_millis(300)).await;
    }


}


fn draw_spinner<D>(display: &mut D, cx: i32, cy: i32, frame: usize)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // 12-point circle lookup table.
    // Values are roughly sin/cos scaled to radius 16.
    let points: [(i32, i32); 12] = [
        (0, -16),
        (8, -14),
        (14, -8),
        (16, 0),
        (14, 8),
        (8, 14),
        (0, 16),
        (-8, 14),
        (-14, 8),
        (-16, 0),
        (-14, -8),
        (-8, -14),
    ];

    // Smaller inner radius for each segment.
    let inner_points: [(i32, i32); 12] = [
        (0, -8),
        (4, -7),
        (7, -4),
        (8, 0),
        (7, 4),
        (4, 7),
        (0, 8),
        (-4, 7),
        (-7, 4),
        (-8, 0),
        (-7, -4),
        (-4, -7),
    ];

    for i in 0..12 {
        let age = (12 + frame as i32 - i as i32) % 12;

        // Only draw the most recent 8 ticks.
        // This creates the fading-tail look on a monochrome display.
        if age >= 8 {
            continue;
        }

        let outer = points[i];
        let inner = inner_points[i];

        let style = if age == 0 {
            PrimitiveStyle::with_stroke(BinaryColor::On, 3)
        } else if age <= 2 {
            PrimitiveStyle::with_stroke(BinaryColor::On, 2)
        } else {
            PrimitiveStyle::with_stroke(BinaryColor::On, 1)
        };

        Line::new(
            Point::new(cx + inner.0, cy + inner.1),
            Point::new(cx + outer.0, cy + outer.1),
        )
        .into_styled(style)
        .draw(display)
        .ok();
    }
}


fn render_input_state<D>(display: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    render_in(display, &BUTTON_1_STATUS, 1);
    render_in(display, &BUTTON_2_STATUS, 2);
    render_in(display, &BUTTON_3_STATUS, 3);
    render_in(display, &BUTTON_4_STATUS, 4);
    render_in(display, &BUTTON_5_STATUS, 5);
    render_in(display, &BUTTON_6_STATUS, 6);

        
}

fn render_in<D>(display: &mut D, var: &'static AtomicBool, no: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{

    let pressed = var.load(Ordering::Relaxed);
    if !pressed {
        Circle::new(Point::new((128/10) * (no - 1), 0), 10)
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(display)
            .ok();
    } else {
        Circle::new(Point::new((128/10) * (no - 1), 0), 10)
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)
            .ok();
    }

}