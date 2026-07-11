use defmt::*;
use embassy_rp::uart::{Async, UartRx};
use crate::dmx_state::{DmxParams, DMX_SIGNAL};

#[embassy_executor::task]
pub async fn dmx_rx_task(mut rx: UartRx<'static, embassy_rp::peripherals::UART1, Async>) {
    let mut frame = [0u8; 513];
    loop {
        match rx.read(&mut frame).await {
            Ok(_) => {
                const START_CH: usize = 97;

                let extracted = DmxParams {
                    r: frame[START_CH + 0],
                    g: frame[START_CH + 1],
                    b: frame[START_CH + 2],
                    base_effect_id: frame[START_CH + 3],
                    top_effect_id: frame[START_CH + 4],
                    speed: frame[START_CH + 5],
                    r2: frame[START_CH + 6],
                    g2: frame[START_CH + 7],
                    b2: frame[START_CH + 8],
                };
                DMX_SIGNAL.signal(extracted);
            }
            Err(embassy_rp::uart::Error::Break) => {}
            Err(embassy_rp::uart::Error::Framing) => {}
            Err(e) => {
                warn!("DMX connection drop or frame issue: {:?}", e);
            }
        }
    }
}