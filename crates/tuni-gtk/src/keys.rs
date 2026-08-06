//! The dialog keys are looked at in.
//!
//! Every command that can ask a question, which is making a key, putting one on
//! a host, and handing one to the agent, is typed into a terminal pane instead
//! of run from here: each of them wants a passphrase or a password sooner or
//! later and this window has nowhere to put the answer. `ssh-agent` and the
//! desktop's keyring hold the secrets. Tuni holds the list.
//!
//! The one thing written from here is a key pasted in from somewhere else,
//! because there is no command to type for that: the key is already in a
//! clipboard rather than on a disk. It goes into `~/.ssh` and nowhere tuni
//! owns, so it ends up in the same place a key made in a pane would, and every
//! ssh tool on the machine finds it there.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use tuni_core::ssh::{self, Agent, Key};

/// What the list holds while it is being read, and what the actions look a key
/// up in afterwards.
type Held = Rc<RefCell<Vec<Key>>>;

/// A command on its way to a pane. Nothing here runs one.
type Run = Rc<dyn Fn(Vec<String>)>;

/// Opens the list over `parent`. `run` is handed the argv of a command that has
/// to be typed somewhere a person can answer it.
pub fn present(parent: &impl IsA<gtk::Widget>, run: impl Fn(Vec<String>) + 'static) {
    let keys: Held = Rc::new(RefCell::new(Vec::new()));
    let aliases: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .description("Reading ~/.ssh")
        .build();
    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    rows.add_css_class("boxed-list");
    group.add(&rows);
    page.add(&group);

    let dialog = adw::Dialog::builder()
        .title("Keys")
        .content_width(640)
        .content_height(460)
        .build();

    let menu = gio::Menu::new();
    menu.append(Some("Make a Key"), Some("keys.generate"));
    menu.append(Some("Paste a Key"), Some("keys.import"));
    let add = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add a key")
        .menu_model(&menu)
        .build();
    add.add_css_class("flat");
    let bar = adw::HeaderBar::new();
    bar.pack_end(&add);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&bar);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));

    // A command going out means the pane behind this is where the answer is
    // typed, so the dialog gets out of the way rather than sitting over it.
    let run: Run = Rc::new(run);
    let fire: Run = Rc::new(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        run,
        move |argv| {
            run(argv);
            dialog.close();
        }
    ));
    // Reading the list again is what an import ends with, so the action group
    // is handed the same closure that fills it the first time.
    let reload: Rc<dyn Fn()> = Rc::new(glib::clone!(
        #[weak]
        group,
        #[weak]
        rows,
        #[strong]
        keys,
        #[strong]
        aliases,
        move || load(&group, &rows, &keys, &aliases)
    ));
    dialog.insert_action_group(
        "keys",
        Some(&actions(&dialog, &keys, &aliases, &fire, &reload)),
    );
    dialog.present(Some(parent));
    reload();
}

/// Reads `~/.ssh` and the agent, and draws what they say.
///
/// One subprocess per key and one for the agent, so the list arrives rather
/// than being there.
fn load(
    group: &adw::PreferencesGroup,
    rows: &gtk::ListBox,
    keys: &Held,
    aliases: &Rc<RefCell<Vec<String>>>,
) {
    glib::spawn_future_local(glib::clone!(
        #[weak]
        group,
        #[weak]
        rows,
        #[strong]
        keys,
        #[strong]
        aliases,
        async move {
            let Ok((scanned, agent, hosts)) = gio::spawn_blocking(|| {
                let keys = ssh::keys();
                let agent = ssh::agent();
                let hosts = ssh::Hosts::load()
                    .all()
                    .iter()
                    .map(|host| host.alias.clone())
                    .collect::<Vec<_>>();
                (keys, agent, hosts)
            })
            .await
            else {
                return;
            };
            group.set_description(Some(&describe(&agent)));
            *keys.borrow_mut() = scanned;
            *aliases.borrow_mut() = hosts;
            draw(&rows, &keys, &agent);
        }
    ));
}

/// What the agent has to say about itself, under the list the keys are in.
fn describe(agent: &Agent) -> String {
    match agent {
        Agent::Missing => {
            "No agent is running, so a key with a passphrase asks for it every time".to_owned()
        }
        Agent::Empty => "The agent is running and holding nothing".to_owned(),
        Agent::Holding(fingerprints) => match fingerprints.len() {
            1 => "The agent is holding one key".to_owned(),
            held => format!("The agent is holding {held} keys"),
        },
    }
}

fn draw(rows: &gtk::ListBox, keys: &Held, agent: &Agent) {
    while let Some(row) = rows.first_child() {
        rows.remove(&row);
    }
    if keys.borrow().is_empty() {
        let empty = adw::ActionRow::builder().title("No keys in ~/.ssh").build();
        empty.set_sensitive(false);
        rows.append(&empty);
        return;
    }

    for key in keys.borrow().iter() {
        let path = key.path.to_string_lossy().into_owned();
        let row = adw::ActionRow::builder()
            .title(key.title())
            .subtitle(format!("{} {}  {}", key.kind, key.bits, key.fingerprint))
            .subtitle_lines(1)
            .build();
        row.set_tooltip_text(Some(&path));

        if key.loaded {
            let held = gtk::Label::new(Some("in the agent"));
            held.add_css_class("caption");
            held.add_css_class("dim-label");
            row.add_suffix(&held);
        }

        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy the public key")
            .valign(gtk::Align::Center)
            .action_name("keys.copy")
            .action_target(&path.to_variant())
            .build();
        copy.add_css_class("flat");
        row.add_suffix(&copy);

        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("What can be done with this key")
            .valign(gtk::Align::Center)
            .menu_model(&menu(key, agent))
            .build();
        more.add_css_class("flat");
        row.add_suffix(&more);

        rows.append(&row);
    }
}

/// The menu for one key. `ssh-add` needs an agent to hand the key to, and a key
/// already in one has nothing to gain from being added twice.
fn menu(key: &Key, agent: &Agent) -> gio::Menu {
    let path = key.path.to_string_lossy().into_owned().to_variant();
    let menu = gio::Menu::new();

    let item = gio::MenuItem::new(Some("Copy to a Host"), None);
    item.set_action_and_target_value(Some("keys.copy-to-host"), Some(&path));
    menu.append_item(&item);

    if !key.loaded && *agent != Agent::Missing {
        let item = gio::MenuItem::new(Some("Add to the Agent"), None);
        item.set_action_and_target_value(Some("keys.add-to-agent"), Some(&path));
        menu.append_item(&item);
    }

    let item = gio::MenuItem::new(Some("Show in Files"), None);
    item.set_action_and_target_value(Some("keys.show"), Some(&path));
    menu.append_item(&item);
    menu
}

fn actions(
    dialog: &adw::Dialog,
    keys: &Held,
    aliases: &Rc<RefCell<Vec<String>>>,
    run: &Run,
    reload: &Rc<dyn Fn()>,
) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let string = Some(glib::VariantTy::STRING);

    let copy = gio::SimpleAction::new("copy", string);
    copy.connect_activate(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        keys,
        move |_, target| {
            let Some(key) = found(&keys, target) else {
                return;
            };
            dialog.clipboard().set_text(&key.public);
        }
    ));

    let add = gio::SimpleAction::new("add-to-agent", string);
    add.connect_activate(glib::clone!(
        #[strong]
        keys,
        #[strong]
        run,
        move |_, target| {
            let Some(key) = found(&keys, target) else {
                return;
            };
            run(vec![
                "ssh-add".to_owned(),
                key.private().to_string_lossy().into_owned(),
            ]);
        }
    ));

    let copy_to_host = gio::SimpleAction::new("copy-to-host", string);
    copy_to_host.connect_activate(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        keys,
        #[strong]
        aliases,
        #[strong]
        run,
        move |_, target| {
            let Some(key) = found(&keys, target) else {
                return;
            };
            let path = key.path.to_string_lossy().into_owned();
            pick_host(
                &dialog,
                &aliases.borrow(),
                glib::clone!(
                    #[strong]
                    run,
                    move |alias: String| {
                        run(vec![
                            "ssh-copy-id".to_owned(),
                            "-i".to_owned(),
                            path.clone(),
                            alias,
                        ]);
                    }
                ),
            );
        }
    ));

    let show = gio::SimpleAction::new("show", string);
    show.connect_activate(glib::clone!(
        #[weak]
        dialog,
        move |_, target| {
            let Some(path) = target.and_then(glib::Variant::str) else {
                return;
            };
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
            let window = dialog.root().and_downcast::<gtk::Window>();
            launcher.open_containing_folder(window.as_ref(), gio::Cancellable::NONE, |_| ());
        }
    ));

    let generate = gio::SimpleAction::new("generate", None);
    generate.connect_activate(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        run,
        move |_, _| generate_key(&dialog, &run)
    ));

    let import = gio::SimpleAction::new("import", None);
    import.connect_activate(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        reload,
        move |_, _| import_key(&dialog, &reload)
    ));

    for action in [copy, add, copy_to_host, show, generate, import] {
        group.add_action(&action);
    }
    group
}

/// The key an action was fired against, by the path its menu item carries.
fn found(keys: &Held, target: Option<&glib::Variant>) -> Option<Key> {
    let path = target.and_then(glib::Variant::str)?;
    keys.borrow()
        .iter()
        .find(|key| key.path.as_os_str() == path)
        .cloned()
}

/// Which host a key is being put on. A list rather than a text field: the point
/// of `ssh-copy-id` is the machine, and the machines are already known.
fn pick_host(parent: &adw::Dialog, aliases: &[String], chosen: impl Fn(String) + 'static) {
    if aliases.is_empty() {
        let empty = adw::AlertDialog::new(
            Some("No hosts to copy to"),
            Some("Nothing in ~/.ssh/config or tuni's own list names a host yet."),
        );
        empty.add_response("close", "Close");
        empty.present(Some(parent));
        return;
    }

    let names: Vec<&str> = aliases.iter().map(String::as_str).collect();
    let host = adw::ComboRow::builder()
        .title("Host")
        .subtitle("Where the public key is added to authorized_keys")
        .model(&gtk::StringList::new(&names))
        .build();
    let group = adw::PreferencesGroup::new();
    group.add(&host);
    let page = adw::PreferencesPage::new();
    page.add(&group);

    let dialog = adw::Dialog::builder()
        .title("Copy to a Host")
        .follows_content_size(true)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Copy");
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
    let owned = aliases.to_vec();
    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let Some(alias) = owned.get(host.selected() as usize) else {
                return;
            };
            chosen(alias.clone());
            dialog.close();
        }
    ));
    dialog.present(Some(parent));
}

/// A key that already exists somewhere else, on its way into `~/.ssh`.
///
/// The private half is typed or pasted here and nowhere else: it goes straight
/// to [`ssh::import`], which writes it 0600 and hands back the file. Tuni keeps
/// no copy, and the buffer it came through is emptied on the way out, so a
/// dialog left open behind another one is not a private key sitting in a widget.
///
/// The public half is asked for rather than required. A key without a
/// passphrase gives its own up to `ssh-keygen -y`; a key with one cannot, and
/// this window has nowhere to ask for a passphrase.
fn import_key(parent: &adw::Dialog, reload: &Rc<dyn Fn()>) {
    let name = adw::EntryRow::builder().title("File name").build();
    let named = adw::PreferencesGroup::builder()
        .description("Written into ~/.ssh, where every ssh tool looks for a key")
        .build();
    named.add(&name);

    let private = gtk::TextBuffer::new(None);
    let public = gtk::TextBuffer::new(None);
    let page = adw::PreferencesPage::new();
    page.add(&named);
    page.add(&area(
        "Private key",
        "The whole file, BEGIN and END lines and all",
        &private,
    ));
    page.add(&area(
        "Public key",
        "Worked out from the private half when it has no passphrase",
        &public,
    ));

    let dialog = adw::Dialog::builder()
        .title("Paste a Key")
        .content_width(560)
        .content_height(560)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Import");
    confirm.add_css_class("suggested-action");
    let bar = adw::HeaderBar::new();
    bar.set_show_end_title_buttons(false);
    bar.pack_start(&cancel);
    bar.pack_end(&confirm);
    let banner = adw::Banner::new("");
    let view = adw::ToolbarView::new();
    view.add_top_bar(&bar);
    view.add_top_bar(&banner);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    let ready = glib::clone!(
        #[weak]
        name,
        #[weak]
        confirm,
        #[strong]
        private,
        move || {
            let written = name.text().trim().to_owned();
            confirm.set_sensitive(!written.is_empty() && !text(&private).trim().is_empty());
        }
    );
    name.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    private.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    ready();

    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        banner,
        #[weak]
        name,
        #[strong]
        private,
        #[strong]
        public,
        #[strong]
        reload,
        move |confirm| {
            let written = name.text().trim().to_owned();
            let secret = text(&private);
            let published = text(&public);
            let (private, public) = (private.clone(), public.clone());
            let reload = reload.clone();
            let confirm = confirm.clone();
            // Two files and a couple of `ssh-keygen` runs, and a paste of a
            // thousand lines is still a paste, so it goes off the main thread.
            confirm.set_sensitive(false);
            glib::spawn_future_local(async move {
                let done =
                    gio::spawn_blocking(move || ssh::import(&written, &secret, &published)).await;
                let complaint = match done {
                    Ok(Ok(_)) => {
                        // Emptied rather than left to the dialog being dropped:
                        // what a widget holds is what a screen reader, a
                        // clipboard manager or the next paste can reach.
                        private.set_text("");
                        public.set_text("");
                        reload();
                        dialog.close();
                        return;
                    }
                    Ok(Err(reason)) => reason,
                    Err(_) => "The write did not finish".to_owned(),
                };
                banner.set_title(&complaint);
                banner.set_revealed(true);
                confirm.set_sensitive(true);
            });
        }
    ));

    dialog.present(Some(parent));
    name.grab_focus();
}

fn text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

/// One titled box a key is pasted into.
fn area(title: &str, description: &str, buffer: &gtk::TextBuffer) -> adw::PreferencesGroup {
    let view = gtk::TextView::builder()
        .buffer(buffer)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::Char)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .height_request(120)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    // The border rather than the card background: a card of the same colour as
    // the page is a text area nobody can see the edges of.
    scroller.add_css_class("frame");
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build();
    group.add(&scroller);
    group
}

/// The one key tuni offers to make. Ed25519 because there is no reason to
/// choose anything else in 2026, and the choice is the first thing a page of
/// options would ask about and the last thing that matters.
fn generate_key(parent: &adw::Dialog, run: &Run) {
    let directory = glib::home_dir().join(".ssh");
    let name = adw::EntryRow::builder()
        .title("File name")
        .text("id_ed25519")
        .build();
    let comment = adw::EntryRow::builder()
        .title("Comment")
        .text(format!(
            "{}@{}",
            glib::user_name().to_string_lossy(),
            glib::host_name()
        ))
        .build();
    let group = adw::PreferencesGroup::builder()
        .description("Written into ~/.ssh. The passphrase is asked for in the pane.")
        .build();
    group.add(&name);
    group.add(&comment);
    let page = adw::PreferencesPage::new();
    page.add(&group);

    let dialog = adw::Dialog::builder()
        .title("Make a Key")
        .content_width(420)
        .follows_content_size(true)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Make");
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

    // A name that is already a key would be overwritten, and overwriting a
    // private key is losing it. `ssh-keygen` does ask, but by then the command
    // is halfway typed and the question is easy to answer wrong.
    let ready = glib::clone!(
        #[weak]
        name,
        #[weak]
        confirm,
        #[strong]
        directory,
        move || {
            let written = name.text().trim().to_owned();
            let bad =
                written.is_empty() || written.contains('/') || directory.join(&written).exists();
            confirm.set_sensitive(!bad);
            if bad && !written.is_empty() {
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
        run,
        #[strong]
        name,
        #[strong]
        comment,
        move |_| {
            let path = directory.join(name.text().trim());
            run(vec![
                "ssh-keygen".to_owned(),
                "-t".to_owned(),
                "ed25519".to_owned(),
                "-f".to_owned(),
                path.to_string_lossy().into_owned(),
                "-C".to_owned(),
                comment.text().trim().to_owned(),
            ]);
            dialog.close();
        }
    ));

    dialog.present(Some(parent));
    name.grab_focus();
}
