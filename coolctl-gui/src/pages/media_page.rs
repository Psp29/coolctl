use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use coolctl_core::{Orientation, Response, PANEL_HEIGHT, PANEL_WIDTH};
use gstreamer::ClockTime;
use gtk4::prelude::*;
use gtk4::{
    glib, ApplicationWindow, Button, CheckButton, DrawingArea, EventControllerScroll, EventControllerScrollFlags, FileDialog, GestureDrag, Label,
    Orientation as BoxOrientation, Scale,
};

use crate::editor::crop_view::{self, CropState};
use crate::editor::image_source::{self, SourceFrame};
use crate::editor::video_source::{self, LivePreviewPipeline};
use crate::ipc_client;
use crate::widgets::progress_popup::ProgressPopup;

const MIN_TRIM_SECONDS: f64 = 5.0;
const GIF_TICK_MS: u64 = 33;
const VIDEO_TICK_MS: u64 = 150;

enum MediaSource {
    Frames(Vec<SourceFrame>),
    Video { path: PathBuf, pipeline: Rc<LivePreviewPipeline> },
}

/// Result of the background video-load, sent back to the main thread. Only
/// the video path is backgrounded — `cairo::ImageSurface` (inside
/// `SourceFrame`, used by the image/GIF path) is not `Send` and can never
/// cross a thread boundary at all, so image/GIF loading stays a plain
/// synchronous call on the main thread, same as before this feature (it's
/// also not the slow case this popup exists for — a video's `Discoverer`
/// probe + pipeline preroll is what can block for many seconds).
enum OpenVideoOutcome {
    Loaded(PathBuf, LivePreviewPipeline),
    Failed(String),
}

/// Messages the background Export thread sends back to the main thread.
enum ExportProgress {
    Phase { label: &'static str, current: usize, total: usize },
    Sending,
    Cancelled,
    Done(Result<Response, String>),
}

struct EditorState {
    source: Option<MediaSource>,
    preview_surface: Option<cairo::ImageSurface>,
    crop: CropState,
    drag_start: (f64, f64),
    last_window_size: (f64, f64),
    /// Bumped on every new "Open" (including a cancelled one) — periodic
    /// timers (GIF animation, video live-pull) and the background Open load
    /// capture the value at spawn time and self-terminate/discard their
    /// result the moment it no longer matches, regardless of what replaced
    /// them. Simpler than tracking each async operation kind separately.
    generation: u64,

    // Animated Frames source (GIF/animated-WebP) playback position.
    anim_index: usize,
    anim_started_at: Instant,

    // Video source trim/playback state.
    video_in: Option<ClockTime>,
    video_out: Option<ClockTime>,
    video_playing: bool,
    video_duration_s: f64,
    video_duration_known: bool,
}

impl EditorState {
    fn new() -> Self {
        EditorState {
            source: None,
            preview_surface: None,
            crop: CropState::new(),
            drag_start: (0.0, 0.0),
            last_window_size: (640.0, 480.0),
            generation: 0,
            anim_index: 0,
            anim_started_at: Instant::now(),
            video_in: None,
            video_out: None,
            video_playing: false,
            video_duration_s: 0.0,
            video_duration_known: false,
        }
    }
}

/// hh:mm:ss.mmm, e.g. "00:01:23.456".
fn format_duration(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let s = total_secs % 60;
    let total_mins = total_secs / 60;
    let m = total_mins % 60;
    let h = total_mins / 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

fn target_dims(orientation: Orientation) -> (f64, f64) {
    match orientation {
        Orientation::Deg0 | Orientation::Deg180 => (PANEL_WIDTH as f64, PANEL_HEIGHT as f64),
        Orientation::Deg90 | Orientation::Deg270 => (PANEL_HEIGHT as f64, PANEL_WIDTH as f64),
    }
}

/// Crop + rotate + RGB565-convert every frame, ready for `set_media`. Shared
/// by both the already-in-memory image/GIF path and the freshly-extracted
/// video path. Checks `cancel` between frames and reports progress via
/// `on_progress(done, total)`; returns `None` if cancelled (any
/// already-converted frames are discarded — callers should abandon the whole
/// export rather than send a partial payload).
fn frames_to_media_payload(
    frames: &[SourceFrame],
    export_state: &CropState,
    target_w: f64,
    target_h: f64,
    orientation: Orientation,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(usize, usize),
) -> Option<(Vec<u8>, Vec<u64>, u32)> {
    // A single-frame export is a static image, not an animation: coolctld's Media-mode
    // render loop re-sends each frame at its own delay with no other throttling, so a
    // GIF-appropriate 20ms floor here would flood the panel with an identical frame ~50x/sec
    // forever. Give a lone frame the same easygoing cadence as Solid/Monitor mode instead.
    let min_delay_ms = if frames.len() == 1 { 500 } else { 20 };
    let total = frames.len();
    let mut raw_frames = Vec::new();
    let mut delays_ms = Vec::new();
    for (i, frame) in frames.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut export_surface = crop_view::render_crop_export(export_state, target_w as i32, target_h as i32, &frame.surface);
        let mut bytes = crop_view::argb32_to_rgb565(&mut export_surface);
        if orientation != Orientation::Deg0 {
            bytes = coolctl_core::rotate_rgb565(&bytes, target_w as u32, target_h as u32, orientation);
        }
        raw_frames.extend_from_slice(&bytes);
        delays_ms.push(frame.delay_ms.max(min_delay_ms));
        on_progress(i + 1, total);
    }
    let frame_count = frames.len() as u32;
    Some((raw_frames, delays_ms, frame_count))
}

/// Builds the Media Editor page. `window` is only needed as the parent for
/// the file-open dialog. `orientation` is shared, live state owned by the
/// caller (the orientation selector lives at the top level, common to both
/// tabs) — this page always reads the current value rather than snapshotting
/// it once, so a change made while this page is open takes effect immediately.
///
/// Returns the page widget plus a callback the caller should invoke whenever
/// `orientation` changes, so the crop viewport can re-fit to the new aspect
/// ratio (any in-progress crop/pan/zoom is reset, same as opening a new file).
pub fn build(window: &ApplicationWindow, orientation: Rc<Cell<Orientation>>) -> (gtk4::Box, Rc<dyn Fn()>) {
    let state = Rc::new(RefCell::new(EditorState::new()));

    let drawing_area = DrawingArea::new();
    drawing_area.set_content_width(640);
    drawing_area.set_content_height(480);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);

    let status_label = Label::new(Some("Open an image, GIF, or video to begin"));
    status_label.set_wrap(true);

    {
        let state = state.clone();
        let orientation = orientation.clone();
        drawing_area.set_draw_func(move |_area, cr, width, height| {
            let (target_w, target_h) = target_dims(orientation.get());
            let mut s = state.borrow_mut();
            s.last_window_size = (width as f64, height as f64);
            if let Some(surface) = s.preview_surface.clone() {
                crop_view::draw_crop_view(cr, width, height, &s.crop, target_w, target_h, &surface);
            } else {
                cr.set_source_rgb(0.08, 0.08, 0.1);
                cr.paint().unwrap();
                cr.set_source_rgb(0.6, 0.6, 0.7);
                cr.set_font_size(16.0);
                cr.move_to(20.0, height as f64 / 2.0);
                let _ = cr.show_text("Open an image, GIF, or video to begin");
            }
        });
    }

    // Pan.
    let drag = GestureDrag::new();
    {
        let state = state.clone();
        drag.connect_drag_begin(move |_gesture, _x, _y| {
            let mut s = state.borrow_mut();
            s.drag_start = (s.crop.offset_x, s.crop.offset_y);
        });
    }
    {
        let state = state.clone();
        let drawing_area = drawing_area.clone();
        drag.connect_drag_update(move |_gesture, dx, dy| {
            let mut s = state.borrow_mut();
            let (start_x, start_y) = s.drag_start;
            s.crop.offset_x = start_x + dx;
            s.crop.offset_y = start_y + dy;
            drop(s);
            drawing_area.queue_draw();
        });
    }
    drawing_area.add_controller(drag);

    // Zoom.
    let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    {
        let state = state.clone();
        let drawing_area = drawing_area.clone();
        scroll.connect_scroll(move |_controller, _dx, dy| {
            let mut s = state.borrow_mut();
            let zoom_step = 0.1;
            s.crop.scale = (s.crop.scale - dy * zoom_step).clamp(0.05, 5.0);
            drop(s);
            drawing_area.queue_draw();
            glib::Propagation::Proceed
        });
    }
    drawing_area.add_controller(scroll);

    // --- Video controls row: built once, shown only while a video is loaded. ---
    let play_button = Button::with_label("Play");
    let position_scale = Scale::with_range(BoxOrientation::Horizontal, 0.0, 1.0, 0.1);
    position_scale.set_hexpand(true);
    position_scale.set_draw_value(false);
    let position_label = Label::new(Some("Position: 0.0s"));
    let range_label = Label::new(Some("In: - / Out: - / Length: - / Duration: -"));
    let set_in_button = Button::with_label("Set In");
    let set_out_button = Button::with_label("Set Out");

    let video_buttons_row = gtk4::Box::new(BoxOrientation::Horizontal, 8);
    video_buttons_row.append(&play_button);
    video_buttons_row.append(&set_in_button);
    video_buttons_row.append(&set_out_button);

    let video_controls = gtk4::Box::new(BoxOrientation::Vertical, 4);
    video_controls.append(&position_scale);
    video_controls.append(&position_label);
    video_controls.append(&range_label);
    video_controls.append(&video_buttons_row);
    video_controls.set_visible(false);

    let update_range_label = {
        let state = state.clone();
        let range_label = range_label.clone();
        move || {
            let s = state.borrow();
            let in_text = s.video_in.map(|t| format_duration(t.nseconds() as f64 / 1e9)).unwrap_or_else(|| "-".to_string());
            let out_text = s.video_out.map(|t| format_duration(t.nseconds() as f64 / 1e9)).unwrap_or_else(|| "-".to_string());
            let length_text = match (s.video_in, s.video_out) {
                (Some(i), Some(o)) if o > i => format_duration((o.nseconds() as f64 - i.nseconds() as f64) / 1e9),
                (Some(_), Some(_)) => "invalid (out before in)".to_string(),
                _ => "-".to_string(),
            };
            let duration_text = if s.video_duration_known { format_duration(s.video_duration_s) } else { "-".to_string() };
            range_label.set_text(&format!("In: {in_text} / Out: {out_text} / Length: {length_text} / Duration: {duration_text}"));
        }
    };

    // Play/Pause.
    {
        let state = state.clone();
        play_button.connect_clicked(move |button| {
            let s = state.borrow();
            let Some(MediaSource::Video { pipeline, .. }) = &s.source else { return };
            let pipeline = pipeline.clone();
            let now_playing = !s.video_playing;
            drop(s);
            if now_playing {
                pipeline.play();
                button.set_label("Pause");
            } else {
                pipeline.pause();
                button.set_label("Play");
            }
            state.borrow_mut().video_playing = now_playing;
        });
    }

    // User-driven seek — debounced (change-value fires repeatedly during a
    // drag; flooding the pipeline with overlapping flushing seeks makes
    // scrubbing look like it does nothing).
    let pending_seek: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
    {
        let state = state.clone();
        let position_label = position_label.clone();
        let pending_seek = pending_seek.clone();
        position_scale.connect_change_value(move |_, _scroll_type, value| {
            let s = state.borrow();
            let Some(MediaSource::Video { pipeline, .. }) = &s.source else { return glib::Propagation::Proceed };
            let pipeline = pipeline.clone();
            let clamped = value.clamp(0.0, s.video_duration_s.max(0.1));
            drop(s);
            position_label.set_text(&format!("Position: {}", format_duration(clamped)));

            if let Some(id) = pending_seek.take() {
                id.remove();
            }
            let pending_seek_inner = pending_seek.clone();
            let id = glib::timeout_add_local_once(Duration::from_millis(80), move || {
                pending_seek_inner.set(None);
                pipeline.seek(ClockTime::from_nseconds((clamped * 1e9) as u64));
            });
            pending_seek.set(Some(id));

            glib::Propagation::Proceed
        });
    }

    set_in_button.connect_clicked({
        let state = state.clone();
        let update_range_label = update_range_label.clone();
        move |_| {
            let position = {
                let s = state.borrow();
                let Some(MediaSource::Video { pipeline, .. }) = &s.source else { return };
                pipeline.position()
            };
            if let Some(position) = position {
                state.borrow_mut().video_in = Some(position);
                update_range_label();
            }
        }
    });

    set_out_button.connect_clicked({
        let state = state.clone();
        let update_range_label = update_range_label.clone();
        move |_| {
            let position = {
                let s = state.borrow();
                let Some(MediaSource::Video { pipeline, .. }) = &s.source else { return };
                pipeline.position()
            };
            if let Some(position) = position {
                state.borrow_mut().video_out = Some(position);
                update_range_label();
            }
        }
    });

    // --- Media-type selector + Open/Export buttons (bare widgets — handlers
    // wired below, after `set_controls_enabled` exists so both handlers can
    // disable/re-enable the same four controls while a popup is active). ---
    // GIF is grouped with Video in the UI, but a picked GIF still never
    // starts a live video pipeline — image_source::load already handles
    // static images and GIFs/animated-WebP uniformly, so a GIF pick just
    // short-circuits to the same path as a plain image (see the handler
    // below). Only a genuine video (image_source::load fails on it) starts one.
    let images_radio = CheckButton::with_label("Images");
    let gif_video_radio = CheckButton::with_label("GIF / Video");
    gif_video_radio.set_group(Some(&images_radio));
    images_radio.set_active(true);

    let open_button = Button::with_label("Open...");
    let export_button = Button::with_label("Export & Send");

    let set_controls_enabled: Rc<dyn Fn(bool)> = {
        let images_radio = images_radio.clone();
        let gif_video_radio = gif_video_radio.clone();
        let open_button = open_button.clone();
        let export_button = export_button.clone();
        Rc::new(move |enabled: bool| {
            images_radio.set_sensitive(enabled);
            gif_video_radio.set_sensitive(enabled);
            open_button.set_sensitive(enabled);
            export_button.set_sensitive(enabled);
        })
    };

    // --- Open: file dialog on the main thread (already async and cheap),
    // then image/GIF loading applied directly and synchronously (unchanged
    // from before this feature — `image_source::load` decodes straight into
    // `cairo::ImageSurface`s, which are not `Send` and can never cross a
    // thread boundary, and decode time isn't the reported problem anyway).
    // Only a genuine video's `LivePreviewPipeline::new` (Discoverer probe +
    // pipeline preroll, up to ~20s for a large file) runs on a background
    // thread behind a progress popup, so the GUI stays responsive for that
    // specific slow case. ---
    let (open_video_tx, open_video_rx) = async_channel::unbounded::<(u64, OpenVideoOutcome)>();
    let open_popup: Rc<RefCell<Option<ProgressPopup>>> = Rc::new(RefCell::new(None));
    {
        let state = state.clone();
        let drawing_area = drawing_area.clone();
        let status_label = status_label.clone();
        let video_controls = video_controls.clone();
        let set_controls_enabled = set_controls_enabled.clone();
        let open_popup = open_popup.clone();
        let position_scale = position_scale.clone();
        let position_label = position_label.clone();
        let play_button = play_button.clone();
        let update_range_label = update_range_label.clone();
        glib::spawn_future_local(async move {
            while let Ok((generation, outcome)) = open_video_rx.recv().await {
                // Stale: Cancel already closed the popup, re-enabled controls,
                // and bumped generation before this result arrived — discard it
                // (dropping a Loaded outcome tears the GStreamer pipeline down
                // via its Drop impl).
                if state.borrow().generation != generation {
                    continue;
                }
                if let Some(popup) = open_popup.borrow_mut().take() {
                    popup.close();
                }
                set_controls_enabled(true);
                match outcome {
                    OpenVideoOutcome::Loaded(path, pipeline) => {
                        let pipeline = Rc::new(pipeline);
                        let mut s = state.borrow_mut();
                        if let Some(MediaSource::Video { pipeline: old, .. }) = s.source.take() {
                            old.stop();
                        }
                        s.crop = CropState::new();
                        s.video_in = None;
                        s.video_out = None;
                        s.video_duration_s = 0.0;
                        s.video_duration_known = false;
                        s.video_playing = true;
                        s.preview_surface = None;
                        s.source = Some(MediaSource::Video { path, pipeline: pipeline.clone() });
                        drop(s);
                        pipeline.play();
                        video_controls.set_visible(true);
                        position_scale_reset(&position_scale, &update_range_label);
                        play_button.set_label("Pause");
                        drawing_area.queue_draw();
                        status_label.set_text("Video playing — set In/Out, position the crop, then Export & Send.");
                        spawn_video_timer(generation, state.clone(), drawing_area.clone(), position_scale.clone(), position_label.clone(), update_range_label.clone());
                    }
                    OpenVideoOutcome::Failed(e) => {
                        status_label.set_text(&format!("Not a recognized image, GIF, or video: {e}"));
                    }
                }
            }
        });
    }

    {
        let state = state.clone();
        let drawing_area = drawing_area.clone();
        let status_label = status_label.clone();
        let window = window.clone();
        let gif_video_radio = gif_video_radio.clone();
        let video_controls = video_controls.clone();
        let set_controls_enabled = set_controls_enabled.clone();
        let open_video_tx = open_video_tx.clone();
        let open_popup = open_popup.clone();
        open_button.connect_clicked(move |_| {
            let state = state.clone();
            let drawing_area = drawing_area.clone();
            let status_label = status_label.clone();
            let window = window.clone();
            let allow_video = gif_video_radio.is_active();
            let video_controls = video_controls.clone();
            let set_controls_enabled = set_controls_enabled.clone();
            let open_video_tx = open_video_tx.clone();
            let open_popup = open_popup.clone();
            glib::spawn_future_local(async move {
                let filter = gtk4::FileFilter::new();
                let patterns: &[&str] = if allow_video {
                    &["*.gif", "*.webp", "*.mp4", "*.webm", "*.mkv", "*.mov", "*.avi"]
                } else {
                    &["*.png", "*.jpg", "*.jpeg", "*.bmp", "*.tiff", "*.webp"]
                };
                for pattern in patterns {
                    filter.add_pattern(pattern);
                }
                let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                let title = if allow_video { "Open GIF or Video" } else { "Open Image" };
                let dialog = FileDialog::builder().title(title).filters(&filters).build();
                let file = match dialog.open_future(Some(&window)).await {
                    Ok(file) => file,
                    Err(e) => {
                        status_label.set_text(&format!("{e}"));
                        return;
                    }
                };
                let Some(path) = file.path() else {
                    status_label.set_text("Selected file has no local path");
                    return;
                };

                // Try as a static image / GIF / animated-WebP first — fast,
                // stays synchronous on the main thread (see comment above).
                match image_source::load(&path) {
                    Ok(frames) if allow_video && frames.len() <= 1 => {
                        status_label.set_text("That's a static image — switch to \"Images\" to open it.");
                        return;
                    }
                    Ok(frames) => {
                        let count = frames.len();
                        let is_animated = frames.len() > 1;
                        let mut s = state.borrow_mut();
                        if let Some(MediaSource::Video { pipeline, .. }) = s.source.take() {
                            pipeline.stop();
                        }
                        s.generation += 1;
                        let generation = s.generation;
                        s.preview_surface = frames.first().map(|f| f.surface.clone());
                        s.anim_index = 0;
                        s.anim_started_at = Instant::now();
                        s.source = Some(MediaSource::Frames(frames));
                        s.crop = CropState::new();
                        drop(s);
                        video_controls.set_visible(false);
                        drawing_area.queue_draw();
                        status_label.set_text(&format!("Loaded {count} frame(s)."));
                        if is_animated {
                            spawn_gif_timer(generation, state.clone(), drawing_area.clone());
                        }
                        return;
                    }
                    Err(e) if !allow_video => {
                        status_label.set_text(&format!("Failed to load: {e}"));
                        return;
                    }
                    Err(_) => {} // fall through to the video path below
                }

                let generation = {
                    let mut s = state.borrow_mut();
                    s.generation += 1;
                    s.generation
                };

                set_controls_enabled(false);
                let popup = ProgressPopup::new(&window, "Opening…", {
                    let state = state.clone();
                    let status_label = status_label.clone();
                    let set_controls_enabled = set_controls_enabled.clone();
                    let open_popup = open_popup.clone();
                    move || {
                        // Soft cancel: the probe/preroll inside LivePreviewPipeline::new
                        // can't be interrupted mid-call, so just bump generation to
                        // discard the eventual background result on arrival, and
                        // unblock the UI right now.
                        state.borrow_mut().generation += 1;
                        status_label.set_text("Cancelled.");
                        set_controls_enabled(true);
                        if let Some(popup) = open_popup.borrow_mut().take() {
                            popup.close();
                        }
                    }
                });
                popup.set_status("Loading video…");
                *open_popup.borrow_mut() = Some(popup);

                std::thread::spawn(move || {
                    let outcome = match LivePreviewPipeline::new(&path) {
                        Ok(pipeline) => OpenVideoOutcome::Loaded(path, pipeline),
                        Err(e) => OpenVideoOutcome::Failed(e),
                    };
                    let _ = open_video_tx.send_blocking((generation, outcome));
                });

                // Popup stays open (in `open_popup`) until the consumer task
                // above closes it — nothing more to do on this task.
            });
        });
    }

    // --- Export & Send button. ---
    // Runs the actual work (crop/convert, and for video the extraction too)
    // on a background thread so it can't freeze the UI; progress/result come
    // back over an async channel consumed on the GTK main context, driving a
    // progress popup instead of just the bottom status label.
    let (result_tx, result_rx) = async_channel::unbounded::<ExportProgress>();
    let export_popup: Rc<RefCell<Option<ProgressPopup>>> = Rc::new(RefCell::new(None));
    {
        let status_label = status_label.clone();
        let export_popup = export_popup.clone();
        let set_controls_enabled = set_controls_enabled.clone();
        glib::spawn_future_local(async move {
            while let Ok(progress) = result_rx.recv().await {
                let popup_ref = export_popup.borrow();
                let Some(popup) = popup_ref.as_ref() else { continue };
                match progress {
                    ExportProgress::Phase { label, current, total } => {
                        popup.set_status(&format!("{label} frame {current}/{total}"));
                        popup.set_determinate(current as f64 / total.max(1) as f64);
                    }
                    ExportProgress::Sending => {
                        popup.set_status("Sending to daemon…");
                        popup.set_indeterminate();
                        popup.set_cancel_enabled(false);
                    }
                    ExportProgress::Cancelled => {
                        drop(popup_ref);
                        if let Some(popup) = export_popup.borrow_mut().take() {
                            popup.close();
                        }
                        status_label.set_text("Cancelled.");
                        set_controls_enabled(true);
                    }
                    ExportProgress::Done(result) => {
                        drop(popup_ref);
                        if let Some(popup) = export_popup.borrow_mut().take() {
                            popup.close();
                        }
                        match result {
                            Ok(Response::Ok) => status_label.set_text("Sent to coolctld successfully."),
                            Ok(Response::Error { message }) => status_label.set_text(&format!("Daemon error: {message}")),
                            Ok(other) => status_label.set_text(&format!("Unexpected response: {other:?}")),
                            Err(e) => status_label.set_text(&format!("Failed: {e}")),
                        }
                        set_controls_enabled(true);
                    }
                }
            }
        });
    }

    {
        let state = state.clone();
        let status_label = status_label.clone();
        let orientation = orientation.clone();
        let window = window.clone();
        let set_controls_enabled = set_controls_enabled.clone();
        let export_popup = export_popup.clone();
        export_button.connect_clicked(move |_| {
            let current_orientation = orientation.get();
            let (target_w, target_h) = target_dims(current_orientation);
            let s = state.borrow();
            let export_state = crop_view::preview_to_export_state(&s.crop, s.last_window_size.0, s.last_window_size.1, target_w, target_h);

            match &s.source {
                None => {
                    drop(s);
                    status_label.set_text("Open an image, GIF, or video first.");
                }
                Some(MediaSource::Frames(frames)) => {
                    // cairo::ImageSurface (inside SourceFrame) is not Send, so the
                    // conversion loop can never move to a background thread — it runs
                    // synchronously right here, same as before this feature. Image/GIF
                    // frame counts are small and conversion is fast (unlike video's
                    // extraction+conversion, which does run in the background below),
                    // so there's no real cancel point to offer — the button starts
                    // disabled rather than promising something that can't work.
                    set_controls_enabled(false);
                    let popup = ProgressPopup::new(&window, "Exporting…", || {});
                    popup.set_status("Converting…");
                    popup.set_cancel_enabled(false);
                    *export_popup.borrow_mut() = Some(popup);

                    let never_cancel = AtomicBool::new(false);
                    let outcome = frames_to_media_payload(frames, &export_state, target_w, target_h, current_orientation, &never_cancel, |_, _| {});
                    drop(s);

                    let Some((raw_frames, delays_ms, frame_count)) = outcome else {
                        unreachable!("never_cancel is never set");
                    };
                    if let Some(popup) = export_popup.borrow().as_ref() {
                        popup.set_status("Sending to daemon…");
                    }
                    let result_tx = result_tx.clone();
                    std::thread::spawn(move || {
                        let result = ipc_client::send_media(frame_count, delays_ms, &raw_frames);
                        let _ = result_tx.send_blocking(ExportProgress::Done(result));
                    });
                }
                Some(MediaSource::Video { path, .. }) => {
                    let (Some(in_ct), Some(out_ct)) = (s.video_in, s.video_out) else {
                        drop(s);
                        status_label.set_text("Set In and Out points first.");
                        return;
                    };
                    let length_s = (out_ct.nseconds() as f64 - in_ct.nseconds() as f64) / 1e9;
                    if out_ct <= in_ct || length_s < MIN_TRIM_SECONDS {
                        let msg = format!("Trim length must be at least {MIN_TRIM_SECONDS}s (currently {length_s:.1}s).");
                        drop(s);
                        status_label.set_text(&msg);
                        return;
                    }
                    let path = path.clone();
                    drop(s);
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    set_controls_enabled(false);
                    *export_popup.borrow_mut() = Some(ProgressPopup::new(&window, "Exporting…", {
                        let cancel_flag = cancel_flag.clone();
                        move || cancel_flag.store(true, Ordering::Relaxed)
                    }));
                    let result_tx = result_tx.clone();
                    std::thread::spawn(move || {
                        let extracted = video_source::extract_trimmed_frames(&path, in_ct, out_ct, 10.0, &cancel_flag, |current, total| {
                            let _ = result_tx.send_blocking(ExportProgress::Phase { label: "Extracting", current, total });
                        });
                        let frames = match extracted {
                            Ok(Some(frames)) => frames,
                            Ok(None) => {
                                let _ = result_tx.send_blocking(ExportProgress::Cancelled);
                                return;
                            }
                            Err(e) => {
                                let _ = result_tx.send_blocking(ExportProgress::Done(Err(format!("extraction failed: {e}"))));
                                return;
                            }
                        };
                        let Some((raw_frames, delays_ms, frame_count)) = frames_to_media_payload(&frames, &export_state, target_w, target_h, current_orientation, &cancel_flag, |current, total| {
                            let _ = result_tx.send_blocking(ExportProgress::Phase { label: "Converting", current, total });
                        }) else {
                            let _ = result_tx.send_blocking(ExportProgress::Cancelled);
                            return;
                        };
                        let _ = result_tx.send_blocking(ExportProgress::Sending);
                        let result = ipc_client::send_media(frame_count, delays_ms, &raw_frames);
                        let _ = result_tx.send_blocking(ExportProgress::Done(result));
                    });
                }
            }
        });
    }

    let button_box = gtk4::Box::new(BoxOrientation::Horizontal, 8);
    button_box.append(&images_radio);
    button_box.append(&gif_video_radio);
    button_box.append(&open_button);
    button_box.append(&export_button);

    let vbox = gtk4::Box::new(BoxOrientation::Vertical, 8);
    vbox.append(&drawing_area);
    vbox.append(&video_controls);
    vbox.append(&button_box);
    vbox.append(&status_label);

    // Orientation changed elsewhere (the top-level selector, common to both
    // tabs): the aspect ratio this page crops against just changed underfoot,
    // so re-fit like a fresh Open would rather than leaving a stale crop.
    let on_orientation_changed: Rc<dyn Fn()> = {
        let state = state.clone();
        let drawing_area = drawing_area.clone();
        Rc::new(move || {
            state.borrow_mut().crop = CropState::new();
            drawing_area.queue_draw();
        })
    };

    (vbox, on_orientation_changed)
}

fn position_scale_reset(position_scale: &Scale, update_range_label: &impl Fn()) {
    position_scale.set_range(0.0, 1.0);
    update_range_label();
}

/// Cycles an animated Frames source's `anim_index` by each frame's own
/// `delay_ms` — same timing model `coolctld`'s own Media-mode loop uses.
/// Self-terminates once `generation` no longer matches (a new file was opened).
fn spawn_gif_timer(generation: u64, state: Rc<RefCell<EditorState>>, drawing_area: DrawingArea) {
    glib::timeout_add_local(Duration::from_millis(GIF_TICK_MS), move || {
        let mut s = state.borrow_mut();
        if s.generation != generation {
            return glib::ControlFlow::Break;
        }
        let advance = if let Some(MediaSource::Frames(frames)) = &s.source {
            if frames.len() > 1 && s.anim_started_at.elapsed() >= Duration::from_millis(frames[s.anim_index].delay_ms) {
                let next = (s.anim_index + 1) % frames.len();
                Some((next, frames[next].surface.clone()))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((next, next_surface)) = advance {
            s.anim_index = next;
            s.anim_started_at = Instant::now();
            s.preview_surface = Some(next_surface);
            drop(s);
            drawing_area.queue_draw();
        }
        glib::ControlFlow::Continue
    });
}

/// Pulls whatever live video frame is ready (non-blocking), resolves the
/// real duration once GStreamer has it (often not available immediately
/// after preroll — see the Milestone 6 fix this carries forward), and
/// updates the position display while playing. Self-terminates once
/// `generation` no longer matches.
#[allow(clippy::too_many_arguments)]
fn spawn_video_timer(
    generation: u64,
    state: Rc<RefCell<EditorState>>,
    drawing_area: DrawingArea,
    position_scale: Scale,
    position_label: Label,
    update_range_label: impl Fn() + 'static,
) {
    glib::timeout_add_local(Duration::from_millis(VIDEO_TICK_MS), move || {
        let mut s = state.borrow_mut();
        if s.generation != generation {
            return glib::ControlFlow::Break;
        }
        let Some(MediaSource::Video { pipeline, .. }) = &s.source else {
            return glib::ControlFlow::Break;
        };
        let pipeline = pipeline.clone();

        if !s.video_duration_known {
            if let Some(duration) = pipeline.duration() {
                let seconds = duration.nseconds() as f64 / 1e9;
                if seconds > 0.0 {
                    s.video_duration_s = seconds;
                    s.video_duration_known = true;
                    position_scale.set_range(0.0, seconds);
                    drop(s);
                    update_range_label();
                    s = state.borrow_mut();
                }
            }
        }

        if let Some(frame) = pipeline.try_pull_frame() {
            s.preview_surface = Some(frame);
            drop(s);
            drawing_area.queue_draw();
            s = state.borrow_mut();
        }

        if s.video_playing {
            if let Some(position) = pipeline.position() {
                let seconds = position.nseconds() as f64 / 1e9;
                position_scale.set_value(seconds);
                position_label.set_text(&format!("Position: {}", format_duration(seconds)));
            }
        }

        glib::ControlFlow::Continue
    });
}
