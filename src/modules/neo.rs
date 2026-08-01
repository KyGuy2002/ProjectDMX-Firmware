use embassy_rp::pio::Pio;
use embassy_time::{Duration, Ticker};
use defmt::info;

use embassy_rp::pio_programs::ws2812::{Rgb, Rgbw, Grb, Grbw, PioWs2812, RgbwPioWs2812, PioWs2812Program};
use smart_leds::{RGB8, RGBW, White};

use crate::hardware::{SlotCNeoResources, NeoIrqs, NEO_PROGRAM};
use crate::read_channels;
use crate::config::*;
use embassy_rp::{peripherals};
use crate::MAX_PIXELS;
use heapless::Vec;

mod neo_effects_2d;
mod tick_neo_effect;
use tick_neo_effect::*;
use crate::pixel_mapping_config::get_layout_map;



#[embassy_executor::task]
pub async fn neo_task(settings: NeoConfig, r: SlotCNeoResources) {

    info!("Starting NeoPixel task");

    // Setup PIO state machines
    let Pio { mut common, sm0, sm1, sm2, sm3, .. } = Pio::new(r.pio, NeoIrqs);
    let program = NEO_PROGRAM.init(PioWs2812Program::new(&mut common));



    // Init drivers - Note pin order is based on module layout
    let mut strip_1 = RgbwPioWs2812::<peripherals::PIO0, 0, MAX_PIXELS, Rgbw>::with_color_order(&mut common,sm0,r.dma1,r.pin3,program);
    // let mut ws2812_2 = PioWs2812::<peripherals::PIO0, 1, MAX_PIXELS, Grb>::with_color_order(&mut common,sm1,r.dma2,r.pin4,program);
    let mut strip_3 = PioWs2812::<peripherals::PIO0, 2, MAX_PIXELS, Grb>::with_color_order(&mut common,sm2,r.dma3,r.pin2,program);
    // let mut ws2812_4 = PioWs2812::<peripherals::PIO0, 3, MAX_PIXELS, Grb>::with_color_order(&mut common,sm3,r.dma4,r.pin1,program);



    // Setup LED buffers
    let mut leds_output_1 = [RGBW::<u8>::default(); MAX_PIXELS];
    // let mut leds_output_2 = [RGB8::default(); MAX_PIXELS];
    let mut leds_output_3 = [RGB8::default(); MAX_PIXELS];
    // let mut leds_output_4 = [RGB8::default(); MAX_PIXELS];



    // Setup effect state
    let layout_table = get_layout_map();
    let mut strip_3_effect_state = NeoEffectState::new();
    let mut strip_1_effect_state = NeoEffectState::new();



    // Setup ticker
    let mut ticker = Ticker::every(Duration::from_millis(10)); // Clean ~50FPS Refresh rate



    loop {


        // Strip 1
        if let Port::Enabled(port_config) = settings.ports[0] {

            if port_config.mode == NeoMode::SolidColor {
                tick_solid_color_rgbw(port_config, &mut leds_output_1);
            } else if port_config.mode == NeoMode::Generator2D {
                tick_wire_effect_rgbw(port_config, &mut strip_1_effect_state, &layout_table, &mut leds_output_1);
            }
            else if port_config.mode == NeoMode::Raw {
                tick_raw_rgbw(port_config, &mut leds_output_1);
            }
        }

        // Strip 3
        if let Port::Enabled(port_config) = settings.ports[2] {

            if port_config.mode == NeoMode::SolidColor {
                tick_solid_color_rgb(port_config, &mut leds_output_3);
            } else if port_config.mode == NeoMode::Generator2D {
                tick_wire_effect_rgb(port_config, &mut strip_3_effect_state, &layout_table, &mut leds_output_3);
            }
            else if port_config.mode == NeoMode::Raw {
                tick_raw_rgb(port_config, &mut leds_output_3);
            }
            
        }

        
        
        embassy_futures::join::join( // <- change to join3 or join4
            strip_1.write(&leds_output_1),
            // strip_2.write(&leds_output_2]),
            strip_3.write(&leds_output_3),
            // strip_4.write(&leds_output_4),
        ).await;

        ticker.next().await;

    }

}


fn tick_solid_color_rgbw(port_config: EnabledPort, leds_output: &mut [RGBW<u8>; MAX_PIXELS]) {

    let ch = read_channels::<5>(port_config.universe as usize, port_config.start_channel as usize);

    for i in 0..port_config.pixel_count {
        leds_output[i] = RGBW {
            r: ch[0],
            g: ch[1],
            b: ch[2],
            a: White(ch[3]),
        };
    }

}

fn tick_solid_color_rgb(port_config: EnabledPort, leds_output: &mut [RGB8; MAX_PIXELS]) {

    let ch = read_channels::<4>(port_config.universe as usize, port_config.start_channel as usize);

    for i in 0..port_config.pixel_count {
        leds_output[i] = RGB8 {
            r: ch[0],
            g: ch[1],
            b: ch[2]
        };
    }

}


fn tick_raw_rgbw(port_config: EnabledPort, leds_output: &mut [RGBW<u8>; MAX_PIXELS]) {

    let mut current_universe = port_config.universe as usize;
    let mut current_channel = port_config.start_channel as usize;

    for i in 0..port_config.pixel_count {

        let dmx = read_channels::<4>(current_universe, current_channel);
        current_channel += 4;
        if current_channel >= 512 {
            current_channel = 0;
            current_universe += 1;
        }

        leds_output[i] = RGBW {
            r: dmx[0],
            g: dmx[1],
            b: dmx[2],
            a: White(dmx[3])
        };

    }

}



fn tick_raw_rgb(port_config: EnabledPort, leds_output: &mut [RGB8; MAX_PIXELS]) {

    let mut current_universe = port_config.universe as usize;
    let mut current_channel = port_config.start_channel as usize;

    for i in 0..port_config.pixel_count {

        let dmx = read_channels::<3>(current_universe, current_channel);
        current_channel += 3;
        if current_channel >= 512 {
            current_channel = 0;
            current_universe += 1;
        }

        leds_output[i] = RGB8 {
            r: dmx[0],
            g: dmx[1],
            b: dmx[2],
        };

    }

}