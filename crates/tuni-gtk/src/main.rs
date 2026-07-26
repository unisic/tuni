//! The application: one window, and the keyboard shortcuts that reach it.
//!
//! Everything the window does lives in `window.rs`; this file is the process.

mod blur;
mod debug;
mod diff;
mod editor;
mod files;
mod find;
mod git;
mod grid;
mod hosts;
mod info;
mod keymap;
mod menu;
mod notify;
mod palette;
mod panel;
mod preferences;
mod remote;
mod sprites;
mod switcher;
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
/// is the exception, because nothing in a shell wants it — and it is not in
/// this table at all: it belongs to the tab switcher, which watches the keys
/// itself so that it can see `Ctrl` being let go.
///
/// Panes take the arrow keys under `Ctrl+Alt`, which is where a tiling window
/// manager puts them and what kero spends ⌘⌥ on. That pushes project switching
/// onto `Ctrl+Alt+Page Up/Down`: moving between panes happens many times a
/// minute and moving between projects a few times an hour, so the shorter
/// shortcut goes to the shorter reach.
const ACCELS: &[(&str, &[&str])] = &[
    ("win.new-tab", &["<Ctrl><Shift>t"]),
    // The host list, in a tab of its own. `Ctrl+Shift` had no `o` on it, and
    // "open a connection" is what the key says everywhere else.
    ("win.new-connection", &["<Ctrl><Shift>o"]),
    // Closing the last pane in a tab closes the tab, which is what one key for
    // both means in practice. Closing a *split* tab whole stays on the tab menu
    // rather than taking `Ctrl+Shift+Q`, which every desktop reads as quit.
    ("win.close-pane", &["<Ctrl><Shift>w"]),
    // `Ctrl+Tab` is not here: it belongs to the tab switcher, which has to see
    // the modifier being let go and so cannot be an accelerator at all.
    ("win.next-tab", &["<Ctrl>Page_Down"]),
    ("win.previous-tab", &["<Ctrl>Page_Up"]),
    ("win.new-project", &["<Ctrl><Shift>n"]),
    ("win.next-project", &["<Ctrl><Alt>Page_Down"]),
    ("win.previous-project", &["<Ctrl><Alt>Page_Up"]),
    ("win.toggle-sidebar", &["F9"]),
    // kero's own key for the panel on the other side.
    ("win.toggle-panel", &["<Ctrl><Shift>b"]),
    // And its key for the repository, which opens the panel on that page.
    ("win.show-git", &["<Ctrl><Shift>g"]),
    // kero's ⇧⌘I, one modifier over: what the shell is running, and on which
    // ports.
    ("win.show-info", &["<Ctrl><Shift>i"]),
    ("win.settings", &["<Ctrl>comma"]),
    // Take the mouse back from whatever has it, and hand it over again. Free
    // in a terminal: `Ctrl+M` is carriage return, so only the shifted key is
    // there to be had.
    ("win.toggle-mouse-reporting", &["<Ctrl><Shift>m"]),
    // Find in whatever the focused pane holds. `Ctrl+F` belongs to the shell —
    // it is emacs-mode forward-char, and readline would never see it again.
    ("win.find", &["<Ctrl><Shift>f"]),
    // What every desktop application on this platform steps through matches
    // with, and free of the shell entirely.
    ("win.find-next", &["F3"]),
    ("win.find-previous", &["<Shift>F3"]),
    // `Ctrl+H` is backspace to a terminal; the shifted key is the one an editor
    // here would have taken.
    ("win.find-replace", &["<Ctrl><Shift>h"]),
    // kero's ⌘K. `Ctrl+K` is kill-line in a shell, so this goes one modifier
    // over, where `clear` is not a command that has to reach the shell at all.
    ("win.clear-terminal", &["<Ctrl><Shift>k"]),
    // kero's palette is ⌘K; `Ctrl+K` is kill-line in a shell, so the palette
    // takes the key every editor on this desktop puts it on.
    ("win.palette", &["<Ctrl><Shift>p"]),
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
    quieten();

    // Not unique: a terminal launched from a shell must inherit *that* shell's
    // working directory, which a single primary instance could not see.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|app| {
        // Names the installed icon rather than carrying one: the desktop looks
        // it up in the icon theme, which is also where the notification daemon
        // and the window switcher look for it.
        gtk::Window::set_default_icon_name(APP_ID);
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

/// The two lines a terminal on this desktop prints before it has drawn
/// anything, neither of which is a fault of the program that lands in.
///
/// A desktop that dresses GTK3 in a dark theme writes
/// `gtk-application-prefer-dark-theme` into `settings.ini` for GTK4 as well,
/// and libadwaita — which decides light and dark for itself, out of
/// `AdwStyleManager` and the desktop's own color scheme, and is doing that here
/// too — warns once that the setting means nothing to it. It is answered by
/// clearing the setting for this process before libadwaita ever looks at it,
/// which is what the empty value would have been anyway. Nothing on screen
/// moves: the property is the one libadwaita ignores.
///
/// Mesa's RADV prints a conformance notice for every Vulkan context, which is
/// what GTK renders with. It is a statement about the driver's certification,
/// not about this machine or this program, and Mesa has an environment
/// variable for saying so once and not again. A value already in the
/// environment is left alone: somebody who asked for the notice can have it.
fn quieten() {
    if std::env::var_os("MESA_VK_IGNORE_CONFORMANCE_WARNING").is_none() {
        // SAFETY: nothing else has started yet — this is the first statement
        // of `main`, before GTK, its renderer, or a thread of ours exists.
        unsafe { std::env::set_var("MESA_VK_IGNORE_CONFORMANCE_WARNING", "1") };
    }

    // Before the application rather than inside its startup: libadwaita reads
    // the setting while GtkApplication starts up, and a warning is only avoided
    // by having answered it first. `gtk::init` is what opens the display the
    // settings come from, and running it twice is a no-op.
    if gtk::init().is_ok()
        && let Some(settings) = gtk::Settings::default()
    {
        settings.set_gtk_application_prefer_dark_theme(false);
    }
}

fn build_window(app: &adw::Application) {
    let settings = settings();
    window::apply_appearance(settings.appearance);
    let window = TuniWindow::new(app, settings);
    // The window the application opens with is the one the saved session
    // belongs to. Windows opened beside it neither restore it nor write it.
    window.own_session();
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
            // A window restored onto a file or a diff has no terminal in the
            // pane that is focused, and a capture of it is still a capture.
            maybe_capture(&window, window.active_terminal().as_ref());
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
/// `TUNI_CAPTURE_WIDGET=window` shoots the whole window, chrome included, and
/// `=active` the window in front, which is how a second window shows up in a
/// capture at all; anything else shoots the terminal alone, which is what the
/// rendering captures want.
///
/// The rest of the family below drives one part of the window each:
/// `TUNI_CAPTURE_ACTIONS`, `TUNI_CAPTURE_OPEN`, `TUNI_CAPTURE_DIFF`,
/// `TUNI_CAPTURE_STAGE`, `TUNI_CAPTURE_FIND`, `TUNI_CAPTURE_SEARCH`,
/// `TUNI_CAPTURE_PALETTE`, `TUNI_CAPTURE_SWITCHER`,
/// `TUNI_CAPTURE_EDIT`, `TUNI_CAPTURE_ZOOM`, `TUNI_CAPTURE_SCROLL`,
/// `TUNI_CAPTURE_RESIZE`, `TUNI_CAPTURE_HOVER`, and `TUNI_CAPTURE_SELECT`.
fn maybe_capture(window: &TuniWindow, terminal: Option<&TuniTerminal>) {
    let Ok(path) = std::env::var("TUNI_CAPTURE_PNG") else {
        return;
    };
    let delay: u64 = std::env::var("TUNI_CAPTURE_DELAY_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500);

    if let Ok(input) = std::env::var("TUNI_CAPTURE_INPUT")
        && let Some(terminal) = terminal
    {
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
    // show a window that has more than one of anything. An "editor." or a
    // "diff." action goes to the pane showing one, since those groups live on
    // the pane rather than on the window.
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
                        let (widget, name) =
                            match (name.strip_prefix("editor."), name.strip_prefix("diff.")) {
                                (Some(rest), _) => match window.active_editor() {
                                    Some(editor) => {
                                        (editor.upcast::<gtk::Widget>(), format!("editor.{rest}"))
                                    }
                                    None => return eprintln!("no file is open: {name}"),
                                },
                                (_, Some(rest)) => match window.active_diff() {
                                    Some(diff) => {
                                        (diff.upcast::<gtk::Widget>(), format!("diff.{rest}"))
                                    }
                                    None => return eprintln!("no diff is open: {name}"),
                                },
                                _ => {
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

    // What changed in a file, put in a pane the way activating a row in the
    // Git panel does. `path` shows the working tree, `path|staged` the index,
    // and `|side` on the end splits the tab it is asked from instead of
    // opening a tab of its own.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_DIFF") {
        for (step, spec) in spec.split(',').map(str::trim).enumerate() {
            let mut parts = spec.split('|').map(str::trim);
            let path = parts.next().unwrap_or_default().to_owned();
            let flags: Vec<&str> = parts.collect();
            let staged = flags.contains(&"staged");
            let side = flags.contains(&"side");
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(350 + 100 * step as u64),
                glib::clone!(
                    #[weak]
                    window,
                    move || {
                        let path = std::path::Path::new(&path);
                        if side {
                            window.open_diff_to_side(path, staged);
                        } else {
                            window.open_diff(path, staged);
                        }
                    }
                ),
            );
        }
    }

    // A scripted hunk, staged or unstaged from the diff on screen: the index
    // it names, counting from zero. Prints what the diff says afterwards, which
    // is the check that git took the patch.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_STAGE")
        && let Ok(index) = spec.trim().parse::<usize>()
    {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(900),
            glib::clone!(
                #[weak]
                window,
                move || {
                    let Some(diff) = window.active_diff() else {
                        eprintln!("TUNI_CAPTURE_STAGE: no diff is open");
                        return;
                    };
                    diff.stage_hunk(index);
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(700),
                        move || println!("hunks after staging: {}", diff.hunk_count()),
                    );
                }
            ),
        );
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

    // A scripted find in the terminal: "needle" or "needle|3" to step forward
    // three times after it. Prints the tally the bar shows, which is the check
    // that the search reached the grid and that stepping moved through it.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_SEARCH") {
        let (needle, steps) = match spec.split_once('|') {
            Some((needle, tail)) => (needle.to_owned(), tail.trim().parse::<u32>().unwrap_or(0)),
            None => (spec, 0),
        };
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(600)),
            glib::clone!(
                #[weak]
                window,
                move || {
                    let (Some(find), Some(terminal)) =
                        (window.find_bar(), window.active_terminal())
                    else {
                        eprintln!("TUNI_CAPTURE_SEARCH: no terminal is open");
                        return;
                    };
                    find.open(&terminal);
                    find.search_text(&needle);
                    // The entry waits a moment before it searches, the way it
                    // does for someone still typing.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        move || {
                            for _ in 0..steps {
                                find.step_match(true);
                            }
                            println!("find: {}", find.tally());
                        },
                    );
                }
            ),
        );
    }

    // The tab switcher, which no scripted key press can reach: it lives on a
    // held modifier, and holding a key is the one thing a script cannot do.
    // "1" is a single `Ctrl+Tab`, "3" is three of them with the modifier still
    // down, "back" walks the other way, and "commit" lets the modifier go.
    // Prints the tab the highlight is on.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_SWITCHER") {
        let mut fields = spec.split('|').map(str::trim);
        let presses: u32 = fields.next().unwrap_or_default().parse().unwrap_or(1);
        let flags: Vec<&str> = fields.collect();
        let forward = !flags.contains(&"back");
        let commit = flags.contains(&"commit");
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(300)),
            glib::clone!(
                #[weak]
                window,
                move || {
                    for _ in 0..presses.max(1) {
                        window.switcher_press(forward);
                    }
                    println!("switcher: {}", window.switcher_highlight());
                    if commit {
                        window.switcher_finish();
                    }
                }
            ),
        );
    }

    // A scripted palette: "query" opens it and types, "query|2" walks two rows
    // down, "query|2|run" runs what that lands on. Prints the row that is
    // selected, which is the check that the ranking put the right thing under
    // Return.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_PALETTE") {
        let mut fields = spec.split('|');
        let query = fields.next().unwrap_or_default().to_owned();
        let rest: Vec<&str> = fields.map(str::trim).collect();
        let steps: u32 = rest
            .iter()
            .find_map(|field| field.parse().ok())
            .unwrap_or(0);
        let run = rest.contains(&"run");
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(600)),
            glib::clone!(
                #[weak]
                window,
                move || {
                    gtk::prelude::WidgetExt::activate_action(&window, "win.palette", None).ok();
                    palette::type_query(&query);
                    // Same wait the find bar needs: the entry searches once the
                    // typing stops.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        move || {
                            palette::move_selection(steps);
                            println!("palette: {}", palette::selection().unwrap_or_default());
                            if run {
                                palette::run_selection();
                            }
                        },
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
        && let Some(terminal) = terminal
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
        && let Some(terminal) = terminal
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

    // A scripted window resize: "1100x700,700x700" walks the window through
    // each size in turn, a frame apart, which is the stream of widths a drag on
    // the window edge produces. The point of driving it from here is that a
    // drag cannot be injected on a locked-down Wayland session, and a resize is
    // where a shell's prompt redraw and the terminal's reflow have to agree.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_RESIZE") {
        // Far enough apart that GTK allocates between them and the shell has a
        // chance to answer, close enough together to stay inside one drag.
        const STEP: u64 = 250;
        for (step, size) in spec.split(',').map(str::trim).enumerate() {
            let Some((width, height)) = size
                .split_once('x')
                .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
            else {
                eprintln!("TUNI_CAPTURE_RESIZE wants WIDTHxHEIGHT, comma-separated");
                break;
            };
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(600 + STEP * step as u64),
                glib::clone!(
                    #[weak]
                    window,
                    move || window.set_default_size(width, height)
                ),
            );
        }
    }

    // A scripted Ctrl-hover, in surface pixels: "x,y". Prints the hyperlink
    // found there, and leaves it highlighted for the capture.
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_HOVER")
        && let Some(terminal) = terminal
    {
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
    if let Ok(spec) = std::env::var("TUNI_CAPTURE_SELECT")
        && let Some(terminal) = terminal
    {
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay.saturating_sub(200)),
            glib::clone!(
                #[weak]
                terminal,
                move || match parse_select(&spec) {
                    Some((x1, y1, x2, y2)) => {
                        // Press, drag, release, with nothing held down: the
                        // gates a real drag meets are the point of the test.
                        let mods = tuni_vt::Mods::empty();
                        terminal.pointer_press(gtk::gdk::BUTTON_PRIMARY, mods, x1, y1, 0);
                        terminal.pointer_motion(mods, x2, y2);
                        terminal.pointer_release(gtk::gdk::BUTTON_PRIMARY, mods, x2, y2);
                        println!("selection: {:?}", terminal.selection());
                    }
                    None => eprintln!("TUNI_CAPTURE_SELECT wants x1,y1,x2,y2"),
                }
            ),
        );
    }

    let widget = std::env::var("TUNI_CAPTURE_WIDGET").unwrap_or_default();
    let whole_window = widget == "window";
    // "active" shoots whichever window is in front rather than this one, which
    // is the only way to see a window opened by an action during the capture.
    let front = widget == "active";
    glib::timeout_add_local_once(
        std::time::Duration::from_millis(delay),
        glib::clone!(
            #[weak]
            window,
            move || {
                let target: gtk::Widget = if front {
                    window
                        .application()
                        .and_then(|app| app.active_window())
                        .map_or_else(|| window.clone().upcast(), gtk::Window::upcast)
                } else if whole_window {
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
                // Held before the close below, which takes the window off the
                // application and would leave nothing to ask for it afterwards.
                let app = window.application();
                window.force_close();
                // A dialog on top of the window keeps it alive, and the
                // capture is over either way. Every window goes, not only this
                // one: a capture that opened a second window has a second
                // window to close, and a loop with a window left runs on.
                if let Some(app) = app {
                    for open in app.windows() {
                        open.destroy();
                    }
                    app.quit();
                }
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
