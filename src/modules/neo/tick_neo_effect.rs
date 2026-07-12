use smart_leds::RGB8;

use crate::{MAX_PIXELS, config::EnabledPort, read_channels};




pub fn tick_wire_effect_rgb(port_config: EnabledPort, leds_output: &mut [RGB8; MAX_PIXELS]) {


    // Get DMX params
    let ch = read_channels::<9>(port_config.universe as usize, port_config.start_channel as usize);

    let dmx_params = DmxParams {
        r: ch[0],
        g: ch[1],
        b: ch[2],
        base_effect_id: ch[3],
        top_effect_id: ch[4],
        speed: ch[5],
        r2: ch[6],
        g2: ch[7],
        b2: ch[8],
    };


    

}







#[derive(Clone, Copy, PartialEq, Debug, defmt::Format)]
pub struct DmxParams {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub base_effect_id: u8,
    pub top_effect_id: u8,
    pub speed: u8,
    pub r2: u8,
    pub g2: u8,
    pub b2: u8,
}

impl Default for DmxParams {
    fn default() -> Self {
        Self {
            r: 255,
            g: 0,
            b: 0,
            base_effect_id: 8,
            top_effect_id: 0,
            speed: 4,
            r2: 0,
            g2: 0,
            b2: 0,
        }
    }
}