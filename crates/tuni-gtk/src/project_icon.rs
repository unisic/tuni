//! The picker a project's sidebar icon is chosen in.
//!
//! Two alphabets, one field. A themed icon follows the desktop's icon theme and
//! is drawn in the row's foreground colour like every other symbolic icon in the
//! window; an emoji is coloured text the font decides. Which of the two a
//! project uses is read back off the string rather than stored beside it, so
//! there is one record and no pair of fields that can both be set.
//!
//! Both are on one scrolling page, emoji first, rather than an emoji button that
//! opens a chooser over the grid: colour is what makes one row findable in a
//! sidebar of six, so the coloured half has to be the half you see first. The
//! forty here are the ones a directory of work tends to be about; the system
//! chooser, with every emoji this desktop has and a search field over them, is
//! the last tile of that grid rather than a second control in the header.
//!
//! The icon grid is a fixed list rather than everything the theme has. A desktop
//! icon theme holds thousands of names, most of them a device, a mimetype or one
//! application's own logo, and a project is none of those. Any the running theme
//! is missing are dropped rather than drawn as a broken image.

use adw::prelude::*;
use gtk::glib;

/// The emoji offered without opening the system chooser: languages, tools, the
/// states a piece of work is in, and enough plain colour to tell two rows apart.
/// The ones written with a trailing U+FE0F carry it on purpose: their code point
/// predates emoji and a font draws it as a black-and-white glyph without the
/// selector, which in a grid of colour reads as a broken tile.
const EMOJI: &[&str] = &[
    "🐙",
    "🚀",
    "🔧",
    "🐛",
    "📦",
    "🧪",
    "🔥",
    "💡",
    "📝",
    "📚",
    "🎨",
    "🎮",
    "🎵",
    "🌐",
    "🔒",
    "🗄\u{fe0f}",
    "🖥\u{fe0f}",
    "📱",
    "⚙\u{fe0f}",
    "✅",
    "⭐",
    "❤\u{fe0f}",
    "🌱",
    "🐍",
    "🦀",
    "🐳",
    "☕",
    "🧩",
    "📊",
    "🗺\u{fe0f}",
    "⏰",
    "💰",
    "🏠",
    "🏢",
    "🎯",
    "🔍",
    "⚡",
    "🧠",
    "👀",
    "🍎",
];

/// The icons offered, in the order they are drawn. Grouped loosely by what a
/// project tends to be: work, a machine, a kind of file, then a mark to tell one
/// row from another at a glance.
const ICONS: &[&str] = &[
    "folder-symbolic",
    "user-home-symbolic",
    "applications-engineering-symbolic",
    "utilities-terminal-symbolic",
    "text-editor-symbolic",
    "system-run-symbolic",
    "package-x-generic-symbolic",
    "emblem-system-symbolic",
    "preferences-system-symbolic",
    "network-server-symbolic",
    "computer-symbolic",
    "drive-harddisk-symbolic",
    "network-wired-symbolic",
    "network-wireless-symbolic",
    "bluetooth-symbolic",
    "channel-secure-symbolic",
    "security-high-symbolic",
    "web-browser-symbolic",
    "mail-send-symbolic",
    "phone-symbolic",
    "printer-symbolic",
    "camera-photo-symbolic",
    "applications-science-symbolic",
    "applications-graphics-symbolic",
    "applications-multimedia-symbolic",
    "applications-games-symbolic",
    "audio-x-generic-symbolic",
    "video-x-generic-symbolic",
    "image-x-generic-symbolic",
    "text-x-generic-symbolic",
    "document-open-symbolic",
    "document-save-symbolic",
    "edit-find-symbolic",
    "view-list-symbolic",
    "view-grid-symbolic",
    "starred-symbolic",
    "bookmark-new-symbolic",
    "emblem-important-symbolic",
    "weather-clear-symbolic",
    "display-brightness-symbolic",
    "media-playback-start-symbolic",
    "battery-symbolic",
    "dialog-information-symbolic",
];

/// Opens the picker over `parent`.
///
/// `current` is what the project draws now, so the picker can mark it, and
/// `apply` is handed the new value: a name, an emoji, or `None` for the folder
/// every project starts with. It is called once and the dialog closes, because
/// picking an icon is one complete change and a Save button over it would only
/// be a second click to say the same thing.
pub fn present<F>(parent: &impl IsA<gtk::Widget>, current: Option<String>, apply: F)
where
    F: Fn(Option<String>) + 'static,
{
    let dialog = adw::Dialog::builder()
        .title("Project Icon")
        .content_width(400)
        .content_height(560)
        .build();

    let apply = std::rc::Rc::new(apply);
    let page = adw::PreferencesPage::new();

    let emoji = group();
    for glyph in EMOJI {
        let button = tile(&gtk::Label::builder().label(*glyph).build());
        button.set_tooltip_text(Some(glyph));
        if current.as_deref() == Some(*glyph) {
            button.add_css_class("suggested-action");
        }
        button.connect_clicked(chooses(&apply, &dialog, Some((*glyph).to_owned())));
        emoji.append(&button);
    }

    // Last rather than first: the forty above answer the question most of the
    // time, and this is the door to the other two thousand.
    let chooser = gtk::EmojiChooser::new();
    chooser.connect_emoji_picked(glib::clone!(
        #[strong]
        apply,
        #[weak]
        dialog,
        move |_, glyph| {
            apply(Some(glyph.to_owned()));
            dialog.close();
        }
    ));
    let more = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Every other emoji")
        .popover(&chooser)
        .build();
    more.add_css_class("flat");
    more.add_css_class("circular");
    emoji.append(&more);

    let icons = group();
    // Asked of the display the dialog is opening on rather than the default: a
    // window dragged to another screen draws with that screen's theme, and an
    // icon missing there would be a hole in the grid.
    let theme = gtk::IconTheme::for_display(&parent.as_ref().display());
    for name in ICONS.iter().filter(|name| theme.has_icon(name)) {
        let button = tile(&gtk::Image::from_icon_name(name));
        button.set_tooltip_text(Some(name));
        if current.as_deref() == Some(*name) {
            button.add_css_class("suggested-action");
        }
        button.connect_clicked(chooses(&apply, &dialog, Some((*name).to_owned())));
        icons.append(&button);
    }

    page.add(&wrap("Emoji", &emoji));
    page.add(&wrap("Icons", &icons));

    // Only when there is something to undo. A reset that is always there and
    // usually does nothing is one more thing to read past on the way to the
    // grid.
    if current.is_some() {
        let reset = gtk::Button::builder()
            .label("Use the Folder Icon")
            .halign(gtk::Align::Center)
            .margin_top(6)
            .build();
        reset.add_css_class("pill");
        reset.connect_clicked(chooses(&apply, &dialog, None));
        let group = adw::PreferencesGroup::new();
        group.add(&reset);
        page.add(&group);
    }

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.present(Some(parent));
}

/// One grid of choices. Homogeneous so an emoji tile and an icon tile are the
/// same square, which is the only way two grids under each other read as one
/// list of options rather than two widgets that happen to be stacked.
fn group() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .row_spacing(4)
        .column_spacing(4)
        .min_children_per_line(7)
        .max_children_per_line(7)
        .build()
}

fn wrap(title: &str, grid: &gtk::FlowBox) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    group.add(grid);
    group
}

/// A square button around one glyph or one icon, big enough to hit and to see:
/// an emoji at the size a symbolic icon is drawn at is a smudge.
fn tile(child: &impl IsA<gtk::Widget>) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(child)
        .width_request(44)
        .height_request(44)
        .build();
    button.add_css_class("flat");
    button.add_css_class("tuni-icon-tile");
    button
}

fn chooses<F>(
    apply: &std::rc::Rc<F>,
    dialog: &adw::Dialog,
    icon: Option<String>,
) -> impl Fn(&gtk::Button) + use<F>
where
    F: Fn(Option<String>) + 'static,
{
    glib::clone!(
        #[strong]
        apply,
        #[weak]
        dialog,
        move |_: &gtk::Button| {
            apply(icon.clone());
            dialog.close();
        }
    )
}
