#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

mod config;
mod hardware;
mod modules;
mod pixel_mapping_config;

mod periphs {
    pub mod dmx;
    pub mod eth;
    pub mod artnet;
    pub mod sacn;
    pub mod oled;
    pub mod sensors;
    pub mod tcp_cmds;
    pub mod sd;
    pub mod audio;
}

use core::cell::RefCell;
use core::future::pending;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex as AsyncMutex;
use embassy_sync::once_lock::OnceLock;
use embassy_net::Ipv4Address;
use static_cell::StaticCell;

use config::*;
use modules::*;


use crate::hardware::AssignedResources;
use crate::hardware::*;




// Global config
pub static CONFIG: OnceLock<BoardInstanceConfig> = OnceLock::new();

// Global DMX buffer
pub const MAX_UNIVERSES: usize = 4;
pub const MAX_PIXELS: usize = 80;

pub static DMX_MATRIX: BlockingMutex<CriticalSectionRawMutex, RefCell<[[u8; 512]; MAX_UNIVERSES]>> =
    BlockingMutex::new(RefCell::new([[0u8; 512]; MAX_UNIVERSES]));

static IP_STATE: StaticCell<AsyncMutex<CriticalSectionRawMutex, Option<Ipv4Address>>> = StaticCell::new();


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
    CONFIG.init(config.clone()).unwrap();



    // Spawn Peripherals
    let ip_state = IP_STATE.init(AsyncMutex::new(None));
    spawner.spawn(periphs::oled::oled_task(r.oled, ip_state)).unwrap(); // OLED
    spawner.spawn(periphs::dmx::dmx_task(r.dmx)).unwrap(); // DMX

    let sd_handle = periphs::sd::init(r.sd); // Mount SD card
    spawner.spawn(periphs::audio::audio_task(config.audio, r.audio, sd_handle)).unwrap(); // DMX-triggered audio playback

    spawner.spawn(modules::ask433::ask433_task(r.slot_b_ask433)).unwrap(); // 433MHz receiver test


    if config.input.source == InputProtocol::Artnet || config.input.source == InputProtocol::sACN {
        let stack = periphs::eth::start_eth(&spawner, r.eth, ip_state).await; // Ethernet
        periphs::sensors::start_sensors(&spawner, r.sensors); // Sensors
        spawner.spawn(periphs::tcp_cmds::tcp_cmds_task(stack)).unwrap(); // TCP Commands

        if config.input.source == InputProtocol::Artnet {
            spawner.spawn(periphs::artnet::artnet_task(stack)).unwrap(); // Art-Net
        } else if config.input.source == InputProtocol::sACN {
            spawner.spawn(periphs::sacn::sacn_task(stack)).unwrap(); // sACN
        }
    }



    // Module Initialization
    // init_slot_a(&spawner, config.modules.slot_a, r.slot_a_relay);
    // init_slot_b(&spawner, config.modules.slot_b, r.slot_b_unused);
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