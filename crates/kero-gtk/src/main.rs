//! Etap 0 shell: one window, one terminal, nothing else.
//!
//! Projects, tabs, and the pane layout arrive in Etapy 2–3. Keeping this file
//! deliberately thin means the widget carries the whole feasibility question,
//! which is what the spike is meant to answer.

mod keymap;
mod terminal;

use adw::prelude::*;
use gtk::glib;

use terminal::KeroTerminal;

const APP_ID: &str = "dev.unisic.Kero";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_command_line(|app, _| {
        app.activate();
        0
    });
    app.connect_activate(build_window);
    app.run()
}

fn build_window(app: &adw::Application) {
    let terminal = KeroTerminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("kero", "");
    header.set_title_widget(Some(&title));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&terminal);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build();

    // The title tracks OSC 0/2. `notify` on a non-property name is a plain
    // signal emission here; Etap 1 turns this into a real GObject property.
    terminal.connect_notify_local(
        None,
        glib::clone!(
            #[weak]
            title,
            move |terminal: &KeroTerminal, _| {
                if let Some(text) = terminal.title() {
                    title.set_title(&text);
                }
            }
        ),
    );

    window.present();

    // Spawn after presenting, so the first allocation has already sized the
    // grid and the shell never sees a bogus 80x24 winsize.
    glib::idle_add_local_once(glib::clone!(
        #[weak]
        terminal,
        #[weak]
        window,
        move || {
            if let Err(error) = terminal.start(std::env::current_dir().ok()) {
                let dialog = adw::AlertDialog::new(Some("Cannot start shell"), Some(&error));
                dialog.add_response("close", "Close");
                dialog.present(Some(&window));
            } else {
                let _ = terminal.grab_focus();
            }
        }
    ));
}
