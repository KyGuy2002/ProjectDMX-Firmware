use defmt::{info, warn, println};
use embassy_net::{Stack, tcp::TcpSocket};
use embassy_rp::gpio::{Input, Pull};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_time::Timer;


pub static BUTTON_STATUS: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
pub async fn tcp_cmds_task(stack: Stack<'static>) {
    println!("TCP Commands task started.");


    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];
    
    // Target your Game Master PC running Chataigne
    let remote_endpoint = embassy_net::IpEndpoint::new(embassy_net::IpAddress::v4(192, 168, 1, 29), 6000);

    // Keeps track of the last state we successfully confirmed over the network
    let mut last_sent_status = BUTTON_STATUS.load(Ordering::Relaxed);


    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        info!("Connecting to Chataigne TCP Server...");
        
        if let Err(e) = socket.connect(remote_endpoint).await {
            warn!("Connection failed: {:?}. Retrying in 2 seconds...", e);
            embassy_time::Timer::after_secs(2).await;
            continue;
        }

        info!("=== Connected safely! Awaiting switch updates...");

        loop {
            // Read the current atomic state updated by your hardware interrupts/GPIO
            let current_status = BUTTON_STATUS.load(Ordering::Relaxed);

            // Edge detection: only act if the state changed
            if current_status != last_sent_status {
                info!("msg");
                let message = if current_status { "SWITCH:1\n" } else { "SWITCH:0\n" };

                // Send over TCP (W5500 handles buffering and physical packet retries)
                if let Err(e) = socket.write(message.as_bytes()).await {
                    warn!("TCP Write Failed: {:?}. Forcing reconnection...", e);
                    break; // Break internal loop to re-establish the socket connection
                }
                
                info!("Sent to Chataigne: {}", message.trim_end());
                last_sent_status = current_status; // Update local tracking on success
            }

            // Yield control back to Embassy executor for 10ms to save processing cycles
            embassy_time::Timer::after_millis(10).await;
        }
    }
}