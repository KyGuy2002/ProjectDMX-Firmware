use embassy_rp::gpio::{Level, Output};
use embassy_rp::uart::{Config as UartConfig, UartRx};
use embassy_rp::peripherals::UART1;
use crate::hardware::DmxPins;

#[embassy_executor::task]
pub async fn dmx_task(uart_peripheral: UART1, pins: DmxPins, rx_dma: embassy_rp::peripherals::DMA_CH1) {

    let is_read = (CONFIG.get().unwrap().input.source == InputProtocol::Dmx);
    
    // RS-485 transceiver enable, low for receive mode
    let _rs485_enable = Output::new(pins.mode.into_ref(),
        if is_read { Level::Low } else { Level::High }
    );

    // DMX UART Peripheral initialization: 250000 baud, 8N2.
    let mut dmx_uart_cfg = UartConfig::default();
    dmx_uart_cfg.baudrate = 250_000;
    dmx_uart_cfg.data_bits = DataBits::DataBits8;
    dmx_uart_cfg.stop_bits = StopBits::STOP2;
    dmx_uart_cfg.parity = Parity::ParityNone;

    let dmx_uart = Uart::new(
        uart_peripheral,
        pins.tx,
        pins.rx,
        Irqs,
        rx_dma,
        pins.dma_tx,
        dmx_uart_cfg,
    );

    let (dmx_tx, dmx_rx) = dmx_uart.split();


    if is_read {
        run_dmx_input_loop(dmx_rx).await;
    } else {
        run_dmx_output_loop(dmx_tx).await;
    }

}



async fn run_dmx_input_loop(
    mut dmx_rx: UartRx<'_, UART1, Async>,
) {

    //     =======================================
    info!("        DMX Receiver Initialized       ");
    info!("          - Writing to Universe 0      ");
    info!("");

    let mut frame = [0u8; 513];
    loop {
        match dmx_rx.read(&mut frame).await {
            Ok(_) => {
                // Ensure Null Start Code (0x00) lighting frame
                if frame[0] == 0 {
                    DMX_MATRIX.lock(|matrix| {
                        matrix.borrow_mut()[0].copy_from_slice(&frame[1..513]);
                    });
                }
            }
            Err(embassy_rp::uart::Error::Break) => {}
            Err(embassy_rp::uart::Error::Framing) => {}
            Err(e) => {
                warn!("DMX connection drop or frame issue: {:?}", e);
            }
        }
    }
}


// TODO make actually 20ms not 20ms + execution time
async fn run_dmx_output_loop(
    mut dmx_tx: UartTx<'_, UART1, Async>,
) {
    let universe_id = CONFIG.get().unwrap().dmx_output.universe;
    
    info!("      DMX Transmitter Initialized      ");
    info!("        - Reading from Universe {}     ", universe_id);
    info!("");
    
    let mut tx_frame = [0u8; 513];

    loop {
        if universe_id < MAX_UNIVERSES {
            DMX_MATRIX.lock(|matrix| {
                let buf = matrix.borrow();
                let active_matrix: &[u8] = &buf[universe_id];
                
                tx_frame[0] = 0x00; // Null Start Code
                tx_frame[1..513].copy_from_slice(active_matrix);
            });
        }

        // Send continuous packets every loop cycle regardless of state changes
        _ = dmx_tx.send_break(22).await; 

        match dmx_tx.write(&tx_frame).await {
            Ok(_) => {}
            Err(e) => warn!("DMX transmit line drop error: {:?}", e),
        }

        // Enforce the steady 20ms (50Hz) frame rate interval
        Timer::after(Duration::from_millis(20)).await;
    }
}