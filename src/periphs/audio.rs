use defmt::println;

use embassy_futures::yield_now;
use embassy_rp::pac;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Instant;
use embedded_sdmmc::Mode;
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};
use static_cell::StaticCell;

use crate::config::AudioConfig;
use crate::hardware::{AudioIrqs, AudioResources, SdResources};
use crate::periphs::sd::{self, SdFile, SdHandle};
use crate::read_channels;

// All source MP3s are expected at this rate - no resampling is done.
const AUDIO_SAMPLE_RATE: u32 = 44100;

// Playback volume, 0.0 - 1.0. Tweak here.
const VOLUME: f32 = 0.3;

// nanomp3 decodes interleaved; MAX_SAMPLES_PER_FRAME counts individual samples,
// so a mono frame is at most half that many.
const MAX_FRAME_SAMPLES: usize = MAX_SAMPLES_PER_FRAME / 2;

// Per-voice SD read buffer. Only topped up once it drops below the threshold so
// each SD transaction pulls a worthwhile chunk. Threshold stays well above one
// MP3 frame's worst-case (~1 KB) so decode is never starved.
const VOICE_MP3_BUF_SIZE: usize = 4 * 1024;
const VOICE_MP3_BUF_REFILL_THRESHOLD: usize = 2 * 1024;

// Frames per I2S DMA transfer / buffer. While one buffer plays (~FRAMES_PER_BATCH
// * 26 ms of DMA) the other is refilled, so this is the headroom `fill()` has to
// decode the next batch and service any SD read latency spike. 8 frames = ~209 ms
// per buffer, comfortably above the worst-case decode + SD stall. (Startup delay
// to first audio is dominated by the MP3s' own silent lead-in, not this.)
const FRAMES_PER_BATCH: usize = 8;
const OUT_BUF_LEN: usize = MAX_FRAME_SAMPLES * FRAMES_PER_BATCH;

type OutBuf = [u32; OUT_BUF_LEN];

// The three output buffers are handed back and forth between the decode task
// and the output task by transferring ownership of a `&'static mut OutBuf`
// through these channels, rather than sharing them behind a lock - the decode
// task gets one from EMPTY_CHANNEL, fills it, and posts it to FILLED_CHANNEL;
// the output task does the reverse. Capacity 3 matches the total buffer count,
// so neither channel can ever be asked to hold more in flight than exist.
//
// Three (not two) so decode can build up to a full extra buffer of lead time:
// with 3 audio voices plus oled/neo sharing the thread-mode executor, any
// single fill() cycle can occasionally run long (e.g. an oled flush landing
// mid-decode) - the third buffer absorbs that without the I2S side underrunning,
// as long as decode keeps up on average.
static BUF_A: StaticCell<OutBuf> = StaticCell::new();
static BUF_B: StaticCell<OutBuf> = StaticCell::new();
static BUF_C: StaticCell<OutBuf> = StaticCell::new();
static FILLED_CHANNEL: Channel<CriticalSectionRawMutex, &'static mut OutBuf, 3> = Channel::new();
static EMPTY_CHANNEL: Channel<CriticalSectionRawMutex, &'static mut OutBuf, 3> = Channel::new();

// PIO1 SM0 drives the I2S output. Its FDEBUG.TXSTALL bit latches whenever the
// state machine runs the TX FIFO dry waiting for the next DMA word - i.e. an
// audio underrun / audible glitch. Reading + clearing it each buffer turns
// "sounds like it stutters sometimes" into a hard count.
const I2S_SM_MASK: u8 = 0b0001;

fn i2s_underran() -> bool {
    pac::PIO1.fdebug().read().txstall() & I2S_SM_MASK != 0
}

fn clear_i2s_underrun() {
    pac::PIO1.fdebug().write(|w| w.set_txstall(I2S_SM_MASK));
}

#[derive(Clone, Copy, PartialEq)]
enum PlaybackMode {
    Once,
    Loop,
}

impl PlaybackMode {
    fn loops(self) -> bool {
        matches!(self, PlaybackMode::Loop)
    }
}

/// Maps a DMX value to `(file_index, mode)`:
/// - `0` => `None` (stop)
/// - `1..=128` => `(v - 1, Once)`
/// - `129..=255` => `(v - 129, Loop)`
/// - resolved index past the end of the list => `None` (stop)
fn decode_value(v: u8, num_files: usize) -> Option<(usize, PlaybackMode)> {
    if v == 0 {
        return None;
    }

    let (idx, mode) = if v <= 128 {
        ((v - 1) as usize, PlaybackMode::Once)
    } else {
        ((v - 129) as usize, PlaybackMode::Loop)
    };

    (idx < num_files).then_some((idx, mode))
}

/// Reads the first 10 bytes of an MP3 and, if they are an ID3v2 tag header,
/// returns the byte offset where the actual audio starts. Real songs routinely
/// carry tens of KB of ID3v2 metadata (embedded album art); seeking past it up
/// front keeps the decoder from grinding through all of it on every file open.
/// Returns 0 when there is no recognisable tag.
fn id3v2_data_start(file: &mut SdFile<'static>) -> u32 {
    let mut header = [0u8; 10];
    let mut read = 0;

    while read < header.len() {
        match file.read(&mut header[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => break,
        }
    }

    // "ID3" magic, and the 4 size bytes must be syncsafe (top bit clear).
    let is_id3 = read == header.len()
        && &header[..3] == b"ID3"
        && header[6] < 0x80
        && header[7] < 0x80
        && header[8] < 0x80
        && header[9] < 0x80;

    if !is_id3 {
        return 0;
    }

    let size = ((header[6] as u32) << 21)
        | ((header[7] as u32) << 14)
        | ((header[8] as u32) << 7)
        | (header[9] as u32);

    let mut total = 10 + size;
    if header[5] & 0x10 != 0 {
        total += 10; // optional footer
    }

    total
}

struct Voice {
    file_index: usize,
    mode: PlaybackMode,
    // Offset of the first audio byte (past any ID3v2 tag). Loop-rewinds seek here
    // rather than to 0 so the tag is only ever scanned past once.
    data_start: u32,
    // Set once a one-shot plays through to EOF. The voice is kept (silent) rather
    // than dropped, so reconcile() doesn't see "nothing playing" and restart it
    // every fill. Cleared only by selecting a different file (or 0).
    finished: bool,

    // Kept so reads can go through `sd::read_yielding` (needs a handle to hand
    // back to the caller's signature, even though the actual read goes through
    // `file`'s own volume-manager reference).
    handle: SdHandle,
    file: SdFile<'static>,
    decoder: Decoder,
    mp3_buf: [u8; VOICE_MP3_BUF_SIZE],
    buf_len: usize,

    // Mono samples decoded but not yet copied into the output buffer.
    carry: [f32; MAX_FRAME_SAMPLES],
    carry_len: usize,
    carry_pos: usize,
}

impl Voice {
    fn start(
        handle: SdHandle,
        file_index: usize,
        mode: PlaybackMode,
        filename: &str,
    ) -> Option<Voice> {
        let mut file = match sd::open_file(handle, filename, Mode::ReadOnly) {
            Ok(f) => f,
            Err(error) => {
                println!(
                    "Audio: failed to open {}: {:?}",
                    filename,
                    defmt::Debug2Format(&error)
                );
                return None;
            }
        };

        let data_start = id3v2_data_start(&mut file);
        if file.seek_from_start(data_start).is_err() {
            let _ = file.seek_from_start(0);
        }

        Some(Voice {
            file_index,
            mode,
            data_start,
            finished: false,
            handle,
            file,
            decoder: Decoder::new(),
            mp3_buf: [0u8; VOICE_MP3_BUF_SIZE],
            buf_len: 0,
            carry: [0f32; MAX_FRAME_SAMPLES],
            carry_len: 0,
            carry_pos: 0,
        })
    }

    /// Decodes the next MP3 frame into `self.carry` as mono (downmixing a stereo
    /// source). Loops back to the start of the file at EOF when `self.looping`.
    /// Returns `false` once there is genuinely no more audio (one-shot EOF).
    async fn decode_next_frame(&mut self, scratch: &mut [f32; MAX_SAMPLES_PER_FRAME]) -> bool {
        let mut eof = false;
        let mut rewound = false;

        loop {
            if !eof && self.buf_len < VOICE_MP3_BUF_REFILL_THRESHOLD {
                match sd::read_yielding(self.handle, &mut self.file, &mut self.mp3_buf[self.buf_len..]).await {
                    Ok(0) => eof = true,
                    Ok(n) => self.buf_len += n,
                    Err(_) => eof = true,
                }
            }

            if self.buf_len == 0 {
                if eof {
                    if self.mode.loops() && !rewound {
                        if self.file.seek_from_start(self.data_start).is_err() {
                            return false;
                        }
                        eof = false;
                        rewound = true;
                        continue;
                    }
                    return false;
                }
                continue;
            }

            let (mut consumed, info) = self.decoder.decode(&self.mp3_buf[..self.buf_len], scratch);

            if consumed == 0 && info.is_none() {
                if eof || self.buf_len >= VOICE_MP3_BUF_SIZE {
                    // A frame header sits at offset 0 but the frame isn't complete
                    // and no more data is coming - skip to the next sync candidate
                    // (0xFF followed by 0xE_/0xF_) in one move rather than nudging
                    // one byte at a time.
                    let mut skip = 1;
                    while skip + 1 < self.buf_len
                        && !(self.mp3_buf[skip] == 0xFF && (self.mp3_buf[skip + 1] & 0xE0) == 0xE0)
                    {
                        skip += 1;
                    }
                    consumed = skip;
                } else {
                    // Decode needs more bytes than are buffered. Force a read now;
                    // without this the loop can never make progress (no state
                    // change, no await) and freezes the executor.
                    match sd::read_yielding(self.handle, &mut self.file, &mut self.mp3_buf[self.buf_len..]).await {
                        Ok(0) => eof = true,
                        Ok(n) => self.buf_len += n,
                        Err(_) => eof = true,
                    }
                    continue;
                }
            }

            self.mp3_buf.copy_within(consumed..self.buf_len, 0);
            self.buf_len -= consumed;

            if let Some(info) = info {
                let channels = info.channels.num() as usize;
                let n = info.samples_produced;

                if channels > 1 {
                    for i in 0..n {
                        self.carry[i] = (scratch[i * 2] + scratch[i * 2 + 1]) * 0.5;
                    }
                } else {
                    self.carry[..n].copy_from_slice(&scratch[..n]);
                }

                self.carry_len = n;
                self.carry_pos = 0;
                return true;
            }
        }
    }

    /// Fills `out[..count]` with mono samples. Returns how many were actually
    /// written - `< count` (or 0) once a one-shot has ended; the caller zero-fills
    /// the remainder.
    async fn produce(
        &mut self,
        out: &mut [f32],
        count: usize,
        scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    ) -> usize {
        if self.finished {
            return 0;
        }

        for i in 0..count {
            if self.carry_pos >= self.carry_len && !self.decode_next_frame(scratch).await {
                self.finished = true;
                return i;
            }

            out[i] = self.carry[self.carry_pos];
            self.carry_pos += 1;
        }

        count
    }
}

/// Brings `voice` in line with the current DMX value. Keeps the voice untouched
/// when it already matches `(index, looping)` (so a finished one-shot stays
/// silent); otherwise opens the new file, or clears the voice on `0` / an
/// out-of-range selection.
fn reconcile(
    voice: &mut Option<Voice>,
    failed_selection: &mut Option<(usize, PlaybackMode)>,
    handle: SdHandle,
    files: &[heapless::String<{ crate::config::MAX_FILENAME_LEN }>],
    dmx: u8,
) {
    match decode_value(dmx, files.len()) {
        None => {
            *voice = None;
            *failed_selection = None;
        }
        Some((idx, mode)) => {
            let matches = voice
                .as_ref()
            .is_some_and(|v| v.file_index == idx && v.mode == mode);

            if matches {
                *failed_selection = None;
            } else if *failed_selection != Some((idx, mode)) {
                let _ = voice.take();
                *voice = Voice::start(handle, idx, mode, files[idx].as_str());
                *failed_selection = voice.is_none().then_some((idx, mode));
            }
        }
    }
}

/// Reads all three DMX channels, reconciles each voice independently, and
/// renders a full stereo `out` buffer. `bg_voice` is mixed into both outputs;
/// `left_voice`/`right_voice` are routed to their own output only. Yields once
/// per frame.
async fn fill(
    cfg: &AudioConfig,
    handle: SdHandle,
    bg_voice: &mut Option<Voice>,
    left_voice: &mut Option<Voice>,
    right_voice: &mut Option<Voice>,
    bg_failed_selection: &mut Option<(usize, PlaybackMode)>,
    left_failed_selection: &mut Option<(usize, PlaybackMode)>,
    right_failed_selection: &mut Option<(usize, PlaybackMode)>,
    scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    out: &mut [u32; OUT_BUF_LEN],
) {
    let channels = read_channels::<3>(cfg.universe as usize, cfg.start_channel as usize);
    reconcile(
        bg_voice,
        bg_failed_selection,
        handle,
        &cfg.bg_files,
        channels[0],
    );
    reconcile(
        left_voice,
        left_failed_selection,
        handle,
        &cfg.left_files,
        channels[1],
    );
    reconcile(
        right_voice,
        right_failed_selection,
        handle,
        &cfg.right_files,
        channels[2],
    );

    let mut pos = 0;
    while pos + MAX_FRAME_SAMPLES <= OUT_BUF_LEN {
        let mut bg = [0f32; MAX_FRAME_SAMPLES];
        let mut left = [0f32; MAX_FRAME_SAMPLES];
        let mut right = [0f32; MAX_FRAME_SAMPLES];

        let bg_produced = match bg_voice {
            Some(v) => v.produce(&mut bg, MAX_FRAME_SAMPLES, scratch).await,
            None => 0,
        };
        for sample in &mut bg[bg_produced..] {
            *sample = 0.0;
        }
        // Decoding a frame is pure CPU with no await inside unless an SD refill
        // is due, so three voices' worth back-to-back can hold the thread-mode
        // executor long enough to starve sibling tasks (neo_task's pixel timing,
        // oled flush). Yielding between each voice bounds the worst case to one
        // voice's decode instead of three.
        yield_now().await;

        let left_produced = match left_voice {
            Some(v) => v.produce(&mut left, MAX_FRAME_SAMPLES, scratch).await,
            None => 0,
        };
        for sample in &mut left[left_produced..] {
            *sample = 0.0;
        }
        yield_now().await;

        let right_produced = match right_voice {
            Some(v) => v.produce(&mut right, MAX_FRAME_SAMPLES, scratch).await,
            None => 0,
        };
        for sample in &mut right[right_produced..] {
            *sample = 0.0;
        }
        yield_now().await;

        for i in 0..MAX_FRAME_SAMPLES {
            let left_mixed = left[i] + bg[i];
            let right_mixed = right[i] + bg[i];

            let left_s16 =
                (left_mixed.clamp(-1.0, 1.0) * VOLUME * 32767.0) as i32 as i16 as u16;
            let right_s16 =
                (right_mixed.clamp(-1.0, 1.0) * VOLUME * 32767.0) as i32 as i16 as u16;
            out[pos + i] = ((left_s16 as u32) << 16) | (right_s16 as u32);
        }

        pos += MAX_FRAME_SAMPLES;
        yield_now().await;
    }
}

/// Decodes MP3/reads SD and posts filled buffers to `audio_output_task`. Runs
/// on the default thread-mode executor (NOT `AUDIO_EXECUTOR`): decode and SD
/// reads don't need interrupt priority, and running them here is what makes
/// `sd::read_yielding`'s cooperative yields actually useful - they hand time
/// to real sibling tasks (`neo_task`, `oled_task`, ...), which isn't possible
/// when a task is alone on an interrupt-priority executor (see below).
#[embassy_executor::task]
pub async fn audio_decode_task(cfg: AudioConfig, sd_r: SdResources) {
    println!("Audio decode task started.");

    // Mount here rather than in main(): the returned handle is not Send, and this
    // task now owns the only reference to it.
    let handle = sd::init(sd_r);

    let mut bg_voice: Option<Voice> = None;
    let mut left_voice: Option<Voice> = None;
    let mut right_voice: Option<Voice> = None;
    let mut bg_failed_selection: Option<(usize, PlaybackMode)> = None;
    let mut left_failed_selection: Option<(usize, PlaybackMode)> = None;
    let mut right_failed_selection: Option<(usize, PlaybackMode)> = None;
    let mut scratch = [0f32; MAX_SAMPLES_PER_FRAME];

    let buf_a = BUF_A.init([0u32; OUT_BUF_LEN]);
    let buf_b = BUF_B.init([0u32; OUT_BUF_LEN]);
    let buf_c = BUF_C.init([0u32; OUT_BUF_LEN]);
    EMPTY_CHANNEL.send(buf_a).await;
    EMPTY_CHANNEL.send(buf_b).await;
    EMPTY_CHANNEL.send(buf_c).await;

    loop {
        let buf = EMPTY_CHANNEL.receive().await;
        fill(
            &cfg,
            handle,
            &mut bg_voice,
            &mut left_voice,
            &mut right_voice,
            &mut bg_failed_selection,
            &mut left_failed_selection,
            &mut right_failed_selection,
            &mut scratch,
            buf,
        )
        .await;
        FILLED_CHANNEL.send(buf).await;
    }
}

/// Feeds the I2S PIO/DMA output from buffers produced by `audio_decode_task`.
/// Runs on `AUDIO_EXECUTOR`, a dedicated high-priority interrupt executor, so
/// it preempts every thread-mode task (OLED I2C flush, NeoPixel effects, sACN
/// parsing, ...). The I2S PIO FIFO only holds ~180us of samples between DMA
/// transfers; any cooperative task that blocks the shared executor longer than
/// that at a buffer boundary causes an audible underrun, so this task does
/// nothing but wait for a full buffer and hand it to the DMA - no decode, no
/// SD access, nothing that can run long between yields.
#[embassy_executor::task]
pub async fn audio_output_task(r: AudioResources) {
    println!("Audio output task started.");

    let Pio { mut common, sm0, .. } = Pio::new(r.pio, AudioIrqs);
    let i2s_program = PioI2sOutProgram::new(&mut common);

    let mut i2s = PioI2sOut::new(
        &mut common,
        sm0,
        r.dma,
        r.din,
        r.bck,
        r.lck,
        AUDIO_SAMPLE_RATE,
        16,
        &i2s_program,
    );

    let mut first_write = true;
    let mut underruns: u32 = 0;
    let mut reported: u32 = 0;
    let mut last_report = Instant::now();
    // DIAG: buffers where decode hadn't finished the next one by the time the
    // previous one finished playing (vs. a late DMA hand-off).
    let mut decode_late: u32 = 0;

    loop {
        if !first_write && FILLED_CHANNEL.is_empty() {
            decode_late += 1;
        }
        let buf = FILLED_CHANNEL.receive().await;
        i2s.write(&buf[..]).await;

        if first_write {
            // The SM stalls continuously until the first DMA transfer feeds the
            // FIFO; clear that expected startup latch so the counter only sees
            // real underruns.
            clear_i2s_underrun();
            first_write = false;
        } else if i2s_underran() {
            underruns += 1;
            clear_i2s_underrun();
        }

        // Report only when the count moved, and at most every 2s, so a clean run
        // is silent and a bad run doesn't flood the log (which would make it worse).
        let now = Instant::now();
        if underruns != reported && (now - last_report).as_millis() >= 2000 {
            let ms = (now - last_report).as_millis() as u32;
            let oled_us = crate::periphs::oled::OLED_BUSY_US.swap(0, core::sync::atomic::Ordering::Relaxed);
            let rf = crate::periphs::ask433::RF_PULSES.swap(0, core::sync::atomic::Ordering::Relaxed);
            println!(
                "AUDIO underruns: {} total (+{} in {}ms) | DIAG decode late {}, cpu {}%, oled busy {}ms/s, rf {} pulses/s",
                underruns,
                underruns - reported,
                ms,
                decode_late,
                crate::CPU_STALL_PCT.load(core::sync::atomic::Ordering::Relaxed),
                oled_us / ms.max(1),
                rf * 1000 / ms.max(1),
            );
            decode_late = 0;
            reported = underruns;
            last_report = now;
        }

        EMPTY_CHANNEL.send(buf).await;
    }
}
