use core::sync::atomic::{AtomicBool, Ordering};

use defmt::info;

use embassy_rp::pio::program::pio_asm;
use embassy_rp::pio::{Config, Direction, Pio};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_time::{Duration, Instant};
use heapless::Vec;

use crate::hardware::{Ask433Irqs, SlotBAsk433Resources};

/// A sensor/fob you've identified. EV1527/PT2262 frames are 20 address bits
/// + 4 data bits: `address` is fixed per physical device, `open_data`/
/// `closed_data` are whatever 4-bit values that device sends for each state.
/// `status` (true = open) is updated here and read by tcp_cmds to notify
/// on change - `name` is used as-is in logs; tcp_cmds prefixes it with
/// "ASK_" for the TCP message.
///
/// To learn a new device: open/close it, watch the "unknown sensor" log
/// lines below for its address and data value, then add an entry here.
pub struct KnownSensor {
    pub address: u32,
    pub name: &'static str,
    pub open_data: u8,
    pub closed_data: u8,
    pub status: &'static AtomicBool,
}

pub static BOX1_STATUS: AtomicBool = AtomicBool::new(false);
pub static BOX2_STATUS: AtomicBool = AtomicBool::new(false);

pub const KNOWN_SENSORS: &[KnownSensor] = &[
    KnownSensor { address: 0x87e5a, name: "BOX1", open_data: 0x6, closed_data: 0x9, status: &BOX1_STATUS },
    KnownSensor { address: 0x8d37d, name: "BOX2", open_data: 0x6, closed_data: 0x9, status: &BOX2_STATUS },
];

// Cheap superheterodyne receivers have no squelch, so the DATA pin is never
// quiet - with no transmitter in range it's just RF noise. Real fixed-code
// transmitters (EV1527/PT2262, used by ~all cheap door sensors and fobs)
// always precede a frame with a short-high/long-low sync pulse, then encode
// each bit as a high:low pulse pair with a ~1:3 or ~3:1 ratio. Noise almost
// never reproduces that shape, so gating on it is what turns "constant spam"
// into "prints only when something actually transmits".

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
/// How many distinct sensor addresses to remember the last state of.
const MAX_TRACKED_SENSORS: usize = 8;
/// If a sensor hasn't been heard from in this long, forget its last known
/// state - a stale tracked entry can never get "stuck" suppressing a
/// genuine repeat indefinitely.
const STALE_TIMEOUT: Duration = Duration::from_secs(3);

fn lookup_sensor(address: u32) -> Option<&'static KnownSensor> {
    KNOWN_SENSORS.iter().find(|s| s.address == address)
}

fn log_frame(code: u32, bit_count: u32) {
    if bit_count != 24 {
        // Garbled edge-of-burst frame - never a valid state, not worth logging.
        return;
    }

    let address = code >> 4;
    let data = (code & 0xF) as u8;

    match lookup_sensor(address) {
        Some(sensor) if data == sensor.open_data => {
            sensor.status.store(true, Ordering::Relaxed);
            info!("ask433 {}: OPEN", sensor.name);
        }
        Some(sensor) if data == sensor.closed_data => {
            sensor.status.store(false, Ordering::Relaxed);
            info!("ask433 {}: CLOSED", sensor.name);
        }
        Some(sensor) => info!("ask433 {}: unknown state (data=0x{:x})", sensor.name, data),
        None => info!("ask433 unknown sensor: address=0x{:05x} data=0x{:x}", address, data),
    }
}

#[embassy_executor::task]
pub async fn ask433_task(r: SlotBAsk433Resources) {
    info!("Starting ASK433 receiver test task");

    let Pio { mut common, mut sm0, .. } = Pio::new(r.pio, Ask433Irqs);

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
    let rx_pin = common.make_pio_pin(r.pin1);
    sm0.set_pin_dirs(Direction::In, &[&rx_pin]);

    let mut cfg = Config::default();
    cfg.use_program(&loaded, &[]);
    cfg.set_jmp_pin(&rx_pin);
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

    // Last (code, when) seen per sensor address. Only ever touched by a
    // clean 24-bit frame; a garbled frame in between (common up close, where
    // a stronger signal means more edge reflections) must not disturb this.
    // Printing is gated on the decoded state actually changing for that
    // address - or on the entry being stale - so repeats of the same state
    // are suppressed without any short-window timing guesswork.
    let mut last_states: Vec<(u32, u32, Instant), MAX_TRACKED_SENSORS> = Vec::new();

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
                let address = code >> 4;
                let now = Instant::now();

                let changed = match last_states.iter_mut().find(|(a, ..)| *a == address) {
                    Some(entry) => {
                        let stale = now.duration_since(entry.2) >= STALE_TIMEOUT;
                        let changed = stale || entry.1 != code;
                        entry.1 = code;
                        entry.2 = now;
                        changed
                    }
                    // First time seeing this address - always report it, and
                    // start tracking it (silently drops past MAX_TRACKED_SENSORS).
                    None => {
                        let _ = last_states.push((address, code, now));
                        true
                    }
                };

                if changed {
                    log_frame(code, bit_count);
                }
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
