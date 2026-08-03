use defmt::println;

// For SPI
use embassy_rp::spi;
use embassy_rp::spi::Spi;
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;

// For CS Pin
use embassy_rp::gpio::{Level, Output};

// For SdCard
use embedded_sdmmc::{Mode, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};

// For I2S audio output
use embassy_futures::yield_now;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};

use crate::hardware::{AudioIrqs, AudioResources, SdResources};

// Path (relative to the SD card root) of the MP3 file to play on boot
const MP3_PATH: &str = "test.mp3";

// Default playback volume (0.0 - 1.0)
const DEFAULT_VOLUME: f32 = 0.5;

// MP3 bitstream read buffer. Topped back up from the SD card whenever it isn't full.
const MP3_BUF_SIZE: usize = 8 * 1024;


// Dummy Clock structure for embedded-sdmmc
struct DummyClock;
impl embedded_sdmmc::TimeSource for DummyClock {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp::from_calendar(2026, 1, 1, 0, 0, 0).unwrap()
    }
}


#[embassy_executor::task]
pub async fn sd_task(r: SdResources, audio: AudioResources) {
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

    // Card is initialized (had to be done at 400kHz) - bump the SPI clock up for data transfer
    sdcard.spi(|dev| dev.bus_mut().set_frequency(16_000_000));


    let volume_mgr = VolumeManager::new(sdcard, DummyClock);
    let volume0 = volume_mgr
        .open_volume(VolumeIdx(0))
        .expect("failed to open volume");

    let root_dir = volume0.open_root_dir().expect("failed to open root dir");

    let mp3_file = root_dir
        .open_file_in_dir(MP3_PATH, Mode::ReadOnly)
        .expect("failed to open mp3 file");

    println!("Opened {} - starting playback", MP3_PATH);

    // Load the PIO I2S output program (pin/DMA driven, matches AUDIO_DIN/BCK/LCK pins)
    let Pio { mut common, sm0, .. } = Pio::new(audio.pio, AudioIrqs);
    let i2s_program = PioI2sOutProgram::new(&mut common);

    let mut decoder = Decoder::new();
    let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];
    let mut out = [0u32; MAX_SAMPLES_PER_FRAME / 2];

    let mut mp3_buf = [0u8; MP3_BUF_SIZE];
    let mut buf_len = 0usize;
    let mut eof = false;

    // Prime the decoder: read/decode until the first real MP3 frame is found so we
    // know the sample rate (bit depth is always 16 for our I2S output).
    let sample_rate = loop {
        if !eof && buf_len < MP3_BUF_SIZE {
            match mp3_file.read(&mut mp3_buf[buf_len..]) {
                Ok(0) => eof = true,
                Ok(n) => buf_len += n,
                Err(_) => eof = true,
            }
        }

        if buf_len == 0 {
            panic!("no valid MP3 frames found in {}", MP3_PATH);
        }

        let (mut consumed, info) = decoder.decode(&mp3_buf[..buf_len], &mut pcm);

        if consumed == 0 && info.is_none() {
            if eof || buf_len >= MP3_BUF_SIZE {
                // Buffer is as full as it'll get and still no valid frame - skip a byte to resync
                consumed = 1;
            } else {
                // Not enough data buffered yet to decide - wait for more
                yield_now().await;
                continue;
            }
        }

        mp3_buf.copy_within(consumed..buf_len, 0);
        buf_len -= consumed;

        if let Some(info) = info {
            break info.sample_rate;
        }

        yield_now().await;
    };

    println!("Starting I2S output at {} Hz", sample_rate);
    let mut i2s = PioI2sOut::new(
        &mut common,
        sm0,
        audio.dma,
        audio.din,
        audio.bck,
        audio.lck,
        sample_rate,
        16,
        &i2s_program,
    );

    loop {
        // Top up the read buffer from the SD card
        if !eof && buf_len < MP3_BUF_SIZE {
            match mp3_file.read(&mut mp3_buf[buf_len..]) {
                Ok(0) => eof = true,
                Ok(n) => buf_len += n,
                Err(_) => eof = true,
            }
        }

        if buf_len == 0 {
            if eof {
                // End of file reached - loop the track from the start
                println!("Playback complete, restarting {}", MP3_PATH);
                mp3_file.seek_from_start(0).expect("failed to rewind mp3 file");
                eof = false;
            }

            yield_now().await;
            continue;
        }

        let (mut consumed, info) = decoder.decode(&mp3_buf[..buf_len], &mut pcm);

        if consumed == 0 && info.is_none() {
            if eof || buf_len >= MP3_BUF_SIZE {
                // Buffer is as full as it'll get and still no valid frame - skip a byte to resync
                consumed = 1;
            } else {
                // Not enough data buffered yet to decide - wait for more
                yield_now().await;
                continue;
            }
        }

        mp3_buf.copy_within(consumed..buf_len, 0);
        buf_len -= consumed;

        if let Some(info) = info {
            let channels = info.channels.num() as usize;
            let frame_count = info.samples_produced;

            for i in 0..frame_count {
                let l = pcm[i * channels];
                let r = if channels > 1 { pcm[i * channels + 1] } else { l };

                let l16 = (l * DEFAULT_VOLUME * 32767.0) as i32 as i16 as u16;
                let r16 = (r * DEFAULT_VOLUME * 32767.0) as i32 as i16 as u16;

                out[i] = ((l16 as u32) << 16) | (r16 as u32);
            }

            i2s.write(&out[..frame_count]).await;
        }
    }
}