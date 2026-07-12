#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

mod config;
mod hardware;
mod modules;

mod periphs {
    pub mod dmx;
    pub mod eth;
    pub mod artnet;
}

use core::cell::RefCell;
use core::future::pending;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::once_lock::OnceLock;

use config::*;
use modules::*;


use crate::hardware::AssignedResources;
use crate::hardware::*;




// Global config
pub static CONFIG: OnceLock<BoardInstanceConfig> = OnceLock::new();

// Global DMX buffer
pub const MAX_UNIVERSES: usize = 4;
pub const MAX_PIXELS: usize = 300;

pub static DMX_MATRIX: Mutex<CriticalSectionRawMutex, RefCell<[[u8; 512]; MAX_UNIVERSES]>> =
    Mutex::new(RefCell::new([[0u8; 512]; MAX_UNIVERSES]));



#[embassy_executor::main]
async fn main(spawner: Spawner) {


    info!("=======================================");
    info!("");
    info!("     ProjectDMX Controller Booting     ");
    info!("              Version 0.1r             ");
    info!("");



    // Embassy init
    let hardware_config = embassy_rp::config::Config::default();
    let p = embassy_rp::init(hardware_config);



    // Manage pins and peripherals
    let r = split_resources!(p);



    // JSONC Configuration
    let config = load_config();
    CONFIG.init(config).unwrap();



    // Spawn Peripherals
    spawner.spawn(periphs::dmx::dmx_task(r.dmx)).unwrap(); // DMX

    if config.input.source == InputProtocol::Artnet {
        let _stack = periphs::eth::start_eth(&spawner, r.eth).await; // Ethernet
        spawner.spawn(periphs::artnet::artnet_task(_stack)).unwrap(); // ArtNet
    }



    // Module Initialization
    init_slot_a(&spawner, config.modules.slot_a, r.slot_a_relay);
    init_slot_b(&spawner, config.modules.slot_b, r.slot_b_unused);
    init_slot_c(&spawner, config.modules.slot_c, r.slot_c_neo);
    init_slot_d(&spawner, config.modules.slot_d, r.slot_d_dimmer);



    pending::<()>().await;


}








/**
 * Reads a slice of DMX channel values from the DMX_MATRIX for a given universe and starting channel.
 */
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