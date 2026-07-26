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

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use tuni_core::settings::{Appearance, Settings};
use tuni_core::theme;
use tuni_core::{FONT_SIZE_MAX, FONT_SIZE_MIN};

/// Appearances, in the order the dropdown lists them.
const APPEARANCES: [(Appearance, &str); 3] = [
    (Appearance::System, "Follow the Desktop"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
];

pub fn present(window: &crate::window::TuniWindow, settings: &Settings) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.add(&appearance_page(window, settings));
    dialog.add(&terminal_page(window, settings));
    dialog.present(Some(window));
}

fn appearance_page(window: &crate::window::TuniWindow, settings: &Settings) -> adw::PreferencesPage {
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
    page.add(&window_group);

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

    let chooser = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    chooser.set_level(gtk::FontLevel::Font);
    chooser.set_valign(gtk::Align::Center);
    chooser.set_font_desc(&pango::FontDescription::from_string(&format!(
        "{} {}",
        settings.terminal.font_family, settings.terminal.font_size
    )));
    chooser.connect_font_desc_notify(glib::clone!(
        #[weak]
        window,
        move |chooser| {
            let Some(desc) = chooser.font_desc() else {
                return;
            };
            let family = desc.family().map(|family| family.to_string());
            let size = f64::from(desc.size()) / f64::from(pango::SCALE);
            edit(&window, move |settings| {
                if let Some(family) = family.filter(|family| !family.trim().is_empty()) {
                    settings.terminal.font_family = family;
                }
                if (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(&size) {
                    settings.terminal.font_size = size;
                }
            });
        }
    ));
    let font = adw::ActionRow::builder()
        .title("Font")
        .subtitle("Family and size")
        .activatable_widget(&chooser)
        .build();
    font.add_suffix(&chooser);
    font_group.add(&font);

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
    page.add(&behavior);

    // --- the session

    let session = adw::PreferencesGroup::builder()
        .title("Session")
        .description("The window's shape is always restored. Its output is not.")
        .build();
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

/// Reads the settings in force, changes one thing, and hands them back.
fn edit(window: &crate::window::TuniWindow, change: impl FnOnce(&mut Settings)) {
    let mut settings = window.settings();
    change(&mut settings);
    window.apply_settings(settings);
}

fn list<'a>(items: impl Iterator<Item = &'a str>) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(item);
    }
    list
}
