use defmt::println;

// For SPI
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_time::{Delay, Instant};
use embedded_hal_bus::spi::ExclusiveDevice;

// For CS Pin
use embassy_rp::gpio::{Level, Output};

// For SdCard
use embedded_sdmmc::{
    Error, File, LfnBuffer, Mode, RawDirectory, SdCard, SdCardError, ShortFileName, VolumeIdx,
    VolumeManager,
};

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

    // Card is initialized (had to be done at 400kHz) - bump the SPI clock up for data transfer.
    // A single sector read is the one truly non-yieldable blocking chunk in the
    // whole audio pipeline (read_yielding yields between sectors, never during
    // one), so a faster clock directly caps how long that worst case can run.
    sdcard.spi(|dev| dev.bus_mut().set_frequency(24_000_000));

    let volume_mgr = SdVolumeManager::new_with_limits(sdcard, DummyClock, 0);
    let mgr: &'static SdVolumeManager = VOLUME_MGR.init(volume_mgr);

    let raw_volume = mgr.open_raw_volume(VolumeIdx(0)).expect("failed to open volume");
    let root_dir = mgr.open_root_dir(raw_volume).expect("failed to open root dir");

    SdHandle { mgr, root_dir }
}

/// Opens `name` in the SD card's root directory, read-only.
pub fn open_file(handle: SdHandle, name: &str, mode: Mode) -> Result<SdFile<'static>, SdError> {
    let mut lfn_storage = [0u8; 256];
    let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
    let mut short_name: Option<ShortFileName> = None;

    handle
        .mgr
        .iterate_dir_lfn(handle.root_dir, &mut lfn_buffer, |entry, long_name| {
            if long_name.is_some_and(|candidate| candidate.eq_ignore_ascii_case(name)) {
                short_name = Some(entry.name.clone());
            }
        })?;

    let raw_file = match short_name {
        Some(short_name) => handle
            .mgr
            .open_file_in_dir(handle.root_dir, short_name, mode)?,
        None => handle.mgr.open_file_in_dir(handle.root_dir, name, mode)?,
    };
    Ok(raw_file.to_file(handle.mgr))
}

/// Reads into `buffer` one 512-byte SD sector at a time, yielding to the
/// executor after every sector. `embedded-sdmmc`'s read is synchronous and a
/// large transfer can otherwise block this thread's executor for several
/// milliseconds straight - long enough to starve `Timer::after_millis()`
/// wakeups on other tasks (e.g. NeoPixel animation). Yielding once per sector
/// caps the worst-case stall to a single-sector transfer instead of the whole
/// buffer.
///
/// `_handle` is accepted for signature symmetry with the rest of this module;
/// the read itself goes through `file`, which already carries its own
/// `&SdVolumeManager` reference.
///
/// Returns the total bytes read, which is less than `buffer.len()` at EOF.
pub async fn read_yielding(
    _handle: SdHandle,
    file: &mut SdFile<'static>,
    buffer: &mut [u8],
) -> Result<usize, SdError> {
    let start = Instant::now(); // DIAG: remove after measuring SD stall duration
    let mut total_read = 0;

    for chunk in buffer.chunks_mut(512) {
        let n = file.read(chunk)?;
        total_read += n;

        embassy_futures::yield_now().await;

        if n == 0 {
            break;
        }
    }

    // DIAG: remove after measuring. Total wall time (spanning all the yields)
    // that this read held the AUDIO_EXECUTOR interrupt busy for.
    let elapsed_ms = (Instant::now() - start).as_millis();
    if elapsed_ms > 1 {
        // println!("DIAG read_yielding: {} bytes in {}ms", total_read, elapsed_ms);
    }

    Ok(total_read)
}
