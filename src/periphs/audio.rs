use defmt::println;


use crate::{config::{AudioConfig, MAX_AUDIO_FILES, MAX_FILENAME_LEN}, hardware::AudioResources, periphs::sd::SdHandle};

#[embassy_executor::task]
pub async fn audio_task(cfg: AudioConfig, r: AudioResources, handle: SdHandle) {
    println!("Audio task started.");

    
}
