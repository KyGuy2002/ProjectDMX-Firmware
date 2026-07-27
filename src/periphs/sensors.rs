use defmt::println;
use embassy_net::Stack;
use embassy_rp::gpio::{Input, Pull};
use embassy_time::{Duration, Timer};

use crate::hardware::{SensorResources};


#[embassy_executor::task]
pub async fn sensor_task(stack: Stack<'static>, r: SensorResources) {

    println!("Sensor task started.");


    // Init pins
    let mut sensor1 = Input::new(r.in1, Pull::None);

    
    

    loop {
        
        // Wait for the button pin to be pulled low (pressed)
        sensor1.wait_for_falling_edge().await;

        // Simple async debouncing delay
        Timer::after(Duration::from_millis(30)).await;

        if sensor1.is_low() {
            // Confirmed button press! Trigger the OSC send here.
            println!("Sensor 1 pressed! Sending OSC message...");
            
            // Wait until button is released before continuing the loop
            sensor1.wait_for_rising_edge().await;
            Timer::after(Duration::from_millis(30)).await;
        }

    }


}