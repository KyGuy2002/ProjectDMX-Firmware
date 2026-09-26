use embassy_time::{Duration, Timer};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use defmt::info;

use crate::config::FogMachineConfig;
use crate::hardware::SlotARelayResources;
use crate::read_channels;

/// Slot A fog machine: one relay driven from a single DMX channel.
///   pin1 = input (pull-up), status line from the fog machine, logged on change
///   pin2 = relay output, inverted: held HIGH (relay energized) at idle, pulled
///          LOW above DMX 127. The relay's NO/NC contacts are wired backwards,
///          so de-energizing it is what triggers the fog machine.
#[embassy_executor::task]
pub async fn fog_task(settings: FogMachineConfig, r: SlotARelayResources) {
    info!("Starting fog task (slot A)");

    let status = Input::new(r.pin1, Pull::Up);
    let mut relay = Output::new(r.pin2, Level::High);
    let mut last_status = status.is_high();

    loop {
        let [level] = read_channels::<1>(settings.universe as usize, settings.start_channel as usize);
        relay.set_level(if level > 127 { Level::Low } else { Level::High });

        let s = status.is_high();
        if s != last_status {
            info!("Fog status input: {}", s);
            last_status = s;
        }

        Timer::after(Duration::from_millis(20)).await;
    }
}
