//! Persist/reload the processed media frame sequence to/from `/etc/coolctld/media/`
//! — a small JSON manifest (frame count + per-frame delays) plus the raw
//! concatenated RGB565 bytes. No re-decoding needed on reload: the GUI (Phase 2)
//! already did all the cropping/orientation/format work before sending it here.

use serde::{Deserialize, Serialize};

use coolctl_core::{PANEL_HEIGHT, PANEL_WIDTH};

use crate::state::MediaSequence;

const MEDIA_DIR: &str = "/etc/coolctld/media";
const MANIFEST_PATH: &str = "/etc/coolctld/media/manifest.json";
const FRAMES_PATH: &str = "/etc/coolctld/media/frames.bin";

const FRAME_BYTES: usize = (PANEL_WIDTH * PANEL_HEIGHT * 2) as usize;

#[derive(Serialize, Deserialize)]
struct Manifest {
    frame_count: u32,
    delays_ms: Vec<u64>,
}

pub fn save(frame_count: u32, delays_ms: &[u64], raw_frames: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(MEDIA_DIR)?;

    let manifest = Manifest { frame_count, delays_ms: delays_ms.to_vec() };
    let json = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    std::fs::write(MANIFEST_PATH, json)?;
    std::fs::write(FRAMES_PATH, raw_frames)?;
    Ok(())
}

pub fn load() -> Option<MediaSequence> {
    let manifest_json = std::fs::read_to_string(MANIFEST_PATH).ok()?;
    let manifest: Manifest = serde_json::from_str(&manifest_json).ok()?;
    let raw = std::fs::read(FRAMES_PATH).ok()?;

    let expected_len = FRAME_BYTES * manifest.frame_count as usize;
    if raw.len() != expected_len || manifest.delays_ms.len() != manifest.frame_count as usize {
        eprintln!("media_store: manifest/frame data size mismatch, ignoring stored media");
        return None;
    }

    let frames: Vec<Vec<u8>> = raw.chunks_exact(FRAME_BYTES).map(|c| c.to_vec()).collect();
    Some(MediaSequence { frames, delays_ms: manifest.delays_ms })
}
