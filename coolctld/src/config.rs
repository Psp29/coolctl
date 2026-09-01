//! Load/save `DisplayState` as TOML at `/etc/coolctld/config.toml`, so the last
//! mode/orientation/solid color survive a reboot — read once at startup, written
//! on every state-changing IPC action.

use std::path::Path;

use serde::{Deserialize, Serialize};

use coolctl_core::Orientation;

use crate::state::{DisplayState, Mode};

const CONFIG_PATH: &str = "/etc/coolctld/config.toml";

#[derive(Serialize, Deserialize)]
struct ConfigFile {
    mode: String, // "monitor" | "solid" | "media"
    solid_color: [u8; 3],
    orientation_degrees: u32,
    tick_interval_ms: u64,
}

impl From<&DisplayState> for ConfigFile {
    fn from(state: &DisplayState) -> Self {
        ConfigFile {
            mode: match state.mode {
                Mode::Monitor => "monitor".to_string(),
                Mode::Solid => "solid".to_string(),
                Mode::Media => "media".to_string(),
            },
            solid_color: state.solid_color,
            orientation_degrees: state.orientation.degrees(),
            tick_interval_ms: state.tick_interval_ms,
        }
    }
}

impl ConfigFile {
    /// Note: this only restores the scalar fields — `mode == Media` needs the
    /// caller to separately load the frame sequence via media_store::load()
    /// and populate `media`/`media_frame_index`, since that's real binary data
    /// that doesn't belong in a small TOML config file.
    fn into_state(self) -> DisplayState {
        DisplayState {
            mode: match self.mode.as_str() {
                "solid" => Mode::Solid,
                "media" => Mode::Media,
                _ => Mode::Monitor,
            },
            solid_color: self.solid_color,
            orientation: Orientation::from_degrees(self.orientation_degrees).unwrap_or(Orientation::Deg0),
            tick_interval_ms: self.tick_interval_ms,
            ..DisplayState::default()
        }
    }
}

/// Load persisted state, falling back to defaults on first run or a bad/missing file.
pub fn load() -> DisplayState {
    let Ok(content) = std::fs::read_to_string(CONFIG_PATH) else {
        return DisplayState::default(); // no config yet, first run
    };
    match toml::from_str::<ConfigFile>(&content) {
        Ok(cfg) => cfg.into_state(),
        Err(e) => {
            eprintln!("warning: failed to parse {CONFIG_PATH}: {e}, using defaults");
            DisplayState::default()
        }
    }
}

pub fn save(state: &DisplayState) {
    let cfg = ConfigFile::from(state);
    let toml_str = match toml::to_string_pretty(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: failed to serialize config: {e}");
            return;
        }
    };

    if let Some(parent) = Path::new(CONFIG_PATH).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("warning: failed to create {}: {e}", parent.display());
            return;
        }
    }
    if let Err(e) = std::fs::write(CONFIG_PATH, toml_str) {
        eprintln!("warning: failed to write {CONFIG_PATH}: {e}");
    }
}
