use core::sync::atomic::{AtomicU32, Ordering};

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

use crate::config::{AudioConfig, AudioFile};
use crate::hardware::{AudioIrqs, AudioResources, SdResources};
use crate::periphs::sd::{self, SdFile, SdHandle};
use crate::read_channels;

// All source MP3s are expected at this rate - no resampling is done.
const AUDIO_SAMPLE_RATE: u32 = 44100;

// Playback volume, 0.0 - 1.0. Tweak here. Each file's config `volume` scales
// on top of this.
const VOLUME: f32 = 0.3;

// DMX audio channels, in channel order from `start_channel`: background (both
// speakers), left (Bones), right (Frank), left FX, right FX. The FX voices play
// on top of their side's track so a jumpscare doesn't interrupt (and desync) it.
const VOICE_COUNT: usize = 5;
const BG: usize = 0;
const LEFT: usize = 1;
const RIGHT: usize = 2;
const LEFT_FX: usize = 3;
const RIGHT_FX: usize = 4;

/// Which speakers each voice feeds: (left, right).
const ROUTING: [(bool, bool); VOICE_COUNT] = [
    (true, true),  // bg
    (true, false), // left
    (false, true), // right
    (true, false), // left FX
    (false, true), // right FX
];

// Gain on a side's track (left/right) while that side's FX voice is playing.
// 1.0 = keep it at full volume underneath, 0.0 = mute it. It keeps playing
// (and stays in sync) either way.
const FX_DUCK: f32 = 1.0;

// nanomp3 decodes interleaved; MAX_SAMPLES_PER_FRAME counts individual samples,
// so a mono frame is at most half that many.
const MAX_FRAME_SAMPLES: usize = MAX_SAMPLES_PER_FRAME / 2;

// Per-voice SD read buffer. Only topped up once it drops below the threshold so
// each SD transaction pulls a worthwhile chunk. Threshold stays well above one
// MP3 frame's worst-case (~1 KB) so decode is never starved.
const VOICE_MP3_BUF_SIZE: usize = 4 * 1024;
const VOICE_MP3_BUF_REFILL_THRESHOLD: usize = 2 * 1024;

// Frames per I2S DMA transfer / buffer. While one buffer plays (~FRAMES_PER_BATCH
// * 26 ms of DMA) the others are refilled, so this is the headroom `fill()` has to
// decode the next batch and service any SD read latency spike.
//
// It is also the audio latency: DMX is sampled once per buffer, and a new file
// can only be heard after the buffers already queued ahead of it play out, so a
// DMX change takes ~(1..3) * FRAMES_PER_BATCH * 26 ms to be heard. 8 frames made
// that ~0.2-0.6 s, audibly late against the lights; 2 frames = ~52 ms per
// buffer, ~50-160 ms latency. Raise it again if "AUDIO underruns" shows up.
const FRAMES_PER_BATCH: usize = 2;
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

/// Running total of microseconds spent inside `Decoder::decode`. Wraps; readers
/// diff two snapshots. Only the decode task decodes, so a diff around one
/// voice's `produce()` is exactly that voice's decode time.
static DECODE_US: AtomicU32 = AtomicU32::new(0);

/// How often the audio CPU log prints.
const CPU_LOG_WINDOW_US: u64 = 2_000_000;

/// Per-voice decode and SD read time over one log window. Both are CPU-busy
/// time: the SD SPI is blocking, so the core spins for the whole transfer.
struct CpuStats {
    window_start: Instant,
    decode_us: [u32; VOICE_COUNT],
    sd_us: [u32; VOICE_COUNT],
}

impl CpuStats {
    fn new() -> Self {
        Self { window_start: Instant::now(), decode_us: [0; VOICE_COUNT], sd_us: [0; VOICE_COUNT] }
    }

    fn snapshot() -> (u32, u32) {
        (DECODE_US.load(Ordering::Relaxed), sd::SD_READ_US.load(Ordering::Relaxed))
    }

    /// Charges everything since `before` (from `snapshot()`) to `voice`.
    fn charge(&mut self, voice: usize, before: (u32, u32)) {
        let (decode, sd) = Self::snapshot();
        self.decode_us[voice] = self.decode_us[voice].wrapping_add(decode.wrapping_sub(before.0));
        self.sd_us[voice] = self.sd_us[voice].wrapping_add(sd.wrapping_sub(before.1));
    }

    /// Prints and resets once per window. Percentages are of wall time, so the
    /// total is the share of the core the audio pipeline is eating.
    fn maybe_report(&mut self) {
        let window_us = self.window_start.elapsed().as_micros();
        if window_us < CPU_LOG_WINDOW_US {
            return;
        }

        let pct = |us: u32| (us as u64 * 1000 / window_us) as u32; // tenths of a percent
        let total: u32 = self.decode_us.iter().chain(self.sd_us.iter()).sum();

        let d = |v: usize| pct(self.decode_us[v]);
        let r = |v: usize| pct(self.sd_us[v]);
        println!(
            "AUDIO CPU ({}ms): bg dec {}.{}% sd {}.{}% | L dec {}.{}% sd {}.{}% | R dec {}.{}% sd {}.{}% | Lfx dec {}.{}% sd {}.{}% | Rfx dec {}.{}% sd {}.{}% | total {}.{}%",
            window_us / 1000,
            d(BG) / 10, d(BG) % 10, r(BG) / 10, r(BG) % 10,
            d(LEFT) / 10, d(LEFT) % 10, r(LEFT) / 10, r(LEFT) % 10,
            d(RIGHT) / 10, d(RIGHT) % 10, r(RIGHT) / 10, r(RIGHT) % 10,
            d(LEFT_FX) / 10, d(LEFT_FX) % 10, r(LEFT_FX) / 10, r(LEFT_FX) % 10,
            d(RIGHT_FX) / 10, d(RIGHT_FX) % 10, r(RIGHT_FX) / 10, r(RIGHT_FX) % 10,
            pct(total) / 10, pct(total) % 10,
        );

        *self = Self::new();
    }
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

// ---------------------------------------------------------------------------
// IMA ADPCM (.wav)
// ---------------------------------------------------------------------------
//
// 4 bits per sample (~22 KB/s mono at 44.1 kHz, vs ~16 KB/s for a 128 kbps
// MP3), decoded with a table lookup and a few adds per sample - next to free
// compared to MP3 decoding. Make files with:
//   ffmpeg -i in.mp3 -ac 1 -ar 44100 -c:a adpcm_ima_wav out.wav
//
// The data is a run of `block_align`-byte blocks. Each block starts with a
// 4-byte header (first sample as i16 LE, step index, reserved), then 2 samples
// per byte, low nibble first.

const IMA_STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230,
    253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327,
    3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442,
    11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

const IMA_INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

// Data bytes decoded per `decode_next_frame` call: 2 samples each, plus a
// possible block-header sample, must fit in `carry`.
const ADPCM_CHUNK_BYTES: usize = (MAX_FRAME_SAMPLES - 1) / 2;

// Largest block accepted: a whole block header plus data always fits in the
// voice buffer after a refill.
const ADPCM_MAX_BLOCK_ALIGN: usize = VOICE_MP3_BUF_REFILL_THRESHOLD;

struct Adpcm {
    block_align: usize,
    // Size of the data chunk, and how much of it hasn't been consumed yet this
    // pass through the file. Bytes past the data chunk (trailing metadata) are
    // never decoded.
    data_len: u32,
    data_left: u32,
    // Data bytes left in the current block; 0 = next byte starts a block header.
    block_left: usize,
    predictor: i32,
    step_index: i32,
}

impl Adpcm {
    fn sample(&mut self, nibble: u8) -> f32 {
        let step = IMA_STEP_TABLE[self.step_index as usize];
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        if nibble & 8 != 0 {
            self.predictor -= diff;
        } else {
            self.predictor += diff;
        }
        self.predictor = self.predictor.clamp(-32768, 32767);
        self.step_index = (self.step_index + IMA_INDEX_TABLE[nibble as usize]).clamp(0, 88);
        self.predictor as f32 / 32768.0
    }
}

struct WavInfo {
    data_start: u32,
    data_len: u32,
    block_align: usize,
    sample_rate: u32,
}

/// Fills `buf` from the file; `false` if the file ended first.
fn read_exact(file: &mut SdFile<'static>, buf: &mut [u8]) -> bool {
    let mut read = 0;
    while read < buf.len() {
        match file.read(&mut buf[read..]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => read += n,
        }
    }
    true
}

/// Walks the RIFF chunks of a .wav and returns where its IMA ADPCM data is.
/// Only mono IMA ADPCM is accepted (see the ffmpeg line above).
fn parse_ima_wav(file: &mut SdFile<'static>) -> Result<WavInfo, &'static str> {
    let mut riff = [0u8; 12];
    if !read_exact(file, &mut riff) || &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        return Err("not a RIFF/WAVE file");
    }

    let file_len = file.length();
    let mut pos: u32 = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // format, channels, rate, block_align

    loop {
        let mut chunk = [0u8; 8];
        if !read_exact(file, &mut chunk) {
            return Err("no data chunk");
        }
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        pos += 8;

        if &chunk[..4] == b"fmt " {
            let mut f = [0u8; 16];
            if size < 16 || !read_exact(file, &mut f) {
                return Err("bad fmt chunk");
            }
            fmt = Some((
                u16::from_le_bytes([f[0], f[1]]),
                u16::from_le_bytes([f[2], f[3]]),
                u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                u16::from_le_bytes([f[12], f[13]]),
            ));
        } else if &chunk[..4] == b"data" {
            let Some((format, channels, sample_rate, block_align)) = fmt else {
                return Err("data chunk before fmt chunk");
            };
            if format != 0x11 {
                return Err("not IMA ADPCM (re-encode with -c:a adpcm_ima_wav)");
            }
            if channels != 1 {
                return Err("not mono (re-encode with -ac 1)");
            }
            let block_align = block_align as usize;
            if !(5..=ADPCM_MAX_BLOCK_ALIGN).contains(&block_align) {
                return Err("unsupported ADPCM block size");
            }
            return Ok(WavInfo {
                data_start: pos,
                data_len: size.min(file_len.saturating_sub(pos)),
                block_align,
                sample_rate,
            });
        }

        // Chunks are padded to an even length.
        pos = pos.saturating_add(size).saturating_add(size & 1);
        if pos >= file_len || file.seek_from_start(pos).is_err() {
            return Err("no data chunk");
        }
    }
}

enum Codec {
    Mp3(Decoder),
    Adpcm(Adpcm),
}

fn is_wav(filename: &str) -> bool {
    let bytes = filename.as_bytes();
    bytes.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".wav")
}

struct Voice {
    file_index: usize,
    mode: PlaybackMode,
    // Offset of the first audio byte (past any ID3v2 tag, or the start of a
    // .wav's data chunk). Loop-rewinds seek here.
    data_start: u32,
    // Set once a one-shot plays through to EOF. The voice is kept (silent) rather
    // than dropped, so reconcile() doesn't see "nothing playing" and restart it
    // every fill. Cleared only by selecting a different file (or 0).
    finished: bool,
    // Set after the sample rate has been checked (first MP3 frame, or the .wav
    // header).
    rate_checked: bool,
    // The file's config volume, 0.0..=1.0.
    gain: f32,

    // Kept so reads can go through `sd::read_yielding` (needs a handle to hand
    // back to the caller's signature, even though the actual read goes through
    // `file`'s own volume-manager reference).
    handle: SdHandle,
    file: SdFile<'static>,
    codec: Codec,
    // Undecoded file bytes (MP3 or ADPCM).
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
        audio_file: &AudioFile,
    ) -> Option<Voice> {
        let filename = audio_file.file.as_str();
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

        let (data_start, codec, rate_checked) = if is_wav(filename) {
            let info = match parse_ima_wav(&mut file) {
                Ok(info) => info,
                Err(reason) => {
                    println!("Audio: can't play {}: {}", filename, reason);
                    return None;
                }
            };
            if info.sample_rate != AUDIO_SAMPLE_RATE {
                println!(
                    "Audio: {} is {} Hz, expected {} Hz - will play at the wrong speed",
                    filename,
                    info.sample_rate,
                    AUDIO_SAMPLE_RATE
                );
            }
            let adpcm = Adpcm {
                block_align: info.block_align,
                data_len: info.data_len,
                data_left: info.data_len,
                block_left: 0,
                predictor: 0,
                step_index: 0,
            };
            (info.data_start, Codec::Adpcm(adpcm), true)
        } else {
            (id3v2_data_start(&mut file), Codec::Mp3(Decoder::new()), false)
        };

        if file.seek_from_start(data_start).is_err() {
            let _ = file.seek_from_start(0);
        }

        Some(Voice {
            file_index,
            mode,
            data_start,
            finished: false,
            rate_checked,
            gain: audio_file.volume.min(100) as f32 / 100.0,
            handle,
            file,
            codec,
            mp3_buf: [0u8; VOICE_MP3_BUF_SIZE],
            buf_len: 0,
            carry: [0f32; MAX_FRAME_SAMPLES],
            carry_len: 0,
            carry_pos: 0,
        })
    }

    /// Decodes the next run of samples into `self.carry` as mono. Loops back to
    /// the start of the audio at EOF when the mode loops. Returns `false` once
    /// there is genuinely no more audio (one-shot EOF).
    async fn decode_next_frame(&mut self, scratch: &mut [f32; MAX_SAMPLES_PER_FRAME]) -> bool {
        match self.codec {
            Codec::Mp3(_) => self.decode_mp3_frame(scratch).await,
            Codec::Adpcm(_) => self.decode_adpcm_chunk().await,
        }
    }

    /// Decodes up to `ADPCM_CHUNK_BYTES` of IMA ADPCM into `self.carry`.
    async fn decode_adpcm_chunk(&mut self) -> bool {
        let mut rewound = false;
        let mut eof = false;

        loop {
            let Codec::Adpcm(adpcm) = &mut self.codec else {
                return false;
            };

            if adpcm.data_left == 0 {
                if self.mode.loops() && !rewound {
                    if self.file.seek_from_start(self.data_start).is_err() {
                        return false;
                    }
                    self.buf_len = 0;
                    adpcm.data_left = adpcm.data_len;
                    adpcm.block_left = 0;
                    rewound = true;
                    eof = false;
                    continue;
                }
                return false;
            }

            if !eof && self.buf_len < VOICE_MP3_BUF_REFILL_THRESHOLD {
                match sd::read_yielding(self.handle, &mut self.file, &mut self.mp3_buf[self.buf_len..]).await {
                    Ok(0) | Err(_) => eof = true,
                    Ok(n) => self.buf_len += n,
                }
            }

            let avail = self.buf_len.min(adpcm.data_left as usize);
            let mut n = 0;
            let mut consumed = 0;

            if adpcm.block_left == 0 {
                if avail < 4 {
                    // Truncated file, or a trailing block too short to hold
                    // any samples: done.
                    if eof || avail == adpcm.data_left as usize {
                        adpcm.data_left = 0;
                    }
                    continue;
                }
                let first = i16::from_le_bytes([self.mp3_buf[0], self.mp3_buf[1]]);
                adpcm.predictor = first as i32;
                adpcm.step_index = (self.mp3_buf[2] as i32).clamp(0, 88);
                adpcm.block_left = (adpcm.block_align - 4).min(adpcm.data_left as usize - 4);
                self.carry[0] = first as f32 / 32768.0;
                n = 1;
                consumed = 4;
            }

            let take = adpcm.block_left.min(avail - consumed).min(ADPCM_CHUNK_BYTES);
            for &byte in &self.mp3_buf[consumed..consumed + take] {
                self.carry[n] = adpcm.sample(byte & 0x0F);
                self.carry[n + 1] = adpcm.sample(byte >> 4);
                n += 2;
            }
            consumed += take;
            adpcm.block_left -= take;
            adpcm.data_left -= consumed as u32;

            self.mp3_buf.copy_within(consumed..self.buf_len, 0);
            self.buf_len -= consumed;

            if n > 0 {
                self.carry_len = n;
                self.carry_pos = 0;
                return true;
            }
            if eof {
                // Data chunk claims more bytes than the file has.
                adpcm.data_left = 0;
            }
        }
    }

    /// Decodes the next MP3 frame into `self.carry` as mono (downmixing a stereo
    /// source).
    async fn decode_mp3_frame(&mut self, scratch: &mut [f32; MAX_SAMPLES_PER_FRAME]) -> bool {
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

            let decode_start = Instant::now();
            let Codec::Mp3(decoder) = &mut self.codec else {
                return false;
            };
            let (mut consumed, info) = decoder.decode(&self.mp3_buf[..self.buf_len], scratch);
            DECODE_US.fetch_add(decode_start.elapsed().as_micros() as u32, Ordering::Relaxed);

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

                // Output runs at a fixed AUDIO_SAMPLE_RATE with no resampling, so a
                // file at any other rate plays at the wrong speed and drifts out of
                // sync with the lights (48 kHz plays ~9% slow).
                if !self.rate_checked {
                    self.rate_checked = true;
                    if info.sample_rate != AUDIO_SAMPLE_RATE {
                        println!(
                            "Audio: file #{} is {} Hz, expected {} Hz - will play at the wrong speed",
                            self.file_index,
                            info.sample_rate,
                            AUDIO_SAMPLE_RATE
                        );
                    }
                }

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
    files: &[AudioFile],
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
                *voice = Voice::start(handle, idx, mode, &files[idx]);
                *failed_selection = voice.is_none().then_some((idx, mode));
            }
        }
    }
}

/// Reads the audio DMX channels, reconciles each voice independently, and
/// renders a full stereo `out` buffer, routing each voice per `ROUTING`.
/// Yields after each voice's frame.
async fn fill(
    cfg: &AudioConfig,
    handle: SdHandle,
    voices: &mut [Option<Voice>; VOICE_COUNT],
    failed_selections: &mut [Option<(usize, PlaybackMode)>; VOICE_COUNT],
    scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    out: &mut [u32; OUT_BUF_LEN],
    last_dmx: &mut [u8; VOICE_COUNT],
    stats: &mut CpuStats,
) {
    let channels = read_channels::<VOICE_COUNT>(cfg.universe as usize, cfg.start_channel as usize);

    // TEMP latency debug: log every DMX value change with how much already-mixed
    // audio is queued ahead of this buffer (1 playing + FILLED_CHANNEL.len()
    // waiting), i.e. roughly how long until this change can be heard.
    let changed = channels != *last_dmx;
    let reconcile_start = Instant::now();
    if changed {
        let buf_ms = (OUT_BUF_LEN as u64 * 1000) / AUDIO_SAMPLE_RATE as u64;
        let queued = FILLED_CHANNEL.len() as u64 + 1;
        println!(
            "Audio DMX [bg,L,R,Lfx,Rfx] {=[u8]} -> {=[u8]} at {}ms, ~{}-{}ms of audio queued ahead",
            last_dmx[..],
            channels[..],
            reconcile_start.as_millis(),
            (queued - 1) * buf_ms,
            queued * buf_ms,
        );
        *last_dmx = channels;
    }

    let file_lists = [
        &cfg.bg_files,
        &cfg.left_files,
        &cfg.right_files,
        &cfg.left_fx_files,
        &cfg.right_fx_files,
    ];
    for v in 0..VOICE_COUNT {
        reconcile(&mut voices[v], &mut failed_selections[v], handle, file_lists[v], channels[v]);
    }

    if changed {
        println!("Audio: file open/seek took {}ms", reconcile_start.elapsed().as_millis());
    }

    let playing = |voice: &Option<Voice>| voice.as_ref().is_some_and(|v| !v.finished);

    let mut pos = 0;
    while pos + MAX_FRAME_SAMPLES <= OUT_BUF_LEN {
        let mut left_mix = [0f32; MAX_FRAME_SAMPLES];
        let mut right_mix = [0f32; MAX_FRAME_SAMPLES];
        let mut samples = [0f32; MAX_FRAME_SAMPLES];

        let mut duck = [1.0f32; VOICE_COUNT];
        if playing(&voices[LEFT_FX]) {
            duck[LEFT] = FX_DUCK;
        }
        if playing(&voices[RIGHT_FX]) {
            duck[RIGHT] = FX_DUCK;
        }

        for v in 0..VOICE_COUNT {
            let Some(voice) = &mut voices[v] else {
                continue;
            };

            let before = CpuStats::snapshot();
            let produced = voice.produce(&mut samples, MAX_FRAME_SAMPLES, scratch).await;
            stats.charge(v, before);

            let gain = voice.gain * duck[v];
            let (to_left, to_right) = ROUTING[v];
            for i in 0..produced {
                let s = samples[i] * gain;
                if to_left {
                    left_mix[i] += s;
                }
                if to_right {
                    right_mix[i] += s;
                }
            }

            // Decoding is pure CPU with no await inside unless an SD refill is
            // due, so several voices back-to-back can hold the thread-mode
            // executor long enough to starve sibling tasks (neo_task's pixel
            // timing, oled flush). Yielding between voices bounds the worst case
            // to one voice's decode.
            yield_now().await;
        }

        for i in 0..MAX_FRAME_SAMPLES {
            // Volume before the clamp, so several voices summed together only
            // clip if the result is actually out of range.
            let left_s16 = ((left_mix[i] * VOLUME).clamp(-1.0, 1.0) * 32767.0) as i32 as i16 as u16;
            let right_s16 = ((right_mix[i] * VOLUME).clamp(-1.0, 1.0) * 32767.0) as i32 as i16 as u16;
            out[pos + i] = ((left_s16 as u32) << 16) | (right_s16 as u32);
        }

        pos += MAX_FRAME_SAMPLES;
        yield_now().await;
    }

    stats.maybe_report();
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

    let mut voices: [Option<Voice>; VOICE_COUNT] = core::array::from_fn(|_| None);
    let mut failed_selections: [Option<(usize, PlaybackMode)>; VOICE_COUNT] = [None; VOICE_COUNT];
    let mut scratch = [0f32; MAX_SAMPLES_PER_FRAME];
    let mut last_dmx = [0u8; VOICE_COUNT];
    let mut stats = CpuStats::new();

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
            &mut voices,
            &mut failed_selections,
            &mut scratch,
            buf,
            &mut last_dmx,
            &mut stats,
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

    loop {
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
            println!(
                "AUDIO underruns: {} total (+{} in {}ms)",
                underruns,
                underruns - reported,
                (now - last_report).as_millis(),
            );
            reported = underruns;
            last_report = now;
        }

        EMPTY_CHANNEL.send(buf).await;
    }
}
