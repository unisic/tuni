//! The dialog a host is written in.
//!
//! Unlike [`crate::preferences`], which writes every row through the moment it
//! is touched, this one holds its changes until Save. A half-typed address is
//! not a host anybody can connect to, and a file `ssh` reads is not a place to
//! put one.
//!
//! It edits a host from tuni's own file, and it will happily open one read out
//! of `~/.ssh/config`, but saving that writes a copy tuni owns rather than
//! touching the user's file, and the banner at the top says so. That file has
//! `Include`, `Match`, first-value-wins and comments people rely on; a terminal
//! that reformats it loses trust once and for good.
//!
//! The order of the cards is the order somebody fills them in: the address,
//! then what to call the machine, then how to get onto it. Everything a host
//! usually does not have — a snippet on connect, forwards, options written in
//! ssh's own words — is behind Show more, because four cards of empty advanced
//! fields is what makes an editor feel like a form rather than a question.
//! What is behind it opens with the dialog when the host already uses any of
//! it, so nothing a host carries is hidden from the person editing it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use tuni_core::ssh::{Forward, Host, Meta, Source};

use crate::{forward_editor, icon_picker};

/// What a host's row and its icon tile draw when it has not been given an icon.
pub(crate) const ICON: &str = "network-server-symbolic";

/// What the dialog hands back: the host as a file will hold it, and the part of
/// it ssh syntax cannot say.
pub struct Edited {
    /// The name the host had when the dialog opened, so a rename can take the
    /// old block out rather than leave two.
    pub original: Option<String>,
    pub host: Host,
    pub meta: Meta,
}

/// Opens the editor over `parent`. `host` is `None` for a host being added, and
/// `jumps` are the aliases a jump host can be picked from.
pub fn present<F>(
    parent: &impl IsA<gtk::Widget>,
    host: Option<Host>,
    meta: Meta,
    jumps: Vec<String>,
    save: F,
) where
    F: Fn(Edited) + 'static,
{
    let editing = host.is_some();
    // A host being added is tuni's from the start; the default source is the
    // user's file, which is the one thing this dialog never writes.
    let host = host.unwrap_or(Host {
        source: Source::Tuni,
        ..Host::default()
    });
    let original = editing.then(|| host.alias.clone());

    // The page a preferences page would have been. Built by hand because the
    // advanced half sits in a revealer, and a `PreferencesPage` takes groups
    // rather than a widget that can hold three of them.
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();

    // The icon is metadata rather than ssh syntax, and the tile beside the
    // address is where it is picked: a list of servers is a list of rows that
    // say the same thing, and the icon is what tells one from another.
    let icon = Rc::new(RefCell::new(meta.icon.clone()));

    let where_group = adw::PreferencesGroup::builder().title("Address").build();
    let address = adw::EntryRow::builder()
        .title("Address")
        .text(&host.hostname)
        .build();
    // Raised rather than flat, and square: it is the one control in the dialog
    // that is not a field, and a dim icon at the head of a row reads as
    // decoration rather than as something to press.
    let tile = gtk::Button::builder()
        .tooltip_text("Choose an icon")
        .valign(gtk::Align::Center)
        .width_request(34)
        .height_request(34)
        .build();
    tile.add_css_class("tuni-icon-tile");
    tile.set_child(Some(&icon_picker::image(
        icon.borrow().as_deref(),
        ICON,
        false,
    )));
    tile.connect_clicked(glib::clone!(
        #[strong]
        icon,
        move |tile| {
            let current = icon.borrow().clone();
            icon_picker::present(
                tile,
                "Host Icon",
                "Use the Server Icon",
                current,
                glib::clone!(
                    #[strong]
                    icon,
                    #[weak]
                    tile,
                    move |chosen: Option<String>| {
                        tile.set_child(Some(&icon_picker::image(chosen.as_deref(), ICON, false)));
                        icon.replace(chosen);
                    }
                ),
            );
        }
    ));
    address.add_prefix(&tile);
    where_group.add(&address);
    page.append(&where_group);

    let general = adw::PreferencesGroup::builder().title("General").build();
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(&host.alias)
        .build();
    let label = adw::EntryRow::builder()
        .title("Label")
        .text(&meta.label)
        .build();
    let tags = adw::EntryRow::builder()
        .title("Tags")
        .text(meta.tags.join(", "))
        .build();
    general.add(&name);
    general.add(&label);
    general.add(&tags);
    page.append(&general);

    let connection = adw::PreferencesGroup::builder().title("Connection").build();
    let user = adw::EntryRow::builder()
        .title("User")
        .text(&host.user)
        .build();
    user.add_prefix(&prefix("avatar-default-symbolic"));
    let port = adw::SpinRow::builder()
        .title("Port")
        .subtitle("Zero leaves the port to ssh, which means 22")
        .adjustment(&gtk::Adjustment::new(
            f64::from(host.port),
            0.0,
            f64::from(u16::MAX),
            1.0,
            10.0,
            0.0,
        ))
        .build();
    let identity = adw::EntryRow::builder()
        .title("Identity file")
        .text(host.identities.first().map_or("", String::as_str))
        .build();
    identity.add_prefix(&prefix("dialog-password-symbolic"));
    let choose = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Choose a key")
        .valign(gtk::Align::Center)
        .build();
    choose.add_css_class("flat");
    // The keys in `~/.ssh` are the answer nearly every time, so they are one
    // click rather than a file chooser. Insensitive until the scan comes back,
    // because that is a subprocess per key and this dialog opens now.
    let known = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("A key from ~/.ssh")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    known.add_css_class("flat");
    identity.add_suffix(&known);
    identity.add_suffix(&choose);
    offer_keys(&known, &identity);
    // Nothing is a real answer here, and it is the first one, so the index of a
    // chosen alias is one past where it sits in the list.
    let mut names = vec!["None".to_owned()];
    names.extend(jumps.iter().filter(|alias| **alias != host.alias).cloned());
    let jump = adw::ComboRow::builder()
        .title("Jump host")
        .subtitle("Reached through this host first")
        .model(&string_list(&names))
        .selected(
            names
                .iter()
                .position(|alias| *alias == host.proxy_jump)
                .unwrap_or(0) as u32,
        )
        .build();
    connection.add(&port);
    connection.add(&user);
    connection.add(&identity);
    connection.add(&jump);
    page.append(&connection);

    // Everything below is what most hosts leave alone, so it is folded away
    // unless this one already uses it.
    let advanced = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .reveal_child(crowded(&host, &meta))
        .child(&advanced)
        .build();
    let more = disclosure(revealer.reveals_child());
    more.connect_clicked(glib::clone!(
        #[weak]
        revealer,
        move |button| {
            let opening = !revealer.reveals_child();
            revealer.set_reveal_child(opening);
            button.set_child(Some(&disclosure_face(opening)));
        }
    ));
    page.append(&more);
    page.append(&revealer);

    // Same shape as the jump host: nothing is the first answer and a real one.
    let mut snippets = vec!["None".to_owned()];
    snippets.extend(
        tuni_core::snippets::Snippets::load()
            .all()
            .iter()
            .map(|snippet| snippet.name.clone()),
    );
    let on_connect = adw::ComboRow::builder()
        .title("Run on connect")
        .subtitle("Typed into the pane once the connection is up")
        .model(&string_list(&snippets))
        .selected(
            snippets
                .iter()
                .position(|name| *name == meta.on_connect)
                .unwrap_or(0) as u32,
        )
        .build();
    let session = adw::PreferencesGroup::builder().title("Session").build();
    session.add(&on_connect);
    advanced.append(&session);

    // Held apart from the rows, because a forward is added and removed rather
    // than typed, and Save is what puts the list in the file.
    let forwards = Rc::new(RefCell::new(host.forwards.clone()));
    let forwards_group = adw::PreferencesGroup::builder()
        .title("Port Forwarding")
        .description("Brought up by ssh with the connection, every time")
        .build();
    let forward_rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    forward_rows.add_css_class("boxed-list");
    let add_forward = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add a forward")
        .valign(gtk::Align::Center)
        .build();
    add_forward.add_css_class("flat");
    forwards_group.set_header_suffix(Some(&add_forward));
    forwards_group.add(&forward_rows);
    advanced.append(&forwards_group);
    draw(&forward_rows, &forwards);
    add_forward.connect_clicked(glib::clone!(
        #[strong]
        forwards,
        #[weak]
        forward_rows,
        move |button| {
            forward_editor::present(
                button,
                None,
                glib::clone!(
                    #[strong]
                    forwards,
                    #[weak]
                    forward_rows,
                    move |forward| {
                        forwards.borrow_mut().push(forward);
                        draw(&forward_rows, &forwards);
                    }
                ),
            );
        }
    ));

    let extra_group = adw::PreferencesGroup::builder()
        .title("Extra ssh options")
        .description("One `Keyword value` per line, as ~/.ssh/config writes them")
        .build();
    let extra = gtk::TextView::builder()
        .monospace(true)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(6)
        .right_margin(6)
        .build();
    extra.buffer().set_text(&host.extra.join("\n"));
    let extra_frame = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(90)
        .child(&extra)
        .build();
    extra_frame.add_css_class("card");
    // A text view paints its own background, which is a shade darker than the
    // card around it and reads as a hole in the page rather than as a field.
    extra.add_css_class("tuni-plain-view");
    extra_group.add(&extra_frame);
    advanced.append(&extra_group);

    let column = adw::Clamp::builder()
        .maximum_size(480)
        .tightening_threshold(480)
        .child(&page)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build();

    // A size rather than the content's own: rows this narrow wrap their
    // subtitles into paragraphs, and the dropdown loses the name it is showing.
    let dialog = adw::Dialog::builder()
        .title(if editing { "Edit Host" } else { "Add Host" })
        .content_width(480)
        .content_height(700)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Save");
    confirm.add_css_class("suggested-action");
    let bar = adw::HeaderBar::new();
    // Cancel is the way out, and a close button beside it would be a second one
    // that means the same thing.
    bar.set_show_end_title_buttons(false);
    bar.pack_start(&cancel);
    bar.pack_end(&confirm);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&bar);
    if host.source == Source::SshConfig {
        let banner = adw::Banner::builder()
            .title("Declared in ~/.ssh/config. Saving writes a copy tuni owns.")
            .revealed(true)
            .build();
        view.add_top_bar(&banner);
    }
    view.set_content(Some(&scroller));
    dialog.set_child(Some(&view));

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    // A host with no name cannot be written and a host with no address is one
    // ssh would look up by its own name, which is a different host.
    let ready = glib::clone!(
        #[weak]
        name,
        #[weak]
        address,
        #[weak]
        confirm,
        move || {
            let filled = !name.text().trim().is_empty() && !address.text().trim().is_empty();
            confirm.set_sensitive(filled);
        }
    );
    name.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    address.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    ready();

    choose.connect_clicked(glib::clone!(
        #[weak]
        identity,
        #[weak]
        dialog,
        move |_| pick_key(&dialog, &identity)
    ));

    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        name,
        move |_| {
            let buffer = extra.buffer();
            let written = Host {
                alias: name.text().trim().to_owned(),
                hostname: address.text().trim().to_owned(),
                port: port.value() as u16,
                user: user.text().trim().to_owned(),
                identities: once(identity.text().trim()),
                proxy_jump: names
                    .get(jump.selected() as usize)
                    .filter(|_| jump.selected() != 0)
                    .cloned()
                    .unwrap_or_default(),
                forwards: forwards.borrow().clone(),
                extra: lines(&buffer.text(&buffer.start_iter(), &buffer.end_iter(), false)),
                source: Source::Tuni,
                origin: None,
                shadowed: false,
            };
            let written_meta = Meta {
                label: label.text().trim().to_owned(),
                tags: split_tags(&tags.text()),
                icon: icon.borrow().clone(),
                on_connect: snippets
                    .get(on_connect.selected() as usize)
                    .filter(|_| on_connect.selected() != 0)
                    .cloned()
                    .unwrap_or_default(),
                ..meta.clone()
            };
            save(Edited {
                original: original.clone(),
                host: written,
                meta: written_meta,
            });
            dialog.close();
        }
    ));

    dialog.present(Some(parent));
    name.grab_focus();
}

/// Whether a host already uses anything the advanced half holds, which is what
/// decides whether that half opens with the dialog. Folding away a forward
/// somebody wrote would be hiding the host from the person editing it.
fn crowded(host: &Host, meta: &Meta) -> bool {
    !host.forwards.is_empty() || !host.extra.is_empty() || !meta.on_connect.is_empty()
}

/// The mark in front of a row, saying what the field is for at the width a
/// title has already been read at.
fn prefix(icon: &str) -> gtk::Image {
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("dim-label");
    image
}

/// The Show more control: a flat button rather than a row, because what it
/// opens is three cards and not one field.
fn disclosure(open: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(&disclosure_face(open))
        .halign(gtk::Align::Start)
        .build();
    button.add_css_class("flat");
    button
}

fn disclosure_face(open: bool) -> gtk::Box {
    let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let label = gtk::Label::new(Some(if open { "Show less" } else { "Show more" }));
    label.add_css_class("dim-label");
    face.append(&label);
    face.append(&gtk::Image::from_icon_name(if open {
        "pan-up-symbolic"
    } else {
        "pan-down-symbolic"
    }));
    face
}

/// Draws the forwards a host declares, whole. A handful of rows, each carrying
/// the position it edits, so a change is a redraw rather than a patch.
fn draw(rows: &gtk::ListBox, forwards: &Rc<RefCell<Vec<Forward>>>) {
    while let Some(row) = rows.first_child() {
        rows.remove(&row);
    }
    if forwards.borrow().is_empty() {
        let empty = adw::ActionRow::builder().title("No forwards").build();
        empty.set_sensitive(false);
        rows.append(&empty);
        return;
    }

    for (index, forward) in forwards.borrow().iter().enumerate() {
        let (title, written) = forward_editor::describe(forward);
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(written)
            .activatable(true)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove this forward")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        remove.connect_clicked(glib::clone!(
            #[strong]
            forwards,
            #[weak]
            rows,
            move |_| {
                forwards.borrow_mut().remove(index);
                draw(&rows, &forwards);
            }
        ));
        row.add_suffix(&remove);
        row.connect_activated(glib::clone!(
            #[strong]
            forwards,
            #[weak]
            rows,
            move |row| {
                let editing = forwards.borrow()[index].clone();
                forward_editor::present(
                    row,
                    Some(editing),
                    glib::clone!(
                        #[strong]
                        forwards,
                        #[weak]
                        rows,
                        move |forward| {
                            forwards.borrow_mut()[index] = forward;
                            draw(&rows, &forwards);
                        }
                    ),
                );
            }
        ));
        rows.append(&row);
    }
}

/// Opens the file chooser on `~/.ssh`, where the keys are, and writes what
/// comes back into the row.
/// Hangs the keys `ssh-keygen` can describe under `button`, once they have been
/// read. A machine with none simply keeps an insensitive button, which is the
/// truthful thing for it to say.
fn offer_keys(button: &gtk::MenuButton, row: &adw::EntryRow) {
    glib::spawn_future_local(glib::clone!(
        #[weak]
        button,
        #[weak]
        row,
        async move {
            let Ok(keys) = gtk::gio::spawn_blocking(tuni_core::ssh::keys).await else {
                return;
            };
            if keys.is_empty() {
                return;
            }
            let popover = gtk::Popover::new();
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for key in keys {
                let path = shorten(&key.private());
                let item = gtk::Button::builder().label(&path).build();
                item.add_css_class("flat");
                item.set_tooltip_text(Some(&format!("{} {}", key.kind, key.title())));
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    label.set_xalign(0.0);
                }
                item.connect_clicked(glib::clone!(
                    #[weak]
                    row,
                    #[weak]
                    popover,
                    move |_| {
                        row.set_text(&path);
                        popover.popdown();
                    }
                ));
                list.append(&item);
            }
            popover.set_child(Some(&list));
            button.set_popover(Some(&popover));
            button.set_sensitive(true);
        }
    ));
}

fn pick_key(dialog: &adw::Dialog, row: &adw::EntryRow) {
    let chooser = gtk::FileDialog::builder().title("Choose a Key").build();
    if let Some(home) = std::env::var_os("HOME") {
        chooser.set_initial_folder(Some(&gtk::gio::File::for_path(
            std::path::Path::new(&home).join(".ssh"),
        )));
    }
    let window = dialog.root().and_downcast::<gtk::Window>();
    chooser.open(
        window.as_ref(),
        gtk::gio::Cancellable::NONE,
        glib::clone!(
            #[weak]
            row,
            move |chosen| {
                let Ok(file) = chosen else {
                    return;
                };
                let Some(path) = file.path() else {
                    return;
                };
                row.set_text(&shorten(&path));
            }
        ),
    );
}

/// A path under the home directory, as the configuration would rather hold it:
/// `~/.ssh/id_ed25519` keeps working for a user whose home moves, and it is
/// what somebody reading the file expects to see.
fn shorten(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.and_then(|home| {
        path.strip_prefix(home)
            .ok()
            .map(std::path::Path::to_path_buf)
    }) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn once(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        vec![value.to_owned()]
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn split_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

fn string_list(items: &[String]) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(item);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_read_the_way_somebody_would_type_them() {
        assert_eq!(split_tags(" prod, db ,, "), ["prod", "db"]);
        assert!(split_tags("   ").is_empty());
    }

    #[test]
    fn the_advanced_half_opens_on_a_host_that_uses_any_of_it() {
        let plain = Host::default();
        assert!(!crowded(&plain, &Meta::default()));
        assert!(crowded(
            &Host {
                extra: vec!["Compression yes".to_owned()],
                ..plain.clone()
            },
            &Meta::default()
        ));
        assert!(crowded(
            &plain,
            &Meta {
                on_connect: "tmux".to_owned(),
                ..Meta::default()
            }
        ));
    }

    #[test]
    fn a_blank_line_between_options_is_not_an_option() {
        assert_eq!(
            lines("  Compression yes\n\n  ForwardX11 no\n"),
            ["Compression yes", "ForwardX11 no"]
        );
    }
}
