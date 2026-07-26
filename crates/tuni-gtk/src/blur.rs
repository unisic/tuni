//! Asking the compositor to blur what shows through a translucent window.
//!
//! There is no way to do this by drawing: a translucent window shows the
//! wallpaper exactly as it is unless the compositor is told otherwise. The ask
//! is `ext-background-effect-v1`, spoken over the connection GDK already has
//! open; KWin answers it, and a desktop that does not never hears the question,
//! so the setting quietly does nothing there.

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
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

/// Blurs, or stops blurring, what is behind `window`.
///
/// The window has to be realized: before that it has no surface to blur. The
/// blurred region is the window's own rectangle, so this is worth calling again
/// whenever that rectangle moves or changes size.
pub fn apply(window: &impl IsA<gtk::Window>, on: bool) {
    let window = window.as_ref();
    let Some(surface) = wl_surface(window) else {
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
    let (globals, queue) = registry_queue_init::<Globals>(&connection).ok()?;
    let manager = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
    let compositor = globals.bind(&queue.handle(), 1..=6, ()).ok()?;
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

/// What a registry dispatch needs, which is nothing: the globals are read once,
/// out of the list `registry_queue_init` returns.
struct Globals;

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

delegate_noop!(Globals: ignore ExtBackgroundEffectManagerV1);
delegate_noop!(Globals: ignore ExtBackgroundEffectSurfaceV1);
delegate_noop!(Globals: ignore WlCompositor);
delegate_noop!(Globals: ignore WlRegion);
