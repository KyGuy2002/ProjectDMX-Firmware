use core::convert::Infallible;

use core::fmt::Write;

use defmt::info;

use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};

use embassy_rp::gpio::{Level, Output};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};

use embassy_time::{Delay, Duration, Timer};

use embedded_hal::digital::{ErrorType, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;
use embassy_net::Ipv4Address;
use embassy_sync::once_lock::OnceLock;
use heapless::String;

use crate::hardware::{EthResources, EthSpi};

/// Who this board is on the network. Published once the address is applied;
/// read by the OLED and the mDNS responder.
pub struct NetIdentity {
    pub ip: Ipv4Address,
    /// mDNS host label, e.g. "pdmx-a1b2" (advertised as "pdmx-a1b2.local").
    pub hostname: String<16>,
    /// DNS-SD service instance name, e.g. "PDMX Controller A1B2".
    pub instance: String<24>,
}

pub static NET_IDENTITY: OnceLock<NetIdentity> = OnceLock::new();

/// The factory-programmed OTP chip ID. A board that can't read it has no unique
/// identity to build a MAC from, and a shared MAC would collide on the network,
/// so refuse to boot rather than fall back.
fn chip_id() -> u64 {
    match embassy_rp::otp::get_chipid() {
        Ok(id) => id,
        Err(_) => defmt::panic!("OTP chip ID unreadable - refusing to boot without a unique MAC"),
    }
}

/// Locally administered unicast MAC (02:xx:xx:xx:xx:xx) from the RP2350's
/// factory-programmed random 64-bit chip ID.
fn mac_from_chip_id(id: u64) -> [u8; 6] {
    let b = id.to_be_bytes();
    [0x02, b[3], b[4], b[5], b[6], b[7]]
}

/// RFC 3927 link-local address (169.254.1.0 - 169.254.254.255) picked
/// deterministically from the MAC. There is no ARP probing, so two boards only
/// clash if their MACs hash to the same address.
fn link_local_from_mac(mac: &[u8; 6]) -> Ipv4Address {
    let h = u32::from_be_bytes([0, mac[3], mac[4], mac[5]]);
    let third = 1 + (h % 254) as u8;
    let fourth = 1 + ((h / 254) % 254) as u8;
    Ipv4Address::new(169, 254, third, fourth)
}

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
    r: EthResources,
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

    let chip_id = chip_id();
    let mac = mac_from_chip_id(chip_id);
    let ip = link_local_from_mac(&mac);

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

    static NET_RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();

    // Randomizes TCP ephemeral ports / sequence numbers per board.
    let seed = chip_id;

    let (stack, net_runner) = embassy_net::new(
        device,
        Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(ip, 16),
            gateway: None,
            dns_servers: Default::default(),
        }),
        NET_RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(net_task(net_runner)).unwrap();

    // Config is applied immediately, independent of link state, so a missing
    // cable doesn't hold up boot.
    stack.wait_config_up().await;

    let mut hostname: String<16> = String::new();
    let mut instance: String<24> = String::new();
    // All caps: DNS names are case-insensitive (the mDNS code compares with
    // eq_ignore_ascii_case throughout), so this is purely how it's displayed.
    write!(hostname, "PDMX-{:02X}{:02X}", mac[4], mac[5]).unwrap();
    write!(instance, "PDMX CONTROLLER {:02X}{:02X}", mac[4], mac[5]).unwrap();

    info!("Ethernet: {}.local  {}/16", hostname.as_str(), ip);
    NET_IDENTITY.init(NetIdentity { ip, hostname, instance }).ok();

    stack
}