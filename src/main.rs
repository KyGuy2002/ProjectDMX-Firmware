#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;

// Milliseconds since boot on every log line (shown by `{t}` in the probe-rs
// log format), so boot timing can be read straight off the log.
defmt::timestamp!("{=u64:ms}", embassy_time::Instant::now().as_millis());
use panic_probe as _;

mod config;
mod hardware;
mod logic;
mod modules;
mod pixel_mapping_config;

mod periphs {
    pub mod dmx;
    pub mod eth;
    pub mod http;
    pub mod mdns;
    pub mod artnet;
    pub mod sacn;
    pub mod oled;
    pub mod sensors;
    pub mod fpp;
    pub mod ask433;
    pub mod sd;
    pub mod audio;
}

use core::cell::RefCell;
use core::future::pending;

use embassy_executor::{InterruptExecutor, Spawner};
use embassy_rp::interrupt;
use embassy_rp::interrupt::{InterruptExt, Priority};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::once_lock::OnceLock;

use config::*;
use modules::*;


use crate::hardware::AssignedResources;
use crate::hardware::*;




// Global config
pub static CONFIG: OnceLock<BoardInstanceConfig> = OnceLock::new();

// Global DMX buffer
pub const MAX_UNIVERSES: usize = 4;
// Per-port NeoPixel buffer size. Must cover the longest strip in any config
// (currently the 240-pixel glowing-wire runs). Sizes the neo buffers, the
// layout table, and the PioWs2812 const generic.
pub const MAX_PIXELS: usize = 256;

pub static DMX_MATRIX: BlockingMutex<CriticalSectionRawMutex, RefCell<[[u8; 512]; MAX_UNIVERSES]>> =
    BlockingMutex::new(RefCell::new([[0u8; 512]; MAX_UNIVERSES]));

// Only the I2S DMA feed (audio_output_task) runs on this interrupt executor -
// it preempts every thread-mode task (OLED I2C flush, NeoPixel effects, sACN
// parsing, ...), which matters because the I2S PIO FIFO only holds ~180us of
// samples between DMA transfers; anything that blocks the executor longer than
// that at a buffer boundary causes an audible underrun.
//
// Priorities (embassy-rp only exposes P0..P3, P3 lowest): this executor and
// DMA_IRQ_0 (which completes its I2S transfers; embassy-rp sets it to P3) both
// run at P2, one level above the RF executor below. Equal priorities never
// preempt each other, so with all three at P3 a burst of receiver noise kept
// the RF task busy while an I2S DMA completion waited - audible stutter.
//
// MP3 decode + SD reads (audio_decode_task) deliberately do NOT run here: an
// interrupt executor only lets *sibling tasks on the same executor* get a
// turn when one yields, and audio_output_task is the only thing on it, so a
// yield there is a no-op as far as thread-mode code is concerned - it can't
// relieve starvation of neo_task/oled_task/etc. Decode runs as a normal
// thread-mode task instead, spawned on the default `spawner` below, where its
// cooperative yields (via sd::read_yielding) actually hand time to those
// tasks.
static AUDIO_EXECUTOR: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn SWI_IRQ_1() {
    unsafe { AUDIO_EXECUTOR.on_interrupt() };
}

// The 433 MHz remote decoder (ask433_task). Its PIO pushes every pulse width
// into an 8-deep RX FIFO with `push noblock`, so a thread-mode stall of a few
// ms (OLED flush, neo effects, MP3 decode) overflowed it and silently dropped
// pulses mid-frame - only ~10% of presses decoded. Here it preempts thread
// mode, and at P3 it's below the audio feed and DMA (P2), so it can never
// hold them up.
static RF_EXECUTOR: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn SWI_IRQ_2() {
    unsafe { RF_EXECUTOR.on_interrupt() };
}

// Published by cpu_monitor_task, read by oled_task for the on-screen readout.
// A rough "thread-mode load" percentage, not true CPU usage - see below.
pub static CPU_STALL_PCT: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

// Millis-since-boot (truncated to u32) of the last valid sACN/Art-Net packet, 0 =
// none yet. The on-board switch keeps the W5500's PHY link up whether or not
// anything is plugged into the far side, so received data is the only real
// "connected" signal. Read via `input_active()` for the OLED.
static LAST_INPUT_RX_MS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// E1.31's "network data loss" timeout: a source that has been silent this long
/// is considered gone.
const INPUT_LOSS_TIMEOUT_MS: u32 = 2500;

pub fn mark_input_rx() {
    let now = embassy_time::Instant::now().as_millis() as u32;
    LAST_INPUT_RX_MS.store(now.max(1), core::sync::atomic::Ordering::Relaxed);
}

pub fn input_active() -> bool {
    let last = LAST_INPUT_RX_MS.load(core::sync::atomic::Ordering::Relaxed);
    let now = embassy_time::Instant::now().as_millis() as u32;
    last != 0 && now.wrapping_sub(last) < INPUT_LOSS_TIMEOUT_MS
}

// Measures whether the *default thread-mode executor* (where neo_task,
// dmx_task, oled_task, audio_decode_task etc. all run) is being starved, and
// by how much. Ticks every 5ms; any tick that takes longer means something
// else (most likely audio decode or an oled flush) held the executor busy in
// between without yielding. The overrun accumulated over each rolling second
// is published as CPU_STALL_PCT for the OLED readout - it undercounts true
// utilization (bursts shorter than 5ms never show up), but it's a direct,
// cheap proxy for "how close is thread-mode to starving something."
#[embassy_executor::task]
async fn cpu_monitor_task() {
    use embassy_time::{Instant, Timer};
    use core::sync::atomic::Ordering;
    const EXPECTED_MS: u64 = 5;
    const WINDOW_MS: u64 = 1000;

    let mut last = Instant::now();
    let mut window_start = last;
    let mut overrun_ms: u64 = 0;

    loop {
        Timer::after_millis(EXPECTED_MS).await;
        let now = Instant::now();
        let elapsed = (now - last).as_millis();
        if elapsed > EXPECTED_MS {
            overrun_ms += elapsed - EXPECTED_MS;
        }
        if elapsed > EXPECTED_MS * 2 {
            // warn!("DIAG thread-mode stall: {}ms (wanted {}ms)", elapsed, EXPECTED_MS);
        }
        last = now;

        let window_elapsed = (now - window_start).as_millis();
        if window_elapsed >= WINDOW_MS {
            let pct = ((overrun_ms * 100) / window_elapsed).min(100) as u8;
            CPU_STALL_PCT.store(pct, Ordering::Relaxed);
            overrun_ms = 0;
            window_start = now;
        }
    }
}


#[embassy_executor::main]
async fn main(spawner: Spawner) {


    info!("=======================================");
    info!("");
    info!("     ProjectDMX Controller Booting     ");
    info!("              Version 0.1r             ");
    info!("");



    // Embassy init
    let hardware_config = embassy_rp::config::Config::default();
    let p = embassy_rp::init(hardware_config);



    // Manage pins and peripherals
    let r = split_resources!(p);



    // JSONC Configuration. `None` = use the compiled-in config.jsonc; pass
    // Some(sd_text) here once SD-card config loading lands.
    let config = load_config(None);
    CONFIG.init(config.clone()).unwrap();



    // Spawn Peripherals
    spawner.spawn(periphs::oled::oled_task(r.oled)).unwrap(); // OLED
    spawner.spawn(periphs::dmx::dmx_task(r.dmx)).unwrap(); // DMX
    spawner.spawn(cpu_monitor_task()).unwrap();

    // DMX-triggered audio playback, split across two tasks (see the
    // AUDIO_EXECUTOR comment above for why): the I2S DMA feed runs on its own
    // high-priority interrupt executor, while MP3 decode/SD reads run as a
    // normal thread-mode task. The SD card is mounted inside audio_decode_task:
    // its VolumeManager holds a RefCell (not Send), so the handle must never
    // leave the executor it's used on.
    interrupt::DMA_IRQ_0.set_priority(Priority::P2);
    interrupt::SWI_IRQ_1.set_priority(Priority::P2);
    let audio_spawner = AUDIO_EXECUTOR.start(interrupt::SWI_IRQ_1);
    audio_spawner.spawn(periphs::audio::audio_output_task(r.audio)).unwrap();
    spawner.spawn(periphs::audio::audio_decode_task(config.audio, r.sd)).unwrap();

    if config.input.source == InputProtocol::Artnet || config.input.source == InputProtocol::sACN {
        let stack = periphs::eth::start_eth(&spawner, r.eth).await; // Ethernet
        spawner.spawn(periphs::fpp::fpp_task(stack, config.fpp.host.clone())).unwrap(); // FPP commands
        periphs::sensors::start_sensors(&spawner, r.sensors, config.buttons); // Sensors + show logic
        interrupt::SWI_IRQ_2.set_priority(Priority::P3);
        let rf_spawner = RF_EXECUTOR.start(interrupt::SWI_IRQ_2);
        rf_spawner.spawn(periphs::ask433::ask433_task(r.remote)).unwrap(); // 433 MHz remote
        spawner.spawn(periphs::mdns::mdns_task(stack)).unwrap(); // mDNS responder + resolver
        spawner.spawn(periphs::http::http_task(stack)).unwrap(); // Web status page
        spawner.spawn(periphs::http::http_task(stack)).unwrap(); // (second listener)

        if config.input.source == InputProtocol::Artnet {
            spawner.spawn(periphs::artnet::artnet_task(stack)).unwrap(); // Art-Net
        } else if config.input.source == InputProtocol::sACN {
            spawner.spawn(periphs::sacn::sacn_task(stack)).unwrap(); // sACN
        }
    }



    // Module Initialization
    // init_slot_a(&spawner, config.modules.slot_a, r.slot_a_relay);
    init_slot_b(&spawner, config.modules.slot_b, r.slot_b_dimmer);
    init_slot_c(&spawner, config.modules.slot_c, r.slot_c_neo);
    init_slot_d(&spawner, config.modules.slot_d, r.slot_d_dimmer);



    pending::<()>().await;


}








/**
 * Reads a slice of DMX channel values from the DMX_MATRIX for a given universe and starting channel.
 *
 * Both `universe` (1..=MAX_UNIVERSES) and `start_channel` (1..=512) are 1-based,
 * matching how fixtures and the JSON config are addressed. `DMX_MATRIX` is stored
 * 0-based, so universe 1 / channel 1 is row 0 / index 0. Out-of-range requests,
 * and any tail past channel 512, read as 0.
 */
pub fn read_channels<const N: usize>(universe: usize, start_channel: usize) -> [u8; N] {
    DMX_MATRIX.lock(|matrix| {
        let mut dest = [0u8; N];

        if (1..=MAX_UNIVERSES).contains(&universe) && (1..=512).contains(&start_channel) {
            let universe_index = universe - 1;
            let start_index = start_channel - 1;
            let buf = matrix.borrow();
            let universe_row: &[u8] = &buf[universe_index];

            let end = (start_index + N).min(512);
            let src_slice = &universe_row[start_index..end];

            dest[..src_slice.len()].copy_from_slice(src_slice);
        }

        dest
    })
}
