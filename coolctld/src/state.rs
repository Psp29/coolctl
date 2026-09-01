//! Runtime display state, shared between the render loop and the IPC thread
//! behind a mutex. Also what gets persisted to/reloaded from config.toml (the
//! scalar fields) and media/ (the actual frame bytes, via media_store.rs).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use coolctl_core::Orientation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Monitor,
    Solid,
    Media,
}

/// A loaded, ready-to-play sequence of pre-rendered RGB565 frames — a static
/// image is `frames.len() == 1`, a GIF/video clip is a longer sequence. Every
/// frame is already exactly PANEL_WIDTH*PANEL_HEIGHT*2 bytes; coolctld never
/// decodes or crops anything itself, it just replays what it was given.
pub struct MediaSequence {
    pub frames: Vec<Vec<u8>>,
    pub delays_ms: Vec<u64>,
}

pub struct DisplayState {
    pub mode: Mode,
    pub solid_color: [u8; 3],
    pub orientation: Orientation,
    pub tick_interval_ms: u64,
    pub media: Option<Arc<MediaSequence>>,
    pub media_frame_index: usize,
    pub media_frame_started_at: Instant,
}

impl Default for DisplayState {
    fn default() -> Self {
        DisplayState {
            mode: Mode::Monitor,
            solid_color: [0, 0, 0],
            orientation: Orientation::Deg0,
            tick_interval_ms: 500,
            media: None,
            media_frame_index: 0,
            media_frame_started_at: Instant::now(),
        }
    }
}

pub type SharedState = Arc<Mutex<DisplayState>>;
