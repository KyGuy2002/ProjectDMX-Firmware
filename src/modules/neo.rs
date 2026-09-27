use embassy_futures::join::join4;
use embassy_rp::Peri;
use embassy_rp::dma::Channel;
use embassy_rp::peripherals::PIO0;
use embassy_rp::pio::{Common, Pio, PioPin, StateMachine};
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program, Rgb, Rgbw, RgbwPioWs2812};
use embassy_time::{Duration, Ticker};
use defmt::info;

use smart_leds::{RGB8, RGBW, White};

use crate::MAX_PIXELS;
use crate::config::*;
use crate::hardware::{NEO_PROGRAM, NeoIrqs, SlotCNeoResources};
use crate::pixel_mapping_config::{PixelMeta, get_layout_map};
use crate::read_channels;
use crate::settings::{Slot, output_enabled};

mod neo_effects_2d;
mod tick_neo_effect;
use tick_neo_effect::*;

/// One WS2812 output. RGB strips clock 24 bits per pixel, RGBW 32, so embassy's
/// two driver types can't share a variable - this thin enum picks one at boot
/// from the port's color order. The byte reordering (GRB, BGR, ...) is done here
/// in software rather than baked into the driver generic, so it stays fully
/// config-driven.
enum NeoStrip<'d, const S: usize> {
    Rgb(PioWs2812<'d, PIO0, S, MAX_PIXELS, Rgb>),
    Rgbw(RgbwPioWs2812<'d, PIO0, S, MAX_PIXELS, Rgbw>),
}

impl<'d, const S: usize> NeoStrip<'d, S> {
    fn new(
        common: &mut Common<'d, PIO0>,
        sm: StateMachine<'d, PIO0, S>,
        dma: Peri<'d, impl Channel>,
        pin: Peri<'d, impl PioPin>,
        program: &PioWs2812Program<'d, PIO0>,
        four_channel: bool,
    ) -> Self {
        // embassy's own drivers order the bytes via the ORDER generic; we always
        // ask for the identity order (Rgb / Rgbw) and permute ourselves in render().
        if four_channel {
            NeoStrip::Rgbw(RgbwPioWs2812::with_color_order(common, sm, dma, pin, program))
        } else {
            NeoStrip::Rgb(PioWs2812::with_color_order(common, sm, dma, pin, program))
        }
    }

    /// Reorders `leds` (logical RGBW) into this strip's wire order and pushes one
    /// frame. Always sends `MAX_PIXELS` pixels; the generator left everything past
    /// `pixel_count` black, and a strip with fewer LEDs just ignores the tail.
    async fn render(&mut self, leds: &[RGBW<u8>; MAX_PIXELS], order: ColorOrder) {
        let p = order.perm();
        match self {
            NeoStrip::Rgb(driver) => {
                let mut buf = [RGB8::default(); MAX_PIXELS];
                for (out, led) in buf.iter_mut().zip(leds) {
                    let src = [led.r, led.g, led.b, led.a.0];
                    *out = RGB8 {
                        r: src[p[0] as usize],
                        g: src[p[1] as usize],
                        b: src[p[2] as usize],
                    };
                }
                driver.write(&buf).await;
            }
            NeoStrip::Rgbw(driver) => {
                let mut buf = [RGBW::<u8>::default(); MAX_PIXELS];
                for (out, led) in buf.iter_mut().zip(leds) {
                    let src = [led.r, led.g, led.b, led.a.0];
                    *out = RGBW {
                        r: src[p[0] as usize],
                        g: src[p[1] as usize],
                        b: src[p[2] as usize],
                        a: White(src[p[3] as usize]),
                    };
                }
                driver.write(&buf).await;
            }
        }
    }
}

fn is_four_channel(port: Port) -> bool {
    matches!(port, Port::Enabled(pc) if pc.color_order.perm().len() == 4)
}

#[embassy_executor::task]
pub async fn neo_task(settings: NeoConfig, r: SlotCNeoResources) {
    info!("Starting NeoPixel task");

    let Pio { mut common, sm0, sm1, sm2, sm3, .. } = Pio::new(r.pio, NeoIrqs);
    let program = NEO_PROGRAM.init(PioWs2812Program::new(&mut common));

    // Physical port -> slot-C connector pin, fixed by the PCB (see hardware.rs):
    //   port 0 -> pin3   port 1 -> pin4   port 2 -> pin2   port 3 -> pin1
    let mut strip0 = NeoStrip::<0>::new(&mut common, sm0, r.dma1, r.pin3, program, is_four_channel(settings.ports[0]));
    let mut strip1 = NeoStrip::<1>::new(&mut common, sm1, r.dma2, r.pin4, program, is_four_channel(settings.ports[1]));
    let mut strip2 = NeoStrip::<2>::new(&mut common, sm2, r.dma3, r.pin2, program, is_four_channel(settings.ports[2]));
    let mut strip3 = NeoStrip::<3>::new(&mut common, sm3, r.dma4, r.pin1, program, is_four_channel(settings.ports[3]));

    let layout = get_layout_map();
    let mut states = [
        NeoEffectState::new(),
        NeoEffectState::new(),
        NeoEffectState::new(),
        NeoEffectState::new(),
    ];
    let mut leds = [[RGBW::<u8>::default(); MAX_PIXELS]; 4];

    let mut ticker = Ticker::every(Duration::from_millis(20)); // ~50 FPS

    loop {
        for (i, port) in settings.ports.iter().enumerate() {
            if let Port::Enabled(pc) = *port {
                generate(pc, &mut states[i], &layout, &mut leds[i]);
                // Port disabled by the mask mode: keep generating (so effects
                // don't jump when it comes back) but send black.
                if !output_enabled(Slot::C, i) {
                    leds[i].fill(RGBW::default());
                }
            }
        }

        join4(
            maybe_render(&mut strip0, settings.ports[0], &leds[0]),
            maybe_render(&mut strip1, settings.ports[1], &leds[1]),
            maybe_render(&mut strip2, settings.ports[2], &leds[2]),
            maybe_render(&mut strip3, settings.ports[3], &leds[3]),
        )
        .await;

        ticker.next().await;
    }
}

async fn maybe_render<const S: usize>(
    strip: &mut NeoStrip<'_, S>,
    port: Port,
    leds: &[RGBW<u8>; MAX_PIXELS],
) {
    if let Port::Enabled(pc) = port {
        strip.render(leds, pc.color_order).await;
    }
}

/// Runs the port's generator into `leds` as logical RGBW (white 0 for the RGB
/// generators). Wire color order is applied later, in `NeoStrip::render`.
fn generate(
    port: EnabledPort,
    state: &mut NeoEffectState,
    layout: &[PixelMeta; MAX_PIXELS],
    leds: &mut [RGBW<u8>; MAX_PIXELS],
) {
    let count = port.pixel_count.min(MAX_PIXELS);

    match port.mode {
        NeoMode::SolidColor => {
            let ch = read_channels::<4>(port.universe as usize, port.start_channel as usize);
            let color = RGBW { r: ch[0], g: ch[1], b: ch[2], a: White(ch[3]) };
            for led in leds.iter_mut().take(count) {
                *led = color;
            }
        }

        NeoMode::Generator2D => tick_wire_effect_rgbw(port, state, layout, leds),

        NeoMode::Raw => {
            let bytes_per_pixel = port.color_order.perm().len();
            let mut universe = port.universe as usize;
            let mut channel = port.start_channel as usize;

            for led in leds.iter_mut().take(count) {
                let px = read_channels::<4>(universe, channel);
                *led = RGBW {
                    r: px[0],
                    g: px[1],
                    b: px[2],
                    a: White(if bytes_per_pixel >= 4 { px[3] } else { 0 }),
                };

                channel += bytes_per_pixel;
                if channel > 512 {
                    channel -= 512;
                    universe += 1;
                }
            }
        }
    }
}
