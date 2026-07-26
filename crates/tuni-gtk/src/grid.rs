//! One tab's panes on screen.
//!
//! The layout in `tuni_core::panes` says which pane sits where and how much
//! room each gets; this builds that as widgets and reports back the three
//! things only the pointer can decide: which pane was clicked into, where a
//! dragged pane was dropped, and where a divider was let go.
//!
//! The tree is rebuilt whole whenever the layout's shape changes. Splits and
//! closes are rare, the terminals themselves are held by the window and simply
//! change parent, and a rebuild cannot leave a stale widget behind the way
//! patching the tree in place can.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::BinExt;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};

use tuni_core::panes::{Edge, Layout};
use tuni_core::workspace::Id;

use crate::terminal::TuniTerminal;
use crate::tiles::{GAP, TuniTiles};

/// The box a dragged pane is scaled to fit inside, keeping its own proportions
/// so a tall pane makes a tall thumbnail rather than a cropped one.
const THUMBNAIL: (f64, f64) = (220.0, 160.0);

/// One pane's widgets, kept so the tree can be taken apart again without
/// leaving a terminal parented to something that is going away.
struct PaneWidgets {
    id: Id,
    container: gtk::Box,
    /// Holds the terminal, which belongs to the window rather than to us.
    overlay: gtk::Overlay,
}

mod imp {
    use super::{Edge, Id, PaneWidgets, Rc, RefCell};
    use adw::subclass::prelude::*;
    use gtk::glib;

    /// Shared rather than owned, so it can be taken out of its cell and called
    /// while the handler rebuilds the grid it was called from.
    pub type Handler = Rc<dyn Fn(Message)>;

    #[derive(Default)]
    pub struct TuniGrid {
        pub(super) panes: RefCell<Vec<PaneWidgets>>,
        pub focused: RefCell<Option<Id>>,
        pub message: RefCell<Option<Handler>>,
    }

    /// What the pointer decided, on its way back to the window.
    pub enum Message {
        Focus(Id),
        Move(Id, Edge, Id),
        Columns(Vec<f64>),
        Panes(usize, Vec<f64>),
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniGrid {
        const NAME: &'static str = "TuniGrid";
        type Type = super::TuniGrid;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniGrid {}
    impl WidgetImpl for TuniGrid {}
    impl BinImpl for TuniGrid {}
}

pub use imp::Message;

glib::wrapper! {
    pub struct TuniGrid(ObjectSubclass<imp::TuniGrid>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniGrid {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniGrid {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Everything the pointer decides arrives here.
    pub fn connect_message<F: Fn(Message) + 'static>(&self, callback: F) {
        self.imp().message.replace(Some(Rc::new(callback)));
    }

    fn send(&self, message: Message) {
        // Taken out of the cell before the call, so a handler is free to rebuild
        // this grid — which is what most of these lead to.
        let callback = self.imp().message.borrow().clone();
        if let Some(callback) = callback {
            callback(message);
        }
    }

    /// Builds the layout as widgets, taking the terminals from `terminals`.
    pub fn rebuild(&self, layout: &Layout, terminals: &HashMap<Id, TuniTerminal>) {
        // Hand every terminal back before the old tree goes, so none of them is
        // carried off by a widget being disposed. Taken out of the cell first:
        // unparenting moves the keyboard, and whoever hears about that is
        // entitled to look at this list.
        let old: Vec<PaneWidgets> = self.imp().panes.borrow_mut().drain(..).collect();
        for pane in old {
            pane.overlay.set_child(gtk::Widget::NONE);
        }
        self.set_child(gtk::Widget::NONE);
        self.imp().focused.replace(Some(layout.focused()));

        let split = layout.is_split();
        if !split || layout.is_zoomed() {
            // One pane on screen: either the tab was never split, or the zoom
            // is showing the focused pane alone. Full bleed, and no grip —
            // there is nothing on screen to drop onto.
            let id = if split { layout.focused() } else {
                layout.columns()[0].panes()[0].id()
            };
            let pane = self.build_pane(id, terminals, split, false);
            self.set_child(Some(&pane));
            self.refresh_focus();
            return;
        }

        let root = TuniTiles::new(gtk::Orientation::Horizontal);
        for (index, column) in layout.columns().iter().enumerate() {
            let stack = TuniTiles::new(gtk::Orientation::Vertical);
            for pane in column.panes() {
                let widget = self.build_pane(pane.id(), terminals, true, true);
                stack.append(&widget, pane.weight);
            }
            stack.connect_resized(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |weights| this.send(Message::Panes(index, weights.to_vec()))
            ));
            root.append(&stack, column.weight);
        }
        root.connect_resized(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |weights| this.send(Message::Columns(weights.to_vec()))
        ));

        // The same gap that sits between tiles sits around them, so a split tab
        // has even breathing room on every side.
        root.set_margin_start(GAP);
        root.set_margin_end(GAP);
        root.set_margin_top(GAP);
        root.set_margin_bottom(GAP);
        self.set_child(Some(&root));
        self.refresh_focus();
    }

    /// Moves the focus ring without rebuilding: clicking into a pane changes
    /// nothing about the layout's shape.
    pub fn set_focused(&self, id: Id) {
        self.imp().focused.replace(Some(id));
        self.refresh_focus();
    }

    fn refresh_focus(&self) {
        let focused = *self.imp().focused.borrow();
        for pane in self.imp().panes.borrow().iter() {
            if Some(pane.id) == focused {
                pane.container.add_css_class("focused");
            } else {
                pane.container.remove_css_class("focused");
            }
        }
    }

    /// One tile: the terminal, a ring around it while the tab is split, a grip
    /// to carry it by, and the hint shown when something is dropped on it.
    fn build_pane(
        &self,
        id: Id,
        terminals: &HashMap<Id, TuniTerminal>,
        ring: bool,
        movable: bool,
    ) -> gtk::Box {
        let preview = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        preview.add_css_class("tuni-drop");
        preview.set_visible(false);
        preview.set_can_target(false);

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        if let Some(terminal) = terminals.get(&id) {
            overlay.set_child(Some(terminal));
        }
        overlay.add_overlay(&preview);

        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.add_css_class("tuni-pane");
        if ring {
            container.add_css_class("ringed");
        }

        if movable {
            container.append(&self.build_grip(id, &container));
        }
        container.append(&overlay);

        if movable {
            self.watch_drops(id, &container, &preview);
        }

        self.imp().panes.borrow_mut().push(PaneWidgets {
            id,
            container: container.clone(),
            overlay,
        });
        container
    }

    /// The strip along the top of a pane that carries it onto another one.
    /// Separate from the terminal because dragging inside a terminal already
    /// means selecting text.
    fn build_grip(&self, id: Id, pane: &gtk::Box) -> gtk::Box {
        let grip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        grip.add_css_class("tuni-grip");
        grip.set_cursor_from_name(Some("grab"));

        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&id.raw().to_value()))
        });
        source.connect_drag_begin(glib::clone!(
            #[weak]
            pane,
            move |source, _| {
                if let Some((texture, width, height)) = thumbnail(&pane) {
                    source.set_icon(Some(&texture), width / 2, height / 2);
                }
            }
        ));
        grip.add_controller(source);
        grip
    }

    /// Accepts a pane dropped onto this one, and shows where it would land.
    fn watch_drops(&self, id: Id, pane: &gtk::Box, preview: &gtk::Box) {
        let target = gtk::DropTarget::new(u64::static_type(), gdk::DragAction::MOVE);

        target.connect_motion(glib::clone!(
            #[weak]
            pane,
            #[weak]
            preview,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, x, y| {
                show_preview(&pane, &preview, edge_at(&pane, x, y));
                gdk::DragAction::MOVE
            }
        ));
        target.connect_leave(glib::clone!(
            #[weak]
            preview,
            move |_| preview.set_visible(false)
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            pane,
            #[weak]
            preview,
            #[upgrade_or]
            false,
            move |_, value, x, y| {
                preview.set_visible(false);
                let Ok(dragged) = value.get::<u64>() else {
                    return false;
                };
                this.send(Message::Move(Id::from(dragged), edge_at(&pane, x, y), id));
                true
            }
        ));
        pane.add_controller(target);
    }
}

/// Which edge of a pane a point is nearest: the tile is cut into four triangles
/// by its own diagonals, which is the drop scheme VS Code and Ghostty both use.
fn edge_at(pane: &gtk::Box, x: f64, y: f64) -> Edge {
    let width = f64::from(pane.width()).max(1.0);
    let height = f64::from(pane.height()).max(1.0);
    let dx = (x - width / 2.0) / width;
    let dy = (y - height / 2.0) / height;
    if dx.abs() > dy.abs() {
        if dx < 0.0 { Edge::Left } else { Edge::Right }
    } else if dy < 0.0 {
        Edge::Up
    } else {
        Edge::Down
    }
}

/// Shows the half of the pane the dropped one would take.
fn show_preview(pane: &gtk::Box, preview: &gtk::Box, edge: Edge) {
    let width = pane.width();
    let height = pane.height();
    let (halign, valign) = match edge {
        Edge::Left => (gtk::Align::Start, gtk::Align::Fill),
        Edge::Right => (gtk::Align::End, gtk::Align::Fill),
        Edge::Up => (gtk::Align::Fill, gtk::Align::Start),
        Edge::Down => (gtk::Align::Fill, gtk::Align::End),
    };
    preview.set_halign(halign);
    preview.set_valign(valign);
    preview.set_size_request(
        if edge.is_horizontal() { width / 2 } else { -1 },
        if edge.is_horizontal() { -1 } else { height / 2 },
    );
    preview.set_visible(true);
}

/// The pane as a picture the drag can carry, scaled down to fit
/// [`THUMBNAIL`].
fn thumbnail(pane: &gtk::Box) -> Option<(gdk::Texture, i32, i32)> {
    let width = f64::from(pane.width());
    let height = f64::from(pane.height());
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = (THUMBNAIL.0 / width).min(THUMBNAIL.1 / height).min(1.0);
    let (width, height) = (width * scale, height * scale);

    let renderer = pane.native()?.renderer()?;
    let paintable = gtk::WidgetPaintable::new(Some(pane));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(scale as f32, scale as f32);
    paintable.snapshot(&snapshot, width / scale, height / scale);
    let node = snapshot.to_node()?;
    let texture = renderer.render_texture(&node, Some(&graphene::Rect::new(
        0.0,
        0.0,
        width as f32,
        height as f32,
    )));
    Some((texture, width as i32, height as i32))
}
