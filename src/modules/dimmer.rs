use embassy_executor::Spawner;
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use embassy_time::{Duration, Timer};

use crate::config::DimmerConfig;
use crate::read_channels;

pub fn spawn_single_a(
    spawner: &Spawner,
    settings: DimmerConfig,
    pwm: Pwm<'static>,
    offset: usize,
) {
    spawner.spawn(single_a_task(settings, pwm, offset)).unwrap();
}

pub fn spawn_single_b(
    spawner: &Spawner,
    settings: DimmerConfig,
    pwm: Pwm<'static>,
    offset: usize,
) {
    spawner.spawn(single_b_task(settings, pwm, offset)).unwrap();
}

pub fn spawn_pair(
    spawner: &Spawner,
    settings: DimmerConfig,
    pwm: Pwm<'static>,
    offset_a: usize,
    offset_b: usize,
) {
    spawner
        .spawn(pair_task(settings, pwm, offset_a, offset_b))
        .unwrap();
}

#[embassy_executor::task(pool_size = 8)]
async fn single_a_task(settings: DimmerConfig, mut pwm: Pwm<'static>, offset: usize) {
    let universe = settings.universe as usize;
    let start_channel = settings.start_channel.saturating_sub(1) as usize;

    let mut cfg = PwmConfig::default();
    cfg.top = 255;
    cfg.compare_a = 0;
    cfg.compare_b = 0;

    loop {
        let channels = read_channels::<4>(universe, start_channel);
        cfg.compare_a = channels[offset] as u16;
        pwm.set_config(&cfg);

        Timer::after(Duration::from_millis(20)).await;
    }
}

#[embassy_executor::task(pool_size = 8)]
async fn single_b_task(settings: DimmerConfig, mut pwm: Pwm<'static>, offset: usize) {
    let universe = settings.universe as usize;
    let start_channel = settings.start_channel.saturating_sub(1) as usize;

    let mut cfg = PwmConfig::default();
    cfg.top = 255;
    cfg.compare_a = 0;
    cfg.compare_b = 0;

    loop {
        let channels = read_channels::<4>(universe, start_channel);
        cfg.compare_b = channels[offset] as u16;
        pwm.set_config(&cfg);

        Timer::after(Duration::from_millis(20)).await;
    }
}

#[embassy_executor::task(pool_size = 8)]
async fn pair_task(
    settings: DimmerConfig,
    mut pwm: Pwm<'static>,
    offset_a: usize,
    offset_b: usize,
) {
    let universe = settings.universe as usize;
    let start_channel = settings.start_channel.saturating_sub(1) as usize;

    let mut cfg = PwmConfig::default();
    cfg.top = 255;
    cfg.compare_a = 0;
    cfg.compare_b = 0;

    loop {
        let channels = read_channels::<4>(universe, start_channel);

        cfg.compare_a = channels[offset_a] as u16;
        cfg.compare_b = channels[offset_b] as u16;

        pwm.set_config(&cfg);

        Timer::after(Duration::from_millis(20)).await;
    }
}