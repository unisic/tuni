//! Etap 0 shell: one window, one terminal, nothing else.
//!
//! Projects, tabs, and the pane layout arrive in Etapy 2–3. Keeping this file
//! deliberately thin means the widget carries the whole feasibility question,
//! which is what the spike is meant to answer.

mod keymap;
mod terminal;

use adw::prelude::*;
use gtk::glib;

use terminal::TuniTerminal;

const APP_ID: &str = "dev.unisic.Tuni";

fn main() -> glib::ExitCode {
    // Not unique: a terminal launched from a shell must inherit *that* shell's
    // working directory, which a single primary instance could not see.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(build_window);
    app.run()
}

fn build_window(app: &adw::Application) {
    let terminal = TuniTerminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("tuni", "");
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

    // The window title tracks OSC 0/2 through the widget's `title` property.
    terminal.connect_notify_local(
        Some("title"),
        glib::clone!(
            #[weak]
            title,
            move |terminal: &TuniTerminal, _| {
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
                maybe_capture(&window, &terminal);
            }
        }
    ));
}

/// Debug capture: render the terminal to a PNG and quit.
///
/// Driven by environment variables so it costs nothing in a normal run.
/// `TUNI_CAPTURE_PNG` is the output path, `TUNI_CAPTURE_INPUT` is text typed
/// into the shell first, and `TUNI_CAPTURE_DELAY_MS` is how long to wait for
/// that command to finish before the shot is taken.
fn maybe_capture(window: &adw::ApplicationWindow, terminal: &TuniTerminal) {
    let Ok(path) = std::env::var("TUNI_CAPTURE_PNG") else {
        return;
    };
    let delay: u64 = std::env::var("TUNI_CAPTURE_DELAY_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500);

    if let Ok(input) = std::env::var("TUNI_CAPTURE_INPUT") {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(400),
            glib::clone!(
                #[weak]
                terminal,
                move || terminal.send_text(&input)
            ),
        );
    }

    // A scripted selection, in surface pixels: "x1,y1,x2,y2". Runs the same
    // widget path a real drag takes and prints what came out, because no
    // pointer injection tool exists on a locked-down Wayland session.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_SELECT") {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(200)),
            glib::clone!(
                #[weak]
                terminal,
                move || match parse_select(&spec) {
                    Some((x1, y1, x2, y2)) => {
                        terminal.selection_press(x1, y1, std::time::Duration::ZERO);
                        terminal.selection_drag(x2, y2, false);
                        println!("selection: {:?}", terminal.selection_finish());
                    }
                    None => eprintln!("TUNI_CAPTURE_SELECT wants x1,y1,x2,y2"),
                }
            ),
        );
    }

    glib::timeout_add_local_once(
        std::time::Duration::from_millis(delay),
        glib::clone!(
            #[weak]
            terminal,
            #[weak]
            window,
            move || {
                if let Err(error) = capture(&terminal, &path) {
                    eprintln!("capture failed: {error}");
                }
                window.close();
            }
        ),
    );
}

fn parse_select(spec: &str) -> Option<(f64, f64, f64, f64)> {
    let values: Vec<f64> = spec.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    match values[..] {
        [x1, y1, x2, y2] => Some((x1, y1, x2, y2)),
        _ => None,
    }
}

fn capture(terminal: &TuniTerminal, path: &str) -> Result<(), String> {
    use gtk::prelude::{NativeExt, PaintableExt, TextureExt};

    let renderer = terminal
        .native()
        .and_then(|native| native.renderer())
        .ok_or("widget is not realized")?;

    let paintable = gtk::WidgetPaintable::new(Some(terminal));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(terminal.width()),
        f64::from(terminal.height()),
    );
    let node = snapshot.to_node().ok_or("nothing was drawn")?;

    renderer
        .render_texture(&node, None)
        .save_to_png(path)
        .map_err(|e| e.to_string())
}
