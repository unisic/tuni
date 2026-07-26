//! Popping a context menu up where the pointer is.

use gtk::glib;
use gtk::prelude::*;
use gtk::{gdk, graphene};

/// Pops `menu` up pointing at `point`, in the coordinates of the widget the
/// popover hangs off.
///
/// GTK sizes the popover's surface before its contents have been laid out, so
/// a menu whose model was set just before the popup comes up shorter than it
/// asked for and grows a scrollbar down a list of four items. Pointing it at
/// the same place again, once the frame it was mapped in is over, presents the
/// surface a second time, by which point GTK knows how tall the menu is.
pub fn popup_at(menu: &gtk::PopoverMenu, point: graphene::Point) {
    let rect = gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
    menu.set_pointing_to(Some(&rect));
    menu.popup();
    glib::idle_add_local_once(glib::clone!(
        #[weak]
        menu,
        move || menu.set_pointing_to(Some(&rect))
    ));
}
