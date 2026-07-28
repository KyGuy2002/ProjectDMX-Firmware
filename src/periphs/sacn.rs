use defmt::{info, warn};

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Stack};
use embassy_time::Instant;
use static_cell::StaticCell;

use crate::{DMX_MATRIX, MAX_UNIVERSES};

const SACN_PORT: u16 = 5568;
const ACN_PACKET_IDENTIFIER: &[u8; 12] = b"ASC-E1.17\0\0\0";

// Keep this small for live lighting.
// Too large creates old-packet backlog.
const UDP_RX_PACKET_COUNT: usize = 8;
const UDP_RX_BUF_SIZE: usize = UDP_RX_PACKET_COUNT * 640;

// sACN universes are one-based.
// Universe 4 is stored in DMX_MATRIX[4].
const DEBUG_PIXEL_UNIVERSE: usize = 4;

// E1.31 packet offsets.
const ROOT_VECTOR_OFFSET: usize = 18;
const FRAME_VECTOR_OFFSET: usize = 40;
const PRIORITY_OFFSET: usize = 108;
const SEQUENCE_OFFSET: usize = 111;
const OPTIONS_OFFSET: usize = 112;
const UNIVERSE_OFFSET: usize = 113;
const DMP_VECTOR_OFFSET: usize = 117;
const DMP_TYPE_OFFSET: usize = 118;
const FIRST_ADDRESS_OFFSET: usize = 119;
const ADDRESS_INCREMENT_OFFSET: usize = 121;
const PROPERTY_COUNT_OFFSET: usize = 123;
const START_CODE_OFFSET: usize = 125;
const DMX_DATA_OFFSET: usize = 126;

// E1.31 vectors.
const VECTOR_ROOT_E131_DATA: u32 = 0x0000_0004;
const VECTOR_E131_DATA_PACKET: u32 = 0x0000_0002;
const VECTOR_DMP_SET_PROPERTY: u8 = 0x02;
const DMP_ADDRESS_DATA_TYPE: u8 = 0xa1;

// E1.31 option flags.
const OPTION_PREVIEW_DATA: u8 = 0x80;
const OPTION_STREAM_TERMINATED: u8 = 0x40;

struct SacnPacket<'a> {
    universe: usize,
    sequence: u8,
    priority: u8,
    preview: bool,
    terminated: bool,
    dmx: &'a [u8],
}

fn read_u16_be(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn sacn_multicast_address(universe: usize) -> Ipv4Address {
    Ipv4Address::new(
        239,
        255,
        ((universe >> 8) & 0xff) as u8,
        (universe & 0xff) as u8,
    )
}

fn parse_sacn_packet(packet: &[u8]) -> Option<SacnPacket<'_>> {
    if packet.len() < DMX_DATA_OFFSET {
        return None;
    }

    // ACN preamble size.
    if packet[0] != 0x00 || packet[1] != 0x10 {
        return None;
    }

    // ACN post-amble size.
    if packet[2] != 0x00 || packet[3] != 0x00 {
        return None;
    }

    if &packet[4..16] != ACN_PACKET_IDENTIFIER {
        return None;
    }

    if read_u32_be(packet, ROOT_VECTOR_OFFSET) != VECTOR_ROOT_E131_DATA {
        return None;
    }

    if read_u32_be(packet, FRAME_VECTOR_OFFSET) != VECTOR_E131_DATA_PACKET {
        return None;
    }

    if packet[DMP_VECTOR_OFFSET] != VECTOR_DMP_SET_PROPERTY {
        return None;
    }

    if packet[DMP_TYPE_OFFSET] != DMP_ADDRESS_DATA_TYPE {
        return None;
    }

    // First property address must be zero.
    if packet[FIRST_ADDRESS_OFFSET] != 0x00
        || packet[FIRST_ADDRESS_OFFSET + 1] != 0x00
    {
        return None;
    }

    // Address increment must be one.
    if packet[ADDRESS_INCREMENT_OFFSET] != 0x00
        || packet[ADDRESS_INCREMENT_OFFSET + 1] != 0x01
    {
        return None;
    }

    let universe = read_u16_be(packet, UNIVERSE_OFFSET) as usize;

    // Valid sACN universes are 1 through 63999.
    if universe == 0 || universe > 63_999 {
        return None;
    }

    let property_count = read_u16_be(packet, PROPERTY_COUNT_OFFSET) as usize;

    // Property count includes the start code.
    if property_count < 2 || property_count > 513 {
        return None;
    }

    let required_len = START_CODE_OFFSET.checked_add(property_count)?;

    if packet.len() < required_len {
        return None;
    }

    // Only standard DMX start code is handled.
    if packet[START_CODE_OFFSET] != 0 {
        return None;
    }

    let dmx_len = (property_count - 1).min(512);
    let dmx_end = DMX_DATA_OFFSET.checked_add(dmx_len)?;

    if dmx_end > packet.len() {
        return None;
    }

    let options = packet[OPTIONS_OFFSET];

    Some(SacnPacket {
        universe,
        sequence: packet[SEQUENCE_OFFSET],
        priority: packet[PRIORITY_OFFSET],
        preview: options & OPTION_PREVIEW_DATA != 0,
        terminated: options & OPTION_STREAM_TERMINATED != 0,
        dmx: &packet[DMX_DATA_OFFSET..dmx_end],
    })
}

#[embassy_executor::task]
pub async fn sacn_task(stack: Stack<'static>) -> ! {
    static RX_META: StaticCell<[PacketMetadata; UDP_RX_PACKET_COUNT]> =
        StaticCell::new();

    static TX_META: StaticCell<[PacketMetadata; 1]> =
        StaticCell::new();

    static RX_BUF: StaticCell<[u8; UDP_RX_BUF_SIZE]> =
        StaticCell::new();

    static TX_BUF: StaticCell<[u8; 1]> =
        StaticCell::new();

    let rx_meta =
        RX_META.init([PacketMetadata::EMPTY; UDP_RX_PACKET_COUNT]);

    let tx_meta =
        TX_META.init([PacketMetadata::EMPTY; 1]);

    let rx_buf =
        RX_BUF.init([0u8; UDP_RX_BUF_SIZE]);

    let tx_buf =
        TX_BUF.init([0u8; 1]);

    let mut socket =
        UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);

    socket.bind(SACN_PORT).unwrap();

    // Join the multicast address for every firmware universe.
    //
    // Universe zero is invalid in sACN, so begin at one.
    for universe in 1..MAX_UNIVERSES {
        let multicast_address = sacn_multicast_address(universe);

        match stack.join_multicast_group(multicast_address) {
            Ok(()) => {
                info!("Joined sACN universe {}", universe);
            }

            Err(e) => {
                warn!(
                    "Failed to join sACN universe {}: {:?}",
                    universe,
                    e
                );
            }
        }
    }

    info!("sACN receiver started");

    let mut packet = [0u8; 640];

    let mut universe_counts = [0u32; MAX_UNIVERSES];
    let mut universe_last_values = [0u8; MAX_UNIVERSES];
    let mut universe_changes = [0u32; MAX_UNIVERSES];

    let mut last_sequence = [0u8; MAX_UNIVERSES];
    let mut sequence_jumps = [0u32; MAX_UNIVERSES];

    let mut universe_priority = [0u8; MAX_UNIVERSES];

    let mut last_print = Instant::now();

    loop {
        match socket.recv_from(&mut packet).await {
            Ok((len, _endpoint)) => {
                let Some(sacn) = parse_sacn_packet(&packet[..len]) else {
                    continue;
                };

                if sacn.universe >= MAX_UNIVERSES {
                    continue;
                }

                // Preview packets should not drive live output.
                if sacn.preview {
                    continue;
                }

                // A terminated stream is no longer active.
                if sacn.terminated {
                    universe_priority[sacn.universe] = 0;
                    last_sequence[sacn.universe] = 0;

                    DMX_MATRIX.lock(|matrix| {
                        let mut matrix = matrix.borrow_mut();
                        matrix[sacn.universe].fill(0);
                    });

                    continue;
                }

                // Ignore a source with lower priority than the currently
                // accepted stream for this universe.
                if sacn.priority < universe_priority[sacn.universe] {
                    continue;
                }

                // Reset sequence tracking when a higher-priority stream wins.
                if sacn.priority > universe_priority[sacn.universe] {
                    universe_priority[sacn.universe] = sacn.priority;
                    last_sequence[sacn.universe] = 0;
                }

                if last_sequence[sacn.universe] != 0 {
                    let expected =
                        last_sequence[sacn.universe].wrapping_add(1);

                    if sacn.sequence != expected {
                        sequence_jumps[sacn.universe] =
                            sequence_jumps[sacn.universe].wrapping_add(1);
                    }
                }

                last_sequence[sacn.universe] = sacn.sequence;

                let copy_len = sacn.dmx.len();
                let first_value = sacn.dmx[0];

                DMX_MATRIX.lock(|matrix| {
                    let mut matrix = matrix.borrow_mut();

                    matrix[sacn.universe][0..copy_len]
                        .copy_from_slice(sacn.dmx);

                    // Clear old channel values if a shorter packet arrives.
                    if copy_len < 512 {
                        matrix[sacn.universe][copy_len..512].fill(0);
                    }
                });

                universe_counts[sacn.universe] =
                    universe_counts[sacn.universe].wrapping_add(1);

                if first_value != universe_last_values[sacn.universe] {
                    universe_last_values[sacn.universe] = first_value;

                    universe_changes[sacn.universe] =
                        universe_changes[sacn.universe].wrapping_add(1);
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
                warn!("sACN UDP receive error: {:?}", e);
            }
        }
    }
}