#![no_std]
#![no_main]

// Debugging
use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

mod config;
mod hardware;
mod modules;
mod periphs {
    mod dmx;
}

// Types
use embassy_executor::Spawner;
use embassy_sync::once_lock::OnceLock;
use types::{BoardInstanceConfig, ModuleSlot};
use hardware::PcbLayout;

// Global config
pub static CONFIG: OnceLock<BoardInstanceConfig> = OnceLock::new();

// Global DMX buffer
const MAX_UNIVERSES: usize = 4;
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

    // Pins
    let pcb = PcbLayout::new(p);

    // JSONC Configuration
    CONFIG.init(load_config()).unwrap();

    // Peripheral Initialization
    spawner.spawn(tasks::dmx::dmx_task(p.UART1, pcb.dmx, p.DMA_CH1)).unwrap();

    // Module Initialization
    init_slot(&spawner, CONFIG.get().unwrap().modules.slot_a, pcb.slot_a);
    init_slot(&spawner, CONFIG.get().unwrap().modules.slot_b, pcb.slot_b);
    init_slot(&spawner, CONFIG.get().unwrap().modules.slot_c, pcb.slot_c);
    init_slot(&spawner, CONFIG.get().unwrap().modules.slot_d, pcb.slot_d);


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