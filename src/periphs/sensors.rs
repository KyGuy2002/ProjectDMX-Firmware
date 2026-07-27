use defmt::println;
use embassy_net::Stack;
use embassy_rp::gpio::{Input, Pull};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_time::Timer;


use crate::hardware::SensorResources;


pub static BUTTON_STATUS: AtomicBool = AtomicBool::new(false);


#[embassy_executor::task]
pub async fn sensor_task(r: SensorResources) {
    println!("Sensor task started.");


    let mut sensor1 = Input::new(r.in1, Pull::Up);

    let mut previous = sensor1.is_low();

    BUTTON_STATUS.store(previous, Ordering::Relaxed);


    loop {

        sensor1.wait_for_any_edge().await;

        
        Timer::after_millis(20).await;

        let pressed = sensor1.is_low();

        if pressed != previous {
            previous = pressed;
            BUTTON_STATUS.store(pressed, Ordering::Relaxed);
        }

    }
}