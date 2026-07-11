use embassy_time::{Duration, Timer};

use crate::config::DimmerConfig;
use crate::hardware::SlotPins;


#[embassy_executor::task]
pub async fn dimmer_task(settings: DimmerConfig, pins: SlotPins) {
    
    loop {
        Timer::after(Duration::from_millis(20)).await;
    }
    

}