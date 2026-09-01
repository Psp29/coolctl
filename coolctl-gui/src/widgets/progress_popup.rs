//! Modal progress popup shown during a long-running background operation
//! (loading a video, exporting/sending media). Plain `gtk4::Window` — this
//! project doesn't depend on libadwaita — with a status label, a progress
//! bar (determinate or pulsing indeterminate), and a Cancel button.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{glib, ApplicationWindow, Button, Label, Orientation, ProgressBar, Window};

const PULSE_INTERVAL: Duration = Duration::from_millis(150);

pub struct ProgressPopup {
    window: Window,
    status_label: Label,
    progress_bar: ProgressBar,
    cancel_button: Button,
    pulse_source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl ProgressPopup {
    /// Shows the popup immediately, in indeterminate mode. `on_cancel` runs
    /// once if the Cancel button is pressed — it does not close the popup
    /// itself, callers are expected to do that (and any other cleanup) from
    /// within the callback.
    pub fn new(parent: &ApplicationWindow, title: &str, on_cancel: impl Fn() + 'static) -> Self {
        let status_label = Label::new(Some(title));
        status_label.set_wrap(true);

        let progress_bar = ProgressBar::new();
        progress_bar.set_show_text(false);

        let cancel_button = Button::with_label("Cancel");

        let vbox = gtk4::Box::new(Orientation::Vertical, 12);
        vbox.set_margin_top(16);
        vbox.set_margin_bottom(16);
        vbox.set_margin_start(16);
        vbox.set_margin_end(16);
        vbox.append(&status_label);
        vbox.append(&progress_bar);
        vbox.append(&cancel_button);

        let window = Window::builder()
            .title(title)
            .transient_for(parent)
            .modal(true)
            .deletable(false)
            .default_width(360)
            .resizable(false)
            .child(&vbox)
            .build();

        cancel_button.connect_clicked(move |_| on_cancel());

        window.present();

        let popup = ProgressPopup {
            window,
            status_label,
            progress_bar,
            cancel_button,
            pulse_source: Rc::new(RefCell::new(None)),
        };
        popup.set_indeterminate();
        popup
    }

    pub fn set_status(&self, text: &str) {
        self.status_label.set_text(text);
    }

    /// Starts pulsing the bar on a timer. No-op if already pulsing.
    pub fn set_indeterminate(&self) {
        if self.pulse_source.borrow().is_some() {
            return;
        }
        let progress_bar = self.progress_bar.clone();
        let id = glib::timeout_add_local(PULSE_INTERVAL, move || {
            progress_bar.pulse();
            glib::ControlFlow::Continue
        });
        *self.pulse_source.borrow_mut() = Some(id);
    }

    /// Stops any pulse timer and sets a real fraction (0.0..=1.0).
    pub fn set_determinate(&self, fraction: f64) {
        if let Some(id) = self.pulse_source.borrow_mut().take() {
            id.remove();
        }
        self.progress_bar.set_fraction(fraction.clamp(0.0, 1.0));
    }

    pub fn set_cancel_enabled(&self, enabled: bool) {
        self.cancel_button.set_sensitive(enabled);
    }

    /// Stops the pulse timer (if any) and closes the window.
    pub fn close(self) {
        if let Some(id) = self.pulse_source.borrow_mut().take() {
            id.remove();
        }
        self.window.close();
    }
}
