//! Crop viewport: the interactive pan/zoom preview (promoted from the
//! Milestone 4 spike) plus the native-resolution export path that produces
//! the final RGB565 frame sent to the daemon.

use cairo::{Context, Format, ImageSurface};

#[derive(Clone, Copy)]
pub struct CropState {
    pub offset_x: f64,
    pub offset_y: f64,
    pub scale: f64,
}

impl CropState {
    pub fn new() -> Self {
        CropState { offset_x: 0.0, offset_y: 0.0, scale: 1.0 }
    }
}

impl Default for CropState {
    fn default() -> Self {
        Self::new()
    }
}

const PADDING: f64 = 20.0;
const BG: (f64, f64, f64) = (0.08, 0.08, 0.1); // matches coolctld's monitor-mode background

/// Fixed viewport rect (x, y, w, h) centered in a window of the given size,
/// sized to fit while preserving `target_w:target_h`, plus the fit scale
/// factor that maps target-resolution pixels to on-screen preview pixels.
fn fit_viewport(window_w: f64, window_h: f64, target_w: f64, target_h: f64) -> (f64, f64, f64, f64, f64) {
    let avail_w = window_w - 2.0 * PADDING;
    let avail_h = window_h - 2.0 * PADDING;
    let fit_scale = (avail_w / target_w).min(avail_h / target_h);
    let vp_w = target_w * fit_scale;
    let vp_h = target_h * fit_scale;
    let vp_x = (window_w - vp_w) / 2.0;
    let vp_y = (window_h - vp_h) / 2.0;
    (vp_x, vp_y, vp_w, vp_h, fit_scale)
}

/// Interactive preview: image inside a dimmed, bordered viewport at the given
/// target aspect ratio, fit into the window.
pub fn draw_crop_view(cr: &Context, width: i32, height: i32, state: &CropState, target_w: f64, target_h: f64, image: &ImageSurface) {
    let width = width as f64;
    let height = height as f64;

    cr.set_source_rgb(BG.0, BG.1, BG.2);
    cr.paint().unwrap();

    let (vp_x, vp_y, vp_w, vp_h, _fit_scale) = fit_viewport(width, height, target_w, target_h);

    cr.save().unwrap();
    cr.rectangle(vp_x, vp_y, vp_w, vp_h);
    cr.clip();
    cr.translate(vp_x + state.offset_x, vp_y + state.offset_y);
    cr.scale(state.scale, state.scale);
    cr.set_source_surface(image, 0.0, 0.0).unwrap();
    let _ = cr.paint();
    cr.restore().unwrap();

    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.rectangle(0.0, 0.0, width, vp_y);
    cr.fill().unwrap();
    cr.rectangle(0.0, vp_y + vp_h, width, height - (vp_y + vp_h));
    cr.fill().unwrap();
    cr.rectangle(0.0, vp_y, vp_x, vp_h);
    cr.fill().unwrap();
    cr.rectangle(vp_x + vp_w, vp_y, width - (vp_x + vp_w), vp_h);
    cr.fill().unwrap();

    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.0);
    cr.rectangle(vp_x, vp_y, vp_w, vp_h);
    cr.stroke().unwrap();
}

/// Convert a CropState recorded in on-screen preview pixels (at the given
/// window size) into the equivalent state for a 1:1 render at the panel's
/// true resolution.
///
/// Both `offset` *and* `scale` need dividing by `fit_scale`, not just the
/// offset — `scale` was recorded relative to the enlarged preview viewport
/// (which is `fit_scale` times bigger than native resolution), so 1 source
/// pixel there covered `fit_scale * scale` on-screen pixels. To show the
/// identical crop framing at native resolution (1 source pixel = `export_scale`
/// output pixels, no enlargement), `export_scale` must equal `scale /
/// fit_scale`. (An earlier version of this function left `scale` unconverted,
/// which produced a visibly tighter/more-zoomed export than the preview
/// showed — caught by comparing offline preview-vs-export renders before any
/// live testing.)
pub fn preview_to_export_state(state: &CropState, window_w: f64, window_h: f64, target_w: f64, target_h: f64) -> CropState {
    let (_, _, _, _, fit_scale) = fit_viewport(window_w, window_h, target_w, target_h);
    CropState {
        offset_x: state.offset_x / fit_scale,
        offset_y: state.offset_y / fit_scale,
        scale: state.scale / fit_scale,
    }
}

/// Render the crop at native resolution — no dimming/border, just the
/// cropped image content, ready to convert to RGB565.
pub fn render_crop_export(export_state: &CropState, target_w: i32, target_h: i32, image: &ImageSurface) -> ImageSurface {
    let surface = ImageSurface::create(Format::ARgb32, target_w, target_h).expect("failed to create export surface");
    let cr = Context::new(&surface).expect("failed to create context");

    cr.set_source_rgb(BG.0, BG.1, BG.2);
    cr.paint().unwrap();

    cr.translate(export_state.offset_x, export_state.offset_y);
    cr.scale(export_state.scale, export_state.scale);
    cr.set_source_surface(image, 0.0, 0.0).unwrap();
    let _ = cr.paint();

    drop(cr);
    surface
}

/// Convert a Cairo ARGB32 surface to the panel's RGB565 wire format
/// (row-major, little-endian). Cairo's ARGB32 is premultiplied
/// `0xAARRGGBB`; every pixel here is fully opaque (the background fill is
/// drawn first at full alpha) so premultiplication is a no-op and byte order
/// `[B, G, R, A]` can be read directly as unpremultiplied color.
pub fn argb32_to_rgb565(surface: &mut ImageSurface) -> Vec<u8> {
    let width = surface.width() as usize;
    let height = surface.height() as usize;
    let stride = surface.stride() as usize;
    let data = surface.data().expect("failed to borrow surface data");

    let mut buf = Vec::with_capacity(width * height * 2);
    for y in 0..height {
        let row = &data[y * stride..y * stride + width * 4];
        for x in 0..width {
            let px = &row[x * 4..x * 4 + 4];
            let (b, g, r) = (px[0], px[1], px[2]);
            let r5 = (r >> 3) as u16;
            let g6 = (g >> 2) as u16;
            let b5 = (b >> 3) as u16;
            let value = (r5 << 11) | (g6 << 5) | b5;
            buf.push((value & 0xFF) as u8);
            buf.push((value >> 8) as u8);
        }
    }
    buf
}
