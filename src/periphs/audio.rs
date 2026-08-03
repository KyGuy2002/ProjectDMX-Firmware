use defmt::println;

use embassy_futures::join::join;
use embassy_futures::yield_now;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::i2s::{PioI2sOut, PioI2sOutProgram};
use embedded_sdmmc::Mode;
use heapless::{String, Vec};
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};

use crate::config::{AudioConfig, MAX_AUDIO_FILES, MAX_FILENAME_LEN};
use crate::hardware::{AudioIrqs, AudioResources};
use crate::periphs::sd::{self, SdFile, SdHandle};
use crate::read_channels;

// Fixed output rate - all mp3s are expected to be encoded at this rate (no resampling).
const AUDIO_SAMPLE_RATE: u32 = 44100;

// Playback volume (0.0 - 1.0)
const VOLUME: f32 = 0.1;

// 2 idle voices + 2 effect voices, each possibly with one more fading out during a
// file-change transition = up to 8 concurrent decode streams in the worst case, but
// that's a lot of RAM (nanomp3's Decoder alone is ~6.5KB - MDCT/QMF history buffers,
// not "small" state - so each voice is ~15KB just from Decoder+mp3_buf+carry). 6 covers
// both idle+effect fully switching at once (4 new + 2 of the 4 old still fading) at a
// more sustainable footprint; the rarer case of *both* pairs transitioning with full
// independent-then-diverging overlap could in theory want more than 6, in which case a
// voice request just silently doesn't get a slot rather than crashing.
const MAX_VOICES: usize = 6;

// ~300ms fade-out on file change, at AUDIO_SAMPLE_RATE (300ms * 44100 = 13230 samples)
const FADE_STEP: f32 = 1.0 / 13230.0;

const MAX_FRAME_SAMPLES: usize = MAX_SAMPLES_PER_FRAME / 2; // 1152 (mono)

// Per-voice MP3 read buffer. Only refilled once it drops below the threshold (not
// "whenever it isn't completely full") so each SD read pulls a worthwhile chunk
// instead of topping up by a couple hundred bytes almost every decoded frame - each
// read pays a fixed per-transaction cost (at least a full sector fetch) regardless of
// how little was actually requested, and with several voices decoding at once those
// costs stack up fast. The threshold stays well above one MP3 frame's worst-case size
// (~1KB) so decode is never actually starved by refilling "too late".
const VOICE_MP3_BUF_SIZE: usize = 4 * 1024;
const VOICE_MP3_BUF_REFILL_THRESHOLD: usize = 2 * 1024;

// Frames mixed per I2S DMA transfer (double buffered) - also the yield granularity,
// so other tasks (OLED/DMX/etc) get scheduled roughly every MAX_FRAME_SAMPLES worth of audio.
const FRAMES_PER_BATCH: usize = 8;
const OUT_BUF_LEN: usize = MAX_FRAME_SAMPLES * FRAMES_PER_BATCH;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Idle,
    Effect,
}

#[derive(Clone, Copy, PartialEq)]
enum Routing {
    Left,
    Right,
    Both,
}

struct Voice {
    role: Role,
    file_index: usize,
    looping: bool,
    routing: Routing,

    file: SdFile<'static>,
    decoder: Decoder,
    mp3_buf: [u8; VOICE_MP3_BUF_SIZE],
    buf_len: usize,

    // Leftover mono samples decoded but not yet consumed by produce()
    carry: [f32; MAX_FRAME_SAMPLES],
    carry_len: usize,
    carry_pos: usize,

    // 1.0 = full volume. Ramps to 0.0 over FADE_SAMPLES once `fading` is set, at
    // which point the voice is done and gets dropped. Newly started voices are
    // never faded in - they start at gain 1.0 immediately.
    gain: f32,
    fading: bool,
}

impl Voice {
    fn start(
        handle: SdHandle,
        role: Role,
        file_index: usize,
        looping: bool,
        routing: Routing,
        filename: &str,
    ) -> Option<Voice> {
        let file = match sd::open_file(handle, filename, Mode::ReadOnly) {
            Ok(f) => f,
            Err(_) => {
                println!("Audio: failed to open {}", filename);
                return None;
            }
        };

        Some(Voice {
            role,
            file_index,
            looping,
            routing,
            file,
            decoder: Decoder::new(),
            mp3_buf: [0u8; VOICE_MP3_BUF_SIZE],
            buf_len: 0,
            carry: [0f32; MAX_FRAME_SAMPLES],
            carry_len: 0,
            carry_pos: 0,
            gain: 1.0,
            fading: false,
        })
    }

    fn start_fade(&mut self) {
        self.fading = true;
    }

    /// Decodes the next MP3 frame into `self.carry` (mono - downmixed from stereo
    /// source if needed). Transparently loops back to the start of the file at EOF
    /// if `self.looping`. Returns false once there's genuinely no more audio to give.
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
                        if self.file.seek_from_start(0).is_err() {
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
                    // Buffer is as full as it'll get (or file's exhausted) and still no
                    // valid frame - skip a byte to resync rather than getting stuck.
                    consumed = 1;
                } else {
                    // Decode needs more bytes than are currently buffered, even though
                    // we're above the normal (batched) refill threshold. Force a top-up
                    // now - without this, buf_len sits in the gap between the refill
                    // threshold and VOICE_MP3_BUF_SIZE forever, decode() keeps seeing the
                    // exact same input and returning the exact same "need more" result,
                    // and this loop never terminates (no state changes, no yield point -
                    // a full, permanent executor freeze, not just a stutter).
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

    /// Mixes up to `count` samples of this voice into `mix_l`/`mix_r` according to
    /// its routing, applying volume/fade gain. Returns false once the voice is done
    /// (ran out of audio, or finished fading out) and should be dropped.
    fn produce(
        &mut self,
        mix_l: &mut [f32],
        mix_r: &mut [f32],
        count: usize,
        scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    ) -> bool {
        for i in 0..count {
            if self.carry_pos >= self.carry_len {
                if !self.decode_next_frame(scratch) {
                    return false;
                }
            }

            let sample = self.carry[self.carry_pos] * self.gain;
            self.carry_pos += 1;

            match self.routing {
                Routing::Left => mix_l[i] += sample,
                Routing::Right => mix_r[i] += sample,
                Routing::Both => {
                    mix_l[i] += sample;
                    mix_r[i] += sample;
                }
            }

            if self.fading {
                self.gain -= FADE_STEP;
                if self.gain <= 0.0 {
                    return false;
                }
            }
        }

        true
    }
}

/// Decodes a DMX value into (file_index, looping): 0 = nothing, 1-127 = one-shot
/// files[v-1], 128-255 = looping files[v-128].
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

/// Works out what should be playing for a Left/Right DMX value pair. If both sides
/// resolve to the same (file, loop-mode), that's a single Both-routed entry so it's
/// played as one synced decode instead of two streams that could drift apart.
fn desired_entries(val_l: u8, val_r: u8, num_files: usize) -> [Option<(usize, bool, Routing)>; 2] {
    let l = decode_value(val_l, num_files);
    let r = decode_value(val_r, num_files);

    match (l, r) {
        (Some(lv), Some(rv)) if lv == rv => [Some((lv.0, lv.1, Routing::Both)), None],
        _ => [
            l.map(|(idx, looping)| (idx, looping, Routing::Left)),
            r.map(|(idx, looping)| (idx, looping, Routing::Right)),
        ],
    }
}

/// Reconciles the voice pool for one role (Idle or Effect) against its current
/// Left/Right DMX values. Voices are matched by (file_index, looping) identity, not
/// by which channel triggered them - so a routing-only change (e.g. two sides
/// converging on the same file) just updates in place with no restart/fade, and only
/// an actual file/loop-mode change fades the old voice out while the new one starts
/// immediately at full volume.
fn reconcile_role(
    voices: &mut [Option<Voice>; MAX_VOICES],
    handle: SdHandle,
    role: Role,
    val_l: u8,
    val_r: u8,
    files: &Vec<String<MAX_FILENAME_LEN>, MAX_AUDIO_FILES>,
) {
    let desired = desired_entries(val_l, val_r, files.len());

    for slot in voices.iter_mut() {
        if let Some(v) = slot {
            if v.role == role && !v.fading {
                let still_wanted = desired
                    .iter()
                    .flatten()
                    .any(|d| d.0 == v.file_index && d.1 == v.looping);

                if !still_wanted {
                    v.start_fade();
                }
            }
        }
    }

    for (file_index, looping, routing) in desired.into_iter().flatten() {
        let existing = voices.iter_mut().find_map(|s| {
            s.as_mut()
                .filter(|v| v.role == role && !v.fading && v.file_index == file_index && v.looping == looping)
        });

        if let Some(v) = existing {
            v.routing = routing;
        } else if let Some(slot) = voices.iter_mut().find(|s| s.is_none()) {
            if let Some(name) = files.get(file_index) {
                *slot = Voice::start(handle, role, file_index, looping, routing, name.as_str());
            }
        }
    }
}

async fn fill_and_mix(
    cfg: &AudioConfig,
    handle: SdHandle,
    voices: &mut [Option<Voice>; MAX_VOICES],
    scratch: &mut [f32; MAX_SAMPLES_PER_FRAME],
    out: &mut [u32],
) -> usize {
    let dmx = read_channels::<4>(cfg.universe as usize, cfg.start_channel as usize);
    reconcile_role(voices, handle, Role::Idle, dmx[0], dmx[1], &cfg.idle_files);
    reconcile_role(voices, handle, Role::Effect, dmx[2], dmx[3], &cfg.effect_files);

    let mut out_len = 0usize;

    while out_len + MAX_FRAME_SAMPLES <= out.len() {
        let mut mix_l = [0f32; MAX_FRAME_SAMPLES];
        let mut mix_r = [0f32; MAX_FRAME_SAMPLES];

        for slot in voices.iter_mut() {
            if let Some(voice) = slot {
                if !voice.produce(&mut mix_l, &mut mix_r, MAX_FRAME_SAMPLES, scratch) {
                    *slot = None;
                }
            }
        }

        for i in 0..MAX_FRAME_SAMPLES {
            let l16 = (mix_l[i].clamp(-1.0, 1.0) * VOLUME * 32767.0) as i32 as i16 as u16;
            let r16 = (mix_r[i].clamp(-1.0, 1.0) * VOLUME * 32767.0) as i32 as i16 as u16;
            out[out_len + i] = ((l16 as u32) << 16) | (r16 as u32);
        }

        out_len += MAX_FRAME_SAMPLES;

        // Give other tasks (OLED/DMX/etc) a chance to run every ~26ms of audio,
        // instead of hogging the executor for the whole batch at once.
        yield_now().await;
    }

    out_len
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

    let mut voices: [Option<Voice>; MAX_VOICES] = core::array::from_fn(|_| None);
    let mut scratch = [0f32; MAX_SAMPLES_PER_FRAME];

    let mut buf_a = [0u32; OUT_BUF_LEN];
    let mut buf_b = [0u32; OUT_BUF_LEN];

    let mut len_a = fill_and_mix(&cfg, handle, &mut voices, &mut scratch, &mut buf_a).await;

    loop {
        let (_, len_b) = join(
            i2s.write(&buf_a[..len_a]),
            fill_and_mix(&cfg, handle, &mut voices, &mut scratch, &mut buf_b),
        )
        .await;

        let (_, next_len_a) = join(
            i2s.write(&buf_b[..len_b]),
            fill_and_mix(&cfg, handle, &mut voices, &mut scratch, &mut buf_a),
        )
        .await;
        len_a = next_len_a;
    }
}
