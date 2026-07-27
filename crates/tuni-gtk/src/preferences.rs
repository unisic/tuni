//! The settings window.
//!
//! Every row writes its change straight through: there is no OK button, no
//! Apply, and no way to leave the dialog holding a decision that has not been
//! made yet. The terminals behind it repaint as the rows are touched, which is
//! how a font or a theme is actually chosen — by looking at it.
//!
//! Each handler reads the settings back out of the window rather than sharing a
//! copy with the other rows, so two rows changed in quick succession cannot
//! overwrite each other with a stale snapshot.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use tuni_core::settings::{Appearance, NewTab, Settings};
use tuni_core::theme;
use tuni_core::{CursorStyle, FONT_SIZE_MAX, FONT_SIZE_MIN, OPACITY_MIN, PADDING_MAX};

/// Appearances, in the order the dropdown lists them.
const APPEARANCES: [(Appearance, &str); 3] = [
    (Appearance::System, "Follow the Desktop"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
];

/// What a new tab can open, in the order the dropdown lists them.
const NEW_TABS: [(NewTab, &str); 2] =
    [(NewTab::Shell, "A Shell"), (NewTab::Hosts, "The Host List")];

/// Cursor shapes, in the order the dropdown lists them.
const CURSOR_STYLES: [(CursorStyle, &str); 4] = [
    (CursorStyle::Block, "Block"),
    (CursorStyle::Bar, "Bar"),
    (CursorStyle::Underline, "Underline"),
    (CursorStyle::BlockHollow, "Hollow Block"),
];

/// What repaints the preview when a setting is written.
type Paint = Rc<dyn Fn(&Settings)>;

thread_local! {
    /// The preview in the dialog that is open, if one is. Every row's change
    /// goes through [`edit`], so that is where the preview is told; one slot,
    /// because the settings are one dialog.
    static PREVIEW: RefCell<Option<Paint>> = const { RefCell::new(None) };
}

pub fn present(window: &crate::window::TuniWindow, settings: &Settings) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.add(&appearance_page(window, settings));
    dialog.add(&terminal_page(window, settings));
    dialog.add(&shortcuts_page(window, settings));
    dialog.connect_closed(|_| PREVIEW.with_borrow_mut(|slot| *slot = None));
    dialog.present(Some(window));
}

fn appearance_page(
    window: &crate::window::TuniWindow,
    settings: &Settings,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name("applications-graphics-symbolic")
        .build();

    let window_group = adw::PreferencesGroup::builder().title("Window").build();
    let appearance = adw::ComboRow::builder()
        .title("Appearance")
        .subtitle("Which of the two terminal themes is in use")
        .model(&list(APPEARANCES.iter().map(|(_, label)| *label)))
        .selected(
            APPEARANCES
                .iter()
                .position(|(value, _)| *value == settings.appearance)
                .unwrap_or(0) as u32,
        )
        .build();
    appearance.connect_selected_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let Some((chosen, _)) = APPEARANCES.get(row.selected() as usize) else {
                return;
            };
            edit(&window, |settings| settings.appearance = *chosen);
        }
    ));
    window_group.add(&appearance);

    let tab_bar = adw::SwitchRow::builder()
        .title("Hide the Tab Bar for a Single Tab")
        .subtitle("The bar comes back with the second tab, and takes a row of terminal with it")
        .active(settings.auto_hide_tab_bar)
        .build();
    tab_bar.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.auto_hide_tab_bar = on);
        }
    ));
    window_group.add(&tab_bar);

    let panel_group = adw::PreferencesGroup::builder()
        .title("Panel Pages")
        .description(
            "A page turned off leaves the switcher; its shortcut stays for whoever kept it",
        )
        .build();
    for (title, subtitle, active, write) in [
        (
            "Files",
            "The tree beside the terminals",
            settings.panel_files,
            (|settings: &mut Settings, on| settings.panel_files = on) as fn(&mut Settings, bool),
        ),
        (
            "Git",
            "The repository that directory belongs to",
            settings.panel_git,
            |settings, on| settings.panel_git = on,
        ),
        (
            "Info",
            "The shell, its processes and its ports",
            settings.panel_info,
            |settings, on| settings.panel_info = on,
        ),
        (
            "Debug",
            "Breakpoints, the stack and the locals",
            settings.panel_debug,
            |settings, on| settings.panel_debug = on,
        ),
    ] {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(active)
            .build();
        row.connect_active_notify(glib::clone!(
            #[weak]
            window,
            move |row| {
                let on = row.is_active();
                edit(&window, |settings| write(settings, on));
            }
        ));
        panel_group.add(&row);
    }

    let padding_x = adw::SpinRow::builder()
        .title("Side padding")
        .subtitle("Pixels of nothing between the window and the grid")
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.padding_x,
            0.0,
            PADDING_MAX,
            1.0,
            4.0,
            0.0,
        ))
        .build();
    padding_x.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let pixels = row.value();
            edit(&window, |settings| settings.terminal.padding_x = pixels);
        }
    ));
    window_group.add(&padding_x);

    let padding_y = adw::SpinRow::builder()
        .title("Top and bottom padding")
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.padding_y,
            0.0,
            PADDING_MAX,
            1.0,
            4.0,
            0.0,
        ))
        .build();
    padding_y.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let pixels = row.value();
            edit(&window, |settings| settings.terminal.padding_y = pixels);
        }
    ));
    window_group.add(&padding_y);
    page.add(&window_group);
    page.add(&panel_group);

    let background_group = adw::PreferencesGroup::builder()
        .title("Background")
        .description("What the desktop does with the window is the compositor's call: one that does not composite leaves a transparent window black")
        .build();

    let opacity = adw::SpinRow::builder()
        .title("Opacity")
        .subtitle("Percent. The page color only, so colored text keeps its own background")
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.background_opacity * 100.0,
            OPACITY_MIN * 100.0,
            100.0,
            5.0,
            10.0,
            0.0,
        ))
        .build();
    opacity.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let percent = row.value() / 100.0;
            edit(&window, |settings| {
                settings.terminal.background_opacity = percent;
            });
        }
    ));
    background_group.add(&opacity);

    let blur = adw::SwitchRow::builder()
        .title("Blur")
        .subtitle("Blurs what shows through, on a desktop that offers it. KDE does")
        .active(settings.background_blur)
        .build();
    blur.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.background_blur = on);
        }
    ));
    background_group.add(&blur);
    page.add(&background_group);

    // Two themes rather than one: the desktop decides between light and dark
    // while the application is running, and a terminal that keeps one palette
    // through that ends up unreadable in the other.
    let theme_group = adw::PreferencesGroup::builder()
        .title("Terminal Theme")
        .description("One for each appearance, as Ghostty and kero both do")
        .build();
    theme_group.add(&theme_row(
        window,
        "Light",
        &settings.terminal.theme_light,
        |settings, name| settings.terminal.theme_light = name,
    ));
    theme_group.add(&theme_row(
        window,
        "Dark",
        &settings.terminal.theme_dark,
        |settings, name| settings.terminal.theme_dark = name,
    ));
    page.add(&theme_group);

    page
}

/// One of the two theme pickers. Searchable, because the catalog runs to
/// hundreds of names and scrolling to "Rosé Pine" is not a way to choose.
fn theme_row(
    window: &crate::window::TuniWindow,
    title: &str,
    current: &str,
    set: fn(&mut Settings, String),
) -> adw::ComboRow {
    let names: Vec<&'static str> = theme::names().collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&list(names.iter().copied()))
        .selected(names.iter().position(|name| *name == current).unwrap_or(0) as u32)
        .enable_search(true)
        .build();
    // Search needs to be told what part of the model to match against.
    row.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    row.connect_selected_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let Some(name) = names.get(row.selected() as usize).copied() else {
                return;
            };
            edit(&window, |settings| set(settings, name.to_owned()));
        }
    ));
    row
}

fn terminal_page(window: &crate::window::TuniWindow, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Terminal")
        .icon_name("utilities-terminal-symbolic")
        .build();

    // --- font

    let font_group = adw::PreferencesGroup::builder().title("Font").build();
    if let Some(warning) = missing_symbols(&font_group) {
        font_group.set_description(Some(&warning));
    }

    // The families this machine actually has, rather than a name typed into a
    // box. Monospaced first, because that is what a terminal is drawn on, and
    // then everything else, because a font that fontconfig has not labelled is
    // still a font someone installed on purpose.
    let mut families = font_families(&font_group);
    let current = settings.terminal.font_family.trim().to_owned();
    // Whatever is configured stays selectable even when this machine has no
    // such face, so opening the settings on a machine missing the font does not
    // quietly change what is configured to the first name in the list.
    if !current.is_empty()
        && !families
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&current))
    {
        families.insert(0, current.clone());
    }
    let family = adw::ComboRow::builder()
        .title("Family")
        .subtitle(font_subtitle(&font_group, &current))
        .model(&list(families.iter().map(String::as_str)))
        .selected(
            families
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&current))
                .unwrap_or(0) as u32,
        )
        .enable_search(true)
        .build();
    // Search needs to be told what part of the model to match against.
    family.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    family.connect_selected_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let Some(name) = families.get(row.selected() as usize).cloned() else {
                return;
            };
            row.set_subtitle(&font_subtitle(row, &name));
            edit(&window, |settings| settings.terminal.font_family = name);
        }
    ));
    font_group.add(&family);

    let size = adw::SpinRow::builder()
        .title("Size")
        .subtitle("Points, the same scale Ctrl+= and Ctrl+- move")
        .digits(1)
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.font_size,
            FONT_SIZE_MIN,
            FONT_SIZE_MAX,
            0.5,
            1.0,
            0.0,
        ))
        .build();
    size.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let points = row.value();
            edit(&window, |settings| settings.terminal.font_size = points);
        }
    ));
    font_group.add(&size);

    let ligatures = adw::SwitchRow::builder()
        .title("Ligatures")
        .subtitle("Off by default: one glyph over several cells is still several cells")
        .active(settings.terminal.font_ligatures)
        .build();
    ligatures.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.terminal.font_ligatures = on);
        }
    ));
    font_group.add(&ligatures);

    let line_height = adw::SpinRow::builder()
        .title("Line Spacing")
        .subtitle("Extra pixels between rows")
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.line_height_extra,
            0.0,
            20.0,
            1.0,
            1.0,
            0.0,
        ))
        .build();
    line_height.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let extra = row.value();
            edit(&window, |settings| {
                settings.terminal.line_height_extra = extra;
            });
        }
    ));
    font_group.add(&line_height);
    page.add(&font_group);
    page.add(&preview_group(settings));

    // --- behavior

    let behavior = adw::PreferencesGroup::builder().title("Behavior").build();

    let blink = adw::SwitchRow::builder()
        .title("Blinking Cursor")
        .subtitle("Only when the application running in the terminal has no opinion")
        .active(settings.terminal.cursor_blink)
        .build();
    blink.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.terminal.cursor_blink = on);
        }
    ));
    behavior.add(&blink);

    let cursor = adw::ComboRow::builder()
        .title("Cursor")
        .subtitle("The shape it takes until a program asks for another")
        .model(&list(CURSOR_STYLES.iter().map(|(_, label)| *label)))
        .selected(
            CURSOR_STYLES
                .iter()
                .position(|(style, _)| *style == settings.terminal.cursor_style)
                .unwrap_or(0) as u32,
        )
        .build();
    cursor.connect_selected_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let Some((chosen, _)) = CURSOR_STYLES.get(row.selected() as usize) else {
                return;
            };
            edit(&window, |settings| settings.terminal.cursor_style = *chosen);
        }
    ));
    behavior.add(&cursor);

    let copy_on_select = adw::SwitchRow::builder()
        .title("Copy on Select")
        .subtitle("Highlighting also takes the clipboard, not only the middle-click selection")
        .active(settings.terminal.copy_on_select)
        .build();
    copy_on_select.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.terminal.copy_on_select = on);
        }
    ));
    behavior.add(&copy_on_select);

    let mouse_reporting = adw::SwitchRow::builder()
        .title("Mouse Reporting")
        .subtitle("Programs that ask for the mouse get it. Off keeps the drag for selecting")
        .active(settings.terminal.mouse_reporting)
        .build();
    mouse_reporting.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.terminal.mouse_reporting = on);
        }
    ));
    behavior.add(&mouse_reporting);

    let bell = adw::SwitchRow::builder()
        .title("Bell")
        .subtitle("A program ringing the bell makes a sound and marks its tab")
        .active(settings.terminal.bell)
        .build();
    bell.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.terminal.bell = on);
        }
    ));
    behavior.add(&bell);

    let scrollback = adw::SpinRow::builder()
        .title("Scrollback")
        .subtitle("Lines kept above the screen")
        .adjustment(&gtk::Adjustment::new(
            settings.terminal.scrollback_lines as f64,
            0.0,
            1_000_000.0,
            1000.0,
            1000.0,
            0.0,
        ))
        .build();
    scrollback.connect_value_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let lines = row.value().max(0.0) as usize;
            edit(&window, |settings| {
                settings.terminal.scrollback_lines = lines;
            });
        }
    ));
    behavior.add(&scrollback);

    let wrap = adw::SwitchRow::builder()
        .title("Wrap Lines in Files")
        .subtitle("A line longer than the pane folds onto the next one instead of scrolling")
        .active(settings.wrap_lines)
        .build();
    wrap.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.wrap_lines = on);
        }
    ));
    behavior.add(&wrap);
    page.add(&behavior);

    // --- the shell

    let shell_group = adw::PreferencesGroup::builder()
        .title("Shell")
        .description(shell_description(&settings.terminal.command))
        .build();
    let command = adw::EntryRow::builder()
        .title("Command")
        .text(&settings.terminal.command)
        .show_apply_button(true)
        .build();
    command.connect_apply(glib::clone!(
        #[weak]
        window,
        #[weak]
        shell_group,
        move |row| {
            let command = row.text().trim().to_owned();
            shell_group.set_description(Some(&shell_description(&command)));
            edit(&window, |settings| settings.terminal.command = command);
        }
    ));
    shell_group.add(&command);
    page.add(&shell_group);

    // --- the session

    let session = adw::PreferencesGroup::builder()
        .title("Session")
        .description("What a new tab opens, and how much of an old one comes back.")
        .build();

    let new_tab = adw::ComboRow::builder()
        .title("New Tab")
        .subtitle("Ctrl+Shift+O opens the host list either way")
        .model(&list(NEW_TABS.iter().map(|(_, label)| *label)))
        .selected(
            NEW_TABS
                .iter()
                .position(|(opens, _)| *opens == settings.new_tab)
                .unwrap_or(0) as u32,
        )
        .build();
    new_tab.connect_selected_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let Some((chosen, _)) = NEW_TABS.get(row.selected() as usize) else {
                return;
            };
            edit(&window, |settings| settings.new_tab = *chosen);
        }
    ));
    session.add(&new_tab);

    let restore = adw::SwitchRow::builder()
        .title("Restore Terminal Output")
        .subtitle("Writes what each terminal printed to disk, and replays it above the new prompt")
        .active(settings.restore_history)
        .build();
    restore.connect_active_notify(glib::clone!(
        #[weak]
        window,
        move |row| {
            let on = row.is_active();
            edit(&window, |settings| settings.restore_history = on);
        }
    ));
    session.add(&restore);
    page.add(&session);

    page
}

/// The Shortcuts page: one row per window action, the key it answers to, and
/// a click to change it. The editor's own keys are not here on purpose; they
/// are scoped to the editor widget so a shell never loses them, and a list
/// that mixes the two scopes would promise a configurability the design
/// refuses.
fn shortcuts_page(window: &crate::window::TuniWindow, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Shortcuts")
        .icon_name("input-keyboard-symbolic")
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Window Shortcuts")
        .description("A row is clicked, the new key is pressed; Backspace turns one off")
        .build();
    for (action, defaults) in crate::ACCELS {
        group.add(&shortcut_row(window, settings, action, defaults));
    }
    page.add(&group);
    page
}

fn shortcut_row(
    window: &crate::window::TuniWindow,
    settings: &Settings,
    action: &'static str,
    defaults: &'static [&'static str],
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(crate::shortcuts::label(action))
        .activatable(true)
        .build();
    let shortcut = gtk::ShortcutLabel::new("");
    shortcut.set_disabled_text("Off");
    shortcut.set_valign(gtk::Align::Center);
    let reset = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .tooltip_text("Back to the Default")
        .valign(gtk::Align::Center)
        .build();
    reset.add_css_class("flat");
    row.add_suffix(&shortcut);
    row.add_suffix(&reset);

    refresh_shortcut(settings, action, defaults, &row, &shortcut, &reset);

    reset.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        row,
        #[weak]
        shortcut,
        #[weak(rename_to = undo)]
        reset,
        move |_| {
            edit(&window, |settings| settings.set_key(action, None));
            refresh_shortcut(&window.settings(), action, defaults, &row, &shortcut, &undo);
        }
    ));
    row.connect_activated(glib::clone!(
        #[weak]
        window,
        #[weak]
        shortcut,
        #[weak]
        reset,
        move |row| record_shortcut(&window, action, defaults, row, &shortcut, &reset)
    ));
    row
}

/// Redraws one row from the settings: the key in force, and under the title
/// what happened to the default when something did.
fn refresh_shortcut(
    settings: &Settings,
    action: &str,
    defaults: &[&str],
    row: &adw::ActionRow,
    shortcut: &gtk::ShortcutLabel,
    reset: &gtk::Button,
) {
    let over = settings.key_override(action);
    shortcut.set_accelerator(
        crate::shortcuts::effective(settings, action, defaults)
            .as_deref()
            .unwrap_or(""),
    );
    reset.set_visible(over.is_some());
    let spoken = defaults
        .first()
        .and_then(|accel| gtk::accelerator_parse(*accel))
        .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string());
    row.set_subtitle(&match (over, spoken) {
        (Some(""), Some(default)) => format!("Off; the default is {default}"),
        (Some(_), Some(default)) => format!("The default is {default}"),
        _ => String::new(),
    });
}

/// Takes one keypress and makes it the shortcut. A bare modifier is waited
/// through, Escape keeps what there is, and Backspace turns the shortcut off,
/// which the dialog itself says.
fn record_shortcut(
    window: &crate::window::TuniWindow,
    action: &'static str,
    defaults: &'static [&'static str],
    row: &adw::ActionRow,
    shortcut: &gtk::ShortcutLabel,
    reset: &gtk::Button,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!(
            "Set the Key for {}",
            crate::shortcuts::label(action)
        ))
        .body("Press the new shortcut. Backspace turns it off, Escape changes nothing.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.set_close_response("cancel");

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[weak]
        dialog,
        #[weak]
        row,
        #[weak]
        shortcut,
        #[weak]
        reset,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, keyval, _, state| {
            use gtk::gdk::Key;
            if matches!(
                keyval,
                Key::Shift_L
                    | Key::Shift_R
                    | Key::Control_L
                    | Key::Control_R
                    | Key::Alt_L
                    | Key::Alt_R
                    | Key::Super_L
                    | Key::Super_R
                    | Key::Meta_L
                    | Key::Meta_R
                    | Key::Caps_Lock
                    | Key::ISO_Level3_Shift
            ) {
                return glib::Propagation::Proceed;
            }
            let mods = state & gtk::accelerator_get_default_mod_mask();
            let chosen = if keyval == Key::Escape && mods.is_empty() {
                None
            } else if keyval == Key::BackSpace && mods.is_empty() {
                Some(String::new())
            } else {
                Some(gtk::accelerator_name(keyval, mods).to_string())
            };
            if let Some(accel) = chosen {
                edit(&window, |settings| settings.set_key(action, Some(&accel)));
                refresh_shortcut(
                    &window.settings(),
                    action,
                    defaults,
                    &row,
                    &shortcut,
                    &reset,
                );
            }
            dialog.close();
            glib::Propagation::Stop
        }
    ));
    dialog.add_controller(keys);
    dialog.present(Some(row));
}

/// Reads the settings in force, changes one thing, and hands them back.
fn edit(window: &crate::window::TuniWindow, change: impl FnOnce(&mut Settings)) {
    let mut settings = window.settings();
    change(&mut settings);
    window.apply_settings(settings.clone());

    // Taken out of the slot before it is called: the closure paints a widget
    // inside this dialog, and nothing says a widget cannot reach back here.
    let preview = PREVIEW.with_borrow(Clone::clone);
    if let Some(preview) = preview {
        preview(&settings);
    }
}

/// What the shell group says under its title: which program a new tab will
/// start, or why the one named will not be it. A shell is a name someone types,
/// and a typed name that resolves to nothing is worth saying out loud rather
/// than discovering when the next tab opens on something else.
fn shell_description(command: &str) -> String {
    let command = command.trim();
    if command.is_empty() {
        return "Empty runs the login shell. Tabs already open keep the shell they started with."
            .to_owned();
    }
    match tuni_pty::resolve_shell(command) {
        Some(path) => format!("New tabs run {}", path.display()),
        None => format!("{command} is not on PATH, so new tabs fall back to the login shell"),
    }
}

/// What the font row says under its title: how the list is ordered, or the
/// truth about the family that is chosen. Tuni bundles no fonts, so the default
/// is a family a fresh install may well not have, and the terminal falls back
/// through the Nerd Font symbols to whatever fontconfig calls monospace rather
/// than showing nothing.
fn font_subtitle(widget: &impl IsA<gtk::Widget>, family: &str) -> String {
    let family = family.trim();
    if family.is_empty() {
        return ORDERING.to_owned();
    }
    if !installed(widget, family) {
        return format!("{family} is not installed, so the terminal falls back to monospace");
    }
    // Measured rather than read off fontconfig's label, and only for the one
    // family that was picked: the label is missing from faces that are fixed
    // width and wrong on a few that are not.
    if fixed_width(widget, family) {
        return ORDERING.to_owned();
    }
    format!("{family} is not monospaced, so its columns will not line up")
}

/// What the font row says when there is nothing else to say about the family.
const ORDERING: &str = "The families installed on this machine, monospaced first";

fn installed(widget: &impl IsA<gtk::Widget>, family: &str) -> bool {
    let Some(map) = widget.as_ref().pango_context().font_map() else {
        return true;
    };
    map.list_families()
        .iter()
        .any(|known| known.name().eq_ignore_ascii_case(family))
}

/// Every family this machine has that can draw a line of text, monospaced ones
/// first and the rest after, each alphabetical.
///
/// Not monospaced only. Fontconfig's `spacing` is a label a font either carries
/// or does not, plenty of faces people install for a terminal are missing it,
/// and a list that hides them reads as a list of the fonts that are installed.
/// so the ordering says which are which and the row says so under its title,
/// rather than the list deciding on someone's behalf.
fn font_families(widget: &impl IsA<gtk::Widget>) -> Vec<String> {
    let context = widget.as_ref().pango_context();
    let Some(map) = context.font_map() else {
        return Vec::new();
    };

    let mut monospaced: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    for family in map.list_families() {
        let name = family.name().to_string();
        if family.is_monospace() {
            // Only here: an emoji or symbol face is what carries the label
            // without carrying letters, and it is the monospaced half of the
            // list it would otherwise sit at the top of.
            if writes_text(&map, &context, &name) {
                monospaced.push(name);
            }
        } else {
            rest.push(name);
        }
    }

    for names in [&mut monospaced, &mut rest] {
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    }
    monospaced.extend(rest);
    monospaced
}

/// Whether every glyph in a family advances the same width, measured on the
/// characters that differ most in a proportional face.
fn fixed_width(widget: &impl IsA<gtk::Widget>, family: &str) -> bool {
    let context = widget.as_ref().pango_context();
    let layout = pango::Layout::new(&context);
    let mut description = pango::FontDescription::new();
    description.set_family(family);
    layout.set_font_description(Some(&description));

    let width = |text: &str| {
        layout.set_text(text);
        layout.pixel_size().0
    };
    let m = width("M");
    m > 0 && ["i", "W", "0"].into_iter().all(|text| width(text) == m)
}

/// What the Font group says above its rows when this machine has no Nerd Font.
///
/// The icons a prompt and a `neofetch` print live in a private use area, which
/// means no font is obliged to have them and every font is free to draw a box
/// instead. The chain in [`tuni_core::TerminalConfig::font_stack`] asks for the
/// symbols-only face by the name its own release carries, so saying which font
/// is missing is the whole fix.
fn missing_symbols(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    let map = widget.as_ref().pango_context().font_map()?;
    let has_nerd_font = map
        .list_families()
        .iter()
        .any(|family| family.name().to_lowercase().contains("nerd font"));
    (!has_nerd_font).then(|| {
        "No Nerd Font is installed, so the icons a prompt draws come out as \
         boxes. Symbols Nerd Font Mono, in ~/.local/share/fonts, is enough."
            .to_owned()
    })
}

/// Whether a family can draw the characters a shell prints. An emoji or symbol
/// face answers yes to `is_monospace`, since every glyph in it is the same width,
/// and has no letters at all, and a list that offers one as a terminal font is
/// offering a mistake.
fn writes_text(map: &pango::FontMap, context: &pango::Context, family: &str) -> bool {
    let mut description = pango::FontDescription::new();
    description.set_family(family);
    map.load_font(context, &description).is_some_and(|font| {
        ['M', '0', 'g']
            .into_iter()
            .all(|glyph| font.has_char(glyph))
    })
}

/// A line of terminal, drawn in the font and the theme that are being chosen —
/// kero shows the same thing, and for the same reason: neither setting is
/// decided from a name in a list.
fn preview_group(settings: &Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Preview").build();

    let label = gtk::Label::builder()
        .use_markup(true)
        .xalign(0.0)
        .wrap(false)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    label.add_css_class("tuni-preview");
    group.add(&label);

    let paint = move |settings: &Settings| {
        let theme = settings
            .terminal
            .theme(adw::StyleManager::default().is_dark());
        apply_preview_style(settings, &theme);
        label.set_markup(&preview_markup(&theme));
    };
    paint(settings);
    PREVIEW.with_borrow_mut(|slot| *slot = Some(Rc::new(paint)));

    group
}

/// Three lines that between them show what the two settings do: the shapes a
/// coding font is chosen for, the powerline glyphs a prompt needs a fallback
/// for, and the colors an error arrives in.
fn preview_markup(theme: &theme::Theme) -> String {
    let color = |index: usize| theme.palette[index].to_hex();
    format!(
        "<span foreground=\"{green}\">tuni</span> \
         <span foreground=\"{blue}\">\u{276f}</span> \
         echo \"the quick brown fox\" 0O 1lI\n\
         <span foreground=\"{cyan}\">\u{e0a0} main \u{e0b0} ~/dev/tuni</span>\n\
         <b><span foreground=\"{red}\">error</span></b>: \
         permission denied (os error 13)",
        green = color(2),
        blue = color(4),
        cyan = color(6),
        red = color(1),
    )
}

/// The preview's own colors and font, on the display's provider like the
/// chrome's and the editor's, since a label has no terminal to inherit from.
fn apply_preview_style(settings: &Settings, theme: &theme::Theme) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let css = format!(
        ".tuni-preview {{ font-family: {family}; font-size: {size}pt; \
         background-color: {bg}; color: {fg}; padding: 12px; \
         border-radius: 12px; }}\n",
        family = settings.terminal.font_stack(),
        size = settings.terminal.font_size,
        bg = theme.background.to_hex(),
        fg = theme.foreground.to_hex(),
    );
    PROVIDER.with(|provider| {
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

fn list<'a>(items: impl Iterator<Item = &'a str>) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(item);
    }
    list
}
