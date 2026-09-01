//! Monitor Mode frame rendering — a shared CPU/GPU "stat box" layout, reused for
//! both the landscape (0°/180°) and portrait (90°/270°) canvases so all four
//! orientations get a real, non-cropped dashboard. Uses `image`+`imageproc`+
//! `ab_glyph` (pure Rust, no system Cairo dependency). Icon glyphs ("⚙"/"▣") from
//! the Python version are dropped in favor of plain "CPU"/"GPU" labels for font-
//! coverage robustness.

use ab_glyph::{FontArc, PxScale};
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_circle_mut, draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;

use coolctl_core::{rotate_rgb565, Orientation, PANEL_HEIGHT, PANEL_WIDTH};

const BG: Rgb<u8> = Rgb([14, 14, 18]);
const OUTLINE: Rgb<u8> = Rgb([40, 40, 50]);
const LABEL_COLOR: Rgb<u8> = Rgb([110, 110, 130]);
const DETAIL_COLOR: Rgb<u8> = Rgb([130, 130, 150]);
const BAR_BG: Rgb<u8> = Rgb([30, 30, 38]);

const SCALE_MEDIUM: f32 = 40.0; // big temp numbers
const SCALE_LABEL: f32 = 26.0; // "CPU"/"GPU" labels
const SCALE_SMALL: f32 = 20.0; // usage detail line

// Shared stat-box geometry (relative to each box's own top-left corner), reused
// by both the landscape and portrait canvases.
const BOX_MARGIN: i32 = 12; // left/right canvas margin
const BOX_H: i32 = 112;
const LABEL_OFFSET_Y: i32 = 10;
const DETAIL_OFFSET_Y: i32 = 58;
const BAR_OFFSET_Y: i32 = 83;
const BAR_H: i32 = 18;

pub struct Fonts {
    bold: FontArc,
    regular: FontArc,
}

pub fn load_fonts() -> Fonts {
    let bold_bytes = std::fs::read("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf")
        .expect("missing /usr/share/fonts/TTF/DejaVuSans-Bold.ttf");
    let regular_bytes = std::fs::read("/usr/share/fonts/TTF/DejaVuSans.ttf")
        .expect("missing /usr/share/fonts/TTF/DejaVuSans.ttf");

    Fonts {
        bold: FontArc::try_from_slice(Box::leak(bold_bytes.into_boxed_slice()))
            .expect("invalid DejaVuSans-Bold.ttf"),
        regular: FontArc::try_from_slice(Box::leak(regular_bytes.into_boxed_slice()))
            .expect("invalid DejaVuSans.ttf"),
    }
}

pub struct SystemInfo {
    pub cpu_temp: f32,
    pub cpu_percent: f32,
    pub cpu_freq: f32,
    pub gpu_temp: f32,
    pub gpu_percent: f32,
    pub gpu_freq: f32,
}

fn get_temp_color(temp: f32) -> Rgb<u8> {
    if temp < 40.0 {
        Rgb([100, 200, 255])
    } else if temp < 60.0 {
        Rgb([100, 255, 100])
    } else if temp < 75.0 {
        Rgb([255, 220, 50])
    } else if temp < 85.0 {
        Rgb([255, 140, 0])
    } else {
        Rgb([255, 50, 50])
    }
}

/// Filled rounded rect via the standard "cross of two rects + 4 corner circles" trick.
fn draw_filled_rounded_rect(canvas: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, radius: i32, color: Rgb<u8>) {
    let r = radius.max(0).min(w / 2).min(h / 2);

    let a_w = w - 2 * r;
    if a_w > 0 && h > 0 {
        draw_filled_rect_mut(canvas, Rect::at(x + r, y).of_size(a_w as u32, h as u32), color);
    }
    let b_h = h - 2 * r;
    if b_h > 0 && w > 0 {
        draw_filled_rect_mut(canvas, Rect::at(x, y + r).of_size(w as u32, b_h as u32), color);
    }
    if r > 0 {
        draw_filled_circle_mut(canvas, (x + r, y + r), r, color);
        draw_filled_circle_mut(canvas, (x + w - r - 1, y + r), r, color);
        draw_filled_circle_mut(canvas, (x + r, y + h - r - 1), r, color);
        draw_filled_circle_mut(canvas, (x + w - r - 1, y + h - r - 1), r, color);
    }
}

/// Outline-only rounded rect: fill in the outline color, then punch out the
/// interior in the background color, leaving a ring of the given width.
fn draw_rounded_rect_outline(canvas: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, radius: i32, color: Rgb<u8>, width: i32) {
    draw_filled_rounded_rect(canvas, x, y, w, h, radius, color);
    draw_filled_rounded_rect(canvas, x + width, y + width, w - 2 * width, h - 2 * width, (radius - width).max(0), BG);
}

fn draw_usage_bar(canvas: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, percent: f32) {
    let radius = h / 2;
    draw_filled_rounded_rect(canvas, x, y, w, h, radius, BAR_BG);

    let fill_w = (w as f32 * (percent / 100.0)) as i32;
    if fill_w >= radius * 2 {
        let color = if percent < 75.0 { Rgb([100, 180, 255]) } else { Rgb([255, 150, 50]) };
        draw_filled_rounded_rect(canvas, x, y, fill_w, h, radius, color);
    }
}

fn draw_right_aligned_temp(canvas: &mut RgbImage, canvas_width: i32, y: i32, temp: f32, color: Rgb<u8>, font: &FontArc) {
    let text = format!("{:.0}\u{00b0}", temp);
    let scale = PxScale::from(SCALE_MEDIUM);
    let (text_w, _) = text_size(scale, font, &text);
    draw_text_mut(canvas, color, canvas_width - 20 - text_w as i32, y, scale, font, &text);
}

/// Draw one CPU/GPU stat box (rounded card, label, big temp number, usage/clock
/// line, usage bar) at the given position — reused by both canvas orientations.
#[allow(clippy::too_many_arguments)]
fn draw_stat_box(
    canvas: &mut RgbImage,
    canvas_width: i32,
    box_x: i32,
    box_y: i32,
    box_w: i32,
    label: &str,
    temp: f32,
    percent: f32,
    freq_ghz: f32,
    fonts: &Fonts,
) {
    let color = get_temp_color(temp);

    draw_rounded_rect_outline(canvas, box_x, box_y, box_w, BOX_H, 8, OUTLINE, 2);
    draw_text_mut(canvas, LABEL_COLOR, box_x + 8, box_y + LABEL_OFFSET_Y, PxScale::from(SCALE_LABEL), &fonts.bold, label);
    draw_right_aligned_temp(canvas, canvas_width, box_y + LABEL_OFFSET_Y, temp, color, &fonts.bold);

    let detail = format!("Usage: {:.0}% \u{2022} {:.2} GHz", percent, freq_ghz);
    draw_text_mut(canvas, DETAIL_COLOR, box_x + 8, box_y + DETAIL_OFFSET_Y, PxScale::from(SCALE_SMALL), &fonts.regular, &detail);

    draw_usage_bar(canvas, box_x + 11, box_y + BAR_OFFSET_Y, box_w - 22, BAR_H, percent);
}

/// Landscape canvas (320x240): CPU box on top, GPU box below.
fn render_landscape(info: &SystemInfo, fonts: &Fonts) -> RgbImage {
    let width = PANEL_WIDTH as i32;
    let height = PANEL_HEIGHT as i32;
    let mut img = RgbImage::from_pixel(width as u32, height as u32, BG);

    let box_w = width - 2 * BOX_MARGIN;
    let cpu_box_y = 8;
    let gpu_box_y = 124;

    draw_stat_box(&mut img, width, BOX_MARGIN, cpu_box_y, box_w, "CPU", info.cpu_temp, info.cpu_percent, info.cpu_freq, fonts);
    draw_stat_box(&mut img, width, BOX_MARGIN, gpu_box_y, box_w, "GPU", info.gpu_temp, info.gpu_percent, info.gpu_freq, fonts);

    img
}

/// Portrait canvas (240x320): same two stat boxes, stacked with more room to breathe.
fn render_portrait(info: &SystemInfo, fonts: &Fonts) -> RgbImage {
    let width = PANEL_HEIGHT as i32; // 240
    let height = PANEL_WIDTH as i32; // 320
    let mut img = RgbImage::from_pixel(width as u32, height as u32, BG);

    let box_w = width - 2 * BOX_MARGIN;
    let cpu_box_y = 32;
    let gpu_box_y = 176;

    draw_stat_box(&mut img, width, BOX_MARGIN, cpu_box_y, box_w, "CPU", info.cpu_temp, info.cpu_percent, info.cpu_freq, fonts);
    draw_stat_box(&mut img, width, BOX_MARGIN, gpu_box_y, box_w, "GPU", info.gpu_temp, info.gpu_percent, info.gpu_freq, fonts);

    img
}

/// Render the Monitor Mode dashboard for the given orientation and return the
/// final 320x240 wire-format RGB565 framebuffer, ready to send to the panel.
pub fn render_monitor_frame(info: &SystemInfo, fonts: &Fonts, orientation: Orientation) -> Vec<u8> {
    match orientation {
        Orientation::Deg0 => {
            let img = render_landscape(info, fonts);
            rgb_image_to_rgb565(&img)
        }
        Orientation::Deg180 => {
            let img = render_landscape(info, fonts);
            let raw = rgb_image_to_rgb565(&img);
            rotate_rgb565(&raw, PANEL_WIDTH, PANEL_HEIGHT, Orientation::Deg180)
        }
        Orientation::Deg90 => {
            let img = render_portrait(info, fonts);
            let raw = rgb_image_to_rgb565(&img);
            rotate_rgb565(&raw, PANEL_HEIGHT, PANEL_WIDTH, Orientation::Deg90)
        }
        Orientation::Deg270 => {
            let img = render_portrait(info, fonts);
            let raw = rgb_image_to_rgb565(&img);
            rotate_rgb565(&raw, PANEL_HEIGHT, PANEL_WIDTH, Orientation::Deg270)
        }
    }
}

/// Flat-color frame — orientation-independent (a rotated solid color looks
/// identical), so no rotation is needed here unlike Monitor Mode.
pub fn solid_frame(color: [u8; 3]) -> Vec<u8> {
    let img = RgbImage::from_pixel(PANEL_WIDTH, PANEL_HEIGHT, Rgb(color));
    rgb_image_to_rgb565(&img)
}

/// Convert a rendered RGB frame to the panel's raw RGB565 framebuffer format
/// (row-major, little-endian), matching the validated Python conversion.
pub fn rgb_image_to_rgb565(img: &RgbImage) -> Vec<u8> {
    let mut buf = Vec::with_capacity((img.width() * img.height() * 2) as usize);
    for pixel in img.pixels() {
        let [r, g, b] = pixel.0;
        let r5 = (r >> 3) as u16;
        let g6 = (g >> 2) as u16;
        let b5 = (b >> 3) as u16;
        let value = (r5 << 11) | (g6 << 5) | b5;
        buf.push((value & 0xFF) as u8);
        buf.push((value >> 8) as u8);
    }
    buf
}
