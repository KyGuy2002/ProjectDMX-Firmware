use embassy_rp::pio::Pio;
use embassy_time::{Duration, Ticker};

use embassy_rp::pio_programs::ws2812::{Rgb, Rgbw, Grb, Grbw, RgbwPioWs2812, PioWs2812, PioWs2812Program};
use smart_leds::{RGB8, RGBW, White};

use crate::hardware::{SlotCNeoResources, NeoIrqs, NEO_PROGRAM};
use crate::read_channels;
use crate::config::*;
use embassy_rp::{peripherals};
use crate::MAX_PIXELS;
use embassy_executor::Spawner;
use embassy_futures::join::join4;



pub fn setup_neo_task(spawner: &Spawner, settings: NeoConfig, r: SlotCNeoResources) {


    // Setup PIO state machines
    let Pio { mut common, sm0, sm1, sm2, sm3, .. } = Pio::new(r.pio, NeoIrqs);
    let program = NEO_PROGRAM.init(PioWs2812Program::new(&mut common));


    // Start strip 1 task
    if let Port::Enabled(port_config) = settings.ports[0] {

        let strip = RgbwPioWs2812::<peripherals::PIO0, 0, MAX_PIXELS, Rgbw>::with_color_order(&mut common,sm0,r.dma1,r.pin1,program);
        let leds_output = [RGBW::<u8>::default(); MAX_PIXELS];

        spawner.spawn(neo_single_color_rgbw(port_config, strip, leds_output)).unwrap();
    }

    // Start strip 3 task
    if let Port::Enabled(port_config) = settings.ports[2] {

        let strip = PioWs2812::<peripherals::PIO0, 1, MAX_PIXELS, Grb>::with_color_order(&mut common,sm1,r.dma2,r.pin2,program);
        let leds_output = [RGB8::default(); MAX_PIXELS];

        spawner.spawn(neo_single_color_grb(port_config, strip, leds_output)).unwrap();
    }


}



#[embassy_executor::task(pool_size = 4)]
async fn neo_single_color_rgbw<const SM: usize>(port_config: EnabledPort, mut strip : RgbwPioWs2812<'static, peripherals::PIO0, SM, MAX_PIXELS, Rgbw>, mut leds_output: [RGBW<u8>; MAX_PIXELS]) {

    // Setup ticker
    let mut ticker = Ticker::every(Duration::from_millis(20)); // Clean ~50FPS Refresh rate
    
    loop {

        let ch = read_channels::<5>(port_config.universe as usize, port_config.start_channel as usize);


        for i in 0..port_config.pixel_count {
            leds_output[i] = RGBW {
                r: (ch[0] as u32 / 255) as u8,
                g: (ch[1] as u32 / 255) as u8,
                b: (ch[2] as u32 / 255) as u8,
                a: White((ch[3] as u32 / 255) as u8),
            };
        }
        

        strip.write(&leds_output).await;
        ticker.next().await;

    }

}



#[embassy_executor::task]
async fn neo_single_color_grb(port_config: EnabledPort, mut strip : PioWs2812<'static, peripherals::PIO0, 0, MAX_PIXELS, Grb>, mut leds_output: [RGB8; MAX_PIXELS]) {

    // Setup ticker
    let mut ticker = Ticker::every(Duration::from_millis(20)); // Clean ~50FPS Refresh rate
    
    loop {

        let ch = read_channels::<5>(port_config.universe as usize, port_config.start_channel as usize);


        for i in 0..port_config.pixel_count {
            leds_output[i] = RGB8 {
                r: (ch[0] as u32 / 255) as u8,
                g: (ch[1] as u32 / 255) as u8,
                b: (ch[2] as u32 / 255) as u8
            };
        }
        

        strip.write(&leds_output).await;
        ticker.next().await;

    }

}






/*
 * // Init drivers
    
    // let mut ws2812_2 = PioWs2812::<peripherals::PIO0, 1, MAX_PIXELS, Grb>::new(&mut common,sm1,r.dma2,r.pin2,program);
    let mut ws2812_3 = PioWs2812::<peripherals::PIO0, 2, MAX_PIXELS, Grb>::new(&mut common,sm2,r.dma3,r.pin3,program);
    // let mut ws2812_4 = PioWs2812::<peripherals::PIO0, 3, MAX_PIXELS, Grb>::new(&mut common,sm3,r.dma4,r.pin4,program);



    // Setup LED buffers
    
    // let mut leds_output_2 = [RGB8::default(); MAX_PIXELS];
    let mut leds_output_3 = [RGB8::default(); MAX_PIXELS];
    // let mut leds_output_4 = [RGB8::default(); MAX_PIXELS];
 */