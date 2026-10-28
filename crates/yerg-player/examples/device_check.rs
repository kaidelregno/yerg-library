//! Drive the real player against this machine's audio devices, muted.
//!   cargo run --release -p yerg-player --example device_check -- <audio file>
use std::time::Duration;
use yerg_player::{Player, PlayerEvent};

fn show(p: &Player, what: &str) {
    let s = p.state();
    println!(
        "{what:<34} device={:?} format={:?} pos={:.2}s listened={}ms loaded={} err={:?}",
        s.output_device, s.output_format, s.position.as_secs_f64(), s.played_ms, s.loaded, s.output_error
    );
}

fn main() {
    let file = std::env::args().nth(1).expect("pass an audio file");
    let devices = Player::output_devices();
    println!("default: {:?}", Player::default_output_device());
    for d in &devices {
        println!("device:  {d}");
    }
    let p = Player::with_device(None);
    p.set_volume(0.0); // silent: the device still pulls audio, nothing is heard
    p.load(&file);
    std::thread::sleep(Duration::from_millis(1500));
    show(&p, "default device, after 1.5 s");
    p.seek(Duration::from_millis(500));
    std::thread::sleep(Duration::from_millis(300));
    show(&p, "after seek to 0.5 s (+0.3 s)");
    for d in devices.iter().filter(|d| Some(*d) != Player::default_output_device().as_ref()) {
        p.set_output_device(Some(d.clone()));
        std::thread::sleep(Duration::from_millis(1200));
        show(&p, &format!("switched to {d}"));
    }
    p.set_output_device(Some("No Such Device".into()));
    std::thread::sleep(Duration::from_millis(800));
    show(&p, "asked for a missing device");
    p.set_output_device(None);
    std::thread::sleep(Duration::from_millis(800));
    show(&p, "back to system default");
    // Run to the end
    let start = std::time::Instant::now();
    loop {
        let ev = p.poll_events();
        for e in &ev {
            match e {
                PlayerEvent::TrackEnded { .. } => {
                    show(&p, "track ended");
                    println!("events ok");
                    return;
                }
                PlayerEvent::Error { message, .. } => println!("ERROR: {message}"),
                PlayerEvent::OutputLost { device, reason } => println!("output lost: {device}: {reason}"),
                PlayerEvent::OutputReady { device } => println!("output ready: {device}"),
            }
        }
        if start.elapsed() > Duration::from_secs(15) {
            show(&p, "TIMEOUT waiting for end");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
