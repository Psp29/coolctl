//! Load a static image or animation into Cairo surfaces ready for the crop
//! view — one decode step, reused for both the live preview (frame 1 only)
//! and export (every frame gets the same crop transform applied).
//!
//! Format is detected by content (magic bytes), not file extension: a lot of
//! "GIFs" downloaded from the web (Giphy, Discord, etc.) are actually
//! animated WebP served under a `.gif` filename/URL for smaller size.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use cairo::{Format, ImageSurface};
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Frame, ImageFormat, ImageReader, RgbaImage};

#[derive(Clone)]
pub struct SourceFrame {
    pub surface: ImageSurface,
    pub delay_ms: u64,
}

/// Convert raw, tightly-packed RGBA bytes (row-major, 4 bytes/pixel, no
/// stride padding — what both the `image` crate and a GStreamer RGBA-capped
/// appsink buffer give us) into a premultiplied Cairo ARGB32 surface.
pub fn rgba_bytes_to_argb32_surface(rgba: &[u8], width: u32, height: u32) -> ImageSurface {
    let width = width as i32;
    let height = height as i32;
    let mut surface = ImageSurface::create(Format::ARgb32, width, height).expect("failed to create surface");
    let stride = surface.stride() as usize;
    let src_stride = width as usize * 4;

    {
        let mut data = surface.data().expect("failed to borrow surface data");
        for y in 0..height as usize {
            let dst_row = y * stride;
            let src_row = y * src_stride;
            for x in 0..width as usize {
                let src_i = src_row + x * 4;
                let [r, g, b, a] = [rgba[src_i], rgba[src_i + 1], rgba[src_i + 2], rgba[src_i + 3]];
                // Cairo's ARgb32 wants premultiplied color.
                let af = a as u32;
                let pr = ((r as u32) * af / 255) as u8;
                let pg = ((g as u32) * af / 255) as u8;
                let pb = ((b as u32) * af / 255) as u8;
                let i = dst_row + x * 4;
                data[i] = pb;
                data[i + 1] = pg;
                data[i + 2] = pr;
                data[i + 3] = a;
            }
        }
    }
    surface.mark_dirty();
    surface
}

fn rgba_to_argb32_surface(rgba: &RgbaImage) -> ImageSurface {
    rgba_bytes_to_argb32_surface(rgba.as_raw(), rgba.width(), rgba.height())
}

fn frames_to_source(frames: Vec<Frame>) -> Result<Vec<SourceFrame>, String> {
    if frames.is_empty() {
        return Err("animated image has no frames".to_string());
    }
    Ok(frames
        .into_iter()
        .map(|frame| {
            let delay: std::time::Duration = frame.delay().into();
            // Clamp: some encoders emit 0ms delays, unusable for playback timing.
            let delay_ms = (delay.as_millis() as u64).max(20);
            SourceFrame { surface: rgba_to_argb32_surface(frame.buffer()), delay_ms }
        })
        .collect())
}

fn static_source(img: image::DynamicImage) -> Vec<SourceFrame> {
    // Delay is irrelevant for a single frame — the daemon just plays a
    // 1-frame sequence as a still.
    vec![SourceFrame { surface: rgba_to_argb32_surface(&img.to_rgba8()), delay_ms: 0 }]
}

pub fn load(path: &Path) -> Result<Vec<SourceFrame>, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let format = reader.format();

    match format {
        Some(ImageFormat::Gif) => {
            let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            let decoder = GifDecoder::new(BufReader::new(file)).map_err(|e| format!("failed to decode GIF: {e}"))?;
            let frames = decoder.into_frames().collect_frames().map_err(|e| format!("failed to decode GIF frames: {e}"))?;
            frames_to_source(frames)
        }
        Some(ImageFormat::WebP) => {
            let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            let decoder = WebPDecoder::new(BufReader::new(file)).map_err(|e| format!("failed to decode WebP: {e}"))?;
            if decoder.has_animation() {
                let frames = decoder.into_frames().collect_frames().map_err(|e| format!("failed to decode WebP frames: {e}"))?;
                frames_to_source(frames)
            } else {
                let img = reader.decode().map_err(|e| format!("failed to decode {}: {e}", path.display()))?;
                Ok(static_source(img))
            }
        }
        _ => {
            let img = reader.decode().map_err(|e| format!("failed to decode {}: {e}", path.display()))?;
            Ok(static_source(img))
        }
    }
}
