use defmt::println;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Input, Pin, Pull};
use embassy_rp::Peri;
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_time::Timer;


use crate::hardware::SensorResources;


pub static BUTTON_1_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_2_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_3_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_4_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_5_STATUS: AtomicBool = AtomicBool::new(false);
pub static BUTTON_6_STATUS: AtomicBool = AtomicBool::new(false);



pub fn start_sensors(spawner: &Spawner, r: SensorResources) {
    spawner.spawn(sensor_task_1(&BUTTON_1_STATUS, r.in1)).unwrap();
    spawner.spawn(sensor_task_2(&BUTTON_2_STATUS, r.in2)).unwrap();
    spawner.spawn(sensor_task_3(&BUTTON_3_STATUS, r.in3)).unwrap();
    spawner.spawn(sensor_task_4(&BUTTON_4_STATUS, r.in4)).unwrap();
    spawner.spawn(sensor_task_5(&BUTTON_5_STATUS, r.in5)).unwrap();
    spawner.spawn(sensor_task_6(&BUTTON_6_STATUS, r.in6)).unwrap();
}


async fn run_sensor_task<P: Pin>(no: i32, var: &'static AtomicBool, pin: Peri<'static, P>) {
    println!("Sensor {} task started.", no);

    let mut sensor = Input::new(pin, Pull::Up);

    let mut previous = sensor.is_low();

    var.store(previous, Ordering::Relaxed);

    loop {
        sensor.wait_for_any_edge().await;

        Timer::after_millis(20).await;

        let pressed = sensor.is_low();

        if pressed != previous {
            previous = pressed;
            var.store(pressed, Ordering::Relaxed);
        }
    }
}


#[embassy_executor::task]
async fn sensor_task_1(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_42>) {
    run_sensor_task(1, var, pin).await;
}


#[embassy_executor::task]
async fn sensor_task_2(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_43>) {
    run_sensor_task(2, var, pin).await;
}


#[embassy_executor::task]
async fn sensor_task_3(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_44>) {
    run_sensor_task(3, var, pin).await;
}


#[embassy_executor::task]
async fn sensor_task_4(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_46>) {
    run_sensor_task(4, var, pin).await;
}


#[embassy_executor::task]
async fn sensor_task_5(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_45>) {
    run_sensor_task(5, var, pin).await;
}


#[embassy_executor::task]
async fn sensor_task_6(var: &'static AtomicBool, pin: Peri<'static, embassy_rp::peripherals::PIN_47>) {
    run_sensor_task(6, var, pin).await;
}