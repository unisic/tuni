//! The dialog snippets are kept in.
//!
//! One list and one small editor, and no page of its own: a snippet is run from
//! the command palette, where every other thing the window can do is already
//! looked up by name. This is only where the text is written down.
//!
//! Unlike [`crate::host_editor`], the list writes through as it changes. There
//! is no half-finished state to hold back: adding, editing and removing are each
//! one complete change, and the editor behind them has its own Save.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use tuni_core::snippets::{Snippet, Snippets};

/// Opens the list over `parent`.
pub fn present(parent: &impl IsA<gtk::Widget>) {
    let snippets = Rc::new(RefCell::new(Snippets::load()));

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .description("Typed into the pane that has the keyboard, exactly as written")
        .build();
    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    rows.add_css_class("boxed-list");
    group.add(&rows);
    page.add(&group);
    draw(&rows, &snippets);

    let dialog = adw::Dialog::builder()
        .title("Snippets")
        .content_width(480)
        .content_height(520)
        .build();

    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add a snippet")
        .build();
    add.add_css_class("flat");
    add.connect_clicked(glib::clone!(
        #[strong]
        snippets,
        #[weak]
        rows,
        move |button| {
            let taken = names(&snippets, None);
            edit(
                button,
                None,
                taken,
                glib::clone!(
                    #[strong]
                    snippets,
                    #[weak]
                    rows,
                    move |written| {
                        snippets.borrow_mut().set(None, written);
                        store(&rows, &snippets);
                    }
                ),
            );
        }
    ));
    let bar = adw::HeaderBar::new();
    bar.pack_end(&add);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&bar);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.present(Some(parent));
}

/// Draws the list whole. A handful of rows, each holding the name it edits.
fn draw(rows: &gtk::ListBox, snippets: &Rc<RefCell<Snippets>>) {
    while let Some(row) = rows.first_child() {
        rows.remove(&row);
    }
    if snippets.borrow().all().is_empty() {
        let empty = adw::ActionRow::builder().title("No snippets").build();
        empty.set_sensitive(false);
        rows.append(&empty);
        return;
    }

    for snippet in snippets.borrow().all() {
        let name = snippet.name.clone();
        let row = adw::ActionRow::builder()
            .title(&snippet.name)
            .subtitle(summary(&snippet.body))
            .subtitle_lines(1)
            .activatable(true)
            .build();
        row.set_tooltip_text(Some(&snippet.body));
        // Whether the last character is a newline is the difference between a
        // snippet that runs and one that waits to be finished, and it is the
        // one character a list cannot show.
        if !snippet.body.ends_with('\n') {
            let waits = gtk::Label::new(Some("waits at the prompt"));
            waits.add_css_class("caption");
            waits.add_css_class("dim-label");
            row.add_suffix(&waits);
        }

        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove this snippet")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        remove.connect_clicked(glib::clone!(
            #[strong]
            snippets,
            #[strong]
            name,
            #[weak]
            rows,
            move |_| {
                snippets.borrow_mut().remove(&name);
                store(&rows, &snippets);
            }
        ));
        row.add_suffix(&remove);

        row.connect_activated(glib::clone!(
            #[strong]
            snippets,
            #[strong]
            name,
            #[weak]
            rows,
            move |row| {
                let Some(editing) = snippets.borrow().get(&name).cloned() else {
                    return;
                };
                let taken = names(&snippets, Some(&name));
                edit(
                    row,
                    Some(editing),
                    taken,
                    glib::clone!(
                        #[strong]
                        snippets,
                        #[strong]
                        name,
                        #[weak]
                        rows,
                        move |written| {
                            snippets.borrow_mut().set(Some(&name), written);
                            store(&rows, &snippets);
                        }
                    ),
                );
            }
        ));
        rows.append(&row);
    }
}

/// Writes the list out and redraws it. A file that cannot be written is said
/// out loud: the list on screen would otherwise look like it had been kept.
fn store(rows: &gtk::ListBox, snippets: &Rc<RefCell<Snippets>>) {
    draw(rows, snippets);
    if let Err(error) = snippets.borrow().save() {
        let dialog = adw::AlertDialog::new(
            Some("Cannot save the snippets"),
            Some(&format!("{}: {error}", Snippets::path().display())),
        );
        dialog.add_response("close", "Close");
        dialog.present(Some(rows));
    }
}

/// The names already in use, apart from the one being edited, which is what a
/// second snippet may not take.
fn names(snippets: &Rc<RefCell<Snippets>>, except: Option<&str>) -> Vec<String> {
    snippets
        .borrow()
        .all()
        .iter()
        .map(|snippet| snippet.name.clone())
        .filter(|name| Some(name.as_str()) != except)
        .collect()
}

/// The editor for one. `snippet` is `None` for one being added.
fn edit<F>(parent: &impl IsA<gtk::Widget>, snippet: Option<Snippet>, taken: Vec<String>, save: F)
where
    F: Fn(Snippet) + 'static,
{
    let editing = snippet.is_some();
    let snippet = snippet.unwrap_or_default();
    // The trailing newline is the difference between a snippet that runs and one
    // that waits, so it is a switch rather than a character nobody can see.
    let runs = snippet.body.ends_with('\n');

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let name = adw::EntryRow::builder()
        .title("Name")
        .text(&snippet.name)
        .build();
    let run = adw::SwitchRow::builder()
        .title("Run it")
        .subtitle("Off leaves it on the prompt to be finished by hand")
        .active(runs)
        .build();
    group.add(&name);
    group.add(&run);
    page.add(&group);

    let body_group = adw::PreferencesGroup::builder().title("Text").build();
    let body = gtk::TextView::builder()
        .monospace(true)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(6)
        .right_margin(6)
        .build();
    body.buffer()
        .set_text(snippet.body.strip_suffix('\n').unwrap_or(&snippet.body));
    let frame = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(160)
        .child(&body)
        .build();
    frame.add_css_class("card");
    body_group.add(&frame);
    page.add(&body_group);

    let dialog = adw::Dialog::builder()
        .title(if editing {
            "Edit Snippet"
        } else {
            "Add Snippet"
        })
        .content_width(480)
        .content_height(460)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label(if editing { "Save" } else { "Add" });
    confirm.add_css_class("suggested-action");
    let bar = adw::HeaderBar::new();
    bar.set_show_end_title_buttons(false);
    bar.pack_start(&cancel);
    bar.pack_end(&confirm);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&bar);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    // A name another snippet has would replace that one on save, so it is
    // refused here instead, where the name is still being typed.
    let ready = glib::clone!(
        #[weak]
        name,
        #[weak]
        confirm,
        move || {
            let written = name.text().trim().to_owned();
            let clash = taken.contains(&written);
            confirm.set_sensitive(!written.is_empty() && !clash);
            if clash {
                name.add_css_class("error");
            } else {
                name.remove_css_class("error");
            }
        }
    );
    name.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    ready();

    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        name,
        move |_| {
            let buffer = body.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            let mut written = text.trim_end_matches('\n').to_owned();
            if run.is_active() {
                written.push('\n');
            }
            save(Snippet {
                name: name.text().trim().to_owned(),
                body: written,
            });
            dialog.close();
        }
    ));

    dialog.present(Some(parent));
    name.grab_focus();
}

/// A snippet in one line, for the row above the tooltip that holds all of it.
pub fn summary(body: &str) -> String {
    let first = body.lines().next().unwrap_or_default().trim();
    if body.lines().count() > 1 {
        format!("{first} ...")
    } else {
        first.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_of_several_lines_says_so_in_one() {
        assert_eq!(summary("df -h\n"), "df -h");
        assert_eq!(summary("cd /srv\nls -l\n"), "cd /srv ...");
    }
}
