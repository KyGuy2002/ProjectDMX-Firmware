use defmt::{info, println, warn};
use embassy_executor::Spawner;
use embassy_rp::gpio::{Input, Pin, Pull};
use embassy_rp::Peri;
use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, ThreadModeRawMutex};
use embassy_sync::channel::Channel;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};


use crate::config::ButtonConfig;
use crate::hardware::SensorResources;
use crate::logic;
use crate::periphs::mdns;


/// `true` while the wired input is triggered. `reversed` is applied when the
/// pin is read, so these (and everything downstream) only ever mean
/// "triggered" / "not triggered", never a raw pin level.
pub static BUTTON_1_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_2_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_3_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_4_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_5_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_6_STATUS: AtomicBool = AtomicBool::new(false);

/// Remote buttons, 'A'..='D'.
pub const REMOTE_BUTTONS: [char; 4] = ['A', 'B', 'C', 'D'];

/// Millis-since-boot (truncated to u32) of the last frame for each remote
/// button, 0 = never. A held fob button repeats its frame every few tens of ms.
static REMOTE_SEEN_MS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];

/// How long after its last frame a remote button still counts as held. Longer
/// than the OLED's frame time so a quick tap is always drawn.
const REMOTE_HOLD_MS: u32 = 500;

fn remote_index(button: char) -> Option<usize> {
    REMOTE_BUTTONS.iter().position(|&b| b == button)
}

/// Called for every frame of a remote button, repeats included.
pub fn remote_seen(button: char) {
    if let Some(i) = remote_index(button) {
        let now = embassy_time::Instant::now().as_millis() as u32;
        REMOTE_SEEN_MS[i].store(now.max(1), Ordering::Relaxed);
    }
}

/// Whether remote `button` ('A'..='D') is held right now. For display only.
pub fn remote_active(button: char) -> bool {
    let Some(i) = remote_index(button) else { return false };
    let seen = REMOTE_SEEN_MS[i].load(Ordering::Relaxed);
    let now = embassy_time::Instant::now().as_millis() as u32;
    seen != 0 && now.wrapping_sub(seen) < REMOTE_HOLD_MS
}

/// Whether wired input `input` (1..=6) is triggered right now (after
/// `reversed`). For display only; the logic works on presses, not levels.
pub fn button_active(input: u8) -> bool {
    let wired = match input {
        1 => &BUTTON_1_STATUS,
        2 => &BUTTON_2_STATUS,
        3 => &BUTTON_3_STATUS,
        4 => &BUTTON_4_STATUS,
        5 => &BUTTON_5_STATUS,
        6 => &BUTTON_6_STATUS,
        _ => return false,
    };
    wired.load(Ordering::Relaxed)
}

/// Something for the show logic to handle.
#[derive(Clone, Copy, PartialEq, defmt::Format)]
enum Press {
    /// Wired input 1..=6 (or a `press_later` of one).
    Input(u8),
    /// Remote button 'A'..='D'.
    Remote(char),
    /// FPP has just appeared on the network (see `fpp_watch_task`).
    FppOnline,
}

/// Presses, wired and remote. A critical section rather than
/// ThreadModeRawMutex: the remote sends from the RF interrupt executor. Held
/// only for a queue push/pop, once per press.
static PRESSES: Channel<CriticalSectionRawMutex, Press, 8> = Channel::new();

fn send(press: Press) {
    if PRESSES.try_send(press).is_err() {
        warn!("{} dropped: logic queue full", press);
    }
}

/// Hands a press of wired input `button` (1..=6) to the show logic.
pub fn press(button: u8) {
    send(Press::Input(button));
}

/// Hands a press of remote `button` ('A'..='D') to the show logic.
pub fn press_remote(button: char) {
    send(Press::Remote(button));
}

/// Current show-logic state, for the web page. `None` until `on_boot` has run.
static LOGIC_STATE: BlockingMutex<ThreadModeRawMutex, Cell<Option<logic::State>>> =
    BlockingMutex::new(Cell::new(None));

pub fn logic_state() -> Option<logic::State> {
    LOGIC_STATE.lock(|s| s.get())
}

/// A press scheduled by `press_later`: (button, when).
static SCHEDULED: BlockingMutex<ThreadModeRawMutex, Cell<Option<(u8, Instant)>>> =
    BlockingMutex::new(Cell::new(None));

/// Acts as if wired input `button` were pressed `delay` from now, going
/// through `on_button_pressed` like a real press.
///
/// One slot: scheduling again replaces the pending press. The pending press is
/// cancelled if the state changes before it fires (other than by the very
/// handler that scheduled it), so a stale timer from an earlier run of the show
/// can't fire into a later one.
pub fn press_later(button: u8, delay: Duration) {
    SCHEDULED.lock(|s| s.set(Some((button, Instant::now() + delay))));
}


/// `false` stops the wired inputs being read at all (they stay "not
/// triggered"), so only the remote drives the logic.
const WIRED_INPUTS_ENABLED: bool = true;

pub fn start_sensors(spawner: &Spawner, r: SensorResources, buttons: [ButtonConfig; 6]) {
    spawner.spawn(logic_task()).unwrap();
    spawner.spawn(fpp_watch_task()).unwrap();

    if !WIRED_INPUTS_ENABLED {
        info!("Wired inputs disabled (WIRED_INPUTS_ENABLED = false)");
        return;
    }

    spawner.spawn(sensor_task_1(&BUTTON_1_STATUS, r.in1, buttons[0].reversed)).unwrap();
    spawner.spawn(sensor_task_2(&BUTTON_2_STATUS, r.in2, buttons[1].reversed)).unwrap();
    spawner.spawn(sensor_task_3(&BUTTON_3_STATUS, r.in3, buttons[2].reversed)).unwrap();
    spawner.spawn(sensor_task_4(&BUTTON_4_STATUS, r.in4, buttons[3].reversed)).unwrap();
    spawner.spawn(sensor_task_5(&BUTTON_5_STATUS, r.in5, buttons[4].reversed)).unwrap();
    spawner.spawn(sensor_task_6(&BUTTON_6_STATUS, r.in6, buttons[5].reversed)).unwrap();
}


/// Runs the show logic in `logic.rs`: `on_boot` once, then `on_button_pressed`
/// / `on_remote_pressed` for every press (real or `press_later`), one at a
/// time.
#[embassy_executor::task]
async fn logic_task() -> ! {
    let mut state = logic::on_boot();
    info!("Logic: booted into {}", state);
    LOGIC_STATE.lock(|s| s.set(Some(state)));

    loop {
        let scheduled = SCHEDULED.lock(|s| s.get());
        let (press, timed) = match scheduled {
            Some((button, at)) => match select(PRESSES.receive(), Timer::at(at)).await {
                Either::First(pressed) => (pressed, false),
                Either::Second(()) => {
                    SCHEDULED.lock(|s| s.set(None));
                    (Press::Input(button), true)
                }
            },
            None => (PRESSES.receive().await, false),
        };

        // Re-read: a timed press has just cleared the slot.
        let pending = SCHEDULED.lock(|s| s.get());
        let before = state;
        match press {
            Press::Input(button) => logic::on_button_pressed(button, &mut state),
            Press::Remote(button) => logic::on_remote_pressed(button, &mut state),
            Press::FppOnline => logic::on_fpp_online(&mut state),
        }
        info!("Logic: {} {}, {} -> {}", press, if timed { "timer" } else { "pressed" }, before, state);

        // Cancel a pending press on a state change, unless this handler is the
        // one that scheduled it.
        if state != before && SCHEDULED.lock(|s| s.get()) == pending && pending.is_some() {
            info!("Logic: state changed, scheduled press cancelled");
            SCHEDULED.lock(|s| s.set(None));
        }
        LOGIC_STATE.lock(|s| s.set(Some(state)));
    }
}


/// How long FPP gets after it first answers mDNS before `on_fpp_online` runs.
/// Its name comes up before fppd is ready to play, and commands sent in that
/// gap are accepted but do nothing.
const FPP_SETTLE: Duration = Duration::from_secs(5);

/// Sends `Press::FppOnline` each time FPP appears on the network: once after
/// our boot (whether FPP was already up or boots later), and again after it
/// reboots. Uses the mDNS discovery the web page shows, which forgets FPP ~20s
/// after it stops answering.
#[embassy_executor::task]
async fn fpp_watch_task() -> ! {
    let mut online = false;

    loop {
        let now_online = mdns::fpp_ip().is_some();

        if now_online && !online {
            info!("Logic: FPP found, starting in {}s", FPP_SETTLE.as_secs());
            Timer::after(FPP_SETTLE).await;
            // Only counts if it's still there after settling; otherwise the
            // next appearance gets its own try.
            online = mdns::fpp_ip().is_some();
            if online {
                send(Press::FppOnline);
            }
        } else if !now_online && online {
            info!("Logic: FPP lost");
            online = false;
        }

        Timer::after_secs(1).await;
    }
}


/// `reversed = false`: pulled LOW = triggered, HIGH = idle (a button to
/// ground). `reversed = true`: the opposite, HIGH = triggered.
///
/// Always pulled up (the RP2350's internal pull-downs latch - erratum E9), so
/// a normal input with nothing connected reads idle.
async fn run_sensor_task<P: Pin>(no: u8, var: &'static AtomicBool, pin: Peri<'static, P>, reversed: bool) {
    println!("Sensor {} task started.", no);

    let mut sensor = Input::new(pin, Pull::Up);
    let is_pressed = |s: &Input| s.is_low() != reversed;

    let mut previous = is_pressed(&sensor);

    var.store(previous, Ordering::Relaxed);

    loop {
        sensor.wait_for_any_edge().await;

        Timer::after_millis(20).await;

        let pressed = is_pressed(&sensor);

        if pressed != previous {
            previous = pressed;
            var.store(pressed, Ordering::Relaxed);

            if pressed {
                press(no);
            }
        }
    }
}


#[embassy_executor::task]
async fn sensor_task_1(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_42>, reversed: bool) {
    run_sensor_task(1, var, pin, reversed).await;
}


#[embassy_executor::task]
async fn sensor_task_2(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_43>, reversed: bool) {
    run_sensor_task(2, var, pin, reversed).await;
}


#[embassy_executor::task]
async fn sensor_task_3(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_44>, reversed: bool) {
    run_sensor_task(3, var, pin, reversed).await;
}


#[embassy_executor::task]
async fn sensor_task_4(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_46>, reversed: bool) {
    run_sensor_task(4, var, pin, reversed).await;
}


#[embassy_executor::task]
async fn sensor_task_5(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_45>, reversed: bool) {
    run_sensor_task(5, var, pin, reversed).await;
}


#[embassy_executor::task]
async fn sensor_task_6(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_47>, reversed: bool) {
    run_sensor_task(6, var, pin, reversed).await;
}
