use core::sync::atomic::{AtomicBool, Ordering};

use defmt::info;

use embassy_rp::pio::program::pio_asm;
use embassy_rp::pio::{Config, Direction, Pio};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_time::{Duration, Instant};
use heapless::Vec;

use crate::hardware::{Ask433Irqs, SlotBAsk433Resources};

/// What a 4-bit data value means for a given [`KnownSensor`].
pub enum SensorKind {
    /// Two-state device (door/window sensor): distinct data values for open
    /// and closed.
    DoorSensor { open_data: u8, closed_data: u8 },
    /// On-only device (fob button): a single data value means "pressed".
    /// There's no separate "released" transmission to key off of, so
    /// `status` just goes true on press and is never cleared here.
    Button { data: u8 },
}

impl SensorKind {
    /// If `data` is a value this kind recognizes, the state to store
    /// (true = open / pressed).
    fn resolve(&self, data: u8) -> Option<bool> {
        match *self {
            SensorKind::DoorSensor { open_data, closed_data } => {
                if data == open_data {
                    Some(true)
                } else if data == closed_data {
                    Some(false)
                } else {
                    None
                }
            }
            SensorKind::Button { data: on_data } => (data == on_data).then_some(true),
        }
    }
}

/// A sensor/fob you've identified. EV1527/PT2262 frames are 20 address bits
/// + 4 data bits: `address` is fixed per physical device, `kind` says what
/// its data values mean. Multiple rows may share the same `address` (e.g.
/// one fob, one row per button) - they're told apart by data, not address.
/// `status` is updated here and read by tcp_cmds to notify on change -
/// `name` is used as-is in logs; tcp_cmds prefixes it with "ASK_" for the
/// TCP message.
///
/// To learn a new device: trigger it, watch the "unknown sensor" log lines
/// below for its address and data value, then add an entry here.
pub struct KnownSensor {
    pub address: u32,
    pub name: &'static str,
    pub kind: SensorKind,
    pub status: &'static AtomicBool,
}

impl KnownSensor {
    pub const fn door(address: u32, name: &'static str, open_data: u8, closed_data: u8, status: &'static AtomicBool) -> Self {
        KnownSensor { address, name, kind: SensorKind::DoorSensor { open_data, closed_data }, status }
    }

    pub const fn button(address: u32, name: &'static str, data: u8, status: &'static AtomicBool) -> Self {
        KnownSensor { address, name, kind: SensorKind::Button { data }, status }
    }
}

pub static BOX1_STATUS: AtomicBool = AtomicBool::new(false);
pub static BOX2_STATUS: AtomicBool = AtomicBool::new(false);
pub static FOB_A_STATUS: AtomicBool = AtomicBool::new(false);
pub static FOB_B_STATUS: AtomicBool = AtomicBool::new(false);
pub static FOB_C_STATUS: AtomicBool = AtomicBool::new(false);
pub static FOB_D_STATUS: AtomicBool = AtomicBool::new(false);

pub const KNOWN_SENSORS: &[KnownSensor] = &[
    KnownSensor::door(0x87e5a, "BOX1", 0x6, 0x9, &BOX1_STATUS),
    KnownSensor::door(0x8d37d, "BOX2", 0x6, 0x9, &BOX2_STATUS),
    // Fill in the real address (same for all 4 - it's one fob, one row per
    // button, told apart by data).
    KnownSensor::button(0x3fcad, "FOB_A", 0x8, &FOB_A_STATUS),
    KnownSensor::button(0x3fcad, "FOB_B", 0x4, &FOB_B_STATUS),
    KnownSensor::button(0x3fcad, "FOB_C", 0x2, &FOB_C_STATUS),
    KnownSensor::button(0x3fcad, "FOB_D", 0x1, &FOB_D_STATUS),
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
/// If a known door sensor hasn't been heard from in this long, forget its
/// last open/closed state - a stale tracked entry can never get "stuck"
/// suppressing a genuine repeat indefinitely.
const STALE_TIMEOUT: Duration = Duration::from_secs(3);
/// Same idea, but for buttons: repeats within one press-and-hold arrive
/// every ~10-30ms, so this just needs to be comfortably longer than that -
/// short enough that releasing and clicking again registers as a new event.
const BUTTON_REPEAT_WINDOW: Duration = Duration::from_millis(500);

type SensorMemory = Vec<(u32, u8, Instant), MAX_TRACKED_SENSORS>;

/// Every row sharing `address` (a fob has one row per button, all with the
/// same address) - told apart from each other by which one's data matches.
fn sensors_for(address: u32) -> impl Iterator<Item = &'static KnownSensor> {
    KNOWN_SENSORS.iter().filter(move |s| s.address == address)
}

/// Dedup keyed on (address, data): true if this exact data value was already
/// the last thing logged for this address within `timeout`. Updates the
/// tracker as a side effect - callers must only call this for a value that's
/// actually safe to remember (see the "unknown state" note in handle_frame).
fn is_repeat(last_states: &mut SensorMemory, address: u32, data: u8, now: Instant, timeout: Duration) -> bool {
    match last_states.iter_mut().find(|(a, ..)| *a == address) {
        Some(entry) => {
            let stale = now.duration_since(entry.2) >= timeout;
            let repeat = !stale && entry.1 == data;
            entry.1 = data;
            entry.2 = now;
            repeat
        }
        // First time seeing this address - always report it, and start
        // tracking it (silently drops past MAX_TRACKED_SENSORS).
        None => {
            let _ = last_states.push((address, data, now));
            false
        }
    }
}

fn handle_frame(last_states: &mut SensorMemory, code: u32) {
    let address = code >> 4;
    let data = (code & 0xF) as u8;

    let matched = sensors_for(address).find_map(|s| s.kind.resolve(data).map(|state| (s, state)));

    match matched {
        Some((sensor, state)) => {
            let timeout = match sensor.kind {
                SensorKind::DoorSensor { .. } => STALE_TIMEOUT,
                SensorKind::Button { .. } => BUTTON_REPEAT_WINDOW,
            };
            if !is_repeat(last_states, address, data, Instant::now(), timeout) {
                let label = match sensor.kind {
                    SensorKind::DoorSensor { .. } => {
                        sensor.status.store(state, Ordering::Relaxed);
                        if state { "OPEN" } else { "CLOSED" }
                    }
                    SensorKind::Button { .. } => {
                        // A click is a one-shot event, not a persistent state,
                        // so always storing true would only produce an edge -
                        // and therefore a TCP message via tcp_cmds.rs - on the
                        // very first press ever. Flipping it every press keeps
                        // producing a fresh edge each time, so every click
                        // sends a message; the actual 0/1 value is meaningless.
                        let toggled = !sensor.status.load(Ordering::Relaxed);
                        sensor.status.store(toggled, Ordering::Relaxed);
                        "ON"
                    }
                };
                info!("ask433 {}: {}", sensor.name, label);
            }
        }
        None if sensors_for(address).next().is_some() => {
            // Address matches known row(s), but data doesn't match any of
            // them - a garbled reading, not a real extra state. Report it,
            // but never feed it into the tracker: doing so would overwrite
            // the last genuine value, and the next real repeat could then
            // get compared against garbage instead and wrongly suppressed.
            info!("ask433 address 0x{:05x}: unrecognized data=0x{:x}", address, data);
        }
        // Completely unlisted address - raw sniffer mode: print every single
        // decoded frame, no dedup/debounce at all. Useful for a flaky
        // transmitter where you need to see exactly what came through
        // (including drops/glitches), not a cleaned-up view of it.
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

    // Last (data, when) seen per sensor address - see is_repeat/handle_frame.
    let mut last_states: SensorMemory = Vec::new();

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
                handle_frame(&mut last_states, code);
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
