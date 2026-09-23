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


/// `true` while the wired input is triggered. `reversed` is applied when the
/// pin is read, so these (and everything downstream) only ever mean
/// "triggered" / "not triggered", never a raw pin level.
pub static BUTTON_1_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_2_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_3_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_4_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_5_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_6_STATUS: AtomicBool = AtomicBool::new(false);

/// Millis-since-boot (truncated to u32) of the last remote frame for each
/// input, 0 = never. A held fob button repeats its frame every few tens of ms.
static REMOTE_SEEN_MS: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];

/// How long after its last frame a remote button still counts as held. Longer
/// than the OLED's 300ms refresh so a quick tap is always drawn.
const REMOTE_HOLD_MS: u32 = 500;

/// Called for every frame of a remote button, repeats included.
pub fn remote_seen(input: u8) {
    if let Some(seen) = REMOTE_SEEN_MS.get((input as usize).wrapping_sub(1)) {
        let now = embassy_time::Instant::now().as_millis() as u32;
        seen.store(now.max(1), Ordering::Relaxed);
    }
}

/// Whether input `input` (1..=6) is triggered right now: the wired input
/// (after `reversed`) OR a remote button held for it. For display only; the
/// logic works on presses, not levels.
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
    let seen = REMOTE_SEEN_MS[input as usize - 1].load(Ordering::Relaxed);
    let now = embassy_time::Instant::now().as_millis() as u32;
    let remote = seen != 0 && now.wrapping_sub(seen) < REMOTE_HOLD_MS;
    wired.load(Ordering::Relaxed) || remote
}

/// Button numbers (1..=6), sent on each press (wired or remote). A critical
/// section rather than ThreadModeRawMutex: the remote sends from the RF
/// interrupt executor. Held only for a queue push/pop, once per press.
static BUTTON_PRESSES: Channel<CriticalSectionRawMutex, u8, 8> = Channel::new();

/// Hands a press of input `button` (1..=6) to the show logic.
pub fn press(button: u8) {
    if BUTTON_PRESSES.try_send(button).is_err() {
        warn!("Button {} press dropped: logic queue full", button);
    }
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

/// Acts as if `button` were pressed `delay` from now, going through
/// `on_button_pressed` like a real press.
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
/// for every press (real or `press_later`), one at a time.
#[embassy_executor::task]
async fn logic_task() -> ! {
    let mut state = logic::on_boot();
    info!("Logic: booted into {}", state);
    LOGIC_STATE.lock(|s| s.set(Some(state)));

    loop {
        let scheduled = SCHEDULED.lock(|s| s.get());
        let (button, timed) = match scheduled {
            Some((button, at)) => match select(BUTTON_PRESSES.receive(), Timer::at(at)).await {
                Either::First(pressed) => (pressed, false),
                Either::Second(()) => {
                    SCHEDULED.lock(|s| s.set(None));
                    (button, true)
                }
            },
            None => (BUTTON_PRESSES.receive().await, false),
        };

        // Re-read: a timed press has just cleared the slot.
        let pending = SCHEDULED.lock(|s| s.get());
        let before = state;
        logic::on_button_pressed(button, &mut state);
        info!("Logic: button {} {}, {} -> {}", button, if timed { "timer" } else { "pressed" }, before, state);

        // Cancel a pending press on a state change, unless this handler is the
        // one that scheduled it.
        if state != before && SCHEDULED.lock(|s| s.get()) == pending && pending.is_some() {
            info!("Logic: state changed, scheduled press cancelled");
            SCHEDULED.lock(|s| s.set(None));
        }
        LOGIC_STATE.lock(|s| s.set(Some(state)));
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
