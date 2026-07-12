use defmt::{info, warn};

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::Stack;
use embassy_time::Instant;
use static_cell::StaticCell;

use crate::{DMX_MATRIX, MAX_UNIVERSES};

const ARTNET_PORT: u16 = 6454;
const ARTNET_HEADER: &[u8; 8] = b"Art-Net\0";
const OP_OUTPUT_LO: u8 = 0x00;
const OP_OUTPUT_HI: u8 = 0x50;

// Keep this small for live lighting.
// Too large creates old-packet backlog.
const UDP_RX_PACKET_COUNT: usize = 8;
const UDP_RX_BUF_SIZE: usize = UDP_RX_PACKET_COUNT * 600;

// Set this to the universe your pixels actually read from in firmware.
// If your config says port_config.universe = 4, use 4.
// If your lighting software says "Universe 4" but firmware uses zero-based,
// this may need to be 3.
const DEBUG_PIXEL_UNIVERSE: usize = 4;

#[embassy_executor::task]
pub async fn artnet_task(stack: Stack<'static>) -> ! {
    static RX_META: StaticCell<[PacketMetadata; UDP_RX_PACKET_COUNT]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 1]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; UDP_RX_BUF_SIZE]> = StaticCell::new();
    static TX_BUF: StaticCell<[u8; 1]> = StaticCell::new();

    let rx_meta = RX_META.init([PacketMetadata::EMPTY; UDP_RX_PACKET_COUNT]);
    let tx_meta = TX_META.init([PacketMetadata::EMPTY; 1]);
    let rx_buf = RX_BUF.init([0u8; UDP_RX_BUF_SIZE]);
    let tx_buf = TX_BUF.init([0u8; 1]);

    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);

    socket.bind(ARTNET_PORT).unwrap();

    info!("Art-Net receiver started");

    let mut packet = [0u8; 600];

    let mut universe_counts = [0u32; MAX_UNIVERSES];
    let mut universe_last_values = [0u8; MAX_UNIVERSES];
    let mut universe_changes = [0u32; MAX_UNIVERSES];

    let mut last_sequence = [0u8; MAX_UNIVERSES];
    let mut sequence_jumps = [0u32; MAX_UNIVERSES];

    let mut last_print = Instant::now();

    loop {
        match socket.recv_from(&mut packet).await {
            Ok((len, _endpoint)) => {
                if len < 18 {
                    continue;
                }

                if &packet[0..8] != ARTNET_HEADER {
                    continue;
                }

                if packet[8] != OP_OUTPUT_LO || packet[9] != OP_OUTPUT_HI {
                    continue;
                }

                let sequence = packet[12];
                let universe = u16::from_le_bytes([packet[14], packet[15]]) as usize;
                let dmx_len = u16::from_be_bytes([packet[16], packet[17]]) as usize;

                if universe >= MAX_UNIVERSES {
                    continue;
                }

                let available = len.saturating_sub(18);
                let copy_len = dmx_len.min(available).min(512);

                if copy_len == 0 {
                    continue;
                }

                let first_value = packet[18];

                if sequence != 0 && last_sequence[universe] != 0 {
                    let expected = last_sequence[universe].wrapping_add(1);

                    if sequence != expected {
                        sequence_jumps[universe] = sequence_jumps[universe].wrapping_add(1);
                    }
                }

                if sequence != 0 {
                    last_sequence[universe] = sequence;
                }

                DMX_MATRIX.lock(|matrix| {
                    let mut matrix = matrix.borrow_mut();

                    matrix[universe][0..copy_len]
                        .copy_from_slice(&packet[18..18 + copy_len]);
                });

                universe_counts[universe] = universe_counts[universe].wrapping_add(1);

                if first_value != universe_last_values[universe] {
                    universe_last_values[universe] = first_value;
                    universe_changes[universe] = universe_changes[universe].wrapping_add(1);
                }

                if last_print.elapsed().as_millis() >= 1000 {
                    if DEBUG_PIXEL_UNIVERSE < MAX_UNIVERSES {
                        info!(
                            "pps u0={} upix={}",
                            universe_counts[0],
                            universe_counts[DEBUG_PIXEL_UNIVERSE],
                        );

                        info!(
                            "chg u0={} upix={} vals u0={} upix={}",
                            universe_changes[0],
                            universe_changes[DEBUG_PIXEL_UNIVERSE],
                            universe_last_values[0],
                            universe_last_values[DEBUG_PIXEL_UNIVERSE],
                        );

                        info!(
                            "seqjump u0={} upix={}",
                            sequence_jumps[0],
                            sequence_jumps[DEBUG_PIXEL_UNIVERSE],
                        );
                    } else {
                        warn!(
                            "DEBUG_PIXEL_UNIVERSE {} >= MAX_UNIVERSES {}",
                            DEBUG_PIXEL_UNIVERSE,
                            MAX_UNIVERSES,
                        );
                    }

                    for i in 0..MAX_UNIVERSES {
                        universe_counts[i] = 0;
                        universe_changes[i] = 0;
                        sequence_jumps[i] = 0;
                    }

                    last_print = Instant::now();
                }
            }

            Err(e) => {
                warn!("Art-Net UDP receive error: {:?}", e);
            }
        }
    }
}