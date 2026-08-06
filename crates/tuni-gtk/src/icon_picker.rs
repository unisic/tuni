//! The picker a sidebar icon is chosen in: a project's row, and a host's.
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
//! The distribution logos come second, above the desktop's own icons, because a
//! host list is where this picker is opened from most and "which system is this"
//! is the question a row of identical addresses cannot answer.
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
    "🐧",
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

/// The system a host runs, and the name to say so in the tooltip, since
/// `tuni-pop-os-symbolic` is not what anybody is looking for. Ordered the way
/// the families are related rather than alphabetically: Debian and its
/// children, Red Hat and its rebuilds, SUSE, Arch, the source and small ones,
/// the appliances, the BSDs, the desktops, then where a machine is rented.
///
/// Shipped in `data/icons` rather than looked up in the theme: a desktop has
/// the logo of the distribution it is running on, at best, and a host list is
/// about other machines. The marks belong to those projects; they are here to
/// say which system a row is, and nothing else.
const DISTROS: &[(&str, &str)] = &[
    ("tuni-debian-symbolic", "Debian"),
    ("tuni-ubuntu-symbolic", "Ubuntu"),
    ("tuni-mint-symbolic", "Linux Mint"),
    ("tuni-pop-os-symbolic", "Pop!_OS"),
    ("tuni-elementary-symbolic", "elementary OS"),
    ("tuni-zorin-symbolic", "Zorin OS"),
    ("tuni-fedora-symbolic", "Fedora"),
    ("tuni-redhat-symbolic", "Red Hat"),
    ("tuni-centos-symbolic", "CentOS"),
    ("tuni-alma-symbolic", "AlmaLinux"),
    ("tuni-rocky-symbolic", "Rocky Linux"),
    ("tuni-opensuse-symbolic", "openSUSE"),
    ("tuni-arch-symbolic", "Arch Linux"),
    ("tuni-manjaro-symbolic", "Manjaro"),
    ("tuni-gentoo-symbolic", "Gentoo"),
    ("tuni-alpine-symbolic", "Alpine Linux"),
    ("tuni-nixos-symbolic", "NixOS"),
    ("tuni-void-symbolic", "Void Linux"),
    ("tuni-kali-symbolic", "Kali Linux"),
    ("tuni-linux-symbolic", "Linux"),
    ("tuni-raspberry-pi-symbolic", "Raspberry Pi"),
    ("tuni-openwrt-symbolic", "OpenWrt"),
    ("tuni-proxmox-symbolic", "Proxmox"),
    ("tuni-truenas-symbolic", "TrueNAS"),
    ("tuni-unraid-symbolic", "Unraid"),
    ("tuni-synology-symbolic", "Synology"),
    ("tuni-docker-symbolic", "Docker"),
    ("tuni-kubernetes-symbolic", "Kubernetes"),
    ("tuni-freebsd-symbolic", "FreeBSD"),
    ("tuni-openbsd-symbolic", "OpenBSD"),
    ("tuni-netbsd-symbolic", "NetBSD"),
    ("tuni-windows-symbolic", "Windows"),
    ("tuni-macos-symbolic", "macOS"),
    ("tuni-aws-symbolic", "AWS"),
    ("tuni-google-cloud-symbolic", "Google Cloud"),
    ("tuni-oracle-symbolic", "Oracle Cloud"),
    ("tuni-digitalocean-symbolic", "DigitalOcean"),
    ("tuni-hetzner-symbolic", "Hetzner"),
    ("tuni-ovh-symbolic", "OVH"),
    ("tuni-linode-symbolic", "Linode"),
    ("tuni-vultr-symbolic", "Vultr"),
    ("tuni-cloudflare-symbolic", "Cloudflare"),
];

/// Draws an icon the way this picker writes them down: an emoji is text the
/// font colours, a name is a symbolic icon in the foreground colour, and
/// nothing at all is `fallback`, which is whatever the row draws when it has
/// not been given one.
///
/// `dim` is for a list, where the icon is beside the name rather than the thing
/// being pointed at. An emoji is never dimmed whatever the caller asks: its
/// colour is the reason somebody picked it.
pub fn image(icon: Option<&str>, fallback: &str, dim: bool) -> gtk::Widget {
    match icon {
        Some(glyph) if tuni_core::workspace::is_emoji(glyph) => {
            gtk::Label::builder().label(glyph).build().upcast()
        }
        chosen => {
            let image = gtk::Image::from_icon_name(chosen.unwrap_or(fallback));
            if dim {
                image.add_css_class("dim-label");
            }
            image.upcast()
        }
    }
}

/// Opens the picker over `parent`.
///
/// `title` names what is being given an icon and `reset` says what dropping it
/// goes back to, because a project falls back to a folder and a host to a
/// server. `current` is what the row draws now, so the picker can mark it, and
/// `apply` is handed the new value: a name, an emoji, or `None` for the
/// fallback. It is called once and the dialog closes, because picking an icon
/// is one complete change and a Save button over it would only be a second
/// click to say the same thing.
pub fn present<F>(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    reset: &str,
    current: Option<String>,
    apply: F,
) where
    F: Fn(Option<String>) + 'static,
{
    let dialog = adw::Dialog::builder()
        .title(title)
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

    // Asked of the display the dialog is opening on rather than the default: a
    // window dragged to another screen draws with that screen's theme, and an
    // icon missing there would be a hole in the grid.
    let theme = gtk::IconTheme::for_display(&parent.as_ref().display());
    let chosen = |name: &str, label: &str| {
        let button = tile(&gtk::Image::from_icon_name(name));
        button.set_tooltip_text(Some(label));
        if current.as_deref() == Some(name) {
            button.add_css_class("suggested-action");
        }
        button.connect_clicked(chooses(&apply, &dialog, Some(name.to_owned())));
        button
    };

    let systems = group();
    for (name, label) in DISTROS.iter().filter(|(name, _)| theme.has_icon(name)) {
        systems.append(&chosen(name, label));
    }

    let icons = group();
    for name in ICONS.iter().filter(|name| theme.has_icon(name)) {
        icons.append(&chosen(name, name));
    }

    page.add(&wrap("Emoji", &emoji));
    page.add(&wrap("Systems", &systems));
    page.add(&wrap("Icons", &icons));

    // Only when there is something to undo. A reset that is always there and
    // usually does nothing is one more thing to read past on the way to the
    // grid.
    if current.is_some() {
        let reset = gtk::Button::builder()
            .label(reset)
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

#[cfg(test)]
mod tests {
    use super::DISTROS;

    /// A name in the list with no file behind it is not an error anywhere: the
    /// grid drops what the theme is missing, so a typo is a tile that silently
    /// never appears.
    #[test]
    fn every_system_logo_is_a_file_this_repository_ships() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/icons/hicolor/scalable/actions"
        );
        for (name, label) in DISTROS {
            let path = std::path::Path::new(dir).join(format!("{name}.svg"));
            assert!(path.exists(), "{label} has no {name}.svg");
        }
    }

    #[test]
    fn no_system_is_offered_twice() {
        let mut seen: Vec<_> = DISTROS.iter().map(|(name, _)| *name).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count);
    }
}
