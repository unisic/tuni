//! The application: one window, and the keyboard shortcuts that reach it.
//!
//! Everything the window does lives in `window.rs`; this file is the process.

mod editor;
mod files;
mod git;
mod grid;
mod keymap;
mod panel;
mod preferences;
mod terminal;
mod tiles;
mod window;

use adw::prelude::*;
use gtk::glib;

use terminal::TuniTerminal;
use window::TuniWindow;

const APP_ID: &str = "dev.unisic.Tuni";

/// What the keyboard does, as data rather than as scattered constants, so a
/// configurable keymap has one table to read instead of a scattered set.
///
/// A terminal cannot spend plain `Ctrl` on the application: `Ctrl+C` and
/// `Ctrl+D` belong to the shell. So the application's own actions sit on
/// `Ctrl+Shift`, which is the convention every Linux terminal follows, and tab
/// selection on `Alt`+digit, which is what a tabbed terminal does. `Ctrl+Tab`
/// is the exception, because nothing in a shell wants it.
///
/// Panes take the arrow keys under `Ctrl+Alt`, which is where a tiling window
/// manager puts them and what kero spends ⌘⌥ on. That pushes project switching
/// onto `Ctrl+Alt+Page Up/Down`: moving between panes happens many times a
/// minute and moving between projects a few times an hour, so the shorter
/// shortcut goes to the shorter reach.
const ACCELS: &[(&str, &[&str])] = &[
    ("win.new-tab", &["<Ctrl><Shift>t"]),
    // Closing the last pane in a tab closes the tab, which is what one key for
    // both means in practice. Closing a *split* tab whole stays on the tab menu
    // rather than taking `Ctrl+Shift+Q`, which every desktop reads as quit.
    ("win.close-pane", &["<Ctrl><Shift>w"]),
    ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
    ("win.previous-tab", &["<Ctrl><Shift>Tab", "<Ctrl>Page_Up"]),
    ("win.new-project", &["<Ctrl><Shift>n"]),
    ("win.next-project", &["<Ctrl><Alt>Page_Down"]),
    ("win.previous-project", &["<Ctrl><Alt>Page_Up"]),
    ("win.toggle-sidebar", &["F9"]),
    // kero's own key for the panel on the other side.
    ("win.toggle-panel", &["<Ctrl><Shift>b"]),
    // And its key for the repository, which opens the panel on that page.
    ("win.show-git", &["<Ctrl><Shift>g"]),
    ("win.settings", &["<Ctrl>comma"]),
    ("win.split-right", &["<Ctrl><Shift>d"]),
    ("win.split-down", &["<Ctrl><Shift>e"]),
    ("win.focus-pane-left", &["<Ctrl><Alt>Left"]),
    ("win.focus-pane-right", &["<Ctrl><Alt>Right"]),
    ("win.focus-pane-up", &["<Ctrl><Alt>Up"]),
    ("win.focus-pane-down", &["<Ctrl><Alt>Down"]),
    ("win.next-pane", &["<Ctrl><Shift>bracketright"]),
    ("win.previous-pane", &["<Ctrl><Shift>bracketleft"]),
    ("win.zoom-pane", &["<Ctrl><Shift>Return"]),
    ("win.equalize-panes", &["<Ctrl><Alt>equal"]),
    ("win.resize-pane-left", &["<Ctrl><Alt><Shift>Left"]),
    ("win.resize-pane-right", &["<Ctrl><Alt><Shift>Right"]),
    ("win.resize-pane-up", &["<Ctrl><Alt><Shift>Up"]),
    ("win.resize-pane-down", &["<Ctrl><Alt><Shift>Down"]),
];

fn main() -> glib::ExitCode {
    // Not unique: a terminal launched from a shell must inherit *that* shell's
    // working directory, which a single primary instance could not see.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|app| {
        for (action, accels) in ACCELS {
            app.set_accels_for_action(action, accels);
        }
        for number in 1..=9 {
            app.set_accels_for_action(
                &format!("win.select-tab({number})"),
                &[&format!("<Alt>{number}")],
            );
            app.set_accels_for_action(
                &format!("win.select-project({number})"),
                &[&format!("<Ctrl><Shift>{number}")],
            );
        }
    });
    app.connect_activate(build_window);
    app.run()
}

fn build_window(app: &adw::Application) {
    let settings = settings();
    window::apply_appearance(settings.appearance);
    let window = TuniWindow::new(app, settings);
    window.present();

    // The first project's shell learns its size from the first allocation, so
    // it opens after the window is on screen rather than before.
    glib::idle_add_local_once(glib::clone!(
        #[weak]
        window,
        move || {
            // A saved session opens as it was left; anything else opens as a
            // first run does.
            if !window::session_enabled() || !window.restore_session() {
                window.open_project();
            }
            if let Some(terminal) = window.active_terminal() {
                maybe_capture(&window, &terminal);
            }
        }
    ));
}

/// The settings for this run: the file, then the environment on top of it.
///
/// `TUNI_THEME` names one of the bundled themes for both appearances,
/// `TUNI_FONT` is a font the way Pango writes one, and `TUNI_LIGATURES` turns
/// ligatures on. They override the file without writing to it, which is what
/// makes them useful for looking at a theme rather than choosing one.
fn settings() -> tuni_core::settings::Settings {
    let mut settings = tuni_core::settings::Settings::load();
    if let Ok(name) = std::env::var("TUNI_THEME") {
        settings.terminal.theme_light = name.clone();
        settings.terminal.theme_dark = name;
    }
    if let Ok(font) = std::env::var("TUNI_FONT") {
        settings.terminal.set_font(&font);
    }
    if let Ok(value) = std::env::var("TUNI_LIGATURES") {
        settings.terminal.font_ligatures = matches!(value.trim(), "1" | "true" | "yes" | "on");
    }
    settings
}

/// Debug capture: render the window to a PNG and quit.
///
/// Driven by environment variables so it costs nothing in a normal run.
/// `TUNI_CAPTURE_PNG` is the output path, `TUNI_CAPTURE_INPUT` is text typed
/// into the shell first, and `TUNI_CAPTURE_DELAY_MS` is how long to wait for
/// that command to finish before the shot is taken.
///
/// `TUNI_CAPTURE_WIDGET=window` shoots the whole window, chrome included;
/// anything else shoots the terminal alone, which is what the rendering
/// captures want.
///
/// The rest of the family below drives one part of the window each:
/// `TUNI_CAPTURE_ACTIONS`, `TUNI_CAPTURE_OPEN`, `TUNI_CAPTURE_FIND`,
/// `TUNI_CAPTURE_EDIT`, `TUNI_CAPTURE_ZOOM`, `TUNI_CAPTURE_SCROLL`,
/// `TUNI_CAPTURE_HOVER`, and `TUNI_CAPTURE_SELECT`.
fn maybe_capture(window: &TuniWindow, terminal: &TuniTerminal) {
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

    // Actions typed the way the keyboard would reach them: "win.new-tab" or
    // "win.new-project", one per comma, each a step later, so a capture can
    // show a window that has more than one of anything. An "editor." action
    // goes to the file that is open, since that group lives on the pane rather
    // than on the window.
    if let Ok(actions) = std::env::var("TUNI_CAPTURE_ACTIONS") {
        for (step, action) in actions.split(',').map(str::trim).enumerate() {
            let action = action.to_owned();
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(500 + 250 * step as u64),
                glib::clone!(
                    #[weak]
                    window,
                    move || {
                        let (name, target) = match action.split_once('(') {
                            Some((name, rest)) => (
                                name.to_owned(),
                                rest.trim_end_matches(')').parse::<i32>().ok(),
                            ),
                            None => (action.clone(), None),
                        };
                        let target = target.map(|value| value.to_variant());
                        let (widget, name) = match name.strip_prefix("editor.") {
                            Some(rest) => match window.active_editor() {
                                Some(editor) => {
                                    (editor.upcast::<gtk::Widget>(), format!("editor.{rest}"))
                                }
                                None => return eprintln!("no file is open: {name}"),
                            },
                            None => {
                                let name = name.strip_prefix("win.").unwrap_or(&name);
                                (window.clone().upcast(), format!("win.{name}"))
                            }
                        };
                        gtk::prelude::WidgetExt::activate_action(&widget, &name, target.as_ref())
                            .unwrap_or_else(|_| eprintln!("no such action: {name}"));
                    }
                ),
            );
        }
    }

    // A file put in a pane, the way activating it in the Files panel does.
    // `path` alone opens it in a tab of its own; `path|side` splits the tab it
    // is asked from.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_OPEN") {
        for (step, spec) in spec.split(',').map(str::trim).enumerate() {
            let (path, side) = match spec.split_once('|') {
                Some((path, edge)) => (path.to_owned(), edge.trim() == "side"),
                None => (spec.to_owned(), false),
            };
            glib::timeout_add_local_once(
                // Before the scripted actions, so an "editor." action has a
                // file to reach.
                std::time::Duration::from_millis(350 + 100 * step as u64),
                glib::clone!(
                    #[weak]
                    window,
                    move || {
                        let path = std::path::Path::new(&path);
                        if side {
                            window.open_file_to_side(path);
                        } else {
                            window.open_file(path);
                        }
                    }
                ),
            );
        }
    }

    // A scripted find: the bar opens with the text in it and the count is
    // printed, which is the check that the search reached the file.
    if let Ok(query) = std::env::var("TUNI_CAPTURE_FIND") {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(900),
            glib::clone!(
                #[weak]
                window,
                move || {
                    let Some(editor) = window.active_editor() else {
                        eprintln!("TUNI_CAPTURE_FIND: no file is open");
                        return;
                    };
                    editor.find_text(&query);
                    // The entry waits a moment before it searches, the way it
                    // does for someone still typing.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(500),
                        move || println!("matches: {}", editor.match_text()),
                    );
                }
            ),
        );
    }

    // Text typed into the file that is open, and optionally the save after it:
    // "text" or "text|save". Runs the same buffer the keyboard writes into, so
    // the shot shows the unsaved mark and the disk shows what the save wrote.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_EDIT") {
        let (text, save) = match spec.split_once('|') {
            Some((text, tail)) => (text.to_owned(), tail.trim() == "save"),
            None => (spec, false),
        };
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(900),
            glib::clone!(
                #[weak]
                window,
                move || {
                    let Some(editor) = window.active_editor() else {
                        eprintln!("TUNI_CAPTURE_EDIT: no file is open");
                        return;
                    };
                    editor.insert(&text);
                    if save {
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(300),
                            move || {
                                editor.save();
                                println!("dirty after save: {}", editor.is_dirty());
                            },
                        );
                    }
                }
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

    let whole_window = std::env::var("TUNI_CAPTURE_WIDGET").is_ok_and(|value| value == "window");
    glib::timeout_add_local_once(
        std::time::Duration::from_millis(delay),
        glib::clone!(
            #[weak]
            window,
            move || {
                let target: gtk::Widget = if whole_window {
                    window.clone().upcast()
                } else {
                    match window.active_terminal() {
                        Some(terminal) => terminal.upcast(),
                        None => window.clone().upcast(),
                    }
                };
                if let Err(error) = capture(&target, &path) {
                    eprintln!("capture failed: {error}");
                }
                window.force_close();
            }
        ),
    );
}

fn parse_point(spec: &str) -> Option<(f64, f64)> {
    let values: Vec<f64> = spec
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    match values[..] {
        [x, y] => Some((x, y)),
        _ => None,
    }
}

fn parse_select(spec: &str) -> Option<(f64, f64, f64, f64)> {
    let values: Vec<f64> = spec
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
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
