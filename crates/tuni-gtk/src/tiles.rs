//! A row, or a column, of tiles sized by weight.
//!
//! GTK has `GtkPaned`, but a paned holds two children and nests to hold more,
//! and the pane model here is flat by design: a column is n panes, a tab is n
//! columns, and a drag on one divider must move that divider alone. So the
//! sizing is done here instead, out of the same weights the model keeps and
//! with the same arithmetic its tests cover.
//!
//! The gaps between tiles are the dividers. Nothing is drawn in them — the
//! cursor changing shape is the affordance, which is how a tiling window
//! manager does it too.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use tuni_core::panes::{dragged, shares};

/// Space between two tiles, and so the width of the strip a divider drag can
/// start in.
pub const GAP: i32 = 8;

mod imp {
    use super::{Cell, GAP, RefCell, glib, shares};
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    /// A divider drag in flight: which divider, and the weights it started
    /// from, so the pointer's total travel is always measured against a fixed
    /// baseline rather than against weights it is itself changing.
    /// Told the weights a divider drag settled on.
    pub type Resized = Box<dyn Fn(&[f64])>;

    pub struct Drag {
        pub index: usize,
        pub baseline: Vec<f64>,
        pub available: f64,
    }

    pub struct TuniTiles {
        pub orientation: Cell<gtk::Orientation>,
        pub children: RefCell<Vec<gtk::Widget>>,
        pub weights: RefCell<Vec<f64>>,
        /// Where each tile ended up along the axis, from the last allocation:
        /// what a divider hit test reads.
        pub bounds: RefCell<Vec<(f64, f64)>>,
        pub drag: RefCell<Option<Drag>>,
        /// Told once, when a drag ends, rather than on every frame of it.
        pub resized: RefCell<Option<Resized>>,
    }

    impl Default for TuniTiles {
        fn default() -> Self {
            Self {
                orientation: Cell::new(gtk::Orientation::Horizontal),
                children: RefCell::new(Vec::new()),
                weights: RefCell::new(Vec::new()),
                bounds: RefCell::new(Vec::new()),
                drag: RefCell::new(None),
                resized: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniTiles {
        const NAME: &'static str = "TuniTiles";
        type Type = super::TuniTiles;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TuniTiles {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin(glib::clone!(
                #[weak(rename_to = this)]
                obj,
                move |gesture, x, y| {
                    match this.divider_at(x, y) {
                        Some(index) => this.begin_drag(index),
                        // Nothing was grabbed, so the press belongs to whatever
                        // is underneath rather than to this container.
                        None => {
                            gesture.set_state(gtk::EventSequenceState::Denied);
                        }
                    }
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak(rename_to = this)]
                obj,
                move |_, x, y| this.update_drag(x, y)
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak(rename_to = this)]
                obj,
                move |_, _, _| this.end_drag()
            ));
            obj.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak(rename_to = this)]
                obj,
                move |_, x, y| this.hint_cursor(this.divider_at(x, y).is_some())
            ));
            motion.connect_leave(glib::clone!(
                #[weak(rename_to = this)]
                obj,
                move |_| this.hint_cursor(false)
            ));
            obj.add_controller(motion);
        }

        fn dispose(&self) {
            for child in self.children.borrow_mut().drain(..) {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TuniTiles {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let children = self.children.borrow();
            let mut minimum = 0;
            let mut natural = 0;
            let along = orientation == self.orientation.get();

            for child in children.iter() {
                let (child_min, child_nat, _, _) = child.measure(orientation, for_size);
                if along {
                    minimum += child_min;
                    natural += child_nat;
                } else {
                    minimum = minimum.max(child_min);
                    natural = natural.max(child_nat);
                }
            }
            if along {
                let gaps = GAP * i32::try_from(children.len().saturating_sub(1)).unwrap_or(0);
                minimum += gaps;
                natural += gaps;
            }
            (minimum, natural.max(minimum), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let children = self.children.borrow();
            let horizontal = self.orientation.get() == gtk::Orientation::Horizontal;
            let (along, across) = if horizontal {
                (width, height)
            } else {
                (height, width)
            };
            let available = f64::from(
                along - GAP * i32::try_from(children.len().saturating_sub(1)).unwrap_or(0),
            );

            let sizes = shares(&self.weights.borrow(), available.max(0.0));
            let mut bounds = Vec::with_capacity(children.len());
            // Boundaries are rounded rather than sizes: rounding each tile
            // separately leaves the column ends a pixel adrift of each other.
            let mut cursor = 0.0_f64;
            for (child, size) in children.iter().zip(&sizes) {
                let start = cursor.round() as i32;
                cursor += size;
                let extent = (cursor.round() as i32 - start).max(1);
                let allocation = if horizontal {
                    gtk::Allocation::new(start, 0, extent, across)
                } else {
                    gtk::Allocation::new(0, start, across, extent)
                };
                child.size_allocate(&allocation, -1);
                bounds.push((f64::from(start), f64::from(extent)));
                cursor += f64::from(GAP);
            }
            self.bounds.replace(bounds);
        }
    }
}

glib::wrapper! {
    pub struct TuniTiles(ObjectSubclass<imp::TuniTiles>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TuniTiles {
    #[must_use]
    pub fn new(orientation: gtk::Orientation) -> Self {
        let tiles: Self = glib::Object::new();
        tiles.imp().orientation.set(orientation);
        tiles
    }

    /// Adds a tile at the end, with the share of the axis it should take.
    pub fn append(&self, child: &impl IsA<gtk::Widget>, weight: f64) {
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(self);
        self.imp().children.borrow_mut().push(child);
        self.imp().weights.borrow_mut().push(weight);
        self.queue_resize();
    }

    /// Called once when a divider drag ends, with the weights it settled on.
    pub fn connect_resized<F: Fn(&[f64]) + 'static>(&self, callback: F) {
        self.imp().resized.replace(Some(Box::new(callback)));
    }

    /// Which divider a press at (`x`, `y`) grabs: the gap it landed in.
    fn divider_at(&self, x: f64, y: f64) -> Option<usize> {
        let imp = self.imp();
        let along = if imp.orientation.get() == gtk::Orientation::Horizontal {
            x
        } else {
            y
        };
        let bounds = imp.bounds.borrow();
        (0..bounds.len().saturating_sub(1)).find(|index| {
            let (start, size) = bounds[*index];
            let next = bounds[index + 1].0;
            along >= start + size && along <= next
        })
    }

    fn hint_cursor(&self, over_divider: bool) {
        let name = if !over_divider {
            None
        } else if self.imp().orientation.get() == gtk::Orientation::Horizontal {
            Some("col-resize")
        } else {
            Some("row-resize")
        };
        self.set_cursor_from_name(name);
    }

    fn begin_drag(&self, index: usize) {
        let imp = self.imp();
        let bounds = imp.bounds.borrow();
        let available: f64 = bounds.iter().map(|(_, size)| size).sum();
        drop(bounds);
        imp.drag.replace(Some(imp::Drag {
            index,
            baseline: imp.weights.borrow().clone(),
            available,
        }));
    }

    fn update_drag(&self, x: f64, y: f64) {
        let imp = self.imp();
        let horizontal = imp.orientation.get() == gtk::Orientation::Horizontal;
        let travel = if horizontal { x } else { y };
        let moved = imp.drag.borrow().as_ref().map(|drag| {
            let (first, second) = dragged(&drag.baseline, drag.index, travel, drag.available);
            (drag.index, first, second)
        });
        let Some((index, first, second)) = moved else {
            return;
        };
        {
            let mut weights = imp.weights.borrow_mut();
            weights[index] = first;
            weights[index + 1] = second;
        }
        self.queue_allocate();
    }

    fn end_drag(&self) {
        if self.imp().drag.take().is_none() {
            return;
        }
        let weights = self.imp().weights.borrow().clone();
        if let Some(callback) = self.imp().resized.borrow().as_ref() {
            callback(&weights);
        }
    }
}
