mod config;
mod ipc;
mod media_store;
mod protocol;
mod render;
mod sensors;
mod state;
mod usb;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use state::Mode;
use usb::Lm360;

fn main() {
    println!("coolctld: connecting to LM360...");
    let device = match Lm360::connect() {
        Ok(d) => d,
        Err(e) => {
            // Don't exit(1) here: a boot-time USB enumeration race (device not
            // yet present) would otherwise crash-loop the process and can burn
            // through systemd's default restart-burst limit, landing the unit
            // in `failed` with nothing driving the panel until someone notices
            // and restarts it manually. Wait it out instead, same as the
            // mid-run disconnect recovery below — this only polls libusb
            // open/claim every 2s, it never touches the panel itself, so it
            // can't trigger the earlier frame-flood wedge issue.
            eprintln!("failed to connect: {e} — waiting for device...");
            Lm360::connect_retrying(Duration::from_secs(2))
        }
    };
    let device = Arc::new(Mutex::new(device));

    if let Err(e) = device.lock().unwrap().init_query() {
        eprintln!("init_query failed: {e}");
    }
    std::thread::sleep(Duration::from_millis(500));

    println!("loading fonts...");
    let fonts = render::load_fonts();

    let cpu_temp_source = sensors::resolve_cpu_temp_source();
    if cpu_temp_source.is_none() {
        eprintln!("warning: no CPU temp sensor found (coretemp/k10temp/zenpower) — cpu_temp will read 0");
    }

    let mut initial_state = config::load();
    if initial_state.mode == Mode::Media {
        match media_store::load() {
            Some(sequence) => initial_state.media = Some(std::sync::Arc::new(sequence)),
            None => {
                eprintln!("warning: mode=media but no valid media found on disk, falling back to monitor");
                initial_state.mode = Mode::Monitor;
            }
        }
    }
    println!("loaded state: mode={:?} orientation={}°", initial_state.mode, initial_state.orientation.degrees());
    let display_state = Arc::new(Mutex::new(initial_state));

    ipc::spawn(display_state.clone(), device.clone());

    // Preventive periodic reconnect: a long-running session (observed after ~22h of
    // continuous Media-mode playback) can leave the panel's own firmware silently
    // wedged — accepting/ACKing writes but no longer rendering them — with zero
    // host-side USB error to react to (see project notes on the 2026-09-01/09-05
    // "silent wedge" incidents). Forcing a full disconnect/reconnect/init_query cycle
    // periodically, regardless of mode, bounds how long that wedge can persist
    // unnoticed, at the cost of a few seconds of blank/glitchy panel each time it fires.
    const REINIT_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
    let mut last_reinit = Instant::now();

    println!("entering render loop (Ctrl+C to stop)");
    loop {
        let tick_start = Instant::now();

        if last_reinit.elapsed() >= REINIT_INTERVAL {
            periodic_reinit(&device);
            last_reinit = Instant::now();
        }

        let (mode, orientation, solid_color, tick_interval_ms) = {
            let s = display_state.lock().unwrap();
            (s.mode, s.orientation, s.solid_color, s.tick_interval_ms)
        };

        // Each arm also picks how long to sleep before the next tick — Monitor/Solid
        // use the configured tick_interval_ms, but Media must NOT: it needs to be
        // driven by the media's own per-frame delay, not the (much coarser, sensor-
        // sampling-tuned) outer interval, or fast GIFs get throttled to that interval's
        // rate regardless of how fast they were actually encoded to play.
        let (rgb565, sleep_target) = match mode {
            Mode::Monitor => {
                let cpu_percent = sensors::read_cpu_percent(); // blocks ~100ms
                // Average a few quick reads (~320ms) instead of one instantaneous
                // sample — damps Tdie's fast boost-clock transients.
                let cpu_temp = match &cpu_temp_source {
                    Some(source) => sensors::read_cpu_temp_averaged(source, 5, Duration::from_millis(80)),
                    None => 0.0,
                };
                let cpu_freq = sensors::read_cpu_freq_ghz();
                let gpu = sensors::read_gpu_stats();

                println!(
                    "CPU {cpu_temp:5.1}\u{b0}C ({cpu_percent:4.1}%, {cpu_freq:.2}GHz) | GPU {:5.1}\u{b0}C ({:4.1}%, {:.2}GHz)",
                    gpu.temp_c, gpu.busy_percent, gpu.clock_ghz
                );

                let info = render::SystemInfo {
                    cpu_temp,
                    cpu_percent,
                    cpu_freq,
                    gpu_temp: gpu.temp_c,
                    gpu_percent: gpu.busy_percent,
                    gpu_freq: gpu.clock_ghz,
                };
                (render::render_monitor_frame(&info, &fonts, orientation), Duration::from_millis(tick_interval_ms))
            }
            Mode::Solid => (render::solid_frame(solid_color), Duration::from_millis(tick_interval_ms)),
            Mode::Media => {
                let mut s = display_state.lock().unwrap();
                match s.media.clone() {
                    Some(sequence) if !sequence.frames.is_empty() => {
                        let delay_ms = sequence.delays_ms.get(s.media_frame_index).copied().unwrap_or(100);
                        if s.media_frame_started_at.elapsed() >= Duration::from_millis(delay_ms) {
                            s.media_frame_index = (s.media_frame_index + 1) % sequence.frames.len();
                            s.media_frame_started_at = Instant::now();
                        }
                        let current_delay_ms = sequence.delays_ms.get(s.media_frame_index).copied().unwrap_or(100);
                        let remaining = Duration::from_millis(current_delay_ms).saturating_sub(s.media_frame_started_at.elapsed());
                        (sequence.frames[s.media_frame_index].clone(), remaining.max(Duration::from_millis(5)))
                    }
                    // No media loaded (e.g. mode=media persisted but the daemon
                    // couldn't reload the frames) — show black instead of crashing.
                    _ => (render::solid_frame([0, 0, 0]), Duration::from_millis(tick_interval_ms)),
                }
            }
        };

        let send_result = device.lock().unwrap().send_frame(&rgb565);
        if let Err(e) = send_result {
            eprintln!("send_frame failed: {e}");
            if matches!(e, rusb::Error::NoDevice) {
                reconnect_device(&device);
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < sleep_target {
            std::thread::sleep(sleep_target - elapsed);
        }
    }
}

/// Blocks until the LM360 is back on the bus, then swaps the reconnected handle
/// into `device` in place (same `Arc`, so the IPC thread's clone stays valid).
fn reconnect_device(device: &Arc<Mutex<Lm360>>) {
    println!("device disconnected — attempting to reconnect...");
    let new_device = Lm360::connect_retrying(Duration::from_secs(2));
    if let Err(e) = new_device.init_query() {
        eprintln!("init_query failed after reconnect: {e}");
    }
    *device.lock().unwrap() = new_device;
    println!("reconnected to LM360");
}

/// Forces a fresh reconnect even though the device never actually left the bus —
/// see `REINIT_INTERVAL`'s doc comment above for why. Releases the current handle
/// first (unlike `reconnect_device`, where the device is already gone) so the
/// fresh `connect()` doesn't hit `Resource busy`.
fn periodic_reinit(device: &Arc<Mutex<Lm360>>) {
    println!("performing periodic preventive reconnect...");
    device.lock().unwrap().release();
    let new_device = Lm360::connect_retrying(Duration::from_secs(2));
    if let Err(e) = new_device.init_query() {
        eprintln!("init_query failed after periodic reconnect: {e}");
    }
    *device.lock().unwrap() = new_device;
    println!("periodic reconnect complete");
}
