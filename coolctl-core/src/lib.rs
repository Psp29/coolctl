//! Shared types and constants used by both coolctld and coolctl-gui.

use serde::{Deserialize, Serialize};

pub const SOCKET_PATH: &str = "/run/coolctl.sock";

pub const PANEL_WIDTH: u32 = 320;
pub const PANEL_HEIGHT: u32 = 240;

/// Fixed 90° mounting-orientation steps — the pump can physically sit in the case
/// at any rotation, so content is pre-rotated to compensate and appear upright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Orientation {
    pub fn degrees(self) -> u32 {
        match self {
            Orientation::Deg0 => 0,
            Orientation::Deg90 => 90,
            Orientation::Deg180 => 180,
            Orientation::Deg270 => 270,
        }
    }

    pub fn from_degrees(degrees: u32) -> Option<Self> {
        match degrees % 360 {
            0 => Some(Orientation::Deg0),
            90 => Some(Orientation::Deg90),
            180 => Some(Orientation::Deg180),
            270 => Some(Orientation::Deg270),
            _ => None,
        }
    }
}

fn copy_pixel(src: &[u8], src_w: u32, sx: u32, sy: u32, dst: &mut [u8], dst_w: u32, dx: u32, dy: u32) {
    let s = ((sy * src_w + sx) * 2) as usize;
    let d = ((dy * dst_w + dx) * 2) as usize;
    dst[d] = src[s];
    dst[d + 1] = src[s + 1];
}

/// Rotate a raw RGB565 framebuffer (row-major, 2 bytes/pixel) by a fixed 90° step.
/// 90°/270° swap width and height (rotating a WxH image yields an HxW image);
/// 0°/180° preserve the source dimensions. Rotation is clockwise.
pub fn rotate_rgb565(src: &[u8], src_w: u32, src_h: u32, orientation: Orientation) -> Vec<u8> {
    debug_assert_eq!(src.len(), (src_w * src_h * 2) as usize);

    match orientation {
        Orientation::Deg0 => src.to_vec(),
        Orientation::Deg90 => {
            let dst_w = src_h;
            let mut dst = vec![0u8; src.len()];
            for y in 0..src_h {
                for x in 0..src_w {
                    let dst_x = src_h - 1 - y;
                    let dst_y = x;
                    copy_pixel(src, src_w, x, y, &mut dst, dst_w, dst_x, dst_y);
                }
            }
            dst
        }
        Orientation::Deg180 => {
            let mut dst = vec![0u8; src.len()];
            for y in 0..src_h {
                for x in 0..src_w {
                    let dst_x = src_w - 1 - x;
                    let dst_y = src_h - 1 - y;
                    copy_pixel(src, src_w, x, y, &mut dst, src_w, dst_x, dst_y);
                }
            }
            dst
        }
        Orientation::Deg270 => {
            let dst_w = src_h;
            let mut dst = vec![0u8; src.len()];
            for y in 0..src_h {
                for x in 0..src_w {
                    let dst_x = y;
                    let dst_y = src_w - 1 - x;
                    copy_pixel(src, src_w, x, y, &mut dst, dst_w, dst_x, dst_y);
                }
            }
            dst
        }
    }
}

/// IPC requests sent to coolctld over `SOCKET_PATH`. Every request is a single JSON
/// header line (terminated by `\n`, or by EOF for the small fixed-shape requests).
/// `SetMedia` is the one exception: after its header line, the raw RGB565 frame
/// bytes follow directly (`frame_count * PANEL_WIDTH * PANEL_HEIGHT * 2` bytes,
/// concatenated, no base64) — a 30s/10fps clip is tens of MB, so encoding it as a
/// JSON field would add ~33% overhead plus per-string parse cost for no benefit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    Solid { color: [u8; 3] },
    Monitor,
    SetOrientation { orientation: Orientation },
    BrightnessUp,
    BrightnessDown,
    ZenToggle,
    GetStatus,
    /// Header only — see the doc comment above for what follows on the wire.
    SetMedia { frame_count: u32, delays_ms: Vec<u64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Status { mode: String, orientation: Orientation },
    Error { message: String },
}
