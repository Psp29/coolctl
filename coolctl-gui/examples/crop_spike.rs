//! Phase 2 Milestone 4 spike: an isolated prototype of the pan/zoom crop
//! interaction (fixed-aspect viewport, source image pans/zooms underneath it —
//! the Instagram-style crop-to-frame pattern). Deliberately not wired into any
//! app shell yet; gets promoted into coolctl-gui/src/editor/crop_view.rs once
//! the interaction is proven to feel right.
//!
//! `cargo run --example crop_spike -p coolctl-gui -- --dump-previews` renders a
//! few representative states to PNG with no GTK/display needed, for offline
//! review of the drawing math before touching the live interactive window.
//! `cargo run --example crop_spike -p coolctl-gui` opens the real window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use cairo::{Context, Format, ImageSurface};
use gtk4::prelude::*;
use gtk4::{glib, Application, ApplicationWindow, Button, DrawingArea, EventControllerScroll, EventControllerScrollFlags, GestureDrag, Orientation as BoxOrientation};

const APP_ID: &str = "org.coolctl.CropSpike";
const TEST_IMAGE_PATH: &str = "/tmp/claude-1000/-home-prasad-Documents-lm360/e36826fc-7840-4cff-9ff0-6941752a5df7/scratchpad/crop_test_image.png";

#[derive(Clone, Copy)]
struct CropState {
    offset_x: f64,
    offset_y: f64,
    scale: f64,
    landscape: bool, // true = 320:240, false = 240:320
}

impl CropState {
    fn new() -> Self {
        CropState { offset_x: 0.0, offset_y: 0.0, scale: 1.0, landscape: true }
    }
}

/// Pure-Cairo drawing logic, no GTK dependency — callable from both the live
/// DrawingArea's draw func and the offline PNG-dump path below.
fn draw_crop_view(cr: &Context, width: i32, height: i32, state: &CropState, image: &ImageSurface) {
    let width = width as f64;
    let height = height as f64;

    cr.set_source_rgb(0.08, 0.08, 0.1);
    cr.paint().unwrap();

    let (aspect_w, aspect_h) = if state.landscape { (320.0_f64, 240.0_f64) } else { (240.0_f64, 320.0_f64) };
    let padding = 20.0;
    let avail_w = width - 2.0 * padding;
    let avail_h = height - 2.0 * padding;
    let fit_scale = (avail_w / aspect_w).min(avail_h / aspect_h);
    let vp_w = aspect_w * fit_scale;
    let vp_h = aspect_h * fit_scale;
    let vp_x = (width - vp_w) / 2.0;
    let vp_y = (height - vp_h) / 2.0;

    // Image, clipped to the viewport, transformed by pan/zoom.
    cr.save().unwrap();
    cr.rectangle(vp_x, vp_y, vp_w, vp_h);
    cr.clip();
    cr.translate(vp_x + state.offset_x, vp_y + state.offset_y);
    cr.scale(state.scale, state.scale);
    cr.set_source_surface(image, 0.0, 0.0).unwrap();
    let _ = cr.paint();
    cr.restore().unwrap();

    // Dim everything outside the viewport (4 strips around it).
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.rectangle(0.0, 0.0, width, vp_y);
    cr.fill().unwrap();
    cr.rectangle(0.0, vp_y + vp_h, width, height - (vp_y + vp_h));
    cr.fill().unwrap();
    cr.rectangle(0.0, vp_y, vp_x, vp_h);
    cr.fill().unwrap();
    cr.rectangle(vp_x + vp_w, vp_y, width - (vp_x + vp_w), vp_h);
    cr.fill().unwrap();

    // Viewport border.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.0);
    cr.rectangle(vp_x, vp_y, vp_w, vp_h);
    cr.stroke().unwrap();
}

fn load_test_image() -> ImageSurface {
    let mut file = std::fs::File::open(TEST_IMAGE_PATH)
        .unwrap_or_else(|e| panic!("failed to open test image {TEST_IMAGE_PATH}: {e}"));
    ImageSurface::create_from_png(&mut file).expect("failed to decode test image PNG")
}

fn dump_previews() {
    let image = load_test_image();
    let out_dir = "/tmp/claude-1000/-home-prasad-Documents-lm360/e36826fc-7840-4cff-9ff0-6941752a5df7/scratchpad";

    let cases: [(&str, CropState); 4] = [
        ("centered_landscape", CropState { offset_x: 0.0, offset_y: 0.0, scale: 0.5, landscape: true }),
        ("panned_zoomed_landscape", CropState { offset_x: -150.0, offset_y: -80.0, scale: 0.9, landscape: true }),
        ("centered_portrait", CropState { offset_x: 0.0, offset_y: 0.0, scale: 0.5, landscape: false }),
        ("panned_zoomed_portrait", CropState { offset_x: -300.0, offset_y: -50.0, scale: 0.7, landscape: false }),
    ];

    for (name, state) in cases {
        let (w, h) = (640, 480);
        let surface = ImageSurface::create(Format::ARgb32, w, h).expect("failed to create surface");
        let cr = Context::new(&surface).expect("failed to create context");
        draw_crop_view(&cr, w, h, &state, &image);

        let path = format!("{out_dir}/crop_spike_{name}.png");
        let mut file = std::fs::File::create(&path).unwrap_or_else(|e| panic!("failed to create {path}: {e}"));
        surface.write_to_png(&mut file).expect("failed to write PNG");
        println!("wrote {path}");
    }
}

fn run_app() {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let image = Rc::new(load_test_image());
    let state = Rc::new(RefCell::new(CropState::new()));
    let drag_start = Rc::new(Cell::new((0.0_f64, 0.0_f64)));

    let drawing_area = DrawingArea::new();
    drawing_area.set_content_width(640);
    drawing_area.set_content_height(480);
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);

    {
        let image = image.clone();
        let state = state.clone();
        drawing_area.set_draw_func(move |_area, cr, width, height| {
            draw_crop_view(cr, width, height, &state.borrow(), &image);
        });
    }

    // Pan.
    let drag = GestureDrag::new();
    {
        let state = state.clone();
        let drag_start = drag_start.clone();
        drag.connect_drag_begin(move |_gesture, _x, _y| {
            let s = state.borrow();
            drag_start.set((s.offset_x, s.offset_y));
        });
    }
    {
        let state = state.clone();
        let drag_start = drag_start.clone();
        let drawing_area_clone = drawing_area.clone();
        drag.connect_drag_update(move |_gesture, dx, dy| {
            let (start_x, start_y) = drag_start.get();
            let mut s = state.borrow_mut();
            s.offset_x = start_x + dx;
            s.offset_y = start_y + dy;
            drop(s);
            drawing_area_clone.queue_draw();
        });
    }
    drawing_area.add_controller(drag);

    // Zoom.
    let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    {
        let state = state.clone();
        let drawing_area_clone = drawing_area.clone();
        scroll.connect_scroll(move |_controller, _dx, dy| {
            let mut s = state.borrow_mut();
            let zoom_step = 0.1;
            s.scale = (s.scale - dy * zoom_step).clamp(0.1, 5.0);
            drop(s);
            drawing_area_clone.queue_draw();
            glib::Propagation::Proceed
        });
    }
    drawing_area.add_controller(scroll);

    // Aspect ratio toggle.
    let toggle_button = Button::with_label("Toggle Orientation (320:240 / 240:320)");
    {
        let state = state.clone();
        let drawing_area_clone = drawing_area.clone();
        toggle_button.connect_clicked(move |_| {
            let mut s = state.borrow_mut();
            s.landscape = !s.landscape;
            drop(s);
            drawing_area_clone.queue_draw();
        });
    }

    let vbox = gtk4::Box::new(BoxOrientation::Vertical, 8);
    vbox.append(&drawing_area);
    vbox.append(&toggle_button);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("coolctl crop spike")
        .default_width(680)
        .default_height(560)
        .child(&vbox)
        .build();

    window.present();
}

fn main() {
    if std::env::args().any(|a| a == "--dump-previews") {
        dump_previews();
        return;
    }
    run_app();
}
