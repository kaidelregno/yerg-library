//! Diagnostic: print the default output device and its preferred format.
use rodio::cpal::traits::{DeviceTrait, HostTrait};
fn main() {
    let host = rodio::cpal::default_host();
    let dev = host.default_output_device().expect("no output device");
    println!("device: {}", dev.name().unwrap_or_default());
    match dev.default_output_config() {
        Ok(c) => println!("default config: {} ch, {} Hz, {:?}, buffer {:?}", c.channels(), c.sample_rate().0, c.sample_format(), c.buffer_size()),
        Err(e) => println!("default config error: {e}"),
    }
}
