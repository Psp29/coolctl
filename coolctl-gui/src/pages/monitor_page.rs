use gtk4::prelude::*;
use gtk4::{Button, Label, Orientation as BoxOrientation};

use coolctl_core::{Request, Response};

use crate::ipc_client;

fn describe_status() -> String {
    match ipc_client::send_request(&Request::GetStatus) {
        Ok(Response::Status { mode, orientation }) => format!("Mode: {mode} / Orientation: {}°", orientation.degrees()),
        Ok(Response::Error { message }) => format!("Daemon error: {message}"),
        Ok(other) => format!("Unexpected response: {other:?}"),
        Err(e) => format!("Couldn't reach coolctld: {e}"),
    }
}

fn describe_result(result: Result<Response, String>) -> String {
    match result {
        Ok(Response::Ok) => "Done.".to_string(),
        Ok(Response::Error { message }) => format!("Daemon error: {message}"),
        Ok(other) => format!("Unexpected response: {other:?}"),
        Err(e) => format!("Failed: {e}"),
    }
}

pub fn build() -> gtk4::Box {
    let status_label = Label::new(Some(&describe_status()));
    status_label.set_wrap(true);

    let monitor_button = Button::with_label("Switch to Monitor Mode");
    {
        let status_label = status_label.clone();
        monitor_button.connect_clicked(move |_| {
            let result = ipc_client::send_request(&Request::Monitor);
            status_label.set_text(&describe_result(result));
        });
    }

    // --- Brightness. ---
    let brightness_label = Label::new(Some("Brightness:"));
    brightness_label.set_halign(gtk4::Align::Start);
    let brightness_row = gtk4::Box::new(BoxOrientation::Horizontal, 8);
    let brightness_up = Button::with_label("Brightness +");
    {
        let status_label = status_label.clone();
        brightness_up.connect_clicked(move |_| {
            let result = ipc_client::send_request(&Request::BrightnessUp);
            status_label.set_text(&describe_result(result));
        });
    }
    let brightness_down = Button::with_label("Brightness -");
    {
        let status_label = status_label.clone();
        brightness_down.connect_clicked(move |_| {
            let result = ipc_client::send_request(&Request::BrightnessDown);
            status_label.set_text(&describe_result(result));
        });
    }
    brightness_row.append(&brightness_up);
    brightness_row.append(&brightness_down);

    let refresh_button = Button::with_label("Refresh Status");
    {
        let status_label = status_label.clone();
        refresh_button.connect_clicked(move |_| {
            status_label.set_text(&describe_status());
        });
    }

    let vbox = gtk4::Box::new(BoxOrientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.append(&monitor_button);
    vbox.append(&brightness_label);
    vbox.append(&brightness_row);
    vbox.append(&refresh_button);
    vbox.append(&status_label);

    vbox
}
