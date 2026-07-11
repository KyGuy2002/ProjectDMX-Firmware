use defmt::*;
use embassy_executor::Spawner;

#[embassy_executor::task]
pub async fn net_task() {
    info!("Network task spawned. Awaiting stack initialization...");
    
    loop {
        // Future home of your cyw43 / embassy-net loop mechanics
        embassy_time::Timer::after_secs(1).await;
    }
}