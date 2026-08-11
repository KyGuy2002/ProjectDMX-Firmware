use defmt::{info, warn, println};
use embassy_net::{Stack, tcp::TcpSocket};
use core::sync::atomic::{AtomicBool, Ordering};
use core::fmt::Write;

use crate::modules::ask433::KNOWN_SENSORS;
use crate::periphs::sensors::*;

#[embassy_executor::task]
pub async fn tcp_cmds_task(stack: Stack<'static>) {
    println!("TCP Commands task started.");


    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];
    
    // Target your Game Master PC running Chataigne
    let remote_endpoint = embassy_net::IpEndpoint::new(embassy_net::IpAddress::v4(192, 168, 1, 29), 6000);

    // Keeps track of the last state we successfully confirmed over the network
    let mut last_sent_status_1 = BUTTON_1_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_2 = BUTTON_2_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_3 = BUTTON_3_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_4 = BUTTON_4_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_5 = BUTTON_5_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_6 = BUTTON_6_STATUS.load(Ordering::Relaxed);

    // Same edge-detection, one slot per entry in ask433::KNOWN_SENSORS.
    let mut last_sent_ask433 = [false; KNOWN_SENSORS.len()];
    for (i, sensor) in KNOWN_SENSORS.iter().enumerate() {
        last_sent_ask433[i] = sensor.status.load(Ordering::Relaxed);
    }


    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        info!("Connecting to Chataigne TCP Server...");
        
        if let Err(e) = socket.connect(remote_endpoint).await {
            warn!("Connection failed: {:?}. Retrying in 2 seconds...", e);
            embassy_time::Timer::after_secs(2).await;
            continue;
        }

        info!("Connected safely! Awaiting switch updates...");

        loop {
            
            last_sent_status_1 = send_sensor_status(&mut socket, &BUTTON_1_STATUS, 1, last_sent_status_1).await;
            last_sent_status_2 = send_sensor_status(&mut socket, &BUTTON_2_STATUS, 2, last_sent_status_2).await;
            last_sent_status_3 = send_sensor_status(&mut socket, &BUTTON_3_STATUS, 3, last_sent_status_3).await;
            last_sent_status_4 = send_sensor_status(&mut socket, &BUTTON_4_STATUS, 4, last_sent_status_4).await;
            last_sent_status_5 = send_sensor_status(&mut socket, &BUTTON_5_STATUS, 5, last_sent_status_5).await;
            last_sent_status_6 = send_sensor_status(&mut socket, &BUTTON_6_STATUS, 6, last_sent_status_6).await;

            for (i, sensor) in KNOWN_SENSORS.iter().enumerate() {
                last_sent_ask433[i] =
                    send_ask433_status(&mut socket, sensor, last_sent_ask433[i]).await;
            }

            // Yield control back to Embassy executor for 10ms to save processing cycles
            embassy_time::Timer::after_millis(10).await;
        }
    }
}


async fn send_sensor_status(socket: &mut TcpSocket<'_>, var: &'static AtomicBool, no: i32, last_sent_status: bool) -> bool {
    // Read the current atomic state updated by your hardware interrupts/GPIO
    let current_status = var.load(Ordering::Relaxed);

    // Edge detection: only act if the state changed
    if current_status != last_sent_status {
        info!("msg");
        let mut message: heapless::String<32> = heapless::String::new();
        write!(&mut message, "SWITCH_{}:{}\n", no, current_status as u8).unwrap();

        // Send over TCP (W5500 handles buffering and physical packet retries)
        if let Err(e) = socket.write(message.as_bytes()).await {
            warn!("TCP Write Failed: {:?}. Forcing reconnection...", e);
            return last_sent_status;
        }
        
        info!("Sent to Chataigne: {}", message.trim_end());
        return current_status;
    }

    last_sent_status
}


async fn send_ask433_status(
    socket: &mut TcpSocket<'_>,
    sensor: &crate::modules::ask433::KnownSensor,
    last_sent_status: bool,
) -> bool {
    let current_status = sensor.status.load(Ordering::Relaxed);

    if current_status != last_sent_status {
        let mut message: heapless::String<40> = heapless::String::new();
        // "ASK_" is TCP-only - KNOWN_SENSORS names (e.g. "BOX1") stay bare in logs.
        write!(&mut message, "ASK_{}:{}\n", sensor.name, current_status as u8).unwrap();

        if let Err(e) = socket.write(message.as_bytes()).await {
            warn!("TCP Write Failed: {:?}. Forcing reconnection...", e);
            return last_sent_status;
        }

        info!("Sent to Chataigne: {}", message.trim_end());
        return current_status;
    }

    last_sent_status
}