use core::convert::Infallible;

use defmt::info;

use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};

use embassy_rp::gpio::{Level, Output};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};

use embassy_time::{Delay, Duration, Timer};

use embedded_hal::digital::{ErrorType, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;

use crate::hardware::{EthResources, EthSpi};

struct FakeInt;

impl ErrorType for FakeInt {
    type Error = Infallible;
}

const POLL_INTERVAL: Duration = Duration::from_millis(1);

impl Wait for FakeInt {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        Timer::after(POLL_INTERVAL).await;
        Ok(())
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        Timer::after(POLL_INTERVAL).await;
        Ok(())
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        Timer::after(POLL_INTERVAL).await;
        Ok(())
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        Timer::after(POLL_INTERVAL).await;
        Ok(())
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        Timer::after(POLL_INTERVAL).await;
        Ok(())
    }
}

struct FakeReset;

impl ErrorType for FakeReset {
    type Error = Infallible;
}

impl OutputPin for FakeReset {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

type SpiDev = ExclusiveDevice<Spi<'static, EthSpi, Async>, Output<'static>, Delay>;

#[embassy_executor::task]
async fn eth_task(mut runner: Runner<'static, W5500, SpiDev, FakeInt, FakeReset>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device<'static>>) -> ! {
    runner.run().await
}

pub async fn start_eth(
    spawner: &Spawner,
    r: EthResources
) -> Stack<'static> {
    info!("Starting W5500 Ethernet");

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = 50_000_000;

    let spi = Spi::new(
        r.spi,
        r.sck,
        r.mosi,
        r.miso,
        r.tx_dma,
        r.rx_dma,
        spi_config,
    );

    let cs = Output::new(r.cs, Level::High);
    let spi_device = ExclusiveDevice::new(spi, cs, Delay).unwrap();

    let mac = [0x02, 0x50, 0x44, 0x4D, 0x58, 0x01];

    static W5500_STATE: StaticCell<State<8, 8>> = StaticCell::new();

    let (device, eth_runner) = embassy_net_wiznet::new(
        mac,
        W5500_STATE.init(State::<8, 8>::new()),
        spi_device,
        FakeInt,
        FakeReset,
    )
    .await
    .unwrap();

    spawner.spawn(eth_task(eth_runner)).unwrap();

    static NET_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

    let seed = 0x1234_5678_9ABC_DEF0;

    let (stack, net_runner) = embassy_net::new(
        device,
        Config::dhcpv4(Default::default()),
        NET_RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(net_task(net_runner)).unwrap();

    info!("Waiting for DHCP");
    stack.wait_config_up().await;

    if let Some(config) = stack.config_v4() {
        info!("Ethernet IP: {}", config.address.address());
    }

    stack
}