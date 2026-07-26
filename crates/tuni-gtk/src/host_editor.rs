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

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use tuni_core::ssh::{Forward, Host, Meta, Source};

use crate::forward_editor;

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

    let page = adw::PreferencesPage::new();

    let connection = adw::PreferencesGroup::builder().title("Connection").build();
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(&host.alias)
        .build();
    let address = adw::EntryRow::builder()
        .title("Address")
        .text(&host.hostname)
        .build();
    let user = adw::EntryRow::builder()
        .title("User")
        .text(&host.user)
        .build();
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
    let choose = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Choose a key")
        .valign(gtk::Align::Center)
        .build();
    choose.add_css_class("flat");
    identity.add_suffix(&choose);
    connection.add(&name);
    connection.add(&address);
    connection.add(&user);
    connection.add(&port);
    connection.add(&identity);
    page.add(&connection);

    let options = adw::PreferencesGroup::builder().title("Options").build();
    let label = adw::EntryRow::builder()
        .title("Label")
        .text(&meta.label)
        .build();
    let tags = adw::EntryRow::builder()
        .title("Tags")
        .text(meta.tags.join(", "))
        .build();
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
    options.add(&label);
    options.add(&tags);
    options.add(&jump);
    options.add(&on_connect);
    page.add(&options);

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
    page.add(&forwards_group);
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
    extra_group.add(&extra_frame);
    page.add(&extra_group);

    // A size rather than the content's own: rows this narrow wrap their
    // subtitles into paragraphs, and the dropdown loses the name it is showing.
    let dialog = adw::Dialog::builder()
        .title(if editing { "Edit Host" } else { "Add Host" })
        .content_width(480)
        .content_height(620)
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
    view.set_content(Some(&page));
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
    fn a_blank_line_between_options_is_not_an_option() {
        assert_eq!(
            lines("  Compression yes\n\n  ForwardX11 no\n"),
            ["Compression yes", "ForwardX11 no"]
        );
    }
}
