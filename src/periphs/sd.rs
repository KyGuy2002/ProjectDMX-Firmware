use defmt::println;

// For SPI
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;

// For CS Pin
use embassy_rp::gpio::{Level, Output};

// For SdCard
use embedded_sdmmc::{SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};

use crate::hardware::SdResources;


// Dummy Clock structure for embedded-sdmmc
struct DummyClock;
impl embedded_sdmmc::TimeSource for DummyClock {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp::from_calendar(2026, 1, 1, 0, 0, 0).unwrap()
    }
}


#[embassy_executor::task]
pub async fn sd_task(r: SdResources) {
    println!("SD task started.");

    let cs_pin = Output::new(r.cs, Level::High);

    let mut config = spi::Config::default();
    config.frequency = 400_000;

    let spi_bus = Spi::new_blocking(r.spi, r.sck, r.mosi, r.miso, config);

    let spi_device =
    ExclusiveDevice::new(spi_bus, cs_pin, Delay).expect("Failed to get exclusive device");

    let sdcard = SdCard::new(spi_device, Delay);

    println!("Init SD card controller and retrieve card size...");
    let sd_size = sdcard.num_bytes().expect("failed to get sdcard size");
    println!("card size is {} bytes", sd_size);


    let volume_mgr = VolumeManager::new(sdcard, DummyClock);
    let volume0 = volume_mgr
        .open_volume(VolumeIdx(0))
        .expect("failed to open volume");

    let root_dir = volume0.open_root_dir().expect("failed to open root dir");

    // let my_file = root_dir
    // .open_file_in_dir("hello.txt", embedded_sdmmc::Mode::ReadOnly)
    // .expect("failed to open hello.txt file");

    // while !my_file.is_eof() {
    //     let mut buffer = [0u8; 32];

    //     if let Ok(n) = my_file.read(&mut buffer) {
    //         if let Ok(s) = core::str::from_utf8(&buffer[..n]) {
    //             println!("{}", s);
    //         } else {
    //             println!("{:02x}", &buffer[..n]);
    //         }
    //     }
    // }

}