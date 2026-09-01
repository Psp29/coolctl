//! Offline verification for Milestone 5's export path, run with no GTK/display
//! needed: `cargo run --example verify_export -p coolctl-gui`.
//! 1. Round-trips a known solid color through render_crop_export +
//!    argb32_to_rgb565 to catch a byte-order mistake immediately.
//! 2. Renders the Milestone 4 test image through both the preview path and
//!    the export path at an equivalent crop, dumping both to PNG so the
//!    framing can be visually compared.

use cairo::{Context, Format, ImageSurface};
use coolctl_gui::editor::crop_view::{self, CropState};

const SCRATCH: &str = "/tmp/claude-1000/-home-prasad-Documents-lm360/e36826fc-7840-4cff-9ff0-6941752a5df7/scratchpad";

fn solid_color_surface(width: i32, height: i32, r: u8, g: u8, b: u8) -> ImageSurface {
    let surface = ImageSurface::create(Format::ARgb32, width, height).unwrap();
    let cr = Context::new(&surface).unwrap();
    cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    cr.paint().unwrap();
    drop(cr);
    surface
}

fn rgb565_to_rgb(lo: u8, hi: u8) -> (u8, u8, u8) {
    let value = ((hi as u16) << 8) | lo as u16;
    let r5 = (value >> 11) & 0x1F;
    let g6 = (value >> 5) & 0x3F;
    let b5 = value & 0x1F;
    (((r5 << 3) | (r5 >> 2)) as u8, ((g6 << 2) | (g6 >> 4)) as u8, ((b5 << 3) | (b5 >> 2)) as u8)
}

fn check_roundtrip(name: &str, r: u8, g: u8, b: u8) {
    let source = solid_color_surface(20, 20, r, g, b);
    let state = CropState { offset_x: 0.0, offset_y: 0.0, scale: 1.0 };
    let mut exported = crop_view::render_crop_export(&state, 20, 20, &source);
    let bytes = crop_view::argb32_to_rgb565(&mut exported);
    let (out_r, out_g, out_b) = rgb565_to_rgb(bytes[0], bytes[1]);

    // RGB565 quantizes 8-bit color down to 5/6/5 bits, so allow the
    // corresponding rounding tolerance instead of expecting an exact match.
    let close = |a: u8, b: u8, tol: u8| (a as i16 - b as i16).unsigned_abs() as u8 <= tol;
    let ok = close(out_r, r, 8) && close(out_g, g, 4) && close(out_b, b, 8);
    println!(
        "{name}: input ({r},{g},{b}) -> exported ({out_r},{out_g},{out_b}) [{}]",
        if ok { "OK" } else { "MISMATCH" }
    );
    assert!(ok, "{name}: round-trip color mismatch");
}

fn main() {
    println!("=== round-trip color checks ===");
    check_roundtrip("red", 255, 0, 0);
    check_roundtrip("green", 0, 255, 0);
    check_roundtrip("blue", 0, 0, 255);
    check_roundtrip("white", 255, 255, 255);
    check_roundtrip("gray", 128, 128, 128);
    println!("all round-trip checks passed");

    println!("\n=== preview vs export framing comparison ===");
    let mut test_image_file = std::fs::File::open(format!("{SCRATCH}/crop_test_image.png")).expect("run the Milestone 4 spike's test image generator first");
    let test_image = ImageSurface::create_from_png(&mut test_image_file).unwrap();

    let window_w = 640.0;
    let window_h = 480.0;
    let target_w = 320.0;
    let target_h = 240.0;
    let preview_state = CropState { offset_x: -100.0, offset_y: -60.0, scale: 0.6 };

    // Preview render, at window scale.
    let preview_surface = ImageSurface::create(Format::ARgb32, window_w as i32, window_h as i32).unwrap();
    let cr = Context::new(&preview_surface).unwrap();
    crop_view::draw_crop_view(&cr, window_w as i32, window_h as i32, &preview_state, target_w, target_h, &test_image);
    drop(cr);
    let mut preview_file = std::fs::File::create(format!("{SCRATCH}/verify_preview.png")).unwrap();
    preview_surface.write_to_png(&mut preview_file).unwrap();

    // Export render, at native resolution, using the converted state.
    let export_state = crop_view::preview_to_export_state(&preview_state, window_w, window_h, target_w, target_h);
    let export_surface = crop_view::render_crop_export(&export_state, target_w as i32, target_h as i32, &test_image);
    let mut export_file = std::fs::File::create(format!("{SCRATCH}/verify_export.png")).unwrap();
    export_surface.write_to_png(&mut export_file).unwrap();

    println!("wrote {SCRATCH}/verify_preview.png and {SCRATCH}/verify_export.png for comparison");

    println!("\n=== mislabeled-WebP regression check ===");
    let webp_as_gif = std::path::Path::new("/home/prasad/Downloads/giphy.gif");
    if webp_as_gif.exists() {
        match coolctl_gui::editor::image_source::load(webp_as_gif) {
            Ok(frames) => println!("loaded {} frame(s) from {} (WebP content, .gif filename)", frames.len(), webp_as_gif.display()),
            Err(e) => println!("FAILED to load {}: {e}", webp_as_gif.display()),
        }
    } else {
        println!("skipped: {} not present", webp_as_gif.display());
    }
}
