//! Offline verification for Milestone 6's extraction path, no GTK/display or
//! live playback needed: `cargo run --example verify_video -p coolctl-gui`.
//! Requires the synthetic test clip generated via:
//!   gst-launch-1.0 videotestsrc pattern=ball num-buffers=150 \
//!     ! video/x-raw,width=640,height=480,framerate=30/1 ! vp8enc ! webmmux \
//!     ! filesink location=test_video.webm

use std::sync::atomic::AtomicBool;

use coolctl_gui::editor::video_source;

const SCRATCH: &str = "/tmp/claude-1000/-home-prasad-Documents-lm360/e36826fc-7840-4cff-9ff0-6941752a5df7/scratchpad";

fn main() {
    gstreamer::init().expect("failed to init GStreamer");

    let path = std::path::Path::new(SCRATCH).join("test_video.webm");
    if !path.exists() {
        eprintln!("missing {}, generate it first (see this file's doc comment)", path.display());
        std::process::exit(1);
    }

    println!("=== probe ===");
    let (duration, width, height) = video_source::probe(&path).expect("probe failed");
    println!("duration: {duration}, native size: {width}x{height}");
    assert_eq!((width, height), (640, 480), "unexpected native size");

    println!("\n=== extract_frame_at ===");
    let mid = gstreamer::ClockTime::from_mseconds(2500);
    let frame = video_source::extract_frame_at(&path, mid).expect("extract_frame_at failed");
    println!("extracted frame at {mid}: {}x{}", frame.width(), frame.height());
    let mut mid_out = std::fs::File::create(format!("{SCRATCH}/video_frame_mid.png")).unwrap();
    frame.write_to_png(&mut mid_out).unwrap();

    println!("\n=== extract_trimmed_frames (0s..2s @ 10fps) ===");
    let never_cancel = AtomicBool::new(false);
    let frames = video_source::extract_trimmed_frames(&path, gstreamer::ClockTime::ZERO, gstreamer::ClockTime::from_seconds(2), 10.0, &never_cancel, |current, total| {
        println!("  progress: {current}/{total}");
    })
    .expect("extract_trimmed_frames failed")
    .expect("not cancelled");
    println!("extracted {} frames, delay_ms={}", frames.len(), frames[0].delay_ms);
    assert_eq!(frames.len(), 20, "expected 20 frames for a 2s range at 10fps");

    // Dump first, middle, last frame to confirm they're distinct (the "ball"
    // pattern moves, so identical PNGs across these would mean seeking/pulling
    // isn't actually advancing).
    for (label, idx) in [("first", 0usize), ("mid", frames.len() / 2), ("last", frames.len() - 1)] {
        let mut out = std::fs::File::create(format!("{SCRATCH}/video_trim_{label}.png")).unwrap();
        frames[idx].surface.write_to_png(&mut out).unwrap();
        println!("wrote video_trim_{label}.png (frame {idx})");
    }

    println!("\n=== LivePreviewPipeline (try_pull_frame while playing) ===");
    let live = video_source::LivePreviewPipeline::new(&path).expect("LivePreviewPipeline::new failed");
    live.play();
    let mut pulled = 0;
    let mut last_written: Option<String> = None;
    for i in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(frame) = live.try_pull_frame() {
            pulled += 1;
            if pulled == 1 || pulled == 5 {
                let name = format!("{SCRATCH}/live_pull_{pulled}.png");
                let mut out = std::fs::File::create(&name).unwrap();
                frame.write_to_png(&mut out).unwrap();
                println!("[{i}] pulled frame #{pulled}, wrote {name}");
                last_written = Some(name);
            } else {
                println!("[{i}] pulled frame #{pulled}");
            }
        }
    }
    live.stop();
    assert!(pulled >= 5, "expected at least 5 live-pulled frames over 2s, got {pulled}");
    println!("pulled {pulled} live frames total, last dump: {last_written:?}");

    println!("\nall checks passed");
}
