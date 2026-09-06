use defmt::println;

use embassy_futures::join::join;
use embassy_futures::yield_now;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use embedded_sdmmc::Mode;
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};

use crate::config::AudioConfig;
use crate::hardware::{AudioIrqs, AudioResources};
use crate::periphs::sd::{self, SdFile, SdHandle};
use crate::read_channels;

// All source MP3s are expected at this rate - no resampling is done.
const AUDIO_SAMPLE_RATE: u32 = 44100;

// Playback volume, 0.0 - 1.0. Tweak here.
const VOLUME: f32 = 0.1;

// nanomp3 decodes interleaved; MAX_SAMPLES_PER_FRAME counts individual samples,
// so a mono frame is at most half that many.
const MAX_FRAME_SAMPLES: usize = MAX_SAMPLES_PER_FRAME / 2;

// Per-voice SD read buffer. Only topped up once it drops below the threshold so
// each SD transaction pulls a worthwhile chunk. Threshold stays well above one
// MP3 frame's worst-case (~1 KB) so decode is never starved.
const VOICE_MP3_BUF_SIZE: usize = 4 * 1024;
const VOICE_MP3_BUF_REFILL_THRESHOLD: usize = 2 * 1024;

// Frames per I2S DMA transfer / buffer. Two of these are in flight at once, so
// this sets the delay between starting a voice and hearing it: ~FRAMES_PER_BATCH
// * 2 * 26 ms. Kept small for a snappy response to DMX changes; still ~200 ms of
// buffered audio, far more than one decode+SD-read cycle needs.
const FRAMES_PER_BATCH: usize = 4;
const OUT_BUF_LEN: usize = MAX_FRAME_SAMPLES * FRAMES_PER_BATCH;

/// Maps a DMX value to `(file_index, looping)`:
/// - `0` => `None` (stop)
/// - `1..=127` => `(v - 1, false)` — play once
/// - `128..=255` => `(v - 128, true)` — loop
/// - resolved index past the end of the list => `None` (stop)
fn decode_value(v: u8, num_files: usize) -> Option<(usize, bool)> {
    if v == 0 {
        return None;
    }

    let (idx, looping) = if v < 128 {
        ((v - 1) as usize, false)
    } else {
        ((v - 128) as usize, true)
    };

    (idx < num_files).then_some((idx, looping))
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
    looping: bool,
    // Offset of the first audio byte (past any ID3v2 tag). Loop-rewinds seek here
    // rather than to 0 so the tag is only ever scanned past once.
    data_start: u32,
    // Set once a one-shot plays through to EOF. The voice is kept (silent) rather
    // than dropped, so reconcile() doesn't see "nothing playing" and restart it
    // every fill. Cleared only by selecting a different file (or 0).
    finished: bool,

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
    fn start(handle: SdHandle, file_index: usize, looping: bool, filename: &str) -> Option<Voice> {
        let mut file = match sd::open_file(handle, filename, Mode::ReadOnly) {
            Ok(f) => f,
            Err(_) => {
                println!("Audio: failed to open {}", filename);
                return None;
            }
        };

        let data_start = id3v2_data_start(&mut file);
        if file.seek_from_start(data_start).is_err() {
            let _ = file.seek_from_start(0);
        }

        Some(Voice {
            file_index,
            looping,
            data_start,
            finished: false,
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
    fn decode_next_frame(&mut self, scratch: &mut [f32; MAX_SAMPLES_PER_FRAME]) -> bool {
        let mut eof = false;
        let mut rewound = false;

        loop {
            if !eof && self.buf_len < VOICE_MP3_BUF_REFILL_THRESHOLD {
                match self.file.read(&mut self.mp3_buf[self.buf_len..]) {
                    Ok(0) => eof = true,
                    Ok(n) => self.buf_len += n,
                    Err(_) => eof = true,
                }
            }

            if self.buf_len == 0 {
                if eof {
                    if self.looping && !rewound {
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
                    match self.file.read(&mut self.mp3_buf[self.buf_len..]) {
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
    fn produce(
        &mut self,
        out: &mut [f32],
        count: usize,
        scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    ) -> usize {
        if self.finished {
            return 0;
        }

        for i in 0..count {
            if self.carry_pos >= self.carry_len && !self.decode_next_frame(scratch) {
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
fn reconcile(voice: &mut Option<Voice>, handle: SdHandle, cfg: &AudioConfig, dmx: u8) {
    match decode_value(dmx, cfg.files.len()) {
        None => *voice = None,
        Some((idx, looping)) => {
            let matches = voice
                .as_ref()
                .is_some_and(|v| v.file_index == idx && v.looping == looping);

            if !matches {
                *voice = Voice::start(handle, idx, looping, cfg.files[idx].as_str());
            }
        }
    }
}

/// Reads the DMX channel, reconciles the voice, and renders a full `out` buffer
/// (silence where there is no audio). Yields once per frame.
async fn fill(
    cfg: &AudioConfig,
    handle: SdHandle,
    voice: &mut Option<Voice>,
    scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    out: &mut [u32; OUT_BUF_LEN],
) {
    let dmx = read_channels::<1>(cfg.universe as usize, cfg.start_channel as usize)[0];
    reconcile(voice, handle, cfg, dmx);

    let mut pos = 0;
    while pos + MAX_FRAME_SAMPLES <= OUT_BUF_LEN {
        let mut mono = [0f32; MAX_FRAME_SAMPLES];

        let produced = match voice {
            Some(v) => v.produce(&mut mono, MAX_FRAME_SAMPLES, scratch),
            None => 0,
        };
        for s in &mut mono[produced..] {
            *s = 0.0;
        }

        for i in 0..MAX_FRAME_SAMPLES {
            let s16 = (mono[i].clamp(-1.0, 1.0) * VOLUME * 32767.0) as i32 as i16 as u16;
            // Same sample in the top (left) and bottom (right) half-words - mono
            // on both I2S channels.
            out[pos + i] = ((s16 as u32) << 16) | (s16 as u32);
        }

        pos += MAX_FRAME_SAMPLES;
        yield_now().await;
    }
}

#[embassy_executor::task]
pub async fn audio_task(cfg: AudioConfig, r: AudioResources, handle: SdHandle) {
    println!("Audio task started.");

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

    let mut voice: Option<Voice> = None;
    let mut scratch = [0f32; MAX_SAMPLES_PER_FRAME];
    let mut buf_a = [0u32; OUT_BUF_LEN];
    let mut buf_b = [0u32; OUT_BUF_LEN];

    fill(&cfg, handle, &mut voice, &mut scratch, &mut buf_a).await;

    loop {
        join(
            i2s.write(&buf_a[..]),
            fill(&cfg, handle, &mut voice, &mut scratch, &mut buf_b),
        )
        .await;

        join(
            i2s.write(&buf_b[..]),
            fill(&cfg, handle, &mut voice, &mut scratch, &mut buf_a),
        )
        .await;
    }
}
