#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

mod config;
mod hardware;
mod modules;

mod periphs {
    pub mod artnet;
    pub mod dmx;
    pub mod eth;
}

use core::cell::RefCell;
use core::future::pending;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::once_lock::OnceLock;

use config::{load_config, BoardInstanceConfig};
use hardware::PcbLayout;
use modules::init_slot;

pub static CONFIG: OnceLock<BoardInstanceConfig> = OnceLock::new();

pub const MAX_UNIVERSES: usize = 4;

pub static DMX_MATRIX: Mutex<CriticalSectionRawMutex, RefCell<[[u8; 512]; MAX_UNIVERSES]>> =
    Mutex::new(RefCell::new([[0u8; 512]; MAX_UNIVERSES]));

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("ProjectDMX Controller Booting");

    let hardware_config = embassy_rp::config::Config::default();
    let p = embassy_rp::init(hardware_config);

    let (pcb, uart1, dmx_tx_pin, dmx_rx_pin, dma_ch1, dma_ch2, spi1, dma_ch3, dma_ch4) =
        PcbLayout::new(p);

    let config = load_config();
    CONFIG.init(config).unwrap();

    let stack = periphs::eth::start_eth(&spawner, spi1, pcb.ethernet, dma_ch3, dma_ch4).await;

    spawner
        .spawn(periphs::artnet::artnet_task(stack))
        .unwrap();

    spawner
        .spawn(periphs::dmx::dmx_task(
            uart1,
            dmx_tx_pin,
            dmx_rx_pin,
            pcb.dmx,
            dma_ch1,
            dma_ch2,
        ))
        .unwrap();

    init_slot(&spawner, config.modules.slot_a, pcb.slot_a);
    init_slot(&spawner, config.modules.slot_b, pcb.slot_b);
    init_slot(&spawner, config.modules.slot_c, pcb.slot_c);
    init_slot(&spawner, config.modules.slot_d, pcb.slot_d);

    pending::<()>().await;
}

pub fn read_channels<const N: usize>(universe: usize, start_channel: usize) -> [u8; N] {
    DMX_MATRIX.lock(|matrix| {
        let mut dest = [0u8; N];

        if universe < MAX_UNIVERSES && start_channel < 512 {
            let buf = matrix.borrow();
            let universe_row: &[u8] = &buf[universe];

            let end = (start_channel + N).min(512);
            let src_slice = &universe_row[start_channel..end];

            dest[..src_slice.len()].copy_from_slice(src_slice);
        }

        dest
    })
}