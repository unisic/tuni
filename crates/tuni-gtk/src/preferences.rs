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

use tuni_core::settings::{Appearance, Settings};
use tuni_core::theme;
use tuni_core::{FONT_SIZE_MAX, FONT_SIZE_MIN};

/// Appearances, in the order the dropdown lists them.
const APPEARANCES: [(Appearance, &str); 3] = [
    (Appearance::System, "Follow the Desktop"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
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
        .subtitle(font_subtitle(&chooser, &settings.terminal.font_family))
        .activatable_widget(&chooser)
        .build();
    // The chooser shows "None" for a family fontconfig cannot find, which says
    // nothing about which family that was or what is being drawn instead.
    chooser.connect_font_desc_notify(glib::clone!(
        #[weak]
        font,
        move |chooser| {
            let family = chooser
                .font_desc()
                .and_then(|desc| desc.family())
                .map_or_else(String::new, |family| family.to_string());
            font.set_subtitle(&font_subtitle(chooser, &family));
        }
    ));
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
    window.apply_settings(settings.clone());

    // Taken out of the slot before it is called: the closure paints a widget
    // inside this dialog, and nothing says a widget cannot reach back here.
    let preview = PREVIEW.with_borrow(Clone::clone);
    if let Some(preview) = preview {
        preview(&settings);
    }
}

/// What the font row says under its title: the family and size, or the truth
/// when the family named is not on this machine. Tuni bundles no fonts, so the
/// default is a family a fresh install may well not have, and the terminal
/// falls back through the Nerd Font symbols to whatever fontconfig calls
/// monospace rather than showing nothing.
fn font_subtitle(widget: &impl IsA<gtk::Widget>, family: &str) -> String {
    let family = family.trim();
    if family.is_empty() || installed(widget, family) {
        return "Family and size".to_owned();
    }
    format!("{family} is not installed — falling back to monospace")
}

fn installed(widget: &impl IsA<gtk::Widget>, family: &str) -> bool {
    let Some(map) = widget.as_ref().pango_context().font_map() else {
        return true;
    };
    map.list_families()
        .iter()
        .any(|known| known.name().eq_ignore_ascii_case(family))
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
