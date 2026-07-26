//! An ssh pane: the terminal a connection runs in, and a bar above it for the
//! times there is no connection.
//!
//! A shell that exits closes its pane, because that is what typing `exit` asks
//! for. A connection that ends does not. What is left on the screen is a
//! refused key, an unknown host key, a name that did not resolve, and that
//! text is the entire answer to why it did not work, so the pane stays and
//! this bar appears over it with the one button worth offering.
//!
//! The same bar is what a restored pane wears before it has dialled anything.
//! Restoring a window must not start a login it might have to ask about, so a
//! connection that cannot be resumed silently comes back as an offer.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::terminal::TuniTerminal;

/// What to do when the bar's button is pressed.
pub type Handler = Rc<dyn Fn()>;

mod imp {
    use super::{Handler, RefCell, TuniTerminal, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniRemote {
        pub banner: RefCell<Option<adw::Banner>>,
        pub terminal: RefCell<Option<TuniTerminal>>,
        pub handler: RefCell<Option<Handler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniRemote {
        const NAME: &'static str = "TuniRemote";
        type Type = super::TuniRemote;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniRemote {}
    impl WidgetImpl for TuniRemote {}
    impl BinImpl for TuniRemote {}
}

glib::wrapper! {
    pub struct TuniRemote(ObjectSubclass<imp::TuniRemote>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TuniRemote {
    /// Wraps a terminal that has not been started yet. The bar is down: the
    /// caller is about to either connect or say why it is not going to.
    #[must_use]
    pub fn new(terminal: &TuniTerminal) -> Self {
        let remote: Self = glib::Object::new();
        let imp = remote.imp();

        let banner = adw::Banner::new("");
        banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = this)]
            remote,
            move |_| {
                let handler = this.imp().handler.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            }
        ));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(terminal);
        remote.set_child(Some(&content));

        imp.banner.replace(Some(banner));
        imp.terminal.replace(Some(terminal.clone()));
        remote
    }

    /// What the bar's button asks for. Set once, when the pane is built.
    pub fn connect_open(&self, handler: impl Fn() + 'static) {
        self.imp().handler.replace(Some(Rc::new(handler)));
    }

    /// Puts the bar away: something is running down there.
    pub fn set_running(&self) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_revealed(false);
        }
    }

    /// Raises the bar, reading `message`, over a button reading `action`.
    pub fn set_idle(&self, message: &str, action: &str) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_title(message);
            banner.set_button_label(Some(action));
            banner.set_revealed(true);
        }
    }
}
