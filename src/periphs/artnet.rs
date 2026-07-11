use defmt::{info, warn};

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::Stack;
use static_cell::StaticCell;

use crate::{DMX_MATRIX, MAX_UNIVERSES};

const ARTNET_PORT: u16 = 6454;
const ARTNET_HEADER: &[u8; 8] = b"Art-Net\0";
const OP_OUTPUT_LO: u8 = 0x00;
const OP_OUTPUT_HI: u8 = 0x50;

#[embassy_executor::task]
pub async fn artnet_task(stack: Stack<'static>) -> ! {
    static RX_META: StaticCell<[PacketMetadata; 8]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 1]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 2048]> = StaticCell::new();
    static TX_BUF: StaticCell<[u8; 1]> = StaticCell::new();

    let rx_meta = RX_META.init([PacketMetadata::EMPTY; 8]);
    let tx_meta = TX_META.init([PacketMetadata::EMPTY; 1]);
    let rx_buf = RX_BUF.init([0u8; 2048]);
    let tx_buf = TX_BUF.init([0u8; 1]);

    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);

    socket.bind(ARTNET_PORT).unwrap();

    info!("Art-Net receiver started");

    let mut packet = [0u8; 600];

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

                let universe = u16::from_le_bytes([packet[14], packet[15]]) as usize;
                let dmx_len = u16::from_be_bytes([packet[16], packet[17]]) as usize;

                if universe >= MAX_UNIVERSES {
                    continue;
                }

                let available = len.saturating_sub(18);
                let copy_len = dmx_len.min(available).min(512);

                DMX_MATRIX.lock(|matrix| {
                    let mut matrix = matrix.borrow_mut();

                    matrix[universe].fill(0);
                    matrix[universe][0..copy_len].copy_from_slice(&packet[18..18 + copy_len]);
                });
            }

            Err(e) => {
                warn!("Art-Net UDP receive error: {:?}", e);
            }
        }
    }
}