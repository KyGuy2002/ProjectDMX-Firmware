use defmt::{info, warn};

use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH1, DMA_CH2, PIN_24, PIN_25, UART1};
use embassy_rp::uart::{
    Async, Config as UartConfig, DataBits, InterruptHandler, Parity, StopBits, Uart, UartRx, UartTx,
};
use embassy_rp::Peri;

use embassy_time::{Duration, Timer};

use crate::config::InputProtocol;
use crate::hardware::DmxPins;
use crate::{CONFIG, DMX_MATRIX, MAX_UNIVERSES};

bind_interrupts!(struct Irqs {
    UART1_IRQ => InterruptHandler<UART1>;
});

#[embassy_executor::task]
pub async fn dmx_task(
    uart: Peri<'static, UART1>,
    tx_pin: Peri<'static, PIN_24>,
    rx_pin: Peri<'static, PIN_25>,
    pins: DmxPins,
    tx_dma: Peri<'static, DMA_CH1>,
    rx_dma: Peri<'static, DMA_CH2>,
) {
    let config = CONFIG.get().await;
    let is_read = config.input.source == InputProtocol::Dmx;

    // RS-485 transceiver enable.
    // Low = receive mode.
    // High = transmit mode.
    let _rs485_enable = Output::new(
        pins.mode,
        if is_read { Level::Low } else { Level::High },
    );

    // DMX UART: 250000 baud, 8N2.
    let mut dmx_uart_cfg = UartConfig::default();
    dmx_uart_cfg.baudrate = 250_000;
    dmx_uart_cfg.data_bits = DataBits::DataBits8;
    dmx_uart_cfg.stop_bits = StopBits::STOP2;
    dmx_uart_cfg.parity = Parity::ParityNone;

    let dmx_uart = Uart::new(
        uart,
        tx_pin,
        rx_pin,
        Irqs,
        tx_dma,
        rx_dma,
        dmx_uart_cfg,
    );

    let (dmx_tx, dmx_rx) = dmx_uart.split();

    if is_read {
        run_dmx_input_loop(dmx_rx).await;
    } else {
        run_dmx_output_loop(dmx_tx).await;
    }
}

async fn run_dmx_input_loop(mut dmx_rx: UartRx<'static, Async>) {
    info!("        DMX Receiver Initialized       ");
    info!("          - Writing to Universe 0      ");
    info!("");

    let mut frame = [0u8; 513];

    loop {
        match dmx_rx.read(&mut frame).await {
            Ok(_) => {
                if frame[0] == 0 {
                    DMX_MATRIX.lock(|matrix| {
                        matrix.borrow_mut()[0].copy_from_slice(&frame[1..513]);
                    });
                }
            }

            Err(embassy_rp::uart::Error::Break) => {}

            Err(e) => {
                warn!("DMX connection drop or frame issue: {:?}", e);
            }
        }
    }
}

async fn run_dmx_output_loop(mut dmx_tx: UartTx<'static, Async>) {
    let config = CONFIG.get().await;
    let universe_id = config.dmx_output.universe as usize;

    info!("      DMX Transmitter Initialized      ");
    info!("        - Reading from Universe {}     ", universe_id);
    info!("");

    let mut tx_frame = [0u8; 513];

    loop {
        if universe_id < MAX_UNIVERSES {
            DMX_MATRIX.lock(|matrix| {
                let buf = matrix.borrow();
                let active_matrix: &[u8] = &buf[universe_id];

                tx_frame[0] = 0x00;
                tx_frame[1..513].copy_from_slice(active_matrix);
            });
        }

        let _ = dmx_tx.send_break(22).await;

        match dmx_tx.write(&tx_frame).await {
            Ok(_) => {}
            Err(e) => warn!("DMX transmit line drop error: {:?}", e),
        }

        Timer::after(Duration::from_millis(20)).await;
    }
}