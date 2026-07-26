//! The window: a sidebar of projects, a strip of tabs, and the panes inside
//! them.
//!
//! The model of record for projects, tabs and panes lives in `tuni_core`; the
//! model of record for tab *order and selection* is the `AdwTabView`, which
//! already implements the rules kero wrote by hand — a new tab lands next to
//! the selected one, closing one falls to its neighbor, a drag reorders the
//! strip. Every change the view makes is reported back into the model, so the
//! project name, the directory a new shell starts in, and the session file to
//! come all read from one place. Nothing pushes the other way, which is why
//! there is no guard flag here and no way for the two to drift.
//!
//! Panes are the other way around, because no widget implements a niri layout:
//! the model decides, and [`TuniGrid`] renders whatever it says.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::panes::{Content, Edge, Layout, Pane};
use tuni_core::session::{History, PaneState, Snapshot};
use tuni_core::settings::{Appearance, HISTORY_LINE_LIMIT, NewTab, Settings};
use tuni_core::theme::{Rgb, Theme};
use tuni_core::workspace::{Id, Tab, Workspace};

use crate::diff::TuniDiff;
use crate::editor::TuniEditor;
use crate::find::TuniFind;
use crate::grid::{Message, TuniGrid};
use crate::hosts::TuniHosts;
use crate::notify;
use crate::palette;
use crate::panel::TuniPanel;
use crate::preferences;
use crate::remote::TuniRemote;
use crate::switcher::{Card, TuniSwitcher};
use crate::terminal::{Launch, TuniTerminal};

/// Sidebar width, and the range a narrow or a wide window may take it to. The
/// maximum is kero's own default width rather than a fraction of a wide screen:
/// a list of project names does not read better for being 400px of it, and a
/// split view sizes by fraction where kero's sidebar is dragged to a width and
/// remembers it.
const SIDEBAR_FRACTION: f64 = 0.2;
const SIDEBAR_MIN: i32 = 160;
const SIDEBAR_MAX: i32 = 220;

/// The Files and Git panel, on the other side. kero's default is 240; the
/// minimum is what its three pages measure at their narrowest.
const PANEL_FRACTION: f64 = 0.22;
const PANEL_MIN: i32 = 200;
const PANEL_MAX: i32 = 240;

/// Where a drag may take each of them instead. Wider on both ends than the
/// defaults above, which are what a window nobody has dragged should open at:
/// once someone has taken hold of the edge they have said what they want the
/// width to be, and the only widths worth refusing are the ones that leave the
/// sidebar unreadable or the work beside it too narrow to use.
const SIDEBAR_DRAG_MIN: f64 = 140.0;
const SIDEBAR_DRAG_MAX: f64 = 480.0;
const PANEL_DRAG_MIN: f64 = 180.0;
const PANEL_DRAG_MAX: f64 = 620.0;

/// How much of a sidebar's inner edge takes the pointer. Narrow enough to read
/// as an edge rather than a strip, wide enough to hit without aiming.
const GRIP: i32 = 5;

/// How often the panel re-reads the directories and the repository it is
/// showing. kero's own interval, and for the same reason: a watch on every open
/// directory is a descriptor per directory and a debounce to write, where a
/// read of what is already in the page cache costs nothing anyone can measure.
const PANEL_POLL_SECONDS: u32 = 2;

/// What a Find command found to act on. The bar over a terminal belongs to the
/// window and is handed the terminal it is pointing at; a file pane carries its
/// own search inside GtkSourceView.
enum Search {
    Terminal(TuniFind, TuniTerminal),
    Editor(TuniEditor),
}

/// What the palette lists before the projects and terminals: the window's own
/// actions, by the name a person would search for, with the keys that do the
/// same thing so the palette teaches them. The shortcuts repeat `ACCELS` in
/// main.rs, spelled the way a keycap is rather than the way GTK parses one.
const COMMANDS: &[(&str, &str, Option<&str>, &str)] = &[
    (
        "New Tab",
        "tab-new-symbolic",
        Some("Ctrl+Shift+T"),
        "win.new-tab",
    ),
    (
        "New Connection",
        "network-server-symbolic",
        Some("Ctrl+Shift+O"),
        "win.new-connection",
    ),
    (
        "New Project",
        "folder-new-symbolic",
        Some("Ctrl+Shift+N"),
        "win.new-project",
    ),
    ("New Window", "window-new-symbolic", None, "win.new-window"),
    (
        "Split Right",
        "view-right-pane-symbolic",
        Some("Ctrl+Shift+D"),
        "win.split-right",
    ),
    (
        "Split Down",
        "view-bottom-pane-symbolic",
        Some("Ctrl+Shift+E"),
        "win.split-down",
    ),
    (
        "Close Pane",
        "window-close-symbolic",
        Some("Ctrl+Shift+W"),
        "win.close-pane",
    ),
    (
        "Zoom Pane",
        "view-fullscreen-symbolic",
        Some("Ctrl+Shift+Enter"),
        "win.zoom-pane",
    ),
    (
        "Equalize Panes",
        "view-grid-symbolic",
        Some("Ctrl+Alt+="),
        "win.equalize-panes",
    ),
    (
        "Next Tab",
        "go-next-symbolic",
        Some("Ctrl+Page Down"),
        "win.next-tab",
    ),
    (
        "Previous Tab",
        "go-previous-symbolic",
        Some("Ctrl+Page Up"),
        "win.previous-tab",
    ),
    (
        "Rename Tab",
        "document-edit-symbolic",
        None,
        "win.tab-rename",
    ),
    ("Close Tab", "window-close-symbolic", None, "win.tab-close"),
    (
        "Find",
        "edit-find-symbolic",
        Some("Ctrl+Shift+F"),
        "win.find",
    ),
    ("Find Next", "go-down-symbolic", Some("F3"), "win.find-next"),
    (
        "Find Previous",
        "go-up-symbolic",
        Some("Shift+F3"),
        "win.find-previous",
    ),
    (
        "Find and Replace",
        "edit-find-replace-symbolic",
        Some("Ctrl+Shift+H"),
        "win.find-replace",
    ),
    (
        "Use Selection for Find",
        "edit-select-all-symbolic",
        None,
        "win.use-selection-for-find",
    ),
    (
        "Clear Terminal",
        "edit-clear-all-symbolic",
        Some("Ctrl+Shift+K"),
        "win.clear-terminal",
    ),
    (
        "Save File",
        "document-save-symbolic",
        Some("Ctrl+S"),
        "win.save-file",
    ),
    (
        "Toggle Sidebar",
        "sidebar-show-symbolic",
        Some("F9"),
        "win.toggle-sidebar",
    ),
    (
        "Toggle Panel",
        "sidebar-show-right-symbolic",
        Some("Ctrl+Shift+B"),
        "win.toggle-panel",
    ),
    ("Show Files", "folder-symbolic", None, "win.show-files"),
    (
        "Show Git",
        "media-record-symbolic",
        Some("Ctrl+Shift+G"),
        "win.show-git",
    ),
    (
        "Show Info",
        "dialog-information-symbolic",
        Some("Ctrl+Shift+I"),
        "win.show-info",
    ),
    (
        "Toggle Mouse Reporting",
        "input-mouse-symbolic",
        Some("Ctrl+Shift+M"),
        "win.toggle-mouse-reporting",
    ),
    (
        "Preferences",
        "preferences-system-symbolic",
        Some("Ctrl+,"),
        "win.settings",
    ),
    ("About Tuni", "help-about-symbolic", None, "win.about"),
];

mod imp {
    use super::{
        Cell, HashMap, Id, RefCell, Settings, TuniDiff, TuniEditor, TuniFind, TuniGrid, TuniHosts,
        TuniPanel, TuniRemote, TuniSwitcher, TuniTerminal, Workspace, glib,
    };
    use adw::subclass::prelude::*;
    use gtk::prelude::WidgetExt;

    #[derive(Default)]
    pub struct TuniWindow {
        pub workspace: RefCell<Workspace>,
        /// One terminal per pane, by pane id.
        pub terminals: RefCell<HashMap<Id, TuniTerminal>>,
        /// One editor per pane holding a file, by pane id. A pane is in one map
        /// or the other, never in both.
        pub editors: RefCell<HashMap<Id, TuniEditor>>,
        /// One diff per pane showing what changed in a file, by pane id.
        pub diffs: RefCell<HashMap<Id, TuniDiff>>,
        /// The bar an ssh pane wears above its terminal, by pane id. The
        /// terminal underneath is in `terminals` like any other, since it is
        /// one: only what the grid draws differs.
        pub remotes: RefCell<HashMap<Id, TuniRemote>>,
        /// One host list per pane showing one, by pane id.
        pub hosts: RefCell<HashMap<Id, TuniHosts>>,
        /// One layout of panes per tab, by tab id.
        pub grids: RefCell<HashMap<Id, TuniGrid>>,
        /// The strip entry each tab was given, by tab id.
        pub pages: RefCell<HashMap<Id, adw::TabPage>>,
        /// One tab strip per project, by project id.
        pub views: RefCell<HashMap<Id, adw::TabView>>,
        pub settings: RefCell<Settings>,
        /// Scrollback waiting for the shell it belongs to, by pane id. Emptied
        /// one pane at a time as the restored shells start.
        pub pending_history: RefCell<HashMap<Id, String>>,
        /// Where the cursor was in each restored file pane, by pane id, until
        /// its editor exists to be told.
        pub pending_cursors: RefCell<HashMap<Id, usize>>,
        /// Set once the window has been allowed to close, so the unsaved-work
        /// question is asked once rather than every time the answer is acted
        /// on.
        pub closing: Cell<bool>,
        /// Whether this window is the one the saved session belongs to. The
        /// window the application opens with is; a second window opened beside
        /// it is not, so closing either one cannot overwrite the other's work
        /// with its own.
        pub session_owner: Cell<bool>,

        pub split: RefCell<Option<adw::OverlaySplitView>>,
        pub sidebar: RefCell<Option<gtk::ListBox>>,
        /// The Files and Git panel, and the split it lives in, on the other
        /// side.
        pub panel: RefCell<Option<adw::OverlaySplitView>>,
        pub panel_view: RefCell<Option<TuniPanel>>,
        /// Project name labels, in sidebar order.
        pub labels: RefCell<Vec<gtk::Label>>,
        /// Shared context menu for the sidebar rows, parented once.
        pub row_menu: RefCell<Option<gtk::PopoverMenu>>,
        /// Stack of tab strips, one page per project.
        pub stack: RefCell<Option<gtk::Stack>>,
        /// Tab strips, or the empty state when there is nothing to show.
        pub content: RefCell<Option<gtk::Stack>>,
        /// The find bar floating over them.
        pub find: RefCell<Option<TuniFind>>,
        /// The tab switcher, floating over them too.
        pub switcher: RefCell<Option<TuniSwitcher>>,
        /// Tabs in the order they were last worked in, most recent first, which
        /// is the order the switcher walks. Every project's tabs are in the one
        /// list; the switcher takes the ones belonging to the project in front.
        pub recent: RefCell<Vec<Id>>,
        pub status: RefCell<Option<adw::StatusPage>>,
        pub status_button: RefCell<Option<gtk::Button>>,
        pub tab_bar: RefCell<Option<adw::TabBar>>,
        pub title: RefCell<Option<adw::WindowTitle>>,

        /// The tab whose context menu is open, if any.
        pub menu_page: RefCell<Option<adw::TabPage>>,
        /// Set while the sidebar's selection is being written from the model,
        /// so the row-selected handler does not answer its own echo.
        pub selecting: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniWindow {
        const NAME: &'static str = "TuniWindow";
        type Type = super::TuniWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for TuniWindow {
        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniWindow");
            let obj = self.obj();
            obj.build_ui();
            obj.install_actions();
            obj.install_switcher();
            obj.watch_appearance();
        }

        fn dispose(&self) {
            // A popover is parented rather than packed, so it has to be taken
            // off its parent by hand or the widget dies with a child left.
            if let Some(menu) = self.row_menu.take() {
                menu.unparent();
            }
        }
    }

    impl Drop for TuniWindow {
        fn drop(&mut self) {
            crate::debug::died("TuniWindow");
        }
    }

    impl WidgetImpl for TuniWindow {}

    impl WindowImpl for TuniWindow {
        fn close_request(&self) -> glib::Propagation {
            // A file with unsaved work in it is the one thing worth stopping a
            // close for; everything else here can be rebuilt from the session.
            if !self.closing.get() && self.obj().ask_about_unsaved() {
                return glib::Propagation::Stop;
            }
            // Written before the shells are hung up: the scrollback is read out
            // of the live terminals, and a terminal that has been shut down has
            // nothing left to read.
            self.obj().save_session();
            // Hang up every shell before the widgets go, so the closing window
            // does not race the reader threads.
            for terminal in self.terminals.borrow().values() {
                terminal.shutdown();
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for TuniWindow {}
    impl AdwApplicationWindowImpl for TuniWindow {}
}

glib::wrapper! {
    pub struct TuniWindow(ObjectSubclass<imp::TuniWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl TuniWindow {
    #[must_use]
    pub fn new(app: &adw::Application, settings: Settings) -> Self {
        let window: Self = glib::Object::builder()
            .property("application", app)
            .property("default-width", 1100)
            .property("default-height", 700)
            .build();
        // The widgets were built by `constructed`, before there were any
        // settings to build them from, so the ones a widget holds a copy of are
        // handed over here rather than read during construction.
        let imp = window.imp();
        if let Some(bar) = imp.tab_bar.borrow().as_ref() {
            bar.set_autohide(settings.auto_hide_tab_bar);
        }
        imp.settings.replace(settings);
        // `constructed` painted the chrome before there were any settings to
        // paint it from, so it is repainted here rather than at the first theme
        // change the desktop happens to send.
        let opacity = imp.settings.borrow().terminal.background_opacity;
        apply_chrome(&window.theme(), opacity);
        // There is no surface to ask about until the window is realized, and
        // the setting was read before there was a window at all. The blurred
        // region is the window's own rectangle, so it is asked for again every
        // time the window is laid out at a new size.
        window.connect_realize(|window| {
            let on = window.imp().settings.borrow().background_blur;
            crate::blur::apply(window, on);
            if let Some(surface) = window.surface() {
                surface.connect_layout(glib::clone!(
                    #[weak]
                    window,
                    move |_, _, _| {
                        let on = window.imp().settings.borrow().background_blur;
                        crate::blur::apply(&window, on);
                    }
                ));
            }
        });
        window.connect_unrealize(crate::blur::forget);
        window
    }

    /// Says this window owns the saved session: it is the one restored at
    /// startup, and the only one that writes the session file back.
    pub fn own_session(&self) {
        self.imp().session_owner.set(true);
    }

    /// Opens another window on the same application, with the settings this one
    /// is running under.
    ///
    /// It starts on an empty project rather than on a copy of this window's:
    /// two windows showing the same shells is not something a PTY can do, and
    /// the second window is for the work that does not fit beside the first.
    /// Only the window that opened with the session saves one, so whichever
    /// closes last cannot overwrite the other.
    fn open_window(&self) {
        let Some(app) = self.application().and_downcast::<adw::Application>() else {
            return;
        };
        let window = Self::new(&app, self.imp().settings.borrow().clone());
        window.present();
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            window,
            move || {
                window.open_project();
            }
        ));
    }

    // --- construction ------------------------------------------------------

    fn build_ui(&self) {
        let imp = self.imp();
        load_css();
        crate::editor::apply_font(&imp.settings.borrow().terminal);
        crate::diff::apply_colors(&self.theme());

        let sidebar = gtk::ListBox::new();
        sidebar.set_selection_mode(gtk::SelectionMode::Single);
        sidebar.add_css_class("navigation-sidebar");
        sidebar.connect_row_selected(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, row| {
                if this.imp().selecting.get() {
                    return;
                }
                if let Some(row) = row {
                    this.select_project_at(row.index() as usize);
                }
            }
        ));

        let row_menu = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        row_menu.set_has_arrow(false);
        row_menu.set_halign(gtk::Align::Start);
        row_menu.set_parent(&sidebar);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&sidebar)
            .build();

        let new_project = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Project (Ctrl+Shift+N)")
            .action_name("win.new-project")
            .build();
        new_project.add_css_class("flat");
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.set_margin_start(6);
        footer.set_margin_end(6);
        footer.set_margin_top(3);
        footer.set_margin_bottom(3);
        footer.append(&new_project);

        let sidebar_header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .title_widget(&adw::WindowTitle::new("Projects", ""))
            .build();
        drop_window_icon(&sidebar_header);
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.add_bottom_bar(&footer);
        sidebar_view.set_content(Some(&scroller));

        // --- content

        let title = adw::WindowTitle::new("Tuni", "");
        let toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Toggle Sidebar (F9)")
            .build();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .title_widget(&title)
            .build();
        drop_window_icon(&header);
        header.pack_start(&toggle);

        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(&main_menu())
            .primary(true)
            .build();
        header.pack_end(&menu);

        let show_files = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Files and Git (Ctrl+Shift+B)")
            .build();
        header.pack_end(&show_files);

        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);

        let status_button = gtk::Button::builder()
            .label("New Terminal")
            .action_name("win.new-tab")
            .halign(gtk::Align::Center)
            .build();
        status_button.add_css_class("pill");
        status_button.add_css_class("suggested-action");
        let status = adw::StatusPage::builder()
            .icon_name("utilities-terminal-symbolic")
            .title("No Tabs Open")
            .child(&status_button)
            .build();

        let content_stack = gtk::Stack::new();
        content_stack.add_named(&stack, Some("tabs"));
        content_stack.add_named(&status, Some("empty"));

        // The find bar floats over whichever pane is showing rather than living
        // inside one: a pane's widgets are rebuilt whenever the layout changes
        // shape, and a bar parented there would go with them.
        let find = TuniFind::new();
        let switcher = TuniSwitcher::new();
        let content_overlay = gtk::Overlay::new();
        content_overlay.set_child(Some(&content_stack));
        content_overlay.add_overlay(&find);
        content_overlay.add_overlay(&switcher);

        let new_tab = gtk::Button::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("New Tab (Ctrl+Shift+T)")
            .action_name("win.new-tab")
            .build();
        new_tab.add_css_class("flat");
        // Autohide is a setting, and settings arrive after construction; see
        // `TuniWindow::new`.
        let tab_bar = adw::TabBar::builder()
            .autohide(false)
            .expand_tabs(false)
            .end_action_widget(&new_tab)
            .build();

        // --- the Files and Git panel, on the far side of the terminals

        let panel_view = TuniPanel::new();
        if let Some(files) = panel_view.files() {
            files.connect_message(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |message| this.files_message(&message)
            ));
        }
        if let Some(git) = panel_view.git() {
            git.connect_open(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |path, staged| this.open_diff(&path, staged)
            ));
        }
        // The tab strip belongs to the terminals, so it stops where they do:
        // stretched over the panel it would name tabs for a column that shows
        // the same three pages whichever tab is in front.
        let terminal_view = adw::ToolbarView::new();
        terminal_view.add_top_bar(&tab_bar);
        terminal_view.set_content(Some(&content_overlay));

        let panel = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .content(&terminal_view)
            .show_sidebar(false)
            .sidebar_width_fraction(PANEL_FRACTION)
            .min_sidebar_width(f64::from(PANEL_MIN))
            .max_sidebar_width(f64::from(PANEL_MAX))
            .build();
        add_sidebar_grip(&panel, &panel_view, PANEL_DRAG_MIN, PANEL_DRAG_MAX);
        show_files
            .bind_property("active", &panel, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
        // Opening it is the moment its contents matter, and until then it is
        // not reading the disk at all.
        panel.connect_show_sidebar_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.sync_files()
        ));

        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&header);
        content_view.set_content(Some(&panel));

        let split = adw::OverlaySplitView::builder()
            .content(&content_view)
            .sidebar_width_fraction(SIDEBAR_FRACTION)
            .min_sidebar_width(f64::from(SIDEBAR_MIN))
            .max_sidebar_width(f64::from(SIDEBAR_MAX))
            .build();
        add_sidebar_grip(&split, &sidebar_view, SIDEBAR_DRAG_MIN, SIDEBAR_DRAG_MAX);
        toggle
            .bind_property("active", &split, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
        toggle.set_active(true);

        // A narrow window keeps its terminal full width and shows the sidebar
        // over it instead of beside it.
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 640sp").expect("breakpoint condition"),
        );
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        self.add_breakpoint(breakpoint);

        self.set_content(Some(&split));

        // Re-reads the open directories while the panel is showing, so a file
        // written by the shell beside it appears without being asked for.
        glib::timeout_add_seconds_local(
            PANEL_POLL_SECONDS,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    this.poll_files();
                    this.poll_diffs();
                    glib::ControlFlow::Continue
                }
            ),
        );

        // A host list reads files the user edits in some other window, and
        // coming back to tuni is when they would expect to see the edit. It
        // fires on losing the focus too, hence the question.
        self.connect_is_active_notify(|window| {
            if window.is_active() {
                for hosts in window.imp().hosts.borrow().values() {
                    hosts.refresh_if_stale();
                }
            }
        });

        imp.split.replace(Some(split));
        imp.sidebar.replace(Some(sidebar));
        imp.panel.replace(Some(panel));
        imp.panel_view.replace(Some(panel_view));
        imp.row_menu.replace(Some(row_menu));
        imp.stack.replace(Some(stack));
        imp.content.replace(Some(content_stack));
        imp.find.replace(Some(find));
        imp.switcher.replace(Some(switcher));
        imp.status.replace(Some(status));
        imp.status_button.replace(Some(status_button));
        imp.tab_bar.replace(Some(tab_bar));
        imp.title.replace(Some(title));
    }

    fn install_actions(&self) {
        let uint64 = Some(glib::VariantTy::UINT64);
        let string = Some(glib::VariantTy::STRING);
        self.add_action_entries([
            entry("new-tab", None, |window, _| {
                let project = window.imp().workspace.borrow().selected_id().or_else(|| {
                    window
                        .imp()
                        .workspace
                        .borrow()
                        .projects()
                        .first()
                        .map(|p| p.id())
                });
                match project {
                    Some(project) => window.open_tab(project),
                    None => {
                        window.open_project();
                    }
                }
            }),
            entry("close-tab", None, |window, _| {
                if let Some((view, page)) = window.selected_page() {
                    view.close_page(&page);
                }
            }),
            entry("close-pane", None, |window, _| window.close_focused_pane()),
            entry("split-right", None, |window, _| window.split(Edge::Right)),
            entry("split-down", None, |window, _| window.split(Edge::Down)),
            entry("split-left", None, |window, _| window.split(Edge::Left)),
            entry("split-up", None, |window, _| window.split(Edge::Up)),
            entry("focus-pane-left", None, |window, _| {
                window.focus_toward(Edge::Left)
            }),
            entry("focus-pane-right", None, |window, _| {
                window.focus_toward(Edge::Right)
            }),
            entry("focus-pane-up", None, |window, _| {
                window.focus_toward(Edge::Up)
            }),
            entry("focus-pane-down", None, |window, _| {
                window.focus_toward(Edge::Down)
            }),
            entry("next-pane", None, |window, _| {
                window.navigate(Layout::focus_next);
            }),
            entry("previous-pane", None, |window, _| {
                window.navigate(Layout::focus_previous);
            }),
            entry("zoom-pane", None, |window, _| window.toggle_zoom()),
            entry("equalize-panes", None, |window, _| {
                window.reshape(Layout::equalize);
            }),
            entry("resize-pane-left", None, |window, _| {
                window.resize(Edge::Left)
            }),
            entry("resize-pane-right", None, |window, _| {
                window.resize(Edge::Right)
            }),
            entry("resize-pane-up", None, |window, _| window.resize(Edge::Up)),
            entry("resize-pane-down", None, |window, _| {
                window.resize(Edge::Down)
            }),
            entry("next-tab", None, |window, _| window.shift_tab(1)),
            entry("previous-tab", None, |window, _| window.shift_tab(-1)),
            entry(
                "select-tab",
                Some(glib::VariantTy::INT32),
                |window, target| {
                    if let Some(index) = target.and_then(glib::Variant::get::<i32>) {
                        window.select_tab_at(index);
                    }
                },
            ),
            entry("new-project", None, |window, _| {
                window.open_project();
            }),
            entry("next-project", None, |window, _| {
                window.imp().workspace.borrow_mut().select_next();
                window.show_selected_project();
            }),
            entry("previous-project", None, |window, _| {
                window.imp().workspace.borrow_mut().select_previous();
                window.show_selected_project();
            }),
            entry(
                "select-project",
                Some(glib::VariantTy::INT32),
                |window, target| {
                    if let Some(number) = target.and_then(glib::Variant::get::<i32>) {
                        let count = window.imp().workspace.borrow().projects().len();
                        if count > 0 {
                            let index = if number >= 9 {
                                count - 1
                            } else {
                                (number as usize).saturating_sub(1).min(count - 1)
                            };
                            window.select_project_at(index);
                        }
                    }
                },
            ),
            entry("close-project", uint64, |window, target| {
                if let Some(id) = project_target(target) {
                    window.close_project(id);
                }
            }),
            entry("rename-project", uint64, |window, target| {
                if let Some(id) = project_target(target) {
                    window.rename_project(id);
                }
            }),
            entry("automatic-project-title", uint64, |window, target| {
                if let Some(id) = project_target(target) {
                    if let Some(project) = window.imp().workspace.borrow_mut().project_mut(id) {
                        project.custom_name = None;
                    }
                    window.refresh();
                }
            }),
            entry("set-project-directory", uint64, |window, target| {
                if let Some(id) = project_target(target) {
                    window.choose_project_directory(id);
                }
            }),
            entry("automatic-project-directory", uint64, |window, target| {
                if let Some(id) = project_target(target)
                    && let Some(project) = window.imp().workspace.borrow_mut().project_mut(id)
                {
                    project.custom_directory = None;
                }
            }),
            // The editor has `Ctrl+S` of its own, which a terminal may not:
            // `Ctrl+S` to a shell is flow control, and stopping the output of
            // the pane beside the file being saved is not what was asked for.
            entry("save-file", None, |window, _| {
                if let Some(editor) = window.active_editor() {
                    editor.save();
                }
            }),
            // Searches whatever the focused pane holds: the bar over a terminal,
            // GtkSourceView's own over a file. A diff is a rendering of a
            // command's output rather than something to search through, so it
            // answers to none of these.
            entry("find", None, |window, _| match window.find_target() {
                Some(Search::Terminal(find, terminal)) => find.open(&terminal),
                Some(Search::Editor(editor)) => editor.open_find(),
                None => {}
            }),
            entry("find-next", None, |window, _| window.find_step(true)),
            entry("find-previous", None, |window, _| window.find_step(false)),
            // Only a file can be replaced in: terminal output and diffs are
            // readings of something that already happened.
            entry("find-replace", None, |window, _| {
                if let Some(editor) = window.active_editor() {
                    editor.open_replace();
                }
            }),
            entry("use-selection-for-find", None, |window, _| {
                match window.find_target() {
                    Some(Search::Terminal(find, terminal)) => {
                        if let Some(needle) = terminal.selection() {
                            find.look_up(&terminal, &needle);
                        }
                    }
                    Some(Search::Editor(editor)) => {
                        if let Some(needle) = editor.selection() {
                            editor.find_text(&needle);
                        }
                    }
                    None => {}
                }
            }),
            entry("clear-terminal", None, |window, _| {
                if let Some(terminal) = window.active_terminal() {
                    terminal.clear();
                }
            }),
            // The setting, not a per-window mood: a program takes the mouse in
            // every pane it runs in, so the way out of it is the same knob the
            // preferences show, saved where the preferences save it.
            entry("toggle-mouse-reporting", None, |window, _| {
                let mut settings = window.settings();
                settings.terminal.mouse_reporting = !settings.terminal.mouse_reporting;
                window.apply_settings(settings);
            }),
            entry("new-window", None, |window, _| window.open_window()),
            entry("palette", None, |window, _| window.show_palette()),
            // Jumps to a pane wherever it is: its project, then its tab, then
            // the pane itself. What the palette's second section runs.
            entry("reveal-pane", uint64, |window, target| {
                if let Some(id) = project_target(target) {
                    window.reveal_pane(id);
                }
            }),
            // Opens a connection in a tab of its own. The target is the name
            // `ssh` is given, which is either a saved alias or an address
            // somebody typed.
            entry("connect", string, |window, target| {
                if let Some(alias) = target.and_then(glib::Variant::str) {
                    window.open_pane(Pane::ssh(alias.to_owned()));
                }
            }),
            // Opens the host list in a tab of its own, whatever a new tab is
            // configured to open.
            entry("new-connection", None, |window, _| {
                window.open_pane(Pane::hosts());
            }),
            entry("settings", None, |window, _| window.show_preferences()),
            entry("about", None, |window, _| window.show_about()),
            entry("toggle-sidebar", None, |window, _| {
                if let Some(split) = window.imp().split.borrow().as_ref() {
                    split.set_show_sidebar(!split.shows_sidebar());
                }
            }),
            entry("toggle-panel", None, |window, _| {
                let showing = {
                    let panel = window.imp().panel.borrow();
                    let Some(panel) = panel.as_ref() else {
                        return;
                    };
                    panel.set_show_sidebar(!panel.shows_sidebar());
                    panel.shows_sidebar()
                };
                // Closing it hands the keyboard back to the terminal, which is
                // where it was before the panel took it.
                if !showing {
                    window.focus_pane();
                }
            }),
            // One key per page that both opens the panel and puts it on that
            // page: asking for git while the panel is showing the files is
            // asking for the panel to change page, not to close. Asking for the
            // page already showing is the one that closes it, which is how kero
            // spends the same three keys.
            entry("show-files", None, |window, _| {
                window.show_panel(crate::panel::FILES);
            }),
            entry("show-git", None, |window, _| {
                window.show_panel(crate::panel::GIT);
            }),
            entry("show-info", None, |window, _| {
                window.show_panel(crate::panel::INFO);
            }),
            entry("tab-rename", None, |window, _| window.rename_tab()),
            entry("tab-automatic-title", None, |window, _| {
                let Some(page) = window.imp().menu_page.borrow().clone() else {
                    return;
                };
                window.set_tab_name(&page, None);
            }),
            entry("tab-close", None, |window, _| {
                if let (Some(view), Some(page)) = (
                    window.selected_view(),
                    window.imp().menu_page.borrow().clone(),
                ) {
                    view.close_page(&page);
                }
            }),
            entry("tab-close-others", None, |window, _| {
                if let (Some(view), Some(page)) = (
                    window.selected_view(),
                    window.imp().menu_page.borrow().clone(),
                ) {
                    view.close_other_pages(&page);
                }
            }),
            entry("tab-close-right", None, |window, _| {
                if let (Some(view), Some(page)) = (
                    window.selected_view(),
                    window.imp().menu_page.borrow().clone(),
                ) {
                    view.close_pages_after(&page);
                }
            }),
        ]);
    }

    /// Repaint every terminal when the desktop changes its mind about light and
    /// dark, and repaint the chrome to match.
    fn watch_appearance(&self) {
        let style = adw::StyleManager::default();
        let recolor = glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |style: &adw::StyleManager| {
                let opacity = this.imp().settings.borrow().terminal.background_opacity;
                let theme = this.imp().settings.borrow().terminal.theme(style.is_dark());
                for terminal in this.imp().terminals.borrow().values() {
                    terminal.set_theme(&theme);
                }
                for editor in this.imp().editors.borrow().values() {
                    editor.set_dark(style.is_dark());
                }
                for diff in this.imp().diffs.borrow().values() {
                    diff.set_theme(&theme);
                }
                apply_chrome(&theme, opacity);
            }
        );
        style.connect_dark_notify(recolor.clone());
        recolor(&style);
    }

    /// The colors this run's terminals are painted with.
    fn theme(&self) -> Theme {
        self.imp()
            .settings
            .borrow()
            .terminal
            .theme(adw::StyleManager::default().is_dark())
    }

    // --- settings ----------------------------------------------------------

    /// Takes the edited settings: writes them, repaints every terminal, and
    /// tells the desktop which appearance was asked for.
    ///
    /// Everything is applied to the terminals that are already open rather than
    /// only to the next one, which is the difference between a settings window
    /// and a configuration file.
    pub(crate) fn settings(&self) -> Settings {
        self.imp().settings.borrow().clone()
    }

    pub(crate) fn apply_settings(&self, settings: Settings) {
        let imp = self.imp();
        imp.settings.replace(settings.clone());
        apply_appearance(settings.appearance);

        let theme = self.theme();
        for terminal in imp.terminals.borrow().values() {
            terminal.set_config(&settings.terminal);
            terminal.set_theme(&theme);
        }
        for diff in imp.diffs.borrow().values() {
            diff.set_theme(&theme);
        }
        for editor in imp.editors.borrow().values() {
            editor.set_wrap(settings.wrap_lines);
        }
        if let Some(bar) = imp.tab_bar.borrow().as_ref() {
            bar.set_autohide(settings.auto_hide_tab_bar);
        }
        crate::editor::apply_font(&settings.terminal);
        crate::diff::apply_colors(&theme);
        apply_chrome(&theme, settings.terminal.background_opacity);
        crate::blur::apply(self, settings.background_blur);

        // Turning history off should not leave the last session's output on
        // disk waiting for the setting to be turned back on.
        if !settings.restore_history {
            History::forget();
        }
        if let Err(error) = settings.save() {
            eprintln!("cannot save settings: {error}");
        }
    }

    fn show_preferences(&self) {
        preferences::present(self, &self.imp().settings.borrow().clone());
    }

    /// What this is, who wrote it, and where to report it. `AdwAboutDialog`
    /// draws all of that, including the Troubleshooting page the debug info
    /// goes on, so the only work here is telling it the truth.
    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Tuni")
            // An installed Tuni has its icon in the theme; one run out of the
            // build directory has not, and a missing-icon glyph the size of an
            // about dialog is worse than the terminal the empty window uses.
            .application_icon(
                if gtk::IconTheme::for_display(&WidgetExt::display(self)).has_icon(crate::APP_ID) {
                    crate::APP_ID
                } else {
                    "utilities-terminal-symbolic"
                },
            )
            .developer_name("Unisic")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Terminals, projects, files, and Git in one window.")
            .website("https://github.com/unisic/tuni")
            .issue_url("https://github.com/unisic/tuni/issues")
            .license_type(gtk::License::Gpl30)
            .debug_info(debug_info())
            .build();
        about.present(Some(self));
    }

    // --- the Info, Files and Git panel --------------------------------------

    /// Points the panel at the project's directory.
    ///
    /// The root is re-derived on every call rather than remembered, so a shell
    /// that `cd`s out of one repository and into another takes the tree and the
    /// repository with it; a pinned project directory is what stops that from
    /// happening.
    ///
    /// Info wants both halves: the shell's own directory, which is where a
    /// command was typed, and the root the tree and the repository are anchored
    /// to, which may be a directory above it.
    fn sync_files(&self) {
        let imp = self.imp();
        if !imp
            .panel
            .borrow()
            .as_ref()
            .is_some_and(adw::OverlaySplitView::shows_sidebar)
        {
            return;
        }
        let Some(panel) = imp.panel_view.borrow().clone() else {
            return;
        };
        let directories = {
            let workspace = imp.workspace.borrow();
            workspace.selected_project().map(|project| {
                let cwd = project
                    .selected_tab()
                    .and_then(Tab::directory)
                    .map(PathBuf::from)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                let (root, automatic) = project.panel_root(&cwd);
                (cwd, root, automatic)
            })
        };
        let Some((cwd, root, automatic)) = directories else {
            return;
        };
        // Whatever is focused right now, which is the shell whose children Info
        // is about to list.
        let shell = self
            .active_terminal()
            .and_then(|terminal| terminal.shell_pid());
        panel.sync(&root, &cwd, shell, automatic);
    }

    /// The timer's half of that: the root cannot have moved without something
    /// else having said so, but what is inside it can.
    fn poll_files(&self) {
        let imp = self.imp();
        if !imp
            .panel
            .borrow()
            .as_ref()
            .is_some_and(adw::OverlaySplitView::shows_sidebar)
        {
            return;
        }
        // Cheap when the root has not moved, and it is what catches a `cd`
        // into a different repository.
        self.sync_files();
        if let Some(panel) = imp.panel_view.borrow().as_ref() {
            panel.poll();
        }
    }

    /// A diff is over a file the shell beside it can edit, and nothing else
    /// says when that happened, so the same timer re-reads it. A read that
    /// comes back unchanged leaves the pane alone.
    fn poll_diffs(&self) {
        for diff in self.imp().diffs.borrow().values() {
            // Only the one on screen. A diff on a tab nobody has selected is
            // unmapped, and re-reading it costs three git processes every two
            // seconds for a pane whose content nobody can see. It catches up
            // on `map`, which is what selecting the tab again does.
            if diff.is_mapped() {
                diff.reload();
            }
        }
    }

    fn files_message(&self, message: &crate::files::Message) {
        match message {
            crate::files::Message::Cd(path) => {
                let Some(terminal) = self.active_terminal() else {
                    return;
                };
                terminal.send_text(&format!("cd {}\n", tuni_core::files::shell_quote(path)));
                terminal.grab_focus();
            }
            crate::files::Message::Open(path) => self.open_file(path),
            crate::files::Message::OpenToSide(path) => self.open_file_to_side(path),
        }
    }

    // --- the session ------------------------------------------------------

    /// Writes the window's shape, and the scrollback if that was asked for.
    ///
    /// Runs while the window is closing, so it does the least it can: a
    /// snapshot of the model, one pass over the live terminals, two files.
    fn save_session(&self) {
        if !session_enabled() || !self.imp().session_owner.get() {
            return;
        }
        let imp = self.imp();
        let keep_history = imp.settings.borrow().restore_history;
        let terminals = imp.terminals.borrow();
        let editors = imp.editors.borrow();
        let history = RefCell::new(History::default());

        let snapshot = {
            let workspace = imp.workspace.borrow();
            let mut snapshot = Snapshot::of(&workspace, |pane| PaneState {
                history: keep_history
                    .then(|| {
                        let text = terminals.get(&pane)?.history(HISTORY_LINE_LIMIT)?;
                        // Unique within one saved session, which is all a key has
                        // to be: the file is rewritten whole, so nothing older
                        // survives to collide with.
                        let key = format!("pane-{}", pane.raw());
                        history.borrow_mut().insert(key.clone(), text);
                        Some(key)
                    })
                    .flatten(),
                // Where the work was left in a file pane. The text itself is
                // not saved: what is on disk is the file, and a restored window
                // that quietly held edits nothing wrote would be a worse
                // promise than reopening the file as it is.
                cursor: editors.get(&pane).and_then(TuniEditor::cursor),
            });
            snapshot.sidebar = imp
                .split
                .borrow()
                .as_ref()
                .map(adw::OverlaySplitView::shows_sidebar);
            snapshot.panel = imp
                .panel
                .borrow()
                .as_ref()
                .map(adw::OverlaySplitView::shows_sidebar);
            snapshot.panel_page = imp.panel_view.borrow().as_ref().map(TuniPanel::page);
            snapshot.sidebar_width = imp.split.borrow().as_ref().and_then(pinned_width);
            snapshot.panel_width = imp.panel.borrow().as_ref().and_then(pinned_width);
            snapshot
        };

        if let Err(error) = snapshot.save() {
            eprintln!("cannot save session: {error}");
        }
        if let Err(error) = history.borrow().save() {
            eprintln!("cannot save terminal history: {error}");
        }
    }

    /// Rebuilds the last session's window. `false` when there is nothing saved
    /// to rebuild, which is the caller's cue to open a first project instead.
    pub fn restore_session(&self) -> bool {
        let Some(restored) = Snapshot::load().map(|snapshot| snapshot.restore()) else {
            return false;
        };
        if restored.workspace.is_empty() {
            return false;
        }
        let imp = self.imp();

        if !restored.cursors.is_empty() {
            imp.pending_cursors.replace(restored.cursors.clone());
        }
        if imp.settings.borrow().restore_history && !restored.histories.is_empty() {
            let history = History::load();
            if !history.is_empty() {
                let mut pending = imp.pending_history.borrow_mut();
                for (pane, key) in &restored.histories {
                    if let Some(text) = history.get(key) {
                        pending.insert(*pane, text.to_owned());
                    }
                }
            }
        }

        // Read out before the model is handed over, so nothing below holds a
        // borrow of it while the widgets are being built.
        let selected = restored.workspace.selected_id();
        let plan: Vec<(Id, Vec<Id>, Option<Id>)> = restored
            .workspace
            .projects()
            .iter()
            .map(|project| {
                (
                    project.id(),
                    project.tabs().iter().map(Tab::id).collect(),
                    project.selected_id(),
                )
            })
            .collect();
        imp.workspace.replace(restored.workspace);

        for (project, tabs, chosen) in plan {
            self.attach_project(project);
            for (position, tab) in tabs.iter().enumerate() {
                self.attach_tab(project, *tab, position as i32, false);
            }
            // Inserting pages moved the selection along with them; put it back
            // where the session left it.
            let view = imp.views.borrow().get(&project).cloned();
            let page = chosen.and_then(|tab| imp.pages.borrow().get(&tab).cloned());
            if let (Some(view), Some(page)) = (view, page) {
                view.set_selected_page(&page);
            }
        }

        if let Some(id) = selected {
            imp.workspace.borrow_mut().select(id);
        }
        if let Some(show) = restored.sidebar
            && let Some(split) = imp.split.borrow().as_ref()
        {
            split.set_show_sidebar(show);
        }
        if let Some(show) = restored.panel
            && let Some(panel) = imp.panel.borrow().as_ref()
        {
            panel.set_show_sidebar(show);
        }
        if let Some(page) = restored.panel_page.as_deref()
            && let Some(panel) = imp.panel_view.borrow().as_ref()
        {
            panel.set_page(page);
        }
        // Already in the split's own unit, so it goes back the way it came out.
        // Clamped anyway: the file is text on disk, and a width from a hand
        // edited one is still a width the window has to live with.
        if let Some(width) = restored.sidebar_width
            && let Some(split) = imp.split.borrow().as_ref()
        {
            set_pinned_width(split, width.clamp(SIDEBAR_DRAG_MIN, SIDEBAR_DRAG_MAX));
        }
        if let Some(width) = restored.panel_width
            && let Some(panel) = imp.panel.borrow().as_ref()
        {
            set_pinned_width(panel, width.clamp(PANEL_DRAG_MIN, PANEL_DRAG_MAX));
        }

        self.rebuild_sidebar();
        self.show_selected_project();
        true
    }

    // --- projects ----------------------------------------------------------

    /// Opens a project with one terminal in it, and shows it.
    pub fn open_project(&self) -> Id {
        let id = self.imp().workspace.borrow_mut().open_project();
        self.attach_project(id);
        self.rebuild_sidebar();
        self.show_selected_project();
        self.open_tab(id);
        id
    }

    /// Builds the tab strip for a project the model already holds — the half of
    /// opening one that a restored session needs too.
    fn attach_project(&self, id: Id) {
        let imp = self.imp();
        let view = adw::TabView::new();
        view.set_menu_model(Some(&tab_menu()));
        view.connect_setup_menu(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, page| {
                this.imp().menu_page.replace(page.cloned());
            }
        ));
        // A tab with unsaved work in it asks before it goes; the strip waits
        // for the answer rather than closing and being told afterwards.
        view.connect_close_page(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |view, page| this.confirm_tab_close(view, page)
        ));
        view.connect_page_detached(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, position| this.tab_detached(id, position)
        ));
        view.connect_page_reordered(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, page, position| {
                if let Some(tab) = this.tab_of(&page.child())
                    && let Some(project) = this.imp().workspace.borrow_mut().project_mut(id)
                {
                    project.move_tab(tab, position.max(0) as usize);
                }
            }
        ));
        view.connect_selected_page_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |view| this.tab_selected(id, view)
        ));

        imp.views.borrow_mut().insert(id, view.clone());
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.add_named(&view, Some(&id.raw().to_string()));
        }
    }

    /// Closes a project and every shell in it. The sidebar falls to the
    /// neighbor that took its place.
    pub fn close_project(&self, id: Id) {
        let imp = self.imp();
        let Some(project) = imp.workspace.borrow_mut().close_project(id) else {
            return;
        };
        for tab in project.tabs() {
            for pane in tab.layout().panes() {
                self.forget_pane(pane.id());
            }
            imp.grids.borrow_mut().remove(&tab.id());
            imp.pages.borrow_mut().remove(&tab.id());
        }
        if let Some(view) = imp.views.borrow_mut().remove(&id)
            && let Some(stack) = imp.stack.borrow().as_ref()
        {
            stack.remove(&view);
        }
        self.rebuild_sidebar();
        self.show_selected_project();
    }

    fn select_project_at(&self, index: usize) {
        self.imp().workspace.borrow_mut().select_index(index);
        self.show_selected_project();
    }

    /// Brings the selected project's tab strip on screen and points the tab bar
    /// and the sidebar at it.
    fn show_selected_project(&self) {
        let imp = self.imp();
        let selected = imp.workspace.borrow().selected_id();

        let view = selected.and_then(|id| imp.views.borrow().get(&id).cloned());
        if let (Some(stack), Some(id)) = (imp.stack.borrow().as_ref(), selected) {
            stack.set_visible_child_name(&id.raw().to_string());
        }
        if let Some(bar) = imp.tab_bar.borrow().as_ref() {
            // Pointing the strip at another project rebuilds every tab in it,
            // and each rebuilt tab plays an appear animation that libadwaita
            // then forgets: `adw_tab_box_set_view` frees the tabs it is
            // replacing without stopping their animations, so the *next*
            // switch leaves a timer running on freed memory, and the frame
            // after that is a segmentation fault. Two switches inside the
            // animation's fifth of a second are enough, which is one hand on
            // the sidebar. An unmapped widget skips its animations outright
            // rather than starting them, so the strip is hidden across the
            // switch: no frame is drawn in between, and nothing is left
            // running to tick on a tab that has gone.
            bar.set_visible(false);
            bar.set_view(view.as_ref());
            bar.set_visible(true);
        }

        // Writing the sidebar's selection here would otherwise come straight
        // back as a selection change and re-enter this method.
        if let Some(list) = imp.sidebar.borrow().as_ref() {
            let index = selected.and_then(|id| imp.workspace.borrow().index_of(id));
            imp.selecting.set(true);
            match index {
                Some(index) => list.select_row(list.row_at_index(index as i32).as_ref()),
                None => list.select_row(gtk::ListBoxRow::NONE),
            }
            imp.selecting.set(false);
        }

        self.refresh();
        self.sync_files();
        self.focus_pane();
    }

    // --- tabs --------------------------------------------------------------

    /// Opens a tab in `project`, next to the selected one, with a single
    /// terminal starting where the selected tab's shell is, or with the host
    /// list if that is what `new-tab` says a tab is.
    pub fn open_tab(&self, project: Id) {
        let imp = self.imp();
        let Some(view) = imp.views.borrow().get(&project).cloned() else {
            return;
        };

        let tab = match imp.settings.borrow().new_tab {
            NewTab::Shell => Tab::new(),
            NewTab::Hosts => Tab::with_pane(Pane::hosts()),
        };
        let tab_id = tab.id();
        let position = view
            .selected_page()
            .map_or_else(|| view.n_pages(), |page| view.page_position(&page) + 1);

        if let Some(project) = imp.workspace.borrow_mut().project_mut(project) {
            project.insert_tab(position.max(0) as usize, tab);
        }

        let Some(page) = self.attach_tab(project, tab_id, position, true) else {
            return;
        };
        view.set_selected_page(&page);
        self.refresh();
    }

    /// Builds the widgets for a tab the model already holds, and starts a shell
    /// in every pane of it.
    ///
    /// One tab is one page holding one [`TuniGrid`]; a fresh tab has a single
    /// pane in it and a restored one has however many it was saved with, which
    /// is the only difference between opening a tab and restoring one.
    ///
    /// `requested` says whether somebody asked for this tab just now, as
    /// against a window putting back what it had. It decides one thing: a
    /// connection nobody asked for waits to be asked, because dialling one can
    /// want a password.
    fn attach_tab(
        &self,
        project: Id,
        tab: Id,
        position: i32,
        requested: bool,
    ) -> Option<adw::TabPage> {
        let imp = self.imp();
        let view = imp.views.borrow().get(&project).cloned()?;

        // Read before the page is inserted: inserting into an empty strip
        // selects the new tab, and the directory a new shell starts in is the
        // one the *previous* tab's shell was in.
        let fallback = self.directory_for_new_shell(project);
        let (name, panes) = {
            let workspace = imp.workspace.borrow();
            let entry = workspace.project(project)?.tab(tab)?;
            let panes: Vec<(Id, Option<String>, Content)> = entry
                .layout()
                .panes()
                .map(|pane| (pane.id(), pane.directory.clone(), pane.content.clone()))
                .collect();
            (entry.name().to_owned(), panes)
        };

        let grid = self.new_grid(project, tab);
        let page = view.insert(&grid, position);
        page.set_title(&name);
        page.set_live_thumbnail(true);
        imp.pages.borrow_mut().insert(tab, page.clone());

        // Every pane's widget exists before any of them is drawn, so the grid
        // is laid out once rather than once per pane.
        let started: Vec<(TuniTerminal, Id, Content, Option<PathBuf>)> = panes
            .into_iter()
            .filter_map(|(pane, directory, content)| {
                if !matches!(content, Content::Terminal | Content::Ssh { .. }) {
                    self.new_content(project, tab, pane, &content);
                    return None;
                }
                let cwd = directory
                    .map(PathBuf::from)
                    .filter(|path| path.is_dir())
                    .or_else(|| fallback.clone());
                let terminal = self.new_terminal(project, tab, pane);
                if is_remote(&content) {
                    self.new_remote(project, tab, pane, &terminal);
                }
                Some((terminal, pane, content, cwd))
            })
            .collect();
        self.rebuild_grid(project, tab);
        for (terminal, pane, content, cwd) in started {
            self.start_session(&terminal, pane, &content, cwd, requested);
        }
        Some(page)
    }

    /// Opens a shell beside the focused pane, in the same tab.
    fn split(&self, edge: Edge) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        let cwd = self.directory_for_new_shell(project);
        let pane = Pane::new();
        let pane_id = pane.id();
        if !self.insert_pane(project, tab, pane, edge) {
            return;
        }

        let terminal = self.new_terminal(project, tab, pane_id);
        self.rebuild_grid(project, tab);
        self.refresh();
        self.start_terminal(
            &terminal,
            pane_id,
            Launch {
                cwd,
                ..Launch::default()
            },
        );
    }

    /// Puts a pane the caller built into a tab's layout. `false` when the tab
    /// is gone, which is the caller's cue not to build a widget for it.
    fn insert_pane(&self, project: Id, tab: Id, pane: Pane, edge: Edge) -> bool {
        let imp = self.imp();
        let mut workspace = imp.workspace.borrow_mut();
        let Some(entry) = workspace
            .project_mut(project)
            .and_then(|project| project.tab_mut(tab))
        else {
            return false;
        };
        entry.layout_mut().split(pane, edge);
        true
    }

    /// A terminal for one pane, themed and remembered.
    fn new_terminal(&self, project: Id, tab: Id, pane: Id) -> TuniTerminal {
        let imp = self.imp();
        let terminal = TuniTerminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_config(&imp.settings.borrow().terminal);
        terminal.set_theme(&self.theme());
        imp.terminals.borrow_mut().insert(pane, terminal.clone());
        self.watch_terminal(&terminal, project, tab, pane);
        terminal
    }

    /// The bar an ssh pane wears above its terminal, wired to the one thing it
    /// offers.
    fn new_remote(&self, project: Id, tab: Id, pane: Id, terminal: &TuniTerminal) -> TuniRemote {
        let remote = TuniRemote::new(terminal);
        remote.connect_open(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            terminal,
            move || {
                let content = this.pane_content(project, tab, pane);
                if let Some(content) = content {
                    this.start_session(&terminal, pane, &content, None, true);
                }
            }
        ));
        self.imp().remotes.borrow_mut().insert(pane, remote.clone());
        remote
    }

    /// What a pane holds, read out of the model.
    fn pane_content(&self, project: Id, tab: Id, pane: Id) -> Option<Content> {
        self.imp()
            .workspace
            .borrow()
            .project(project)?
            .tab(tab)?
            .layout()
            .panes()
            .find(|entry| entry.id() == pane)
            .map(|entry| entry.content.clone())
    }

    /// The host list for one pane, wired to the three places a connection it is
    /// asked for can go, and to the one thing it cannot do itself, which is
    /// open the file it only reads.
    fn new_hosts(&self, project: Id, tab: Id, pane: Id) -> TuniHosts {
        use crate::hosts::Message;

        let hosts = TuniHosts::new();
        hosts.set_hexpand(true);
        hosts.set_vexpand(true);
        hosts.connect_message(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |message| match message {
                Message::Connect(alias) => {
                    this.replace_pane(project, tab, pane, Pane::ssh(alias));
                }
                Message::LocalShell => this.replace_pane(project, tab, pane, Pane::new()),
                Message::ConnectToSide(alias) => this.open_pane_to_side(Pane::ssh(alias)),
                Message::ConnectInTab(alias) => this.open_pane(Pane::ssh(alias)),
                Message::OpenFile(path) => this.open_file(&path),
            }
        ));
        self.imp().hosts.borrow_mut().insert(pane, hosts.clone());
        hosts
    }

    /// Puts something else in a pane that is already on screen: the model, the
    /// widget, and the session that goes with it.
    ///
    /// The tile stays the same tile. That is the whole of what picking a host
    /// in the launcher does, and it is why the list is a pane rather than a
    /// dialog: it is standing in the place the connection is about to be.
    fn replace_pane(&self, project: Id, tab: Id, pane: Id, replacement: Pane) {
        let content = replacement.content.clone();
        {
            let mut workspace = self.imp().workspace.borrow_mut();
            let Some(entry) = workspace
                .project_mut(project)
                .and_then(|project| project.pane_mut(tab, pane))
            else {
                return;
            };
            entry.replace_with(replacement);
        }
        let cwd = self.directory_for_new_shell(project);
        self.forget_pane(pane);
        self.fill_pane(project, tab, pane, &content, cwd);
    }

    /// Builds, draws and starts what a pane holds, for a pane already in the
    /// layout. Every way of opening one that is not a whole tab goes through
    /// here; a tab builds all of its panes before drawing any of them.
    fn fill_pane(&self, project: Id, tab: Id, pane: Id, content: &Content, cwd: Option<PathBuf>) {
        let terminal = match content {
            Content::Terminal | Content::Ssh { .. } => {
                let terminal = self.new_terminal(project, tab, pane);
                if is_remote(content) {
                    self.new_remote(project, tab, pane, &terminal);
                }
                Some(terminal)
            }
            _ => {
                self.new_content(project, tab, pane, content);
                None
            }
        };
        self.rebuild_grid(project, tab);
        self.refresh();
        self.focus_pane();
        if let Some(terminal) = terminal {
            self.start_session(&terminal, pane, content, cwd, true);
        }
    }

    /// An editor for one pane, opened on a file and remembered.
    fn new_editor(&self, project: Id, tab: Id, pane: Id, path: &Path) -> TuniEditor {
        let imp = self.imp();
        let editor = TuniEditor::new();
        editor.set_hexpand(true);
        editor.set_vexpand(true);
        editor.set_dark(adw::StyleManager::default().is_dark());
        editor.set_wrap(imp.settings.borrow().wrap_lines);
        editor.open(path);
        if let Some(cursor) = imp.pending_cursors.borrow_mut().remove(&pane) {
            editor.set_cursor(cursor);
        }
        // The tab marks itself while the file is unsaved, so the mark has to
        // arrive as the file becomes dirty rather than at the next redraw.
        editor.connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || this.refresh_names()
        ));
        editor.connect_focused(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || this.pane_focused(project, tab, pane)
        ));
        imp.editors.borrow_mut().insert(pane, editor.clone());
        editor
    }

    /// A diff for one pane, opened on a file and remembered.
    fn new_diff(&self, project: Id, tab: Id, pane: Id, path: &Path, staged: bool) -> TuniDiff {
        let imp = self.imp();
        let diff = TuniDiff::new();
        diff.set_hexpand(true);
        diff.set_vexpand(true);
        diff.set_theme(&self.theme());
        diff.open(path, staged);
        diff.connect_focused(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || this.pane_focused(project, tab, pane)
        ));
        // Staging from the diff changes what the panel is showing, and the
        // panel would otherwise not know until its next poll.
        diff.connect_applied(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || this.refresh_git()
        ));
        imp.diffs.borrow_mut().insert(pane, diff.clone());
        diff
    }

    /// Reads the repository again, and every diff on screen with it.
    fn refresh_git(&self) {
        let imp = self.imp();
        if let Some(git) = imp.panel_view.borrow().as_ref().and_then(TuniPanel::git) {
            git.poll();
        }
        for diff in imp.diffs.borrow().values() {
            diff.reload();
        }
    }

    // --- files in panes ----------------------------------------------------

    /// Opens a file in a tab of its own, next to the selected one.
    pub fn open_file(&self, path: &Path) {
        self.open_pane(Pane::file(path.to_path_buf()));
    }

    /// Opens what changed in a file the same way: the working tree against the
    /// index, or the index against HEAD.
    pub fn open_diff(&self, path: &Path, staged: bool) {
        self.open_pane(Pane::diff(path.to_path_buf(), staged));
    }

    /// Puts a pane in a tab of its own, next to the selected one.
    ///
    /// What the pane holds, already open somewhere in this project, is shown
    /// where it is rather than opened twice — the same rule kero has, and the
    /// reason a second click on a file in the panel goes back to the work in
    /// progress instead of throwing it away.
    fn open_pane(&self, pane: Pane) {
        if self.show_pane(&pane.content) {
            return;
        }
        let Some(project) = self.imp().workspace.borrow().selected_id() else {
            return;
        };
        let imp = self.imp();
        let Some(view) = imp.views.borrow().get(&project).cloned() else {
            return;
        };

        let tab = Tab::with_pane(pane);
        let tab_id = tab.id();
        let position = view
            .selected_page()
            .map_or_else(|| view.n_pages(), |page| view.page_position(&page) + 1);
        if let Some(project) = imp.workspace.borrow_mut().project_mut(project) {
            project.insert_tab(position.max(0) as usize, tab);
        }

        let Some(page) = self.attach_tab(project, tab_id, position, true) else {
            return;
        };
        view.set_selected_page(&page);
        self.refresh();
        self.focus_pane();
    }

    /// Opens a file beside the focused pane, in the tab already on screen.
    pub fn open_file_to_side(&self, path: &Path) {
        self.open_pane_to_side(Pane::file(path.to_path_buf()));
    }

    /// The same for a diff, which is how a file and its changes end up side by
    /// side.
    pub fn open_diff_to_side(&self, path: &Path, staged: bool) {
        self.open_pane_to_side(Pane::diff(path.to_path_buf(), staged));
    }

    fn open_pane_to_side(&self, pane: Pane) {
        let Some((project, tab)) = self.selected_tab() else {
            self.open_pane(pane);
            return;
        };
        if self.show_pane_in(tab, &pane.content) {
            return;
        }
        let pane_id = pane.id();
        let content = pane.content.clone();
        if !self.insert_pane(project, tab, pane, Edge::Right) {
            return;
        }
        let cwd = self.directory_for_new_shell(project);
        self.fill_pane(project, tab, pane_id, &content, cwd);
    }

    /// Builds whatever a pane holds, for a pane already in the layout. Not a
    /// terminal or a connection: those need a directory to start in and
    /// something to start, which only the caller knows.
    fn new_content(&self, project: Id, tab: Id, pane: Id, content: &Content) {
        match content {
            Content::Terminal | Content::Ssh { .. } => (),
            Content::File(path) => {
                self.new_editor(project, tab, pane, path);
            }
            Content::Diff { path, staged } => {
                self.new_diff(project, tab, pane, path, *staged);
            }
            Content::Hosts => {
                self.new_hosts(project, tab, pane);
            }
        }
    }

    /// Brings a pane already open in this project back on screen. `false` when
    /// nothing in it is showing that.
    fn show_pane(&self, content: &Content) -> bool {
        if is_remote(content) {
            return false;
        }
        let imp = self.imp();
        let found = {
            let workspace = imp.workspace.borrow();
            let Some(project) = workspace.selected_project() else {
                return false;
            };
            project.tabs().iter().find_map(|tab| {
                tab.layout()
                    .panes()
                    .find(|pane| &pane.content == content)
                    .map(|pane| (tab.id(), pane.id()))
            })
        };
        let Some((tab, pane)) = found else {
            return false;
        };
        if let Some(page) = imp.pages.borrow().get(&tab).cloned()
            && let Some(view) = self.selected_view()
        {
            view.set_selected_page(&page);
        }
        self.focus_pane_at(tab, pane);
        true
    }

    /// The same, for one tab: what splitting to the side checks before it
    /// splits.
    fn show_pane_in(&self, tab: Id, content: &Content) -> bool {
        if is_remote(content) {
            return false;
        }
        let found = {
            let workspace = self.imp().workspace.borrow();
            workspace
                .selected_project()
                .and_then(|project| project.tab(tab))
                .and_then(|tab| {
                    tab.layout()
                        .panes()
                        .find(|pane| &pane.content == content)
                        .map(tuni_core::panes::Pane::id)
                })
        };
        let Some(pane) = found else {
            return false;
        };
        self.focus_pane_at(tab, pane);
        true
    }

    /// Moves the focus to one pane of a tab, in the model and on screen.
    fn focus_pane_at(&self, tab: Id, pane: Id) {
        let imp = self.imp();
        {
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .selected_project_mut()
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            layout.focus(pane);
        }
        if let Some(grid) = imp.grids.borrow().get(&tab) {
            grid.set_focused(pane);
        }
        self.refresh();
        self.focus_pane();
    }

    /// Starts what a pane holds: a shell straight away, or a connection once
    /// `ssh` has been asked what its command line should be. That question is
    /// a subprocess, so it is not asked on this thread and the pane sits empty
    /// until the answer comes back.
    ///
    /// `requested` is the reconnect rule, which is to dial exactly when doing
    /// so cannot ask anybody anything. Somebody who pressed Connect is there
    /// to answer a prompt. A window putting its panes back is not, so it
    /// dials only where a shared connection is already open and there is
    /// nothing left to authenticate; the rest come back as an offer.
    /// `ssh-reconnect-on-restore` is how somebody whose hosts all answer to an
    /// agent says they would rather have the panes than the question.
    fn start_session(
        &self,
        terminal: &TuniTerminal,
        pane: Id,
        content: &Content,
        cwd: Option<PathBuf>,
        requested: bool,
    ) {
        let Content::Ssh { alias } = content else {
            self.start_terminal(
                terminal,
                pane,
                Launch {
                    cwd,
                    ..Launch::default()
                },
            );
            return;
        };
        let alias = alias.clone();
        let name = alias.clone();
        let settings = self.settings();
        let term = settings.ssh_term.clone();
        let dial = requested || settings.ssh_reconnect_on_restore;
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            terminal,
            async move {
                let Ok(argv) = gio::spawn_blocking(move || {
                    let control = tuni_core::ssh::Control::new(
                        settings.ssh_control_persist,
                        settings.ssh_share_connections,
                    );
                    let host = tuni_core::ssh::host(&alias);
                    (dial || control.is_live(&host.target()))
                        .then(|| tuni_core::ssh::command(&host, &control))
                })
                .await
                else {
                    return;
                };
                let remote = this.imp().remotes.borrow().get(&pane).cloned();
                let Some(argv) = argv else {
                    if let Some(remote) = remote {
                        remote.set_idle(&format!("Not connected to {name}"), "Connect");
                    }
                    return;
                };
                if let Some(remote) = remote {
                    remote.set_running();
                }
                this.start_terminal(
                    &terminal,
                    pane,
                    Launch {
                        argv,
                        env: vec![("TERM".to_owned(), term)],
                        ..Launch::default()
                    },
                );
            }
        ));
    }

    /// Starts a shell once its widget has been allocated.
    ///
    /// The shell learns its window size from that first allocation, so starting
    /// it any earlier opens it at 80x24 and corrects it under its own feet.
    ///
    /// A restored pane replays what it had printed once the shell is up, so the
    /// old output sits above the new prompt rather than racing it.
    fn start_terminal(&self, terminal: &TuniTerminal, pane: Id, launch: Launch) {
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            terminal,
            move || {
                if let Err(error) = terminal.start(&launch) {
                    let dialog = adw::AlertDialog::new(Some("Cannot start shell"), Some(&error));
                    dialog.add_response("close", "Close");
                    dialog.present(Some(&this));
                    return;
                }
                let history = this.imp().pending_history.borrow_mut().remove(&pane);
                if let Some(text) = history {
                    terminal.restore_history(&text);
                }
                // The keyboard goes where the model says it should, not to
                // whichever shell happened to start last.
                this.focus_pane();
            }
        ));
    }

    /// Where the next shell in `project` should start.
    fn directory_for_new_shell(&self, project: Id) -> Option<PathBuf> {
        self.imp()
            .workspace
            .borrow()
            .project(project)
            .and_then(|project| project.directory_for_new_tab().map(PathBuf::from))
            .filter(|path| path.is_dir())
            .or_else(|| std::env::current_dir().ok())
    }

    /// The widget one tab's panes live in, listening for what only the pointer
    /// can decide.
    fn new_grid(&self, project: Id, tab: Id) -> TuniGrid {
        let grid = TuniGrid::new();
        grid.connect_message(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |message| this.grid_message(project, tab, message)
        ));
        self.imp().grids.borrow_mut().insert(tab, grid.clone());
        grid
    }

    fn grid_message(&self, project: Id, tab: Id, message: Message) {
        let imp = self.imp();
        let mut reshaped = false;
        {
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            match message {
                Message::Focus(pane) => layout.focus(pane),
                Message::Move(dragged, edge, target) => {
                    layout.move_pane(dragged, edge, target);
                    reshaped = true;
                }
                Message::Columns(weights) => layout.set_column_weights(&weights),
                Message::Panes(column, weights) => layout.set_pane_weights(column, &weights),
            }
        }
        if reshaped {
            self.rebuild_grid(project, tab);
        }
        self.refresh();
        if reshaped {
            self.focus_pane();
        }
    }

    /// Draws a tab's layout again, and hands the keyboard back to the pane that
    /// had it.
    fn rebuild_grid(&self, project: Id, tab: Id) {
        let imp = self.imp();
        let Some(grid) = imp.grids.borrow().get(&tab).cloned() else {
            return;
        };
        let layout = imp
            .workspace
            .borrow()
            .project(project)
            .and_then(|project| project.tab(tab))
            .map(|tab| tab.layout().clone());
        let Some(layout) = layout else {
            return;
        };
        // A copy of the map rather than a borrow of it: rebuilding moves the
        // keyboard between widgets, and a shell that hangs up while that
        // happens goes straight for these maps.
        grid.rebuild(&layout, &self.pane_widgets());
    }

    /// What every pane holds, by pane id: a terminal, or the editor that took
    /// its place. The grid draws whatever it is given and knows the difference
    /// between the two no better than the layout does.
    fn pane_widgets(&self) -> HashMap<Id, gtk::Widget> {
        let imp = self.imp();
        let mut widgets: HashMap<Id, gtk::Widget> = imp
            .terminals
            .borrow()
            .iter()
            .map(|(id, terminal)| (*id, terminal.clone().upcast()))
            .collect();
        widgets.extend(
            imp.editors
                .borrow()
                .iter()
                .map(|(id, editor)| (*id, editor.clone().upcast())),
        );
        widgets.extend(
            imp.diffs
                .borrow()
                .iter()
                .map(|(id, diff)| (*id, diff.clone().upcast())),
        );
        widgets.extend(
            imp.hosts
                .borrow()
                .iter()
                .map(|(id, hosts)| (*id, hosts.clone().upcast())),
        );
        // Last, so a connection's bar replaces the bare terminal already
        // entered for it above.
        widgets.extend(
            imp.remotes
                .borrow()
                .iter()
                .map(|(id, remote)| (*id, remote.clone().upcast())),
        );
        widgets
    }

    /// Follows one terminal: its title names the tab and the project, its
    /// working directory is where the next shell starts, its bell marks the tab
    /// when it is not the one on screen, clicking into it moves the focus ring,
    /// and its shell's death closes the pane.
    fn watch_terminal(&self, terminal: &TuniTerminal, project: Id, tab: Id, pane: Id) {
        terminal.connect_notify_local(
            Some("title"),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |terminal: &TuniTerminal, _| {
                    let title = terminal.title();
                    if let Some(entry) = this
                        .imp()
                        .workspace
                        .borrow_mut()
                        .project_mut(project)
                        .and_then(|project| project.pane_mut(tab, pane))
                    {
                        entry.title = title;
                    }
                    this.refresh();
                }
            ),
        );

        terminal.connect_notify_local(
            Some("cwd"),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |terminal: &TuniTerminal, _| {
                    let cwd = terminal.cwd();
                    if let Some(entry) = this
                        .imp()
                        .workspace
                        .borrow_mut()
                        .project_mut(project)
                        .and_then(|project| project.pane_mut(tab, pane))
                    {
                        // A remote shell's OSC 7 names a directory on the
                        // machine it is running on, and a bare path carries no
                        // host to tell it apart from one here. Taking it would
                        // drag the file tree and everything beside it to a
                        // local directory that happens to have the same name.
                        if is_remote(&entry.content) {
                            return;
                        }
                        entry.directory = cwd;
                    }
                    this.refresh();
                }
            ),
        );

        // The keyboard is GTK's to give; the model is told where it went rather
        // than asked to move it, which is what keeps a click and a shortcut
        // ending in the same place.
        terminal.connect_notify_local(
            Some("has-focus"),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |terminal: &TuniTerminal, _| {
                    if terminal.has_focus() {
                        this.pane_focused(project, tab, pane);
                    }
                }
            ),
        );

        terminal.connect_closure(
            "bell",
            false,
            glib::closure_local!(
                #[weak(rename_to = this)]
                self,
                move |terminal: TuniTerminal| {
                    if let Some(page) = this.imp().pages.borrow().get(&tab)
                        && !page.is_selected()
                    {
                        page.set_needs_attention(true);
                    }
                    // A bell in the pane being worked in has already been
                    // heard. One from a pane that is out of sight — another
                    // tab, another window on top — is the case the desktop is
                    // for.
                    if !terminal.has_focus() || !this.is_active() {
                        let where_from =
                            terminal.title().unwrap_or_else(|| "a terminal".to_owned());
                        this.notify_desktop(pane, "Bell", &where_from);
                    }
                }
            ),
        );

        terminal.connect_closure(
            "desktop-notify",
            false,
            glib::closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: TuniTerminal, title: String, body: String| {
                    this.notify_desktop(pane, &title, &body);
                }
            ),
        );

        terminal.connect_closure(
            "exited",
            false,
            glib::closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: TuniTerminal| this.session_ended(project, tab, pane)
            ),
        );
    }

    /// A pane's session ended.
    ///
    /// A shell that exits closes its pane, because that is what typing `exit`
    /// asks for. A connection that ends does not: a mistyped hostname, a
    /// refused key or a dropped link leaves an explanation on the screen, and
    /// closing the pane throws the explanation away with it.
    fn session_ended(&self, project: Id, tab: Id, pane: Id) {
        let Some(Content::Ssh { alias }) = self.pane_content(project, tab, pane) else {
            self.close_pane(project, tab, pane);
            return;
        };
        let remote = self.imp().remotes.borrow().get(&pane).cloned();
        if let Some(remote) = remote {
            remote.set_idle(&format!("Disconnected from {alias}"), "Reconnect");
        }
    }

    /// A pane took the keyboard: move the ring, and rename the tab, which is
    /// named by whichever pane is being worked in.
    fn pane_focused(&self, project: Id, tab: Id, pane: Id) {
        let imp = self.imp();
        let changed = {
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            if layout.focused() == pane {
                return;
            }
            layout.focus(pane);
            layout.focused() == pane
        };
        if !changed {
            return;
        }
        if let Some(grid) = imp.grids.borrow().get(&tab) {
            grid.set_focused(pane);
        }
        self.refresh();
        // The panel follows whichever pane is being worked in, so moving
        // between two shells in different repositories moves the tree.
        self.sync_files();
        // Whatever this pane was asking for attention about is now being
        // looked at, so the banner has done its job.
        if let Some(app) = self.application() {
            notify::withdraw(app.upcast_ref(), pane);
        }
    }

    /// Hands a pane's notification to the desktop.
    fn notify_desktop(&self, pane: Id, title: &str, body: &str) {
        let Some(app) = self.application() else {
            return;
        };
        notify::post(app.upcast_ref(), pane, title, body);
    }

    /// Forgets one pane's widgets and hangs up its shell.
    ///
    /// Every close path goes through here rather than repeating the three
    /// registry removals, because a registry emptied on only some of them keeps
    /// the widget alive for the life of the window — and `poll_diffs` visits
    /// every diff it finds there, so a forgotten one keeps shelling out to git
    /// every two seconds for a pane nobody can see.
    fn forget_pane(&self, pane: Id) {
        let imp = self.imp();
        imp.diffs.borrow_mut().remove(&pane);
        if let Some(terminal) = imp.terminals.borrow_mut().remove(&pane) {
            terminal.shutdown();
            crate::debug::watch(&terminal, "TuniTerminal", pane.raw());
        }
        imp.editors.borrow_mut().remove(&pane);
        imp.remotes.borrow_mut().remove(&pane);
        imp.hosts.borrow_mut().remove(&pane);
    }

    /// Closes one pane, and the tab with it when it was the last one.
    fn close_pane(&self, project: Id, tab: Id, pane: Id) {
        let imp = self.imp();
        self.forget_pane(pane);
        let alive = {
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            layout.remove(pane)
        };

        if !alive {
            let page = imp.pages.borrow().get(&tab).cloned();
            if let (Some(view), Some(page)) = (imp.views.borrow().get(&project).cloned(), page) {
                view.close_page(&page);
            }
            return;
        }
        self.rebuild_grid(project, tab);
        self.refresh();
        self.focus_pane();
    }

    fn close_focused_pane(&self) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        let Some(pane) = self.focused_pane() else {
            return;
        };
        let editor = self.imp().editors.borrow().get(&pane).cloned();
        match editor.filter(TuniEditor::is_dirty) {
            Some(editor) => self.ask_before_discarding(&[editor], move |window, go_ahead| {
                if go_ahead {
                    window.close_pane(project, tab, pane);
                }
            }),
            None => self.close_pane(project, tab, pane),
        }
    }

    // --- unsaved work ------------------------------------------------------

    /// Every file open in the window with edits that are not on disk.
    fn dirty_editors(&self) -> Vec<TuniEditor> {
        self.imp()
            .editors
            .borrow()
            .values()
            .filter(|editor| editor.is_dirty())
            .cloned()
            .collect()
    }

    /// The files with unsaved edits inside one tab.
    fn dirty_editors_in(&self, tab: Id) -> Vec<TuniEditor> {
        let imp = self.imp();
        let workspace = imp.workspace.borrow();
        let Some(entry) = workspace
            .projects()
            .iter()
            .find_map(|project| project.tab(tab))
        else {
            return Vec::new();
        };
        let editors = imp.editors.borrow();
        entry
            .layout()
            .panes()
            .filter_map(|pane| editors.get(&pane.id()))
            .filter(|editor| editor.is_dirty())
            .cloned()
            .collect()
    }

    /// Asks before anything with unsaved work in it goes away, and does what
    /// the answer says. Save writes the files and carries on; Discard carries
    /// on without writing; Cancel does neither.
    /// Asks, then hands the answer on: `true` to go ahead without the edits,
    /// `false` to leave everything as it was. Both are answered, always — what
    /// asked may be holding something open until it hears back.
    fn ask_before_discarding<F>(&self, dirty: &[TuniEditor], answer: F)
    where
        F: Fn(&Self, bool) + 'static,
    {
        let dialog = adw::AlertDialog::new(Some("Save Changes?"), Some(&unsaved_message(dirty)));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("discard", "Discard");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let dirty = dirty.to_vec();
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    let go_ahead = match response {
                        "save" => {
                            for editor in &dirty {
                                editor.save();
                            }
                            // A file that could not be written says so in its
                            // own banner and stays open; going ahead here would
                            // throw away exactly what the question was about.
                            !dirty.iter().any(TuniEditor::is_dirty)
                        }
                        "discard" => true,
                        _ => false,
                    };
                    answer(&this, go_ahead);
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// The window is closing with unsaved work in it. `true` when the close
    /// should stop and wait for the answer.
    fn ask_about_unsaved(&self) -> bool {
        let dirty = self.dirty_editors();
        if dirty.is_empty() {
            return false;
        }
        self.ask_before_discarding(&dirty, |window, go_ahead| {
            if go_ahead {
                window.imp().closing.set(true);
                window.close();
            }
        });
        true
    }

    /// Answers the strip's question about closing a tab: straight through
    /// unless a file in it has edits nothing has written.
    fn confirm_tab_close(&self, view: &adw::TabView, page: &adw::TabPage) -> glib::Propagation {
        let Some(tab) = self.tab_of(&page.child()) else {
            return glib::Propagation::Proceed;
        };
        let dirty = self.dirty_editors_in(tab);
        if dirty.is_empty() {
            return glib::Propagation::Proceed;
        }

        let view = view.clone();
        let page = page.clone();
        // The strip is holding the page open until it is told; every path out
        // of the question has to tell it, including the one that changes
        // nothing — a tab never answered for stays half-closed and refuses to
        // close again.
        self.ask_before_discarding(&dirty, move |_, go_ahead| {
            view.close_page_finish(&page, go_ahead);
        });
        glib::Propagation::Stop
    }

    /// The strip has removed a tab: drop it from the model and hang up every
    /// shell that was in it.
    fn tab_detached(&self, project: Id, position: i32) {
        let imp = self.imp();
        let removed = {
            let mut workspace = imp.workspace.borrow_mut();
            let Some(entry) = workspace.project_mut(project) else {
                // The project itself is going away; its shells were hung up
                // with it.
                return;
            };
            let Some(id) = entry
                .tabs()
                .get(position.max(0) as usize)
                .map(tuni_core::workspace::Tab::id)
            else {
                return;
            };
            entry.remove_tab(id)
        };
        let Some(tab) = removed else {
            return;
        };

        for pane in tab.layout().panes() {
            self.forget_pane(pane.id());
        }
        imp.grids.borrow_mut().remove(&tab.id());
        imp.pages.borrow_mut().remove(&tab.id());
        self.refresh();
    }

    // --- panes -------------------------------------------------------------

    /// Runs something over the selected tab's layout that changes only which
    /// pane is focused or how big the tiles are — no rebuild needed, since the
    /// tiles are laid out from the weights the widgets already hold.
    fn navigate(&self, change: impl FnOnce(&mut Layout)) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        let (zoomed, focused) = {
            let imp = self.imp();
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            let was_zoomed = layout.is_zoomed();
            change(layout);
            (was_zoomed != layout.is_zoomed(), layout.focused())
        };
        // Navigation lets go of zoom, and that does change what is on screen.
        if zoomed {
            self.rebuild_grid(project, tab);
        } else if let Some(grid) = self.imp().grids.borrow().get(&tab) {
            grid.set_focused(focused);
        }
        self.refresh();
        self.focus_pane();
    }

    fn focus_toward(&self, edge: Edge) {
        self.navigate(|layout| layout.focus_toward(edge));
    }

    /// Runs something that changes the tiles' sizes but not the layout's shape.
    fn reshape(&self, change: impl FnOnce(&mut Layout)) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        {
            let imp = self.imp();
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            change(layout);
        }
        // Weights live in the widgets as well as in the model, so a size change
        // is the one thing that has to be drawn again rather than nudged.
        self.rebuild_grid(project, tab);
        self.focus_pane();
    }

    fn resize(&self, edge: Edge) {
        self.reshape(|layout| layout.resize(edge));
    }

    fn toggle_zoom(&self) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        {
            let imp = self.imp();
            let mut workspace = imp.workspace.borrow_mut();
            let Some(layout) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
                .map(Tab::layout_mut)
            else {
                return;
            };
            layout.toggle_zoom();
        }
        self.rebuild_grid(project, tab);
        self.focus_pane();
    }

    /// The strip has changed which tab is on screen.
    fn tab_selected(&self, project: Id, view: &adw::TabView) {
        let imp = self.imp();
        let page = view.selected_page();
        if let Some(page) = &page {
            page.set_needs_attention(false);
        }
        let selected = page
            .as_ref()
            .and_then(|page| self.tab_of(&page.child()))
            .filter(|_| view.n_pages() > 0);

        if let Some(entry) = imp.workspace.borrow_mut().project_mut(project) {
            entry.select(selected);
        }
        if let Some(tab) = selected {
            self.mark_recent(tab);
        }
        self.refresh();
        self.sync_files();
        self.focus_pane();
    }

    fn shift_tab(&self, offset: i32) {
        let Some(view) = self.selected_view() else {
            return;
        };
        let count = view.n_pages();
        if count == 0 {
            return;
        }
        let current = view
            .selected_page()
            .map_or(0, |page| view.page_position(&page));
        // Wrapping, because a strip is a ring: stopping at the last tab makes
        // the shortcut feel broken rather than safe.
        let next = (current + offset).rem_euclid(count);
        view.set_selected_page(&view.nth_page(next));
    }

    /// Alt+1 through Alt+9, with the ninth meaning the last tab however many
    /// there are — the convention every tabbed terminal follows.
    fn select_tab_at(&self, number: i32) {
        let Some(view) = self.selected_view() else {
            return;
        };
        let count = view.n_pages();
        if count == 0 {
            return;
        }
        let index = if number >= 9 {
            count - 1
        } else {
            (number - 1).clamp(0, count - 1)
        };
        view.set_selected_page(&view.nth_page(index));
    }

    fn set_tab_name(&self, page: &adw::TabPage, name: Option<String>) {
        let Some(tab) = self.tab_of(&page.child()) else {
            return;
        };
        let Some(project) = self.imp().workspace.borrow().selected_id() else {
            return;
        };
        let title = {
            let mut workspace = self.imp().workspace.borrow_mut();
            let Some(entry) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
            else {
                return;
            };
            entry.custom_name = name;
            entry.name().to_owned()
        };
        page.set_title(&title);
        self.refresh();
    }

    // --- lookups -----------------------------------------------------------

    fn selected_view(&self) -> Option<adw::TabView> {
        let id = self.imp().workspace.borrow().selected_id()?;
        self.imp().views.borrow().get(&id).cloned()
    }

    fn selected_page(&self) -> Option<(adw::TabView, adw::TabPage)> {
        let view = self.selected_view()?;
        let page = view.selected_page()?;
        Some((view, page))
    }

    /// The tab a page's child belongs to. Tab ids are unique across projects,
    /// so the whole map is the right thing to search.
    fn tab_of(&self, child: &gtk::Widget) -> Option<Id> {
        self.imp()
            .grids
            .borrow()
            .iter()
            .find(|(_, grid)| grid.upcast_ref::<gtk::Widget>() == child)
            .map(|(id, _)| *id)
    }

    /// The project and tab on screen.
    fn selected_tab(&self) -> Option<(Id, Id)> {
        let workspace = self.imp().workspace.borrow();
        let project = workspace.selected_project()?;
        Some((project.id(), project.selected_id()?))
    }

    /// The pane holding the keyboard in the tab on screen.
    fn focused_pane(&self) -> Option<Id> {
        let workspace = self.imp().workspace.borrow();
        Some(
            workspace
                .selected_project()?
                .selected_tab()?
                .layout()
                .focused(),
        )
    }

    /// The terminal on screen, for the smoke captures and for focus.
    #[must_use]
    pub fn active_terminal(&self) -> Option<TuniTerminal> {
        let pane = self.focused_pane()?;
        self.imp().terminals.borrow().get(&pane).cloned()
    }

    /// Opens the panel on a page, or closes it if that page is already the one
    /// showing.
    fn show_panel(&self, page: &str) {
        let imp = self.imp();
        let showing = imp
            .panel
            .borrow()
            .as_ref()
            .is_some_and(adw::OverlaySplitView::shows_sidebar);
        let already = imp
            .panel_view
            .borrow()
            .as_ref()
            .is_some_and(|panel| panel.page() == page);

        if showing && already {
            if let Some(panel) = imp.panel.borrow().as_ref() {
                panel.set_show_sidebar(false);
            }
            self.focus_pane();
            return;
        }

        if let Some(panel) = imp.panel_view.borrow().as_ref() {
            panel.set_page(page);
        }
        if let Some(panel) = imp.panel.borrow().as_ref() {
            panel.set_show_sidebar(true);
        }
        self.sync_files();
    }

    /// What a Find command acts on: the pane with the keyboard decides, and a
    /// diff pane decides on nothing.
    fn find_target(&self) -> Option<Search> {
        if let Some(editor) = self.active_editor() {
            return Some(Search::Editor(editor));
        }
        let find = self.imp().find.borrow().clone()?;
        Some(Search::Terminal(find, self.active_terminal()?))
    }

    /// Walks to the next match in the focused pane, or the previous one.
    ///
    /// Over a terminal whose bar is down this opens it first, on the term it
    /// was last searching for — asking for the next match is asking to carry on
    /// with a search, and the bar is where that search lives.
    fn find_step(&self, forward: bool) {
        match self.find_target() {
            Some(Search::Terminal(find, terminal)) => {
                if find.targets(&terminal) {
                    find.step_match(forward);
                } else {
                    find.open(&terminal);
                }
            }
            Some(Search::Editor(editor)) => editor.step_match(forward),
            None => {}
        }
    }

    /// Hands the keyboard to whatever the focused pane holds.
    fn focus_pane(&self) {
        let Some(pane) = self.focused_pane() else {
            return;
        };
        let imp = self.imp();
        if let Some(editor) = imp.editors.borrow().get(&pane) {
            editor.focus_text();
            return;
        }
        if let Some(terminal) = imp.terminals.borrow().get(&pane) {
            terminal.grab_focus();
        }
    }

    /// The editor in the focused pane, if that is what the pane holds.
    /// Quits without asking about files with unsaved edits. A scripted close
    /// has already made that decision, and there is nobody there to answer the
    /// question.
    pub(crate) fn force_close(&self) {
        self.imp().closing.set(true);
        self.close();
    }

    /// The diff in the focused pane, when the focused pane is one.
    pub(crate) fn active_diff(&self) -> Option<TuniDiff> {
        let pane = self.focused_pane()?;
        self.imp().diffs.borrow().get(&pane).cloned()
    }

    // --- the tab switcher ----------------------------------------------------

    /// The keys the switcher lives on, watched before anything else sees them.
    ///
    /// Not an accelerator, because an accelerator cannot see a modifier being
    /// let go, and letting go of `Ctrl` is the whole gesture: the switcher stays
    /// up across as many `Tab` presses as are held for, and commits on release.
    fn install_switcher(&self) {
        let keys = gtk::EventControllerKey::new();
        // The capture phase, so the pane with the keyboard never sees a `Tab`
        // meant for the switcher.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| this.switcher_key(key, state)
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, key, _, _| {
                if matches!(key, gdk::Key::Control_L | gdk::Key::Control_R) {
                    this.switcher_commit();
                }
            }
        ));
        self.add_controller(keys);
    }

    fn switcher_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let Some(switcher) = self.imp().switcher.borrow().clone() else {
            return glib::Propagation::Proceed;
        };
        // Escape is only the switcher's while it is up; the rest of the time it
        // belongs to whatever is in the pane.
        if switcher.is_open() && key == gdk::Key::Escape {
            switcher.close();
            return glib::Propagation::Stop;
        }
        if !matches!(
            key,
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::KP_Tab
        ) {
            return glib::Propagation::Proceed;
        }
        // `Ctrl+Alt+Tab` is the desktop's own, and `Ctrl+Shift+Tab` steps back.
        if !state.contains(gdk::ModifierType::CONTROL_MASK)
            || state.contains(gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let forward = !state.contains(gdk::ModifierType::SHIFT_MASK);

        if switcher.is_open() {
            switcher.step(forward);
            return glib::Propagation::Stop;
        }
        let cards = self.switcher_cards();
        if cards.len() < 2 {
            return glib::Propagation::Proceed;
        }
        // Opening lands on the tab before this one, which is what one press and
        // release is for: the way back to where the work just was.
        let start = if forward { 1 } else { cards.len() - 1 };
        switcher.open(&cards, start);
        glib::Propagation::Stop
    }

    /// Selects whatever the highlight is on and puts the switcher away.
    fn switcher_commit(&self) {
        let Some(switcher) = self.imp().switcher.borrow().clone() else {
            return;
        };
        if !switcher.is_open() {
            return;
        }
        let chosen = switcher.highlighted();
        switcher.close();
        if let Some(tab) = chosen
            && let (Some(view), Some(page)) = (
                self.selected_view(),
                self.imp().pages.borrow().get(&tab).cloned(),
            )
        {
            view.set_selected_page(&page);
        }
    }

    /// The tabs of the project in front, most recently worked in first, with a
    /// picture of what is in each.
    fn switcher_cards(&self) -> Vec<Card> {
        let imp = self.imp();
        let ordered = {
            let workspace = imp.workspace.borrow();
            let Some(project) = workspace.selected_project() else {
                return Vec::new();
            };
            let recent = imp.recent.borrow();
            let mut ordered: Vec<Id> = recent
                .iter()
                .copied()
                .filter(|id| project.tabs().iter().any(|tab| tab.id() == *id))
                .collect();
            // A tab nobody has been in yet is not in the recency list at all, so
            // the strip order fills in behind it.
            let rest: Vec<Id> = project
                .tabs()
                .iter()
                .map(Tab::id)
                .filter(|id| !ordered.contains(id))
                .collect();
            ordered.extend(rest);
            ordered
        };

        ordered
            .into_iter()
            .filter_map(|id| self.switcher_card(id))
            .collect()
    }

    fn switcher_card(&self, tab: Id) -> Option<Card> {
        let imp = self.imp();
        let (title, pane, content) = {
            let workspace = imp.workspace.borrow();
            let entry = workspace
                .projects()
                .iter()
                .find_map(|project| project.tabs().iter().find(|entry| entry.id() == tab))?;
            let pane = entry.layout().focused_pane()?;
            (entry.name().to_owned(), pane.id(), pane.content.clone())
        };

        let (icon, preview) = match &content {
            Content::Terminal => (
                "utilities-terminal-symbolic",
                imp.terminals
                    .borrow()
                    .get(&pane)
                    // A card is a picture of a screen, so it holds a screen's
                    // worth of lines and no more.
                    .map(|terminal| terminal.preview(28))
                    .unwrap_or_default(),
            ),
            Content::Ssh { alias } => (
                "network-server-symbolic",
                imp.terminals
                    .borrow()
                    .get(&pane)
                    .map(|terminal| terminal.preview(28))
                    .unwrap_or_else(|| alias.clone()),
            ),
            Content::Hosts => ("network-server-symbolic", "Connect".to_owned()),
            Content::File(path) => ("text-x-generic-symbolic", shorten(&path.to_string_lossy())),
            Content::Diff { path, staged } => (
                "media-record-symbolic",
                format!(
                    "{}{}",
                    shorten(&path.to_string_lossy()),
                    if *staged { " (staged)" } else { "" }
                ),
            ),
        };

        Some(Card {
            tab,
            title,
            icon,
            preview,
        })
    }

    /// A `Ctrl+Tab` on the harness's behalf, down the same path a real one
    /// takes: the gesture the switcher lives on is a held modifier, and nothing
    /// scripted can hold a key.
    pub(crate) fn switcher_press(&self, forward: bool) {
        let mut state = gdk::ModifierType::CONTROL_MASK;
        if !forward {
            state |= gdk::ModifierType::SHIFT_MASK;
        }
        let _ = self.switcher_key(gdk::Key::Tab, state);
    }

    /// Releases the modifier on the harness's behalf.
    pub(crate) fn switcher_finish(&self) {
        self.switcher_commit();
    }

    /// The tab the highlight is on, by name, for the harness to check.
    #[must_use]
    pub(crate) fn switcher_highlight(&self) -> String {
        let chosen = self
            .imp()
            .switcher
            .borrow()
            .as_ref()
            .and_then(TuniSwitcher::highlighted);
        let workspace = self.imp().workspace.borrow();
        workspace
            .projects()
            .iter()
            .flat_map(|project| project.tabs())
            .find(|tab| Some(tab.id()) == chosen)
            .map(|tab| tab.name().to_owned())
            .unwrap_or_default()
    }

    /// Remembers that a tab has just been worked in, which is the order the
    /// switcher walks.
    fn mark_recent(&self, tab: Id) {
        let mut recent = self.imp().recent.borrow_mut();
        recent.retain(|id| *id != tab);
        recent.insert(0, tab);
    }

    // --- the command palette -----------------------------------------------

    /// Brings a pane on screen wherever it is: its project, then its tab within
    /// that project, then the pane within that tab.
    fn reveal_pane(&self, pane: Id) {
        let imp = self.imp();
        let found = {
            let workspace = imp.workspace.borrow();
            workspace.projects().iter().find_map(|project| {
                project.tabs().iter().find_map(|tab| {
                    tab.layout()
                        .panes()
                        .any(|candidate| candidate.id() == pane)
                        .then_some((project.id(), tab.id()))
                })
            })
        };
        let Some((project, tab)) = found else {
            return;
        };

        if imp.workspace.borrow().selected_id() != Some(project) {
            // Taken as its own statement: an `if let` holds its scrutinee's
            // temporaries for the whole body, so borrowing here and asking for
            // the mutable borrow inside would panic on the one path that
            // reaches it — revealing a pane that lives in another project.
            let index = imp.workspace.borrow().index_of(project);
            if let Some(index) = index {
                imp.workspace.borrow_mut().select_index(index);
            }
            self.show_selected_project();
        }
        let page = imp.pages.borrow().get(&tab).cloned();
        if let (Some(view), Some(page)) = (self.selected_view(), page) {
            view.set_selected_page(&page);
        }
        self.focus_pane_at(tab, pane);
    }

    fn show_palette(&self) {
        crate::palette::present(self, self.palette_entries());
    }

    /// What the palette lists: the window's own actions, every host ssh knows
    /// about, the projects around this one, and every terminal in the
    /// workspace.
    fn palette_entries(&self) -> Vec<palette::Entry> {
        use palette::Entry;

        let mut entries: Vec<Entry> = COMMANDS
            .iter()
            .map(|(title, icon, shortcut, action)| Entry::command(title, icon, *shortcut, action))
            .collect();

        // Reading the configuration is files and no subprocess, and the
        // palette opens on a keystroke rather than on a timer, so this stays
        // on this thread.
        for host in tuni_core::ssh::Hosts::load().all() {
            entries.push(palette::Entry {
                // Widened so that typing the address finds the alias, and so
                // that "ssh" alone lists everything connectable.
                search: format!("{} {} ssh connect", host.alias, host.address()),
                title: host.alias.clone(),
                subtitle: Some(host.address()),
                icon: "network-server-symbolic",
                shortcut: None,
                action: "win.connect",
                target: Some(host.alias.to_variant()),
                terminal: false,
            });
        }

        let workspace = self.imp().workspace.borrow();
        let selected = workspace.selected_id();
        for (index, project) in workspace.projects().iter().enumerate() {
            if Some(project.id()) == selected {
                entries.push(
                    Entry::command(
                        &format!("Close Project: {}", project.name()),
                        "window-close-symbolic",
                        None,
                        "win.close-project",
                    )
                    .with_target(project.id().raw().to_variant()),
                );
                continue;
            }
            entries.push(
                Entry::command(
                    &format!("Switch to Project: {}", project.name()),
                    "folder-symbolic",
                    None,
                    "win.select-project",
                )
                // The action counts projects from one, the way the keys that
                // select them do.
                .with_target((index as i32 + 1).to_variant()),
            );
        }

        for project in workspace.projects() {
            for tab in project.tabs() {
                for pane in tab.layout().panes() {
                    // Only shells: a file pane is reached through the tree
                    // beside it, and it is the terminals that scatter across
                    // projects until nobody can find them.
                    if !matches!(pane.content, Content::Terminal | Content::Ssh { .. }) {
                        continue;
                    }
                    let title = pane
                        .title
                        .clone()
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| tab.name().to_owned());
                    let directory = pane.directory.as_deref().map(shorten);
                    entries.push(palette::Entry {
                        search: format!(
                            "{title} {} {}",
                            project.name(),
                            directory.as_deref().unwrap_or_default()
                        ),
                        title,
                        subtitle: directory,
                        icon: if is_remote(&pane.content) {
                            "network-server-symbolic"
                        } else {
                            "utilities-terminal-symbolic"
                        },
                        shortcut: None,
                        action: "win.reveal-pane",
                        target: Some(pane.id().raw().to_variant()),
                        terminal: true,
                    });
                }
            }
        }
        entries
    }

    /// The find bar, for the smoke captures.
    #[must_use]
    pub(crate) fn find_bar(&self) -> Option<TuniFind> {
        self.imp().find.borrow().clone()
    }

    pub(crate) fn active_editor(&self) -> Option<TuniEditor> {
        let pane = self.focused_pane()?;
        self.imp().editors.borrow().get(&pane).cloned()
    }

    // --- rendering the model -----------------------------------------------

    /// Titles, names, and which of the two empty states is showing. Cheap
    /// enough to call on every change.
    fn refresh(&self) {
        let imp = self.imp();
        let (name, subtitle, tabs, projects) = {
            let workspace = imp.workspace.borrow();
            let project = workspace.selected_project();
            (
                project.map(|project| project.name().to_owned()),
                project
                    .and_then(|project| project.selected_tab())
                    .and_then(|tab| tab.directory().map(shorten)),
                project.is_some_and(|project| !project.is_empty()),
                !workspace.is_empty(),
            )
        };

        if let Some(title) = imp.title.borrow().as_ref() {
            title.set_title(name.as_deref().unwrap_or("Tuni"));
            title.set_subtitle(&subtitle.unwrap_or_default());
        }
        if let Some(content) = imp.content.borrow().as_ref() {
            content.set_visible_child_name(if tabs { "tabs" } else { "empty" });
        }
        if let (Some(status), Some(button)) = (
            imp.status.borrow().as_ref(),
            imp.status_button.borrow().as_ref(),
        ) {
            if projects {
                status.set_title("No Tabs Open");
                status.set_description(Some("Every tab in this project is closed."));
                button.set_label("New Terminal");
                button.set_action_name(Some("win.new-tab"));
            } else {
                status.set_title("No Projects");
                status.set_description(Some("A project is a directory and its terminals."));
                button.set_label("New Project");
                button.set_action_name(Some("win.new-project"));
            }
        }

        // A find bar pointing at a terminal that is no longer the one being
        // worked in is searching something nobody is looking at. Everything that
        // moves the focus — another pane, another tab, another project — comes
        // through here.
        let find = imp.find.borrow().clone();
        if let Some(find) = find {
            find.close_unless(self.active_terminal().as_ref());
        }

        self.refresh_names();
    }

    fn refresh_names(&self) {
        let imp = self.imp();
        let workspace = imp.workspace.borrow();
        for (label, project) in imp.labels.borrow().iter().zip(workspace.projects()) {
            label.set_text(project.name());
        }

        // A tab is named by the pane being worked in, so a title arriving in
        // any pane can be the one the strip should show.
        let pages = imp.pages.borrow();
        for project in workspace.projects() {
            for tab in project.tabs() {
                let Some(page) = pages.get(&tab.id()) else {
                    continue;
                };
                // A file with unsaved edits marks its tab, so a tab in the
                // strip behind the one being worked in still says so.
                let dirty = {
                    let editors = imp.editors.borrow();
                    tab.layout()
                        .panes()
                        .filter_map(|pane| editors.get(&pane.id()))
                        .any(TuniEditor::is_dirty)
                };
                if dirty {
                    page.set_title(&format!("• {}", tab.name()));
                } else {
                    page.set_title(tab.name());
                }
                page.set_tooltip(&tab.directory().map(shorten).unwrap_or_default());
            }
        }
    }

    /// Rebuilds the sidebar rows. Cheap at the count of projects a person keeps
    /// open, and it keeps row order and model order the same thing.
    fn rebuild_sidebar(&self) {
        let imp = self.imp();
        let Some(list) = imp.sidebar.borrow().clone() else {
            return;
        };

        imp.selecting.set(true);
        // By row rather than by child: the shared context menu is parented to
        // the list too, and it is not a row the list can remove.
        while let Some(row) = list.row_at_index(0) {
            list.remove(&row);
        }
        imp.labels.borrow_mut().clear();

        let projects: Vec<(Id, String)> = imp
            .workspace
            .borrow()
            .projects()
            .iter()
            .map(|project| (project.id(), project.name().to_owned()))
            .collect();

        for (id, name) in projects {
            let (row, label) = self.build_row(id, &name);
            list.append(&row);
            imp.labels.borrow_mut().push(label);
        }
        imp.selecting.set(false);
    }

    fn build_row(&self, id: Id, name: &str) -> (gtk::ListBoxRow, gtk::Label) {
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        let label = gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close Project")
            .valign(gtk::Align::Center)
            .build();
        close.add_css_class("flat");
        close.add_css_class("circular");
        close.add_css_class("tuni-row-close");
        close.set_action_name(Some("win.close-project"));
        close.set_action_target_value(Some(&id.raw().to_variant()));

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&icon);
        content.append(&label);
        content.append(&close);

        let row = gtk::ListBoxRow::builder().child(&content).build();

        let press = gtk::GestureClick::new();
        press.set_button(gdk::BUTTON_SECONDARY);
        press.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            row,
            move |_, _, x, y| this.popup_row_menu(&row, x, y)
        ));
        row.add_controller(press);

        // Middle click closes the project, the way it closes a tab in every
        // browser and in the tab bar above: the button on the row is a small
        // target, and reaching for it is a pause in something else.
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        middle.connect_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            row,
            move |_, _, x, y| {
                // A press that ends somewhere else is a press taken back.
                if row.contains(x, y) {
                    this.close_project(id);
                }
            }
        ));
        row.add_controller(middle);

        // Dragging a row up or down reorders the sidebar, as it does in kero.
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(glib::clone!(
            #[weak]
            row,
            #[upgrade_or]
            None,
            move |_, _, _| {
                let index = u32::try_from(row.index()).ok()?;
                Some(gdk::ContentProvider::for_value(&index.to_value()))
            }
        ));
        row.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        target.connect_drop(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            row,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                let Ok(from) = value.get::<u32>() else {
                    return false;
                };
                this.move_project(from as usize, row.index().max(0) as usize);
                true
            }
        ));
        row.add_controller(target);

        (row, label)
    }

    fn move_project(&self, from: usize, to: usize) {
        let imp = self.imp();
        let id = imp
            .workspace
            .borrow()
            .projects()
            .get(from)
            .map(tuni_core::workspace::Project::id);
        let Some(id) = id else {
            return;
        };
        imp.workspace.borrow_mut().move_project(id, to);
        self.rebuild_sidebar();
        self.show_selected_project();
    }

    fn popup_row_menu(&self, row: &gtk::ListBoxRow, x: f64, y: f64) {
        let imp = self.imp();
        let index = row.index().max(0) as usize;
        let Some((id, named, pinned)) = imp.workspace.borrow().projects().get(index).map(|p| {
            (
                p.id(),
                p.custom_name.is_some(),
                p.custom_directory.is_some(),
            )
        }) else {
            return;
        };
        let Some(menu) = imp.row_menu.borrow().clone() else {
            return;
        };

        menu.set_menu_model(Some(&project_menu(id, named, pinned)));
        // The gesture reports the click in the row's coordinates; the popover
        // is parented to the list, which is where it has to point.
        let point = row
            .compute_point(
                imp.sidebar.borrow().as_ref().unwrap(),
                &gtk::graphene::Point::new(x as f32, y as f32),
            )
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        crate::menu::popup_at(&menu, point);
    }

    // --- dialogs -----------------------------------------------------------

    fn rename_project(&self, id: Id) {
        let current = self
            .imp()
            .workspace
            .borrow()
            .project(id)
            .map(|project| project.name().to_owned())
            .unwrap_or_default();

        self.ask_for_name("Rename Project", &current, move |window, name| {
            if let Some(project) = window.imp().workspace.borrow_mut().project_mut(id) {
                project.custom_name = name;
            }
            window.refresh();
        });
    }

    fn rename_tab(&self) {
        let Some(page) = self.imp().menu_page.borrow().clone() else {
            return;
        };
        let current = page.title().to_string();
        self.ask_for_name("Rename Tab", &current, move |window, name| {
            window.set_tab_name(&page, name);
        });
    }

    /// The rename prompt both menus share. An empty answer means "use the
    /// automatic title" rather than an empty name.
    fn ask_for_name<F>(&self, heading: &str, current: &str, apply: F)
    where
        F: Fn(&Self, Option<String>) + 'static,
    {
        let entry = gtk::Entry::builder()
            .text(current)
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::new(Some(heading), None);
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("rename", "Rename");
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    if response != "rename" {
                        return;
                    }
                    let text = entry.text().trim().to_owned();
                    apply(&this, (!text.is_empty()).then_some(text));
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn choose_project_directory(&self, id: Id) {
        let dialog = gtk::FileDialog::builder()
            .title("Set Project Directory")
            .accept_label("Choose")
            .build();
        if let Some(current) = self
            .imp()
            .workspace
            .borrow()
            .project(id)
            .and_then(|project| project.custom_directory.clone())
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(current)));
        }

        dialog.select_folder(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |result| {
                    let Some(path) = result.ok().and_then(|folder| folder.path()) else {
                        return;
                    };
                    if let Some(project) = this.imp().workspace.borrow_mut().project_mut(id) {
                        project.custom_directory = Some(path.to_string_lossy().into_owned());
                    }
                }
            ),
        );
    }
}

/// One action, spelled the way `add_action_entries` wants it.
fn entry<F>(
    name: &str,
    parameter: Option<&glib::VariantTy>,
    activate: F,
) -> gio::ActionEntry<TuniWindow>
where
    F: Fn(&TuniWindow, Option<&glib::Variant>) + 'static,
{
    let builder = gio::ActionEntry::builder(name)
        .activate(move |window: &TuniWindow, _, target| activate(window, target));
    match parameter {
        Some(ty) => builder.parameter_type(Some(ty)).build(),
        None => builder.build(),
    }
}

/// Whether a pane holds a connection rather than something local.
///
/// The two places this decides are the ones that answer a request with a pane
/// that is already open. That is right for a file, where opening it twice is
/// two views of one buffer, and wrong for a host: two shells on one machine is
/// an ordinary thing to want, and it is what a second tab is for.
fn is_remote(content: &Content) -> bool {
    matches!(content, Content::Ssh { .. })
}

fn project_target(target: Option<&glib::Variant>) -> Option<Id> {
    target.and_then(glib::Variant::get::<u64>).map(Id::from)
}

/// The context menu on a sidebar row. Built per row because "Use Automatic
/// Title" only belongs there when there is a custom one to drop.
fn project_menu(id: Id, named: bool, pinned: bool) -> gio::Menu {
    let target = id.raw().to_variant();

    let naming = gio::Menu::new();
    naming.append_item(&item("Rename…", "win.rename-project", &target));
    if named {
        naming.append_item(&item(
            "Use Automatic Title",
            "win.automatic-project-title",
            &target,
        ));
    }

    let directory = gio::Menu::new();
    directory.append_item(&item(
        "Set Project Directory…",
        "win.set-project-directory",
        &target,
    ));
    if pinned {
        directory.append_item(&item(
            "Use Automatic Directory",
            "win.automatic-project-directory",
            &target,
        ));
    }

    let closing = gio::Menu::new();
    closing.append_item(&item("Close Project", "win.close-project", &target));

    let menu = gio::Menu::new();
    menu.append_section(None, &naming);
    menu.append_section(None, &directory);
    menu.append_section(None, &closing);
    menu
}

fn item(label: &str, action: &str, target: &glib::Variant) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(target));
    item
}

/// The header bar's menu — everything reachable by keyboard that a pointer
/// should be able to reach too.
fn main_menu() -> gio::Menu {
    let opening = gio::Menu::new();
    opening.append(Some("New Tab"), Some("win.new-tab"));
    opening.append(Some("New Project"), Some("win.new-project"));
    opening.append(Some("New Window"), Some("win.new-window"));

    let panes = gio::Menu::new();
    panes.append(Some("Split Right"), Some("win.split-right"));
    panes.append(Some("Split Down"), Some("win.split-down"));
    panes.append(Some("Zoom Pane"), Some("win.zoom-pane"));
    panes.append(Some("Even Out Panes"), Some("win.equalize-panes"));

    let panels = gio::Menu::new();
    panels.append(Some("Projects"), Some("win.toggle-sidebar"));
    panels.append(Some("Files"), Some("win.show-files"));
    panels.append(Some("Git"), Some("win.show-git"));
    panels.append(Some("Info"), Some("win.show-info"));

    let searching = gio::Menu::new();
    searching.append(Some("Find"), Some("win.find"));
    searching.append(Some("Find and Replace"), Some("win.find-replace"));
    searching.append(
        Some("Use Selection for Find"),
        Some("win.use-selection-for-find"),
    );
    searching.append(Some("Clear Terminal"), Some("win.clear-terminal"));

    let file = gio::Menu::new();
    file.append(Some("Save File"), Some("win.save-file"));

    let application = gio::Menu::new();
    application.append(Some("Preferences"), Some("win.settings"));
    application.append(Some("About Tuni"), Some("win.about"));

    let menu = gio::Menu::new();
    menu.append_section(None, &opening);
    menu.append_section(None, &panes);
    menu.append_section(None, &searching);
    menu.append_section(None, &file);
    menu.append_section(None, &panels);
    menu.append_section(None, &application);
    menu
}

/// The context menu on a tab. The strip tells us which tab it belongs to
/// through `setup-menu`, so none of these carry a target.
fn tab_menu() -> gio::Menu {
    let naming = gio::Menu::new();
    naming.append(Some("Rename…"), Some("win.tab-rename"));
    naming.append(Some("Use Automatic Title"), Some("win.tab-automatic-title"));

    let closing = gio::Menu::new();
    closing.append(Some("Close"), Some("win.tab-close"));
    closing.append(Some("Close Others"), Some("win.tab-close-others"));
    closing.append(Some("Close Tabs to the Right"), Some("win.tab-close-right"));

    let menu = gio::Menu::new();
    menu.append_section(None, &naming);
    menu.append_section(None, &closing);
    menu
}

/// Hands a split view its sidebar with a drag handle down the inner edge.
///
/// `AdwOverlaySplitView` has no divider to take hold of: the sidebar is a
/// fraction of the window, clamped between a minimum and a maximum width, and
/// that is the whole of it. The handle is the missing divider — a few pixels of
/// the sidebar's inner edge that take the pointer — and dragging it pins the
/// clamp shut at the width it lands on, so the sidebar keeps that width instead
/// of going back to tracking the window.
///
/// `minimum` and `maximum` are read in the split's own width unit, the same as
/// the clamp they are written into.
fn add_sidebar_grip(
    split: &adw::OverlaySplitView,
    content: &impl IsA<gtk::Widget>,
    minimum: f64,
    maximum: f64,
) {
    let content = content.as_ref();
    content.set_hexpand(true);

    let grip = gtk::Box::new(gtk::Orientation::Vertical, 0);
    grip.set_size_request(GRIP, -1);
    grip.add_css_class("tuni-sidebar-grip");
    grip.set_cursor_from_name(Some("col-resize"));

    // The handle belongs on the edge the content is on: after a sidebar packed
    // before the content, before one packed after it. Said in packing order
    // rather than in left and right, so a right-to-left desktop puts it on the
    // other side without being asked.
    let sidebar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if split.sidebar_position() == gtk::PackType::Start {
        sidebar.append(content);
        sidebar.append(&grip);
    } else {
        sidebar.append(&grip);
        sidebar.append(content);
    }

    // Where in the handle it was taken hold of, so the pointer keeps the same
    // grip on it however far the drag goes.
    let grabbed = std::rc::Rc::new(Cell::new(0.0_f64));
    let drag = gtk::GestureDrag::new();
    drag.connect_drag_begin(glib::clone!(
        #[strong]
        grabbed,
        move |_, x, _| grabbed.set(x)
    ));
    drag.connect_drag_update(glib::clone!(
        #[strong]
        grabbed,
        #[weak]
        split,
        #[weak]
        grip,
        move |gesture, _, _| {
            // Where the pointer is, rather than how far it has come from where
            // the drag began: the handle travels with the edge it is resizing,
            // so an offset is measured against a start point that has moved out
            // from under it, and a drag on one stalls after a pixel.
            let Some((x, _)) = gesture.point(None) else {
                return;
            };
            // Where the handle starts, which is where the pointer is less the
            // part of the handle it is holding.
            let Some(point) = grip.compute_point(
                &split,
                &gtk::graphene::Point::new((x - grabbed.get()) as f32, 0.0),
            ) else {
                return;
            };
            // That edge is the width, measured from whichever side of the split
            // the sidebar is against.
            let edge = f64::from(point.x());
            let width = if sidebar_on_left(&split) {
                edge + f64::from(grip.width())
            } else {
                f64::from(split.width()) - edge
            };
            pin_sidebar(&split, width, minimum, maximum);
        }
    ));
    grip.add_controller(drag);

    split.set_sidebar(Some(&sidebar));
}

/// Whether a split view's sidebar is the one on the left of the screen, which
/// is what its packing says until a right-to-left desktop says otherwise.
/// Keeps the desktop's window buttons and drops the window icon beside them.
///
/// A KDE session lays its decorations out as `icon:minimize,maximize,close`,
/// and GTK draws that `icon` from the icon theme, which on a Tuni that has
/// never been installed is a missing-icon glyph in the corner of every window.
/// It is not a button either, so the one thing it tells anyone is that
/// something is wrong. The rest of the layout is the desktop's to decide, so
/// only the icon is taken out, and it is taken out again whenever the desktop
/// changes its mind about the layout.
/// What a bug report wants pasted into it. The library versions are the ones
/// running rather than the ones this was built against, and the two variables
/// are the ones that change what gets drawn — a Wayland session and an X11 one
/// do not have the same window to report about.
fn debug_info() -> String {
    let variable = |name: &str| std::env::var(name).unwrap_or_else(|_| "unset".to_owned());
    format!(
        "Tuni {}\nGTK {}.{}.{}\nlibadwaita {}.{}.{}\nXDG_SESSION_TYPE {}\nGSK_RENDERER {}",
        env!("CARGO_PKG_VERSION"),
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version(),
        variable("XDG_SESSION_TYPE"),
        variable("GSK_RENDERER"),
    )
}

fn drop_window_icon(header: &adw::HeaderBar) {
    let settings = header.settings();
    let apply = |header: &adw::HeaderBar, settings: &gtk::Settings| {
        let layout = settings.gtk_decoration_layout().unwrap_or_default();
        header.set_decoration_layout(Some(&without_icon(&layout)));
    };
    apply(header, &settings);
    settings.connect_gtk_decoration_layout_notify(glib::clone!(
        #[weak]
        header,
        move |settings| apply(&header, settings)
    ));
}

/// A GTK decoration layout without its `icon` element: two comma-separated
/// lists of elements, split by the colon that says which side each is on.
fn without_icon(layout: &str) -> String {
    let side = |side: &str| {
        side.split(',')
            .filter(|element| element.trim() != "icon")
            .collect::<Vec<_>>()
            .join(",")
    };
    match layout.split_once(':') {
        Some((start, end)) => format!("{}:{}", side(start), side(end)),
        None => side(layout),
    }
}

fn sidebar_on_left(split: &adw::OverlaySplitView) -> bool {
    (split.sidebar_position() == gtk::PackType::Start)
        != (split.direction() == gtk::TextDirection::Rtl)
}

/// Fixes a split view's sidebar at `pixels` wide, whatever the window does
/// next.
///
/// A width that is at least the minimum and at most the maximum can only be the
/// one width when the two ends of the clamp meet, so shutting the clamp is how
/// a fraction of the window becomes a width. The clamp is read in the split's
/// own unit — scalable pixels, so a desktop set to larger text gets a
/// proportionally wider sidebar — where a drag is measured in real ones, which
/// is what the conversion is for.
fn pin_sidebar(split: &adw::OverlaySplitView, pixels: f64, minimum: f64, maximum: f64) {
    let unit = split.sidebar_width_unit();
    let settings = split.settings();
    let width = unit.from_px(pixels, Some(&settings));
    // Never past half the window: a sidebar is beside the work rather than
    // instead of it, and a clamp is obeyed whether or not there is room for it.
    let room = unit
        .from_px(f64::from(split.width()) / 2.0, Some(&settings))
        .max(minimum);
    set_pinned_width(split, width.clamp(minimum, maximum.min(room)));
}

/// Shuts a split view's clamp on one width, in the split's own unit.
fn set_pinned_width(split: &adw::OverlaySplitView, width: f64) {
    split.set_min_sidebar_width(width);
    split.set_max_sidebar_width(width);
}

/// The width a sidebar was dragged to, or `None` for one still sizing itself
/// off the window.
///
/// A dragged sidebar is the one whose clamp has been shut, and a shut clamp is
/// the one whose minimum has caught up with its maximum — every default here
/// leaves daylight between the two.
fn pinned_width(split: &adw::OverlaySplitView) -> Option<f64> {
    let width = split.max_sidebar_width();
    (split.min_sidebar_width() >= width).then_some(width)
}

/// Tuni's own styling, as opposed to the theme's. Loaded once.
fn load_css() {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }
    let Some(display) = gdk::Display::default() else {
        return;
    };
    PROVIDER.with(|provider| {
        provider.load_from_string(
            "row .tuni-row-close { opacity: 0; min-width: 20px; min-height: 20px; padding: 0; }\n\
             row:hover .tuni-row-close, row:selected .tuni-row-close { opacity: 1; }\n\
             /* The ring is only drawn once a tab is split: a lone pane is the\n\
                whole tab, and outlining it says nothing. */\n\
             .tuni-pane.ringed { border-radius: 8px; border: 1px solid alpha(currentColor, 0.14); }\n\
             .tuni-pane.ringed.focused { border-color: alpha(@accent_color, 0.85); }\n\
             .tuni-pane > .tuni-grip { min-height: 6px; opacity: 0; }\n\
             .tuni-pane.ringed:hover > .tuni-grip { opacity: 0.35; \
              background-image: radial-gradient(circle, currentColor 1px, transparent 1px); \
              background-size: 4px 4px; }\n\
             /* Invisible until it is pointed at, like the edge of a window:\n\
                the sidebar already has a border there, and a second line\n\
                drawn over it would only say the same thing twice. */\n\
             .tuni-sidebar-grip:hover { background-color: alpha(@accent_color, 0.5); }\n\
             .tuni-drop { background-color: alpha(@accent_color, 0.28); border-radius: 6px; }\n\
             .tuni-switcher { border-radius: 26px; padding: 8px; }\n\
             .tuni-switch-card { padding: 9px 9px 14px 9px; border-radius: 16px; }\n\
             .tuni-switch-card.selected { background-color: alpha(currentColor, 0.20); }\n\
             .tuni-switch-preview { border-radius: 12px; padding: 4px; \
              border: 1px solid alpha(currentColor, 0.16); \
              background-color: alpha(@window_bg_color, 0.7); }\n\
             /* Small enough that a screen's worth of terminal fits the card,\n\
                which is what makes a thumbnail readable as itself. */\n\
             .tuni-switch-preview label { font-size: 5pt; }\n",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// Whether this run reads and writes the saved session at all.
///
/// `TUNI_SESSION=off` opens an empty window and leaves the saved one exactly
/// as it was, which is what the smoke captures want: a capture that restored
/// yesterday's window would be a picture of yesterday, and one that saved its
/// own would throw away a real session.
#[must_use]
pub fn session_enabled() -> bool {
    !std::env::var("TUNI_SESSION")
        .is_ok_and(|value| matches!(value.trim(), "0" | "off" | "no" | "false"))
}

/// Tell libadwaita which appearance was asked for.
///
/// `System` is [`adw::ColorScheme::Default`] rather than a guess at what the
/// desktop currently wants: the desktop is allowed to change its mind, and
/// `Default` is how an application says it will follow.
pub fn apply_appearance(appearance: Appearance) {
    adw::StyleManager::default().set_color_scheme(match appearance {
        Appearance::System => adw::ColorScheme::Default,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    });
}

/// Paint the window chrome from the terminal's theme.
///
/// libadwaita builds its whole stylesheet out of named colors, so overriding
/// those recolors the header bar, dialogs, and popovers at once, and it keeps
/// working as libadwaita adds widgets. One provider, reloaded, so switching
/// themes does not stack stylesheets on the display.
///
/// The window background is the only color the opacity setting reaches, and it
/// is the only one that may be translucent: the shades libadwaita paints on top
/// of it are shades of the same color, and two translucent layers of one color
/// leave that corner of the window more solid than the rest. Below full opacity
/// they give way to the background instead, which is also the single layer the
/// compositor is asked to blur. What is drawn over the window stays solid: a
/// menu or a dialog is something to read, and reading it through the wallpaper
/// is not a feature.
pub fn apply_chrome(theme: &Theme, opacity: f64) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }

    let accent = theme.accent();
    let over_background = |color: Rgb| {
        if opacity < 1.0 {
            "transparent".to_owned()
        } else {
            color.to_hex()
        }
    };
    let css = format!(
        "@define-color window_bg_color {bg};\n\
         @define-color window_fg_color {fg};\n\
         @define-color view_bg_color {clear};\n\
         @define-color view_fg_color {fg};\n\
         @define-color headerbar_bg_color {header};\n\
         @define-color headerbar_fg_color {fg};\n\
         @define-color headerbar_border_color {border};\n\
         @define-color headerbar_backdrop_color {clear};\n\
         @define-color sidebar_bg_color {sidebar};\n\
         @define-color sidebar_fg_color {fg};\n\
         @define-color sidebar_border_color {border};\n\
         @define-color sidebar_backdrop_color {clear};\n\
         /* The Files panel is a sidebar inside the projects sidebar's\n\
            content, which libadwaita paints as a secondary sidebar: its own\n\
            gray unless these say otherwise, and solid where everything else\n\
            went translucent. */\n\
         @define-color secondary_sidebar_bg_color {sidebar};\n\
         @define-color secondary_sidebar_fg_color {fg};\n\
         @define-color secondary_sidebar_border_color {border};\n\
         @define-color secondary_sidebar_backdrop_color {clear};\n\
         @define-color popover_bg_color {raised};\n\
         @define-color popover_fg_color {fg};\n\
         @define-color dialog_bg_color {raised};\n\
         @define-color dialog_fg_color {fg};\n\
         @define-color card_bg_color {raised};\n\
         @define-color card_fg_color {fg};\n\
         @define-color accent_color {accent};\n\
         @define-color accent_bg_color {accent};\n\
         @define-color accent_fg_color {on_accent};\n",
        bg = {
            let Rgb { r, g, b } = theme.background;
            format!("rgba({r},{g},{b},{opacity})")
        },
        clear = over_background(theme.background),
        fg = theme.foreground.to_hex(),
        header = over_background(theme.surface(0.06)),
        sidebar = over_background(theme.surface(0.03)),
        raised = theme.surface(0.10).to_hex(),
        border = theme.surface(0.20).to_hex(),
        accent = accent.to_hex(),
        on_accent = accent.contrasting().to_hex(),
    );

    let Some(display) = gdk::Display::default() else {
        return;
    };
    PROVIDER.with(|provider| {
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// What the question about unsaved work says. One file is named; several are
/// counted, since a list of them would be a dialog rather than a sentence.
fn unsaved_message(dirty: &[TuniEditor]) -> String {
    match dirty {
        [editor] => format!(
            "“{}” has changes that have not been written to disk.",
            editor.name()
        ),
        editors => format!(
            "{} files have changes that have not been written to disk.",
            editors.len()
        ),
    }
}

/// A path as a person would write it: `/home/me/src` is `~/src`.
pub(crate) fn shorten(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}
