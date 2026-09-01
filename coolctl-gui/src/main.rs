use std::cell::Cell;
use std::rc::Rc;

use coolctl_core::{Orientation, Request, Response};
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Button, Label, Orientation as BoxOrientation, Stack, StackSwitcher};

use coolctl_gui::ipc_client;
use coolctl_gui::pages::{media_page, monitor_page};

const APP_ID: &str = "org.coolctl.Gui";

/// Falls back to landscape with a warning if coolctld isn't reachable at
/// launch, rather than crashing — useful for developing the GUI without the
/// daemon running.
fn fetch_orientation() -> (Orientation, Option<String>) {
    match ipc_client::send_request(&Request::GetStatus) {
        Ok(Response::Status { orientation, .. }) => (orientation, None),
        Ok(other) => (Orientation::Deg0, Some(format!("unexpected daemon response: {other:?}"))),
        Err(e) => (Orientation::Deg0, Some(format!("couldn't reach coolctld ({e}), defaulting to landscape"))),
    }
}

fn main() {
    gstreamer::init().expect("failed to initialize GStreamer");

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("coolctl")
        .default_width(720)
        .default_height(700)
        .build();

    let (initial_orientation, warning) = fetch_orientation();
    let orientation = Rc::new(Cell::new(initial_orientation));

    let stack = Stack::new();
    stack.set_vexpand(true);
    let (media_widget, on_orientation_changed) = media_page::build(&window, orientation.clone());
    let monitor_widget = monitor_page::build();
    stack.add_titled(&media_widget, Some("media"), "Media Editor");
    stack.add_titled(&monitor_widget, Some("monitor"), "Monitor");

    let switcher = StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_halign(gtk4::Align::Center);

    // --- Orientation selector: common to both tabs, so it lives above the
    // stack rather than inside either page. Sends SetOrientation to the
    // daemon (takes effect immediately for Monitor Mode) and notifies the
    // Media Editor page so its crop viewport re-fits to the new aspect ratio.
    let orientation_bar = gtk4::Box::new(BoxOrientation::Horizontal, 8);
    orientation_bar.set_margin_top(8);
    orientation_bar.set_margin_start(8);
    orientation_bar.set_margin_end(8);
    orientation_bar.append(&Label::new(Some("Orientation:")));

    let orientation_status = Label::new(Some(&warning.unwrap_or_else(|| format!("{}°", initial_orientation.degrees()))));
    orientation_status.set_hexpand(true);
    orientation_status.set_halign(gtk4::Align::End);

    for o in [Orientation::Deg0, Orientation::Deg90, Orientation::Deg180, Orientation::Deg270] {
        let button = Button::with_label(&format!("{}°", o.degrees()));
        let orientation = orientation.clone();
        let orientation_status = orientation_status.clone();
        let on_orientation_changed = on_orientation_changed.clone();
        button.connect_clicked(move |_| match ipc_client::send_request(&Request::SetOrientation { orientation: o }) {
            Ok(Response::Ok) => {
                orientation.set(o);
                orientation_status.set_text(&format!("{}°", o.degrees()));
                on_orientation_changed();
            }
            Ok(Response::Error { message }) => orientation_status.set_text(&format!("Daemon error: {message}")),
            Ok(other) => orientation_status.set_text(&format!("Unexpected response: {other:?}")),
            Err(e) => orientation_status.set_text(&format!("Failed: {e}")),
        });
        orientation_bar.append(&button);
    }
    orientation_bar.append(&orientation_status);

    let vbox = gtk4::Box::new(BoxOrientation::Vertical, 0);
    vbox.append(&orientation_bar);
    vbox.append(&switcher);
    vbox.append(&stack);

    window.set_child(Some(&vbox));
    window.present();
}
