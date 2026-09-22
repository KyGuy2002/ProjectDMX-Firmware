use defmt::{info, warn, println};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack, tcp::{State, TcpSocket}};
use embassy_time::{Duration, Timer, with_timeout};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::fmt::Write;

use crate::config::MAX_HOSTNAME_LEN;
use crate::periphs::{mdns, sensors::*};

/// How the link to the Game Master PC (Chataigne / FPP) is doing, for the OLED.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
pub enum ChataigneStatus {
    /// Initial state: first lookup + connect attempt still in progress.
    Lookup,
    /// mDNS found nothing for the configured name.
    NoHost,
    /// Address known, but the TCP connect failed or timed out.
    NoConn,
    Connected,
    /// Was connected, then the connection dropped. Stays until the next
    /// attempt succeeds or fails, so a drop is visible for at least a retry.
    Lost,
}

static STATUS: AtomicU8 = AtomicU8::new(ChataigneStatus::Lookup as u8);

fn set_status(status: ChataigneStatus) {
    STATUS.store(status as u8, Ordering::Relaxed);
}

pub fn chataigne_status() -> ChataigneStatus {
    match STATUS.load(Ordering::Relaxed) {
        x if x == ChataigneStatus::NoHost as u8 => ChataigneStatus::NoHost,
        x if x == ChataigneStatus::NoConn as u8 => ChataigneStatus::NoConn,
        x if x == ChataigneStatus::Connected as u8 => ChataigneStatus::Connected,
        x if x == ChataigneStatus::Lost as u8 => ChataigneStatus::Lost,
        _ => ChataigneStatus::Lookup,
    }
}

const RETRY_DELAY: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[embassy_executor::task]
pub async fn tcp_cmds_task(stack: Stack<'static>, host: heapless::String<MAX_HOSTNAME_LEN>, port: u16) {
    println!("TCP Commands task started.");


    let mut rx_buffer = [0; 1024];
    let mut tx_buffer = [0; 1024];

    // Keeps track of the last state we successfully confirmed over the network
    let mut last_sent_status_1 = BUTTON_1_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_2 = BUTTON_2_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_3 = BUTTON_3_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_4 = BUTTON_4_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_5 = BUTTON_5_STATUS.load(Ordering::Relaxed);
    let mut last_sent_status_6 = BUTTON_6_STATUS.load(Ordering::Relaxed);

    // Resolve + connect forever. The name is looked up again on every attempt so
    // a peer that came back with a different link-local address is found.
    loop {
        info!("Looking up Chataigne ({})...", host.as_str());

        let ip = match host.parse::<Ipv4Address>() {
            Ok(ip) => ip,
            Err(_) => match mdns::resolve(&host).await {
                Some(ip) => ip,
                None => {
                    warn!("mDNS: no answer for {}. Retrying...", host.as_str());
                    set_status(ChataigneStatus::NoHost);
                    Timer::after(RETRY_DELAY).await;
                    continue;
                }
            },
        };

        // No gateway, so anything off 169.254/16 can't be reached.
        if ip.octets()[..2] != [169, 254] {
            warn!("{} resolved to {}, outside 169.254.0.0/16 - unreachable without a gateway", host.as_str(), ip);
        }

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        // Without these a vanished peer (cable pulled, PC asleep) is never
        // noticed: we only ever write on a switch change.
        socket.set_keep_alive(Some(Duration::from_secs(3)));
        socket.set_timeout(Some(Duration::from_secs(10)));

        info!("Connecting to Chataigne at {}:{}...", ip, port);
        let remote_endpoint = IpEndpoint::new(IpAddress::Ipv4(ip), port);
        let connected = with_timeout(CONNECT_TIMEOUT, socket.connect(remote_endpoint)).await;

        if !matches!(connected, Ok(Ok(()))) {
            warn!("Connection to {}:{} failed. Retrying...", ip, port);
            socket.abort();
            set_status(ChataigneStatus::NoConn);
            Timer::after(RETRY_DELAY).await;
            continue;
        }

        info!("Connected safely! Awaiting switch updates...");
        set_status(ChataigneStatus::Connected);

        // Runs until the connection is gone, then falls through to re-resolve.
        loop {
            // Peer closed (FIN -> CloseWait), reset, or keep-alive timed out.
            if socket.state() != State::Established || !stack.is_link_up() {
                break;
            }

            // Chataigne shouldn't send anything, but don't let stray data fill the
            // receive window.
            if socket.can_recv() {
                let mut scratch = [0u8; 64];
                let _ = socket.read(&mut scratch).await;
            }

            let sent = async {
                last_sent_status_1 = send_sensor_status(&mut socket, &BUTTON_1_STATUS, 1, last_sent_status_1).await?;
                last_sent_status_2 = send_sensor_status(&mut socket, &BUTTON_2_STATUS, 2, last_sent_status_2).await?;
                last_sent_status_3 = send_sensor_status(&mut socket, &BUTTON_3_STATUS, 3, last_sent_status_3).await?;
                last_sent_status_4 = send_sensor_status(&mut socket, &BUTTON_4_STATUS, 4, last_sent_status_4).await?;
                last_sent_status_5 = send_sensor_status(&mut socket, &BUTTON_5_STATUS, 5, last_sent_status_5).await?;
                last_sent_status_6 = send_sensor_status(&mut socket, &BUTTON_6_STATUS, 6, last_sent_status_6).await?;
                Ok::<(), ()>(())
            }
            .await;

            if sent.is_err() {
                break;
            }

            // Yield control back to Embassy executor for 10ms to save processing cycles
            Timer::after_millis(10).await;
        }

        warn!("Lost connection to Chataigne. Reconnecting...");
        socket.abort();
        set_status(ChataigneStatus::Lost);
        Timer::after(RETRY_DELAY).await;
    }
}


/// `Err` if the write failed (the connection is dead); otherwise the state to remember as last sent.
async fn send_sensor_status(socket: &mut TcpSocket<'_>, var: &'static AtomicBool, no: i32, last_sent_status: bool) -> Result<bool, ()> {
    // Read the current atomic state updated by your hardware interrupts/GPIO
    let current_status = var.load(Ordering::Relaxed);

    // Edge detection: only act if the state changed
    if current_status != last_sent_status {
        let mut message: heapless::String<32> = heapless::String::new();
        write!(&mut message, "SWITCH_{}:{}\n", no, current_status as u8).unwrap();

        // Send over TCP (W5500 handles buffering and physical packet retries)
        if let Err(e) = socket.write(message.as_bytes()).await {
            warn!("TCP Write Failed: {:?}. Forcing reconnection...", e);
            return Err(());
        }

        // info!("Sent to Chataigne: {}", message.trim_end());
        return Ok(current_status);
    }

    Ok(last_sent_status)
}
