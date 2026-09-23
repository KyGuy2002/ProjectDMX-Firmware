//! 433 MHz ASK remote (EV1527 / PT2262 fixed-code fob) on a cheap
//! superheterodyne receiver. Each fob button acts as a press of one of the
//! board's inputs, going through `logic.rs` exactly like a wired press.
//!
//! Runs on its own interrupt executor (RF_EXECUTOR in main.rs) so thread-mode
//! stalls can't overflow the PIO FIFO and drop pulses.
//!
//! To learn a new fob: press its buttons and watch for the "unknown remote"
//! log lines, then add its address and data values to `REMOTE_BUTTONS`.

use defmt::info;

use embassy_rp::pio::program::pio_asm;
use embassy_rp::pio::{Config, Direction, FifoJoin, Pio};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_time::{Duration, Instant};

use crate::hardware::{RemoteIrqs, RemoteResources};
use crate::periphs::sensors;

/// One fob button: 20-bit fob address, 4-bit data value, and the input number
/// (1..=6) it presses. One fob has the same address on every button; the
/// buttons are told apart by data.
struct RemoteButton {
    address: u32,
    data: u8,
    input: u8,
}

const REMOTE_BUTTONS: &[RemoteButton] = &[
    RemoteButton { address: 0x3fcad, data: 0x8, input: 1 }, // A
    RemoteButton { address: 0x3fcad, data: 0x4, input: 2 }, // B
    RemoteButton { address: 0x3fcad, data: 0x2, input: 3 }, // C
    RemoteButton { address: 0x3fcad, data: 0x1, input: 4 }, // D

    RemoteButton { address: 0xc136d, data: 0x8, input: 1 }, // A
    RemoteButton { address: 0xc136d, data: 0x4, input: 2 }, // B
    RemoteButton { address: 0xc136d, data: 0x2, input: 3 }, // C
    RemoteButton { address: 0xc136d, data: 0x1, input: 4 }, // D
];

// Cheap superheterodyne receivers have no squelch, so the DATA pin is never
// quiet - with no transmitter in range it's just RF noise. Real fixed-code
// transmitters always precede a frame with a short-high/long-low sync pulse,
// then encode each bit as a high:low pulse pair with a ~1:3 or ~3:1 ratio.
// Noise almost never reproduces that shape, so gating on it is what turns
// "constant spam" into "only when something actually transmits".

/// Sub-this-width edges are comparator/RF glitches, not real OOK pulses.
const MIN_PULSE_US: u32 = 80;
/// A low pulse below this can't be a sync gap, no matter the ratio.
const SYNC_MIN_LOW_US: u32 = 1500;
/// Sync: low pulse is at least this many times the preceding high pulse.
const SYNC_RATIO: u32 = 8;
/// A real bit's long:short pulse ratio is ~3:1. Anything close to 1:1 (or
/// wildly off) isn't a clean bit - treat the frame as garbled.
const BIT_RATIO_MIN: f32 = 1.5;
const BIT_RATIO_MAX: f32 = 6.0;
/// Safety cap so a stuck/noisy line can't grow `code` past 32 bits.
const MAX_FRAME_BITS: u32 = 32;
/// A held button repeats its frame every ~10-30ms. Same frame again within
/// this window is the same press; releasing and clicking again after it is a
/// new one.
const REPEAT_WINDOW: Duration = Duration::from_millis(500);
#[embassy_executor::task]
pub async fn ask433_task(r: RemoteResources) {
    info!("ASK433 remote receiver started.");

    let Pio { mut common, mut sm0, .. } = Pio::new(r.pio, RemoteIrqs);

    // Counts down a scratch register while the pin stays in the current
    // state, then pushes the elapsed count (~1us per count) to the RX FIFO
    // and repeats for the opposite state. Runs forever, alternating.
    let prg = pio_asm!(
        ".wrap_target",
        "    wait 0 pin 0",
        "    mov x, ~null",
        "low_loop:",
        "    jmp x-- low_test",
        "low_test:",
        "    jmp pin low_done",
        "    jmp low_loop",
        "low_done:",
        "    mov isr, ~x",
        "    push noblock",
        "    mov x, ~null",
        "high_loop:",
        "    jmp x-- high_test",
        "high_test:",
        "    jmp pin high_loop",
        "    mov isr, ~x",
        "    push noblock",
        ".wrap",
    );

    let loaded = common.load_program(&prg.program);
    let rx_pin = common.make_pio_pin(r.data);
    sm0.set_pin_dirs(Direction::In, &[&rx_pin]);

    let mut cfg = Config::default();
    cfg.use_program(&loaded, &[]);
    cfg.set_in_pins(&[&rx_pin]);
    cfg.set_jmp_pin(&rx_pin);
    // No TX use, so take its FIFO too: 8 pulse widths of slack instead of 4.
    cfg.fifo_join = FifoJoin::RxOnly;
    // 2 PIO clock cycles per loop iteration - target 2MHz so each count == 1us.
    cfg.clock_divider = calculate_pio_clock_divider(2_000_000);

    sm0.set_config(&cfg);
    sm0.set_enable(true);

    // The PIO program always starts by waiting for the line low, so the FIFO
    // stream is strictly alternating: low, high, low, high, ... Each "bit" is
    // a (high, low-that-follows-it) pair; the very first low has no
    // preceding high, so it's discarded.
    let _ = sm0.rx().wait_pull().await;

    let mut code: u32 = 0;
    let mut bit_count: u32 = 0;
    let mut collecting = false;

    // Last frame acted on, for dropping a held button's repeats.
    let mut last: Option<(u32, Instant)> = None;

    loop {
        let high = sm0.rx().wait_pull().await;
        let low = sm0.rx().wait_pull().await;

        if high < MIN_PULSE_US || low < MIN_PULSE_US {
            // Noise glitch - drop whatever frame was in progress.
            collecting = false;
        } else if low >= SYNC_MIN_LOW_US && low >= high.saturating_mul(SYNC_RATIO) {
            // Sync gap: end of a frame (if we were collecting one) and the
            // start of the next.
            if collecting && bit_count == 24 {
                handle_frame(&mut last, code);
            }
            code = 0;
            bit_count = 0;
            collecting = true;
        } else if collecting {
            let (bigger, smaller) = if high > low { (high, low) } else { (low, high) };
            let ratio = bigger as f32 / smaller as f32;

            if ratio >= BIT_RATIO_MIN && ratio <= BIT_RATIO_MAX && bit_count < MAX_FRAME_BITS {
                let bit = if high > low { 1 } else { 0 };
                code = (code << 1) | bit;
                bit_count += 1;
            } else {
                // Not a clean bit pulse - garbled frame, wait for next sync.
                collecting = false;
            }
        }
    }
}

fn handle_frame(last: &mut Option<(u32, Instant)>, code: u32) {
    let address = code >> 4;
    let data = (code & 0xF) as u8;

    let Some(button) = REMOTE_BUTTONS.iter().find(|b| b.address == address && b.data == data) else {
        // Data values from a known fob that aren't mapped (it sends 0x0 a lot)
        // are ignored silently; unknown fobs are logged so they can be learned.
        if !REMOTE_BUTTONS.iter().any(|b| b.address == address) {
            info!("ask433 unknown remote: address=0x{:05x} data=0x{:x}", address, data);
        }
        return;
    };

    sensors::remote_seen(button.input);

    // Only frames we act on reset the window, so a garbled frame in the
    // middle of a hold can't split it into two presses.
    let now = Instant::now();
    let repeat = matches!(*last, Some((c, at)) if c == code && now - at < REPEAT_WINDOW);
    *last = Some((code, now));
    if repeat {
        return;
    }

    info!("ask433 remote: input {} pressed", button.input);
    sensors::press(button.input);
}
