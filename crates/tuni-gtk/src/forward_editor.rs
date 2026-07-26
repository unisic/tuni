//! The dialog one forwarded port is written in.
//!
//! Its own file rather than a corner of [`crate::host_editor`], because a
//! forward has two homes: the host's block, where `ssh` brings it up with the
//! connection, and a connection that is already running, where tuni opens and
//! closes it.
//!
//! Three fields of the five are the same three every time, so the dialog says
//! which way the port points in words. `-L` and `-R` are the shortest way to
//! write it down and the worst way to read it back a month later.

use adw::prelude::*;
use gtk::glib;

use tuni_core::ssh::{Direction, Forward};

/// The three ways a port can point, in the order the rows offer them.
const DIRECTIONS: [(Direction, &str); 3] = [
    (Direction::Local, "To the host"),
    (Direction::Remote, "Back from the host"),
    (Direction::Dynamic, "SOCKS proxy"),
];

/// What each of them does, under the row that picks it.
const ABOUT: [&str; 3] = [
    "A port on this machine, answered by something on the far end",
    "A port on the far end, answered by something on this machine",
    "A port on this machine that anything speaking SOCKS can route through",
];

/// Opens the editor over `parent`. `forward` is `None` for one being added.
pub fn present<F>(parent: &impl IsA<gtk::Widget>, forward: Option<Forward>, save: F)
where
    F: Fn(Forward) + 'static,
{
    let editing = forward.is_some();
    let forward = forward.unwrap_or_default();
    let chosen = DIRECTIONS
        .iter()
        .position(|(direction, _)| *direction == forward.direction)
        .unwrap_or(0);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();

    let names = gtk::StringList::new(&[]);
    for (_, name) in DIRECTIONS {
        names.append(name);
    }
    let kind = adw::ComboRow::builder()
        .title("Direction")
        .subtitle(ABOUT[chosen])
        .model(&names)
        .selected(chosen as u32)
        .build();

    let bind = adw::EntryRow::builder()
        .title("Listen on")
        .text(&forward.bind)
        .build();
    let listen_port = adw::SpinRow::builder()
        .title("Listening port")
        .adjustment(&adjustment(forward.listen_port))
        .build();
    let host = adw::EntryRow::builder()
        .title("To host")
        .text(&forward.host)
        .build();
    let port = adw::SpinRow::builder()
        .title("To port")
        .adjustment(&adjustment(forward.port))
        .build();

    group.add(&kind);
    group.add(&bind);
    group.add(&listen_port);
    group.add(&host);
    group.add(&port);
    page.add(&group);

    let dialog = adw::Dialog::builder()
        .title(if editing {
            "Edit Forward"
        } else {
            "Add Forward"
        })
        .content_width(440)
        .content_height(420)
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

    // What the rows mean moves with the direction: a dynamic forward has no far
    // end to name, and only a remote one can ask for whichever port is free.
    let follow = glib::clone!(
        #[weak]
        kind,
        #[weak]
        bind,
        #[weak]
        listen_port,
        #[weak]
        host,
        #[weak]
        port,
        #[weak]
        confirm,
        move || {
            let direction = direction(&kind);
            kind.set_subtitle(ABOUT[kind.selected() as usize % ABOUT.len()]);
            bind.set_title(match direction {
                Direction::Remote => "Listen on, at the far end",
                _ => "Listen on",
            });
            listen_port.set_subtitle(match direction {
                Direction::Remote => "Zero asks the host for whichever port is free",
                _ => "",
            });
            let targeted = direction != Direction::Dynamic;
            host.set_visible(targeted);
            port.set_visible(targeted);

            let enough = (listen_port.value() as u16 != 0 || direction == Direction::Remote)
                && (!targeted || (!host.text().trim().is_empty() && port.value() as u16 != 0));
            confirm.set_sensitive(enough);
        }
    );
    kind.connect_selected_notify(glib::clone!(
        #[strong]
        follow,
        move |_| follow()
    ));
    for row in [&listen_port, &port] {
        row.connect_value_notify(glib::clone!(
            #[strong]
            follow,
            move |_| follow()
        ));
    }
    host.connect_changed(glib::clone!(
        #[strong]
        follow,
        move |_| follow()
    ));
    follow();

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let direction = direction(&kind);
            let targeted = direction != Direction::Dynamic;
            save(Forward {
                direction,
                bind: bind.text().trim().to_owned(),
                listen_port: listen_port.value() as u16,
                host: if targeted {
                    host.text().trim().to_owned()
                } else {
                    String::new()
                },
                port: if targeted { port.value() as u16 } else { 0 },
                label: forward.label.clone(),
            });
            dialog.close();
        }
    ));

    dialog.present(Some(parent));
}

/// How a row says a forward, in the two lines a list row has: what it does, and
/// the line a configuration file would hold it on.
#[must_use]
pub fn describe(forward: &Forward) -> (String, String) {
    let here = if forward.bind.is_empty() {
        forward.listen_port.to_string()
    } else {
        format!("{}:{}", forward.bind, forward.listen_port)
    };
    let title = match forward.direction {
        Direction::Local => format!("{here} here, answered by {}:{}", forward.host, forward.port),
        Direction::Remote if forward.listen_port == 0 => {
            format!(
                "A port on the host, answered by {}:{}",
                forward.host, forward.port
            )
        }
        Direction::Remote => format!(
            "{here} on the host, answered by {}:{}",
            forward.host, forward.port
        ),
        Direction::Dynamic => format!("A SOCKS proxy on {here}"),
    };
    let written = format!(
        "{} {} {}",
        forward.direction.keyword(),
        forward.listen(),
        forward.target()
    );
    (title, written.trim_end().to_owned())
}

fn adjustment(value: u16) -> gtk::Adjustment {
    gtk::Adjustment::new(f64::from(value), 0.0, f64::from(u16::MAX), 1.0, 10.0, 0.0)
}

fn direction(kind: &adw::ComboRow) -> Direction {
    DIRECTIONS
        .get(kind.selected() as usize)
        .map_or(Direction::Local, |(direction, _)| *direction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_is_read_out_in_the_direction_it_points() {
        let local = Forward::parse(Direction::Local, "8080:localhost:80").expect("local");
        assert_eq!(
            describe(&local).0,
            "8080 here, answered by localhost:80".to_owned()
        );

        let dynamic = Forward::parse(Direction::Dynamic, "1080").expect("dynamic");
        assert_eq!(describe(&dynamic).0, "A SOCKS proxy on 1080".to_owned());

        let allocated = Forward::parse(Direction::Remote, "0:localhost:22").expect("remote");
        assert_eq!(
            describe(&allocated).0,
            "A port on the host, answered by localhost:22".to_owned()
        );
    }
}
