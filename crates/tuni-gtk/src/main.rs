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
    terminal.set_config(&config());

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

    // And the subtitle tracks OSC 7, so the window says where the shell is.
    terminal.connect_notify_local(
        Some("cwd"),
        glib::clone!(
            #[weak]
            title,
            move |terminal: &TuniTerminal, _| {
                if let Some(cwd) = terminal.cwd() {
                    title.set_subtitle(&shorten(&cwd));
                }
            }
        ),
    );

    // The theme is the desktop's business, not one terminal's: the style
    // manager says light or dark, the config names a theme for each, and the
    // window paints its chrome to match whatever the terminal ended up with.
    let style = adw::StyleManager::default();
    let recolor = glib::clone!(
        #[weak]
        terminal,
        move |style: &adw::StyleManager| {
            let theme = config().theme(style.is_dark());
            terminal.set_theme(&theme);
            apply_chrome(&theme);
        }
    );
    style.connect_dark_notify(recolor.clone());
    recolor(&style);

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

/// The configuration for this run.
///
/// Etap 4 reads this from disk and a settings window edits it. Until then the
/// defaults are the whole story, except for the environment: `TUNI_THEME` names
/// one of the bundled themes for both appearances, `TUNI_FONT` is a font the
/// way Pango writes one, and `TUNI_LIGATURES` turns ligatures on. Enough to
/// look at any of the 574 themes, and at any font, without a settings UI.
fn config() -> tuni_core::TerminalConfig {
    let mut config = tuni_core::TerminalConfig::default();
    if let Ok(name) = std::env::var("TUNI_THEME") {
        config.theme_light = name.clone();
        config.theme_dark = name;
    }
    if let Ok(font) = std::env::var("TUNI_FONT") {
        config.set_font(&font);
    }
    if let Ok(value) = std::env::var("TUNI_LIGATURES") {
        config.font_ligatures = matches!(value.trim(), "1" | "true" | "yes" | "on");
    }
    config
}

/// Paint the window chrome from the terminal's theme.
///
/// libadwaita builds its whole stylesheet out of named colors, so overriding
/// those recolors the header bar, dialogs, and popovers consistently — far more
/// robust than styling widgets one by one, and it keeps working as libadwaita
/// adds widgets. One provider, reloaded, so switching themes does not stack
/// stylesheets on the display.
fn apply_chrome(theme: &tuni_core::theme::Theme) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }

    let accent = theme.accent();
    let css = format!(
        "@define-color window_bg_color {bg};\n\
         @define-color window_fg_color {fg};\n\
         @define-color view_bg_color {bg};\n\
         @define-color view_fg_color {fg};\n\
         @define-color headerbar_bg_color {header};\n\
         @define-color headerbar_fg_color {fg};\n\
         @define-color headerbar_border_color {border};\n\
         @define-color headerbar_backdrop_color {bg};\n\
         @define-color sidebar_bg_color {sidebar};\n\
         @define-color sidebar_fg_color {fg};\n\
         @define-color sidebar_border_color {border};\n\
         @define-color sidebar_backdrop_color {bg};\n\
         @define-color popover_bg_color {raised};\n\
         @define-color popover_fg_color {fg};\n\
         @define-color dialog_bg_color {raised};\n\
         @define-color dialog_fg_color {fg};\n\
         @define-color card_bg_color {raised};\n\
         @define-color card_fg_color {fg};\n\
         @define-color accent_color {accent};\n\
         @define-color accent_bg_color {accent};\n\
         @define-color accent_fg_color {on_accent};\n",
        bg = theme.background.to_hex(),
        fg = theme.foreground.to_hex(),
        header = theme.surface(0.06).to_hex(),
        sidebar = theme.surface(0.03).to_hex(),
        raised = theme.surface(0.10).to_hex(),
        border = theme.surface(0.20).to_hex(),
        accent = accent.to_hex(),
        on_accent = accent.contrasting().to_hex(),
    );

    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    PROVIDER.with(|provider| {
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// A path as a person would write it: `/home/me/src` is `~/src`.
fn shorten(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// Debug capture: render the terminal to a PNG and quit.
///
/// Driven by environment variables so it costs nothing in a normal run.
/// `TUNI_CAPTURE_PNG` is the output path, `TUNI_CAPTURE_INPUT` is text typed
/// into the shell first, and `TUNI_CAPTURE_DELAY_MS` is how long to wait for
/// that command to finish before the shot is taken.
///
/// The terminal only: `GtkWidgetPaintable` renders our own widget but draws
/// nothing for the window or its content box, so the header bar cannot be
/// captured this way.
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

    // A scripted zoom, in whole steps, driving the same path Ctrl+plus does.
    // The shell is told about the new cell size, so `stty size` in the captured
    // output is the check that the resize actually landed.
    if let Ok(steps) = std::env::var("TUNI_CAPTURE_ZOOM")
        && let Ok(steps) = steps.trim().parse::<i32>()
    {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(300),
            glib::clone!(
                #[weak]
                terminal,
                move || {
                    terminal.zoom(steps);
                    println!("font size: {}", terminal.font_size());
                }
            ),
        );
    }

    // A scripted scroll, in lines, negative for back into the scrollback.
    // Exercises the same path the wheel takes, overlay scrollbar included.
    if let Ok(lines) = std::env::var("TUNI_CAPTURE_SCROLL")
        && let Ok(lines) = lines.trim().parse::<isize>()
    {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(200)),
            glib::clone!(
                #[weak]
                terminal,
                move || terminal.scroll_lines(lines)
            ),
        );
    }

    // A scripted Ctrl-hover, in surface pixels: "x,y". Prints the hyperlink
    // found there, and leaves it highlighted for the capture.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_HOVER") {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(200)),
            glib::clone!(
                #[weak]
                terminal,
                move || match parse_point(&spec) {
                    Some((x, y)) => println!("hover: {:?}", terminal.hover_link(x, y)),
                    None => eprintln!("TUNI_CAPTURE_HOVER wants x,y"),
                }
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
                if let Err(error) = capture(terminal.upcast_ref(), &path) {
                    eprintln!("capture failed: {error}");
                }
                window.close();
            }
        ),
    );
}

fn parse_point(spec: &str) -> Option<(f64, f64)> {
    let values: Vec<f64> = spec.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    match values[..] {
        [x, y] => Some((x, y)),
        _ => None,
    }
}

fn parse_select(spec: &str) -> Option<(f64, f64, f64, f64)> {
    let values: Vec<f64> = spec.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    match values[..] {
        [x1, y1, x2, y2] => Some((x1, y1, x2, y2)),
        _ => None,
    }
}

fn capture(widget: &gtk::Widget, path: &str) -> Result<(), String> {
    use gtk::prelude::{NativeExt, PaintableExt, TextureExt};

    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .ok_or("widget is not realized")?;

    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(widget.width()),
        f64::from(widget.height()),
    );
    let node = snapshot.to_node().ok_or("nothing was drawn")?;

    renderer
        .render_texture(&node, None)
        .save_to_png(path)
        .map_err(|e| e.to_string())
}
