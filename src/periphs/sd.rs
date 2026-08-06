use defmt::println;

// For SPI
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;

// For CS Pin
use embassy_rp::gpio::{Level, Output};

// For SdCard
use embedded_sdmmc::{Error, File, Mode, RawDirectory, SdCard, SdCardError, VolumeIdx, VolumeManager};

use static_cell::StaticCell;

use crate::hardware::SdResources;

// Bumped above the crate default (4) so multiple consumers (e.g. several audio
// voices) can each hold their own open file concurrently.
pub const MAX_OPEN_FILES: usize = 16;

type SpiBus = Spi<'static, embassy_rp::peripherals::SPI0, spi::Blocking>;
type SdCardType = SdCard<ExclusiveDevice<SpiBus, Output<'static>, Delay>, Delay>;
pub type SdVolumeManager = VolumeManager<SdCardType, DummyClock, 4, MAX_OPEN_FILES, 1>;
pub type SdFile<'a> = File<'a, SdCardType, DummyClock, 4, MAX_OPEN_FILES, 1>;
pub type SdError = Error<SdCardError>;

// Dummy Clock structure for embedded-sdmmc
pub struct DummyClock;
impl embedded_sdmmc::TimeSource for DummyClock {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp::from_calendar(2026, 1, 1, 0, 0, 0).unwrap()
    }
}

/// Shared handle: the mounted volume manager plus a standing handle to volume 0's
/// root directory (opened once and kept open for the program's lifetime - MAX_VOLUMES
/// is only 1, so re-opening/closing it per file would be wasteful and, since `Volume`/
/// `Directory` close on drop and their methods tie the returned `File`'s lifetime to
/// their own local borrow, awkward to hand out as a `'static` file anyway).
#[derive(Clone, Copy)]
pub struct SdHandle {
    pub mgr: &'static SdVolumeManager,
    root_dir: RawDirectory,
}

static VOLUME_MGR: StaticCell<SdVolumeManager> = StaticCell::new();

/// Mounts the SD card and returns a shared handle. Call once at boot; any number of
/// tasks can then use the returned handle to open files concurrently (embedded-sdmmc's
/// VolumeManager is internally RefCell-guarded, which is safe here since nothing holds
/// a borrow across an `.await` point).
pub fn init(r: SdResources) -> SdHandle {
    println!("Mounting SD card...");

    let cs_pin = Output::new(r.cs, Level::High);

    let mut config = spi::Config::default();
    config.frequency = 400_000;

    let spi_bus = Spi::new_blocking(r.spi, r.sck, r.mosi, r.miso, config);

    let spi_device =
        ExclusiveDevice::new(spi_bus, cs_pin, Delay).expect("Failed to get exclusive device");

    let sdcard = SdCardType::new(spi_device, Delay);

    let sd_size = sdcard.num_bytes().expect("failed to get sdcard size");
    println!("SD card size is {} bytes", sd_size);

    // Card is initialized (had to be done at 400kHz) - bump the SPI clock up for data transfer
    sdcard.spi(|dev| dev.bus_mut().set_frequency(16_000_000));

    let volume_mgr = SdVolumeManager::new_with_limits(sdcard, DummyClock, 0);
    let mgr: &'static SdVolumeManager = VOLUME_MGR.init(volume_mgr);

    let raw_volume = mgr.open_raw_volume(VolumeIdx(0)).expect("failed to open volume");
    let root_dir = mgr.open_root_dir(raw_volume).expect("failed to open root dir");

    SdHandle { mgr, root_dir }
}

/// Opens `name` in the SD card's root directory, read-only.
pub fn open_file(handle: SdHandle, name: &str, mode: Mode) -> Result<SdFile<'static>, SdError> {
    let raw_file = handle.mgr.open_file_in_dir(handle.root_dir, name, mode)?;
    Ok(raw_file.to_file(handle.mgr))
}
