//! A host's files beside this machine's, in a pane: the two ends of a copy.
//!
//! Two widgets that already exist, side by side, and the one thing neither
//! does alone, which is take a drag from the other. The left is the Files
//! page's tree on a directory here, the right is the Remote page's list on one
//! host, and that host is the pane's for good rather than whichever the focus
//! is on: a pane of its own is a pane somebody opened for that machine.
//!
//! Local rows drag out as files, which the remote list already took from a
//! file manager. Remote rows drag out as [`Dragged`], which only this process
//! reads, and dropped on the left they are fetched into the directory the tree
//! is standing in.

use std::cell::{OnceCell, RefCell};
use std::path::Path;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::files::TuniFiles;
use crate::sftp::{Dragged, TuniSftp};

/// How often the local side is read again while it is on screen. The same
/// two seconds the panel and a diff poll at, so a file a transfer just
/// fetched shows up no later than one the shell beside it wrote.
const POLL_SECONDS: u32 = 2;

mod imp {
    use super::{OnceCell, RefCell, TuniFiles, TuniSftp, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniBrowser {
        pub files: OnceCell<TuniFiles>,
        pub sftp: OnceCell<TuniSftp>,
        /// Armed while the pane is mapped. An unmapped one has nobody to be
        /// fresh for, and its timer would keep reading a disk for nothing.
        pub timer: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniBrowser {
        const NAME: &'static str = "TuniBrowser";
        type Type = super::TuniBrowser;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniBrowser {
        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniBrowser");
        }

        fn dispose(&self) {
            if let Some(timer) = self.timer.take() {
                timer.remove();
            }
        }
    }

    impl Drop for TuniBrowser {
        fn drop(&mut self) {
            crate::debug::died("TuniBrowser");
        }
    }

    impl WidgetImpl for TuniBrowser {}
    impl BinImpl for TuniBrowser {}
}

glib::wrapper! {
    pub struct TuniBrowser(ObjectSubclass<imp::TuniBrowser>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TuniBrowser {
    /// `dial` false is a window putting the pane back, which reads the host
    /// only over a connection already up. See [`TuniSftp::open_host`].
    #[must_use]
    pub fn new(alias: &str, directory: &Path, dial: bool) -> Self {
        let browser: Self = glib::Object::new();
        let files = TuniFiles::new();
        files.sync(directory);
        let sftp = TuniSftp::new();
        sftp.open_host(alias, dial);

        let drop = gtk::DropTarget::new(Dragged::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(glib::clone!(
            #[weak]
            files,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                let Ok(dragged) = value.get::<Dragged>() else {
                    return false;
                };
                let Some(source) = dragged.source.upgrade() else {
                    return false;
                };
                source.fetch(dragged.items, &files.directory());
                true
            }
        ));
        files.add_controller(drop);

        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&files)
            .end_child(&sftp)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .wide_handle(true)
            .build();
        // Halves until somebody moves the handle. A paned left to itself
        // gives the start child what it asks for, and a tree asks for little.
        paned.connect_max_position_notify(|paned| {
            if !paned.is_position_set() {
                paned.set_position(paned.max_position() / 2);
            }
        });
        browser.set_child(Some(&paned));

        browser.connect_map(|this| {
            this.files().poll();
            let mut timer = this.imp().timer.borrow_mut();
            if timer.is_none() {
                *timer = Some(glib::timeout_add_seconds_local(
                    POLL_SECONDS,
                    glib::clone!(
                        #[weak]
                        this,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || {
                            let active = this
                                .root()
                                .and_downcast::<gtk::Window>()
                                .is_none_or(|window| window.is_active());
                            if active {
                                this.files().poll();
                            }
                            glib::ControlFlow::Continue
                        }
                    ),
                ));
            }
        });
        browser.connect_unmap(|this| {
            if let Some(timer) = this.imp().timer.borrow_mut().take() {
                timer.remove();
            }
        });

        let _ = browser.imp().files.set(files);
        let _ = browser.imp().sftp.set(sftp);
        browser
    }

    pub fn files(&self) -> &TuniFiles {
        self.imp().files.get().expect("set in new")
    }

    pub fn sftp(&self) -> &TuniSftp {
        self.imp().sftp.get().expect("set in new")
    }
}
