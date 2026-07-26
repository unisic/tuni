//! What the desktop actually says about cursor blinking.
//!
//! The terminal follows GtkSettings rather than a default of its own, so the
//! answer to "should the blink ever stop on its own" is a property of the
//! machine the audit runs on, not of the code. Prints the three values the
//! terminal reads.
//!
//! Run: `cargo run -p tuni-gtk --example blink_settings`

use gtk::prelude::*;

fn main() {
    let app = gtk::Application::builder()
        .application_id("dev.unisic.Tuni.BlinkSettings")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(|app| {
        let window = gtk::ApplicationWindow::new(app);
        window.present();
        let display = gtk::prelude::WidgetExt::display(&window);
        let settings = gtk::Settings::for_display(&display);
        println!("gtk-cursor-blink         = {}", settings.is_gtk_cursor_blink());
        println!(
            "gtk-cursor-blink-time    = {} ms",
            settings.gtk_cursor_blink_time()
        );
        let timeout = settings.gtk_cursor_blink_timeout();
        println!(
            "gtk-cursor-blink-timeout = {timeout} s{}",
            if timeout >= i32::MAX / 2 {
                "  (never stops)"
            } else {
                ""
            }
        );
        app.quit();
    });

    app.run_with_args::<&str>(&[]);
}
