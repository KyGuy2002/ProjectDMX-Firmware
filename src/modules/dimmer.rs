use embassy_time::{Duration, Timer};
use embassy_rp::pwm::{Config as PwmConfig, Pwm};

use crate::hardware::SlotDDimmerResources;
use crate::read_channels;
use crate::config::DimmerConfig;


#[embassy_executor::task]
pub async fn dimmer_task(settings: DimmerConfig, r: SlotDDimmerResources) {

    let mut cfg1 = PwmConfig::default();
    cfg1.top = 255;
    cfg1.compare_a = 0;
    cfg1.compare_b = 0;

    let mut cfg2 = PwmConfig::default();
    cfg2.top = 255;
    cfg2.compare_a = 0;
    cfg2.compare_b = 0;


    // Order here is hardcoded based on slices and channels
    let mut pwm1 = Pwm::new_output_ab(r.pwm1, r.pin2, r.pin1, cfg1.clone());
    let mut pwm2 = Pwm::new_output_ab(r.pwm0, r.pin4, r.pin3, cfg2.clone());

    loop {
        let ch = read_channels::<4>(settings.universe as usize, settings.start_channel as usize);

        // Order here is hardcoded based on slices and channels
        cfg1.compare_b = ch[0] as u16;
        cfg1.compare_a = ch[1] as u16;
        cfg2.compare_b = ch[2] as u16;
        cfg2.compare_a = ch[3] as u16;

        pwm1.set_config(&cfg1);
        pwm2.set_config(&cfg2);

        Timer::after(Duration::from_millis(10)).await;
    }
    

}