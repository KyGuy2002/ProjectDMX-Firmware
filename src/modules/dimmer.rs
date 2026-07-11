use defmt::info;

use embassy_executor::Spawner;
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use embassy_time::{Duration, Timer};

use crate::config::DimmerConfig;
use crate::hardware::SlotPwm;
use crate::read_channels;

pub fn spawn_dimmer<S>(
    spawner: &Spawner,
    settings: DimmerConfig,
    slot: S,
) where
    S: SlotPwm + 'static,
{
    let mut cfg = PwmConfig::default();
    cfg.top = 255;
    cfg.compare_a = 0;
    cfg.compare_b = 0;

    let pwm = slot.into_pwm(cfg);

    spawner.spawn(dimmer_task(settings, pwm)).unwrap();
}

#[embassy_executor::task(pool_size = 4)]
async fn dimmer_task(settings: DimmerConfig, mut pwm: Pwm<'static>) {
    let universe = settings.universe as usize;
    let start_channel = settings.start_channel.saturating_sub(1) as usize;

    let mut cfg = PwmConfig::default();
    cfg.top = 255;

    loop {
        let ch = read_channels::<1>(universe, start_channel);
        let value = ch[0] as u16;

        cfg.compare_a = value;
        cfg.compare_b = value;

        pwm.set_config(&cfg);

        info!("Dimmer PWM: {}", value);

        Timer::after(Duration::from_millis(10)).await;
    }
}