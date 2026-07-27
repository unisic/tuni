//! Asking the compositor to blur what shows through a translucent window.
//!
//! There is no way to do this by drawing: a translucent window shows the
//! wallpaper exactly as it is unless the compositor is told otherwise. On
//! Wayland the ask is `ext-background-effect-v1`, spoken over the connection
//! GDK already has open, and only once the manager's `capabilities` event has
//! named blur among the effects it renders. On X11 it is a window property,
//! KWin's `_KDE_NET_WM_BLUR_BEHIND_REGION`. These are the two asks Ghostty
//! makes, and there is no third: a desktop that answers neither, GNOME first
//! among them, offers no way to ask at all, so the setting quietly does
//! nothing there.

use std::cell::RefCell;
use std::collections::HashMap;

use gdk4_wayland::prelude::*;
use gdk4_wayland::{WaylandDisplay, WaylandSurface};
use gtk::gdk;
use gtk::prelude::*;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::{
    Capability, Event as ManagerEvent, ExtBackgroundEffectManagerV1,
};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

/// Blurs, or stops blurring, what is behind `window`.
///
/// The window has to be realized: before that it has no surface to blur. The
/// blurred region is the window's own rectangle, so this is worth calling again
/// whenever that rectangle moves or changes size.
pub fn apply(window: &impl IsA<gtk::Window>, on: bool) {
    let window = window.as_ref();
    let Some(surface) = wl_surface(window) else {
        xorg::apply(window, on);
        return;
    };
    let shape = if on { shape(window) } else { None };

    EFFECTS.with_borrow_mut(|slot| {
        if let Effects::Blurs(effects) = slot.get_or_insert_with(Effects::look_up) {
            effects.blur(&surface, shape);
        }
    });

    // The compositor picks the region up with the surface's next commit, and
    // GTK commits when it draws.
    window.queue_draw();
}

/// Drops what was held for `window`'s surface, which is about to go away.
///
/// The objects would be inert afterwards rather than harmless: Wayland hands
/// out the ids of dead objects again, and a later window given the same id
/// would find an effect it cannot use.
pub fn forget(window: &impl IsA<gtk::Window>) {
    let Some(surface) = wl_surface(window.as_ref()) else {
        xorg::forget(window.as_ref());
        return;
    };
    EFFECTS.with_borrow_mut(|slot| {
        if let Some(Effects::Blurs(effects)) = slot.as_mut()
            && let Some(blurred) = effects.blurred.remove(&surface.id())
        {
            blurred.effect.destroy();
            let _ = effects.connection.flush();
        }
    });
}

fn wl_surface(window: &gtk::Window) -> Option<WlSurface> {
    window
        .surface()
        .and_then(|surface| surface.downcast::<WaylandSurface>().ok())
        .and_then(|surface| surface.wl_surface())
}

thread_local! {
    /// Looked up once. The protocol is missing on a compositor that does not
    /// blur, and asking the registry again on every settings change would not
    /// make it appear.
    static EFFECTS: RefCell<Option<Effects>> = const { RefCell::new(None) };
}

enum Effects {
    /// No Wayland, or a compositor that offers no background effects.
    Deaf,
    Blurs(Manager),
}

impl Effects {
    fn look_up() -> Self {
        bind().map_or(Self::Deaf, Self::Blurs)
    }
}

struct Manager {
    manager: ExtBackgroundEffectManagerV1,
    /// Regions are made by the compositor rather than described inline.
    compositor: WlCompositor,
    connection: Connection,
    blurred: HashMap<ObjectId, Blurred>,
    /// Nothing here is waited on, but the objects belong to the queue, so it
    /// outlives them.
    queue: EventQueue<Globals>,
}

struct Blurred {
    /// Held for as long as the surface lives. A surface may be given only one
    /// of these, so turning the blur off sets an empty region instead of
    /// dropping the object and asking for another later.
    effect: ExtBackgroundEffectSurfaceV1,
    shape: Option<Rounded>,
}

impl Manager {
    fn blur(&mut self, surface: &WlSurface, shape: Option<Rounded>) {
        let queue = self.queue.handle();
        let manager = &self.manager;
        let blurred = self.blurred.entry(surface.id()).or_insert_with(|| Blurred {
            effect: manager.get_background_effect(surface, &queue, ()),
            shape: None,
        });
        if blurred.shape == shape {
            return;
        }

        match &shape {
            Some(shape) => {
                let region = self.compositor.create_region(&queue, ());
                for (x, y, width, height) in shape.rectangles() {
                    region.add(x, y, width, height);
                }
                blurred.effect.set_blur_region(Some(&region));
                // The region was copied into the request.
                region.destroy();
            }
            None => blurred.effect.set_blur_region(None),
        }
        blurred.shape = shape;
        let _ = self.connection.flush();
    }
}

fn bind() -> Option<Manager> {
    let display = gdk::Display::default()?.downcast::<WaylandDisplay>().ok()?;
    let connection = Connection::from_backend(display.wl_display()?.backend().upgrade()?);
    let (globals, mut queue) = registry_queue_init::<Globals>(&connection).ok()?;
    let manager = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
    let compositor = globals.bind(&queue.handle(), 1..=6, ()).ok()?;
    // Binding the manager makes it say what it can do, and blur may not be on
    // the list: the protocol covers effects this window never asks for. A
    // compositor that offers the manager without blur is as deaf to the
    // question as one with no manager at all.
    let mut heard = Globals { blur: false };
    queue.roundtrip(&mut heard).ok()?;
    if !heard.blur {
        return None;
    }
    Some(Manager {
        manager,
        compositor,
        connection,
        blurred: HashMap::new(),
        queue,
    })
}

/// The window itself, the part of it the compositor calls the window.
///
/// The region is measured from the corner of the window rather than the corner
/// of the surface: a client-side decorated window is bigger than it looks,
/// since the shadow it draws around itself is part of the surface, and the
/// compositor has already taken that off. Corners are rounded for a related
/// reason, a square blur behind a rounded window showing at each corner.
#[derive(PartialEq)]
struct Rounded {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
}

fn shape(window: &gtk::Window) -> Option<Rounded> {
    let square = window
        .surface()?
        .downcast_ref::<gdk::Toplevel>()
        .is_none_or(|top| {
            top.state().intersects(
                gdk::ToplevelState::MAXIMIZED
                    | gdk::ToplevelState::FULLSCREEN
                    | gdk::ToplevelState::TILED,
            )
        });
    // The corner radius libadwaita rounds its windows to, and a pixel of inset
    // so the blur does not show along the edge it is rounded against.
    let radius = if square { 0 } else { 15 };
    let inset = i32::from(!square);

    let width = window.width() - inset * 2;
    let height = window.height() - inset * 2;
    (width > radius * 2 && height > radius * 2).then_some(Rounded {
        x: inset,
        y: inset,
        width,
        height,
        radius,
    })
}

impl Rounded {
    /// The rectangles a region is built out of: the middle of the window in one
    /// piece, and a row per line of each rounded corner.
    fn rectangles(&self) -> Vec<(i32, i32, i32, i32)> {
        let mut rectangles = vec![(
            self.x,
            self.y + self.radius,
            self.width,
            self.height - self.radius * 2,
        )];
        for row in 0..self.radius {
            // How far this row is from the middle of the corner's circle, and
            // how far in the circle has come by then.
            let down = (self.radius - row) as f32 - 0.5;
            let radius = self.radius as f32;
            let across = (radius * radius - down * down).sqrt();
            let indent = (radius - (across + 0.5).round()) as i32;
            let width = self.width - indent * 2;
            rectangles.push((self.x + indent, self.y + row, width, 1));
            rectangles.push((self.x + indent, self.y + self.height - 1 - row, width, 1));
        }
        rectangles
    }
}

/// What dispatching the queue writes down: whether the manager's
/// `capabilities` event named blur. The registry itself asks for nothing, its
/// globals are read once out of the list `registry_queue_init` returns.
struct Globals {
    blur: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for Globals {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Globals: ignore ExtBackgroundEffectSurfaceV1);
delegate_noop!(Globals: ignore WlCompositor);
delegate_noop!(Globals: ignore WlRegion);

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: <ExtBackgroundEffectManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ManagerEvent::Capabilities { flags } = event {
            state.blur = flags
                .into_result()
                .is_ok_and(|flags| flags.contains(Capability::Blur));
        }
    }
}

/// The same ask spoken to an X server: a list of rectangles in device pixels,
/// left on the window as the `_KDE_NET_WM_BLUR_BEHIND_REGION` property for
/// KWin to read. A window manager that has never heard of the atom ignores
/// it, which is this path's way of quietly doing nothing.
mod xorg {
    use std::cell::{Cell, LazyCell, RefCell};
    use std::collections::HashMap;
    use std::ffi::c_long;

    use gdk4_x11::{X11Display, X11Surface};
    use gtk::prelude::*;
    use x11_dl::xlib::{self, Xlib};

    thread_local! {
        /// Xlib's entry points, found at runtime. GDK on an X display has the
        /// library open already, so this is a lookup rather than a load, and
        /// a failure means no X11 and nothing to do.
        static XLIB: LazyCell<Option<Xlib>> = LazyCell::new(|| Xlib::open().ok());
        /// What each window was last given, so a layout pass that moved
        /// nothing does not become a property change.
        static APPLIED: RefCell<HashMap<xlib::Window, Vec<c_long>>> =
            RefCell::new(HashMap::new());
        /// Interned once: asking the server to intern is a round trip.
        static ATOM: Cell<Option<xlib::Atom>> = const { Cell::new(None) };
    }

    pub(super) fn apply(window: &gtk::Window, on: bool) {
        let Some((display, xid)) = x_window(window) else {
            return;
        };
        let rows = if on { rows(window) } else { Vec::new() };
        XLIB.with(|lib| {
            let Some(lib) = lib.as_ref() else {
                return;
            };
            APPLIED.with_borrow_mut(|applied| {
                if applied.get(&xid) == Some(&rows) {
                    return;
                }
                let atom = atom(lib, display);
                // Safe: a display GDK owns and keeps open, a window id the
                // server checks, and a buffer that outlives the call, which
                // copies it.
                unsafe {
                    if rows.is_empty() {
                        (lib.XDeleteProperty)(display, xid, atom);
                    } else {
                        // Format 32 means an array of C longs, Xlib's oldest
                        // surprise, which is what the rows already are.
                        (lib.XChangeProperty)(
                            display,
                            xid,
                            atom,
                            xlib::XA_CARDINAL,
                            32,
                            xlib::PropModeReplace,
                            rows.as_ptr().cast(),
                            rows.len() as i32,
                        );
                    }
                    (lib.XFlush)(display);
                }
                applied.insert(xid, rows);
            });
        });
    }

    /// Drops what was remembered for a window that is going away. The
    /// property needs no farewell: it dies with the X window it is on.
    pub(super) fn forget(window: &gtk::Window) {
        if let Some((_, xid)) = x_window(window) {
            APPLIED.with_borrow_mut(|applied| {
                applied.remove(&xid);
            });
        }
    }

    fn x_window(window: &gtk::Window) -> Option<(*mut xlib::Display, xlib::Window)> {
        let surface = window.surface()?.downcast::<X11Surface>().ok()?;
        let display = RootExt::display(window).downcast::<X11Display>().ok()?;
        // Safe: the pointer is GDK's own connection, live for as long as the
        // display object is, and it is used before either leaves this scope.
        Some((unsafe { display.xdisplay() }, surface.xid()))
    }

    /// The region's rectangles, flattened to the quadruples the property
    /// holds: device pixels measured from the surface's corner. Unlike a
    /// Wayland compositor, the X server has not taken the client-side shadow
    /// off the surface, so the shadow's extent moves the origin.
    fn rows(window: &gtk::Window) -> Vec<c_long> {
        let Some(shape) = super::shape(window) else {
            return Vec::new();
        };
        let (dx, dy) = window.surface_transform();
        let (dx, dy) = (dx as c_long, dy as c_long);
        let scale = c_long::from(window.scale_factor());
        shape
            .rectangles()
            .into_iter()
            .flat_map(|(x, y, width, height)| {
                [
                    (c_long::from(x) + dx) * scale,
                    (c_long::from(y) + dy) * scale,
                    c_long::from(width) * scale,
                    c_long::from(height) * scale,
                ]
            })
            .collect()
    }

    fn atom(lib: &Xlib, display: *mut xlib::Display) -> xlib::Atom {
        if let Some(atom) = ATOM.get() {
            return atom;
        }
        // Safe: the name is a static C string and the display is live.
        let atom =
            unsafe { (lib.XInternAtom)(display, c"_KDE_NET_WM_BLUR_BEHIND_REGION".as_ptr(), 0) };
        ATOM.set(Some(atom));
        atom
    }
}
