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
use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::TerminalConfig;
use tuni_core::panes::{Edge, Layout, Pane};
use tuni_core::theme::Theme;
use tuni_core::workspace::{Id, Tab, Workspace};

use crate::grid::{Message, TuniGrid};
use crate::terminal::TuniTerminal;

/// Sidebar width, and the range a narrow or a wide window may take it to.
const SIDEBAR_FRACTION: f64 = 0.2;
const SIDEBAR_MIN: i32 = 180;
const SIDEBAR_MAX: i32 = 400;

mod imp {
    use super::{Cell, HashMap, Id, RefCell, TerminalConfig, TuniGrid, TuniTerminal, Workspace, glib};
    use adw::subclass::prelude::*;
    use gtk::prelude::WidgetExt;

    #[derive(Default)]
    pub struct TuniWindow {
        pub workspace: RefCell<Workspace>,
        /// One terminal per pane, by pane id.
        pub terminals: RefCell<HashMap<Id, TuniTerminal>>,
        /// One layout of panes per tab, by tab id.
        pub grids: RefCell<HashMap<Id, TuniGrid>>,
        /// The strip entry each tab was given, by tab id.
        pub pages: RefCell<HashMap<Id, adw::TabPage>>,
        /// One tab strip per project, by project id.
        pub views: RefCell<HashMap<Id, adw::TabView>>,
        pub config: RefCell<TerminalConfig>,

        pub split: RefCell<Option<adw::OverlaySplitView>>,
        pub sidebar: RefCell<Option<gtk::ListBox>>,
        /// Project name labels, in sidebar order.
        pub labels: RefCell<Vec<gtk::Label>>,
        /// Shared context menu for the sidebar rows, parented once.
        pub row_menu: RefCell<Option<gtk::PopoverMenu>>,
        /// Stack of tab strips, one page per project.
        pub stack: RefCell<Option<gtk::Stack>>,
        /// Tab strips, or the empty state when there is nothing to show.
        pub content: RefCell<Option<gtk::Stack>>,
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
            let obj = self.obj();
            obj.build_ui();
            obj.install_actions();
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

    impl WidgetImpl for TuniWindow {}

    impl WindowImpl for TuniWindow {
        fn close_request(&self) -> glib::Propagation {
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
    pub fn new(app: &adw::Application, config: TerminalConfig) -> Self {
        let window: Self = glib::Object::builder()
            .property("application", app)
            .property("default-width", 1100)
            .property("default-height", 700)
            .build();
        window.imp().config.replace(config);
        window
    }

    // --- construction ------------------------------------------------------

    fn build_ui(&self) {
        let imp = self.imp();
        load_css();

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
        header.pack_start(&toggle);

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

        let new_tab = gtk::Button::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("New Tab (Ctrl+Shift+T)")
            .action_name("win.new-tab")
            .build();
        new_tab.add_css_class("flat");
        let tab_bar = adw::TabBar::builder()
            .autohide(false)
            .expand_tabs(false)
            .end_action_widget(&new_tab)
            .build();

        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&header);
        content_view.add_top_bar(&tab_bar);
        content_view.set_content(Some(&content_stack));

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar_view)
            .content(&content_view)
            .sidebar_width_fraction(SIDEBAR_FRACTION)
            .min_sidebar_width(f64::from(SIDEBAR_MIN))
            .max_sidebar_width(f64::from(SIDEBAR_MAX))
            .build();
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

        imp.split.replace(Some(split));
        imp.sidebar.replace(Some(sidebar));
        imp.row_menu.replace(Some(row_menu));
        imp.stack.replace(Some(stack));
        imp.content.replace(Some(content_stack));
        imp.status.replace(Some(status));
        imp.status_button.replace(Some(status_button));
        imp.tab_bar.replace(Some(tab_bar));
        imp.title.replace(Some(title));
    }

    fn install_actions(&self) {
        let uint64 = Some(glib::VariantTy::UINT64);
        self.add_action_entries([
            entry("new-tab", None, |window, _| {
                let project = window
                    .imp()
                    .workspace
                    .borrow()
                    .selected_id()
                    .or_else(|| window.imp().workspace.borrow().projects().first().map(|p| p.id()));
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
            entry("focus-pane-left", None, |window, _| window.focus_toward(Edge::Left)),
            entry("focus-pane-right", None, |window, _| window.focus_toward(Edge::Right)),
            entry("focus-pane-up", None, |window, _| window.focus_toward(Edge::Up)),
            entry("focus-pane-down", None, |window, _| window.focus_toward(Edge::Down)),
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
            entry("resize-pane-left", None, |window, _| window.resize(Edge::Left)),
            entry("resize-pane-right", None, |window, _| window.resize(Edge::Right)),
            entry("resize-pane-up", None, |window, _| window.resize(Edge::Up)),
            entry("resize-pane-down", None, |window, _| window.resize(Edge::Down)),
            entry("next-tab", None, |window, _| window.shift_tab(1)),
            entry("previous-tab", None, |window, _| window.shift_tab(-1)),
            entry("select-tab", Some(glib::VariantTy::INT32), |window, target| {
                if let Some(index) = target.and_then(glib::Variant::get::<i32>) {
                    window.select_tab_at(index);
                }
            }),
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
            entry("select-project", Some(glib::VariantTy::INT32), |window, target| {
                if let Some(number) = target.and_then(glib::Variant::get::<i32>) {
                    let count = window.imp().workspace.borrow().projects().len();
                    if count > 0 {
                        let index = if number >= 9 { count - 1 } else { (number as usize).saturating_sub(1).min(count - 1) };
                        window.select_project_at(index);
                    }
                }
            }),
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
            entry("toggle-sidebar", None, |window, _| {
                if let Some(split) = window.imp().split.borrow().as_ref() {
                    split.set_show_sidebar(!split.shows_sidebar());
                }
            }),
            entry("tab-rename", None, |window, _| window.rename_tab()),
            entry("tab-automatic-title", None, |window, _| {
                let Some(page) = window.imp().menu_page.borrow().clone() else {
                    return;
                };
                window.set_tab_name(&page, None);
            }),
            entry("tab-close", None, |window, _| {
                if let (Some(view), Some(page)) =
                    (window.selected_view(), window.imp().menu_page.borrow().clone())
                {
                    view.close_page(&page);
                }
            }),
            entry("tab-close-others", None, |window, _| {
                if let (Some(view), Some(page)) =
                    (window.selected_view(), window.imp().menu_page.borrow().clone())
                {
                    view.close_other_pages(&page);
                }
            }),
            entry("tab-close-right", None, |window, _| {
                if let (Some(view), Some(page)) =
                    (window.selected_view(), window.imp().menu_page.borrow().clone())
                {
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
                let theme = this.imp().config.borrow().theme(style.is_dark());
                for terminal in this.imp().terminals.borrow().values() {
                    terminal.set_theme(&theme);
                }
                apply_chrome(&theme);
            }
        );
        style.connect_dark_notify(recolor.clone());
        recolor(&style);
    }

    /// The colors this run's terminals are painted with.
    fn theme(&self) -> Theme {
        self.imp()
            .config
            .borrow()
            .theme(adw::StyleManager::default().is_dark())
    }

    // --- projects ----------------------------------------------------------

    /// Opens a project with one terminal in it, and shows it.
    pub fn open_project(&self) -> Id {
        let imp = self.imp();
        let id = imp.workspace.borrow_mut().open_project();

        let view = adw::TabView::new();
        view.set_menu_model(Some(&tab_menu()));
        view.connect_setup_menu(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, page| {
                this.imp().menu_page.replace(page.cloned());
            }
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

        self.rebuild_sidebar();
        self.show_selected_project();
        self.open_tab(id);
        id
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
                if let Some(terminal) = imp.terminals.borrow_mut().remove(&pane.id()) {
                    terminal.shutdown();
                }
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
            bar.set_view(view.as_ref());
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
        self.focus_terminal();
    }

    // --- tabs --------------------------------------------------------------

    /// Opens a tab in `project`, next to the selected one, with a single
    /// terminal starting where the selected tab's shell is.
    pub fn open_tab(&self, project: Id) {
        let imp = self.imp();
        let Some(view) = imp.views.borrow().get(&project).cloned() else {
            return;
        };

        let cwd = self.directory_for_new_shell(project);
        let tab = Tab::new();
        let tab_id = tab.id();
        let pane = tab.layout().focused();
        let name = tab.name().to_owned();
        let position = view
            .selected_page()
            .map_or_else(|| view.n_pages(), |page| view.page_position(&page) + 1);

        if let Some(project) = imp.workspace.borrow_mut().project_mut(project) {
            project.insert_tab(position.max(0) as usize, tab);
        }

        let terminal = self.new_terminal(project, tab_id, pane);
        let grid = self.new_grid(project, tab_id);
        let page = view.insert(&grid, position);
        page.set_title(&name);
        page.set_live_thumbnail(true);
        imp.pages.borrow_mut().insert(tab_id, page.clone());

        self.rebuild_grid(project, tab_id);
        view.set_selected_page(&page);
        self.refresh();
        self.start_terminal(&terminal, cwd);
    }

    /// Opens a pane beside the focused one, in the same tab.
    fn split(&self, edge: Edge) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        let cwd = self.directory_for_new_shell(project);
        let pane = Pane::new();
        let pane_id = pane.id();
        {
            let imp = self.imp();
            let mut workspace = imp.workspace.borrow_mut();
            let Some(entry) = workspace
                .project_mut(project)
                .and_then(|project| project.tab_mut(tab))
            else {
                return;
            };
            entry.layout_mut().split(pane, edge);
        }

        let terminal = self.new_terminal(project, tab, pane_id);
        self.rebuild_grid(project, tab);
        self.refresh();
        self.start_terminal(&terminal, cwd);
    }

    /// A terminal for one pane, themed and remembered.
    fn new_terminal(&self, project: Id, tab: Id, pane: Id) -> TuniTerminal {
        let imp = self.imp();
        let terminal = TuniTerminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_config(&imp.config.borrow());
        terminal.set_theme(&self.theme());
        imp.terminals.borrow_mut().insert(pane, terminal.clone());
        self.watch_terminal(&terminal, project, tab, pane);
        terminal
    }

    /// Starts a shell once its widget has been allocated.
    ///
    /// The shell learns its window size from that first allocation, so starting
    /// it any earlier opens it at 80x24 and corrects it under its own feet.
    fn start_terminal(&self, terminal: &TuniTerminal, cwd: Option<PathBuf>) {
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            terminal,
            move || {
                if let Err(error) = terminal.start(cwd) {
                    let dialog = adw::AlertDialog::new(Some("Cannot start shell"), Some(&error));
                    dialog.add_response("close", "Close");
                    dialog.present(Some(&this));
                } else {
                    terminal.grab_focus();
                }
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
            self.focus_terminal();
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
        // happens goes straight for this map.
        let terminals = imp.terminals.borrow().clone();
        grid.rebuild(&layout, &terminals);
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
                    if let Some(entry) = this.imp().workspace.borrow_mut().project_mut(project)
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
                    if let Some(entry) = this.imp().workspace.borrow_mut().project_mut(project)
                        .and_then(|project| project.pane_mut(tab, pane))
                    {
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
                move |_: TuniTerminal| {
                    if let Some(page) = this.imp().pages.borrow().get(&tab)
                        && !page.is_selected()
                    {
                        page.set_needs_attention(true);
                    }
                }
            ),
        );

        terminal.connect_closure(
            "exited",
            false,
            glib::closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: TuniTerminal| this.close_pane(project, tab, pane)
            ),
        );
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
    }

    /// Closes one pane, and the tab with it when it was the last one.
    fn close_pane(&self, project: Id, tab: Id, pane: Id) {
        let imp = self.imp();
        if let Some(terminal) = imp.terminals.borrow_mut().remove(&pane) {
            terminal.shutdown();
        }
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
            if let (Some(view), Some(page)) =
                (imp.views.borrow().get(&project).cloned(), page)
            {
                view.close_page(&page);
            }
            return;
        }
        self.rebuild_grid(project, tab);
        self.refresh();
        self.focus_terminal();
    }

    fn close_focused_pane(&self) {
        let Some((project, tab)) = self.selected_tab() else {
            return;
        };
        let Some(pane) = self.focused_pane() else {
            return;
        };
        self.close_pane(project, tab, pane);
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
            if let Some(terminal) = imp.terminals.borrow_mut().remove(&pane.id()) {
                terminal.shutdown();
            }
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
        self.focus_terminal();
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
        self.focus_terminal();
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
        self.focus_terminal();
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
        self.refresh();
        self.focus_terminal();
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
        let index = if number >= 9 { count - 1 } else { (number - 1).clamp(0, count - 1) };
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

    fn focus_terminal(&self) {
        if let Some(terminal) = self.active_terminal() {
            terminal.grab_focus();
        }
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
        if let (Some(status), Some(button)) =
            (imp.status.borrow().as_ref(), imp.status_button.borrow().as_ref())
        {
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
                page.set_title(tab.name());
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
        menu.set_pointing_to(Some(&gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        menu.popup();
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
fn entry<F>(name: &str, parameter: Option<&glib::VariantTy>, activate: F) -> gio::ActionEntry<TuniWindow>
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
             .tuni-drop { background-color: alpha(@accent_color, 0.28); border-radius: 6px; }\n",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// Paint the window chrome from the terminal's theme.
///
/// libadwaita builds its whole stylesheet out of named colors, so overriding
/// those recolors the header bar, dialogs, and popovers consistently — far more
/// robust than styling widgets one by one, and it keeps working as libadwaita
/// adds widgets. One provider, reloaded, so switching themes does not stack
/// stylesheets on the display.
pub fn apply_chrome(theme: &Theme) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }

    let accent = theme.accent();
    let css = format!(
        "@define-color window_bg_color {bg};\n\
         @define-color window_fg_color {fg};\n\
         @define-color view_bg_color {bg};\n\
         @define-color view_fg_color {fg};\n\
         @define-color headerbar_bg_color {header};\n\
         @define-color headerbar_fg_color {fg};\n\
         @define-color headerbar_border_color {border};\n\
         @define-color headerbar_backdrop_color {bg};\n\
         @define-color sidebar_bg_color {sidebar};\n\
         @define-color sidebar_fg_color {fg};\n\
         @define-color sidebar_border_color {border};\n\
         @define-color sidebar_backdrop_color {bg};\n\
         @define-color popover_bg_color {raised};\n\
         @define-color popover_fg_color {fg};\n\
         @define-color dialog_bg_color {raised};\n\
         @define-color dialog_fg_color {fg};\n\
         @define-color card_bg_color {raised};\n\
         @define-color card_fg_color {fg};\n\
         @define-color accent_color {accent};\n\
         @define-color accent_bg_color {accent};\n\
         @define-color accent_fg_color {on_accent};\n",
        bg = theme.background.to_hex(),
        fg = theme.foreground.to_hex(),
        header = theme.surface(0.06).to_hex(),
        sidebar = theme.surface(0.03).to_hex(),
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

/// A path as a person would write it: `/home/me/src` is `~/src`.
fn shorten(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}
