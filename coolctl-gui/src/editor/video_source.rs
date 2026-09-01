//! GStreamer-backed video preview + frame extraction. Two separate pipeline
//! kinds, never active at once: a live-pulling preview pipeline (drives the
//! crop view while you watch the video play) and short-lived extraction
//! pipelines for the final, accurate export — kept separate since a
//! continuously-playing low-effort preview pull and a paused/seek/preroll-per-
//! frame accurate extraction have different needs, and never run at the same
//! moment in this UI.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use cairo::ImageSurface;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gstreamer_pbutils::Discoverer;

use crate::editor::image_source::{rgba_bytes_to_argb32_surface, SourceFrame};

/// Source video above this on its longer edge gets downscaled (preserving
/// aspect ratio) for both preview and extraction, to bound per-frame
/// decode/convert cost and memory independent of resolution or trim length.
const MAX_DIM: u32 = 720;

fn path_to_uri(path: &Path) -> Result<String, String> {
    let abs = std::fs::canonicalize(path).map_err(|e| format!("failed to resolve {}: {e}", path.display()))?;
    glib::filename_to_uri(&abs, None).map(|s| s.to_string()).map_err(|e| format!("failed to build URI for {}: {e}", path.display()))
}

/// Duration + native width/height, via a metadata-only probe (no decode).
pub fn probe(path: &Path) -> Result<(gstreamer::ClockTime, u32, u32), String> {
    let uri = path_to_uri(path)?;
    let discoverer = Discoverer::new(gstreamer::ClockTime::from_seconds(10)).map_err(|e| format!("failed to create discoverer: {e}"))?;
    let info = discoverer.discover_uri(&uri).map_err(|e| format!("failed to probe {}: {e}", path.display()))?;
    let duration = info.duration().ok_or_else(|| "video has no duration".to_string())?;
    let video_stream = info
        .video_streams()
        .into_iter()
        .next()
        .ok_or_else(|| "no video stream found".to_string())?;
    Ok((duration, video_stream.width(), video_stream.height()))
}

/// Pass through if within MAX_DIM on both axes, otherwise scale down
/// (preserving aspect ratio) so the longer edge is exactly MAX_DIM.
fn target_dims(native_w: u32, native_h: u32) -> (u32, u32) {
    if native_w <= MAX_DIM && native_h <= MAX_DIM {
        return (native_w, native_h);
    }
    if native_w >= native_h {
        let h = (native_h as f64 * MAX_DIM as f64 / native_w as f64).round() as u32;
        (MAX_DIM, h.max(1))
    } else {
        let w = (native_w as f64 * MAX_DIM as f64 / native_h as f64).round() as u32;
        (w.max(1), MAX_DIM)
    }
}

/// Build a `videoconvert ! videoscale ! video/x-raw,format=RGBA,width,height !
/// appsink` sink as a `Bin` (with a ghost pad) ready to plug into a
/// `playbin3`'s `video-sink` property — shared by both pipeline kinds below.
///
/// `sync` matters a lot here: the extraction pipeline (pause → seek →
/// pull_preroll per frame) doesn't care about real-time pacing so it wants
/// `sync=false`, but the *live* preview pipeline needs `sync=true` — with
/// `sync=false` there's nothing pacing decode to the pipeline clock, so it
/// blows through the whole clip as fast as the CPU allows and hits EOS
/// almost immediately instead of producing frames paced to real playback.
fn build_rgba_sink_bin(width: u32, height: u32, sync: bool) -> Result<(gstreamer::Bin, AppSink), String> {
    let caps = gstreamer_video::VideoCapsBuilder::new()
        .format(gstreamer_video::VideoFormat::Rgba)
        .width(width as i32)
        .height(height as i32)
        .build();

    let appsink = AppSink::builder().caps(&caps).max_buffers(1u32).drop(true).sync(sync).build();

    let videoconvert = gstreamer::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| format!("missing videoconvert element: {e}"))?;
    let videoscale = gstreamer::ElementFactory::make("videoscale")
        .build()
        .map_err(|e| format!("missing videoscale element: {e}"))?;

    let sink_bin = gstreamer::Bin::new();
    sink_bin
        .add_many([&videoconvert, &videoscale, appsink.upcast_ref()])
        .map_err(|e| format!("failed to build sink bin: {e}"))?;
    gstreamer::Element::link_many([&videoconvert, &videoscale, appsink.upcast_ref()])
        .map_err(|e| format!("failed to link sink bin: {e}"))?;

    let sink_pad = videoconvert.static_pad("sink").ok_or_else(|| "videoconvert has no sink pad".to_string())?;
    let ghost_pad = gstreamer::GhostPad::with_target(&sink_pad).map_err(|e| format!("failed to create ghost pad: {e}"))?;
    sink_bin.add_pad(&ghost_pad).map_err(|e| format!("failed to add ghost pad: {e}"))?;

    Ok((sink_bin, appsink))
}

fn build_playbin(uri: &str, sink_bin: &gstreamer::Bin) -> Result<gstreamer::Element, String> {
    let pipeline = gstreamer::ElementFactory::make("playbin3")
        .build()
        .map_err(|e| format!("missing playbin3 element: {e}"))?;
    pipeline.set_property("uri", uri);
    pipeline.set_property("video-sink", sink_bin);
    // Only ever displayed silently on an LCD panel or pulled as still frames — no audio needed.
    pipeline.set_property_from_str("flags", "video");
    Ok(pipeline)
}

fn preroll(pipeline: &gstreamer::Element) -> Result<(), String> {
    pipeline.set_state(gstreamer::State::Paused).map_err(|e| format!("failed to pause pipeline: {e}"))?;
    let (result, _, _) = pipeline.state(gstreamer::ClockTime::from_seconds(10));
    result.map(|_| ()).map_err(|e| format!("pipeline failed to preroll: {e}"))
}

struct ExtractionPipeline {
    pipeline: gstreamer::Element,
    appsink: AppSink,
    width: u32,
    height: u32,
}

impl ExtractionPipeline {
    fn new(path: &Path) -> Result<Self, String> {
        let uri = path_to_uri(path)?;
        let (_, native_w, native_h) = probe(path)?;
        let (width, height) = target_dims(native_w, native_h);

        let (sink_bin, appsink) = build_rgba_sink_bin(width, height, false)?;
        let pipeline = build_playbin(&uri, &sink_bin)?;
        preroll(&pipeline)?;

        Ok(Self { pipeline, appsink, width, height })
    }

    fn seek_and_pull(&self, position: gstreamer::ClockTime) -> Result<ImageSurface, String> {
        self.pipeline
            .seek_simple(gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE, position)
            .map_err(|e| format!("seek failed: {e}"))?;
        let (result, _, _) = self.pipeline.state(gstreamer::ClockTime::from_seconds(10));
        result.map_err(|e| format!("pipeline failed to preroll after seek: {e}"))?;

        let sample = self.appsink.pull_preroll().map_err(|e| format!("failed to pull frame: {e}"))?;
        let buffer = sample.buffer().ok_or_else(|| "sample has no buffer".to_string())?;
        let map = buffer.map_readable().map_err(|e| format!("failed to map frame buffer: {e}"))?;
        Ok(rgba_bytes_to_argb32_surface(&map, self.width, self.height))
    }
}

impl Drop for ExtractionPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gstreamer::State::Null);
    }
}

pub fn extract_frame_at(path: &Path, position: gstreamer::ClockTime) -> Result<ImageSurface, String> {
    let extractor = ExtractionPipeline::new(path)?;
    extractor.seek_and_pull(position)
}

/// Extracts frames across `[in_point, out_point)` at `fps`, checking `cancel`
/// before each frame so a Cancel button press stops real work promptly
/// instead of running the extraction to completion pointlessly. Returns
/// `Ok(None)` if cancelled cleanly (some frames may already be gone,
/// deliberately not returned — callers should just discard the whole
/// operation), `Ok(Some(frames))` on success.
pub fn extract_trimmed_frames(
    path: &Path,
    in_point: gstreamer::ClockTime,
    out_point: gstreamer::ClockTime,
    fps: f64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<Option<Vec<SourceFrame>>, String> {
    let extractor = ExtractionPipeline::new(path)?;
    let step = gstreamer::ClockTime::from_nseconds((1_000_000_000.0 / fps) as u64);
    let delay_ms = (1000.0 / fps) as u64;

    let total = ((out_point.nseconds() - in_point.nseconds()) / step.nseconds()).max(1) as usize;
    let mut frames = Vec::new();
    let mut position = in_point;
    while position < out_point {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let surface = extractor.seek_and_pull(position)?;
        frames.push(SourceFrame { surface, delay_ms });
        on_progress(frames.len(), total);
        position += step;
    }
    if frames.is_empty() {
        return Err("no frames extracted".to_string());
    }
    Ok(Some(frames))
}

/// Live preview pipeline: plays continuously and hands back whatever frame is
/// ready on demand (non-blocking) — drives the crop view's `preview_surface`
/// on a periodic tick so cropping can happen while the content actually
/// plays, without any GPU-composited display widget involved.
pub struct LivePreviewPipeline {
    pipeline: gstreamer::Element,
    appsink: AppSink,
    width: u32,
    height: u32,
}

impl LivePreviewPipeline {
    pub fn new(path: &Path) -> Result<Self, String> {
        let uri = path_to_uri(path)?;
        let (_, native_w, native_h) = probe(path)?;
        let (width, height) = target_dims(native_w, native_h);

        let (sink_bin, appsink) = build_rgba_sink_bin(width, height, true)?;
        let pipeline = build_playbin(&uri, &sink_bin)?;
        preroll(&pipeline)?;

        Ok(Self { pipeline, appsink, width, height })
    }

    /// Non-blocking: returns the latest available frame, or `None` if
    /// nothing new is ready yet (call this on a periodic timer).
    pub fn try_pull_frame(&self) -> Option<ImageSurface> {
        let sample = self.appsink.try_pull_sample(gstreamer::ClockTime::ZERO)?;
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        Some(rgba_bytes_to_argb32_surface(&map, self.width, self.height))
    }

    pub fn play(&self) {
        let _ = self.pipeline.set_state(gstreamer::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.pipeline.set_state(gstreamer::State::Paused);
    }

    pub fn seek(&self, position: gstreamer::ClockTime) {
        if let Err(e) = self.pipeline.seek_simple(gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::ACCURATE, position) {
            eprintln!("seek failed: {e}");
        }
    }

    /// Explicitly tear down the pipeline (stop playback) right away, rather
    /// than relying on Drop timing — callers should call this directly
    /// whenever the source is replaced or the app is done with it.
    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gstreamer::State::Null);
    }

    pub fn position(&self) -> Option<gstreamer::ClockTime> {
        self.pipeline.query_position::<gstreamer::ClockTime>()
    }

    pub fn duration(&self) -> Option<gstreamer::ClockTime> {
        self.pipeline.query_duration::<gstreamer::ClockTime>()
    }
}

impl Drop for LivePreviewPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gstreamer::State::Null);
    }
}
