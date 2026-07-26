//! Asking the compositor to blur what shows through a translucent window.
//!
//! There is no desktop-independent way to ask for this: a translucent window
//! shows the wallpaper exactly as it is unless the compositor is told to do
//! something else with it. KWin listens to `org_kde_kwin_blur`, so that is what
//! this speaks, over the connection GDK already has open. A compositor without
//! the protocol never hears the request, and the setting quietly does nothing.

use std::cell::RefCell;
use std::collections::HashMap;

use gdk4_wayland::prelude::*;
use gdk4_wayland::{WaylandDisplay, WaylandSurface};
use gtk::gdk;
use gtk::prelude::*;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

/// Blurs, or stops blurring, what is behind `window`.
///
/// The window has to be realized: before that it has no surface to blur, and
/// the caller is expected to come back once it has one.
pub fn apply(window: &impl IsA<gtk::Window>, on: bool) {
    let Some(wl_surface) = window
        .as_ref()
        .surface()
        .and_then(|surface| surface.downcast::<WaylandSurface>().ok())
        .and_then(|surface| surface.wl_surface())
    else {
        return;
    };

    KWIN.with_borrow_mut(|slot| {
        let Kwin::Blurs(kwin) = slot.get_or_insert_with(Kwin::look_up) else {
            return;
        };
        let id = wl_surface.id();
        if on {
            if kwin.blurred.contains_key(&id) {
                return;
            }
            let blur = kwin.manager.create(&wl_surface, &kwin.queue.handle(), ());
            // No region: the whole window, which is the only shape a terminal
            // has to ask for.
            blur.set_region(None);
            blur.commit();
            kwin.blurred.insert(id, blur);
        } else if let Some(blur) = kwin.blurred.remove(&id) {
            blur.release();
            kwin.manager.unset(&wl_surface);
        }
        let _ = kwin.connection.flush();
    });

    // The compositor picks the change up with the surface's next commit, and
    // GTK commits when it draws.
    window.as_ref().queue_draw();
}

thread_local! {
    /// Looked up once. The protocol is missing on a desktop that is not KWin,
    /// and asking the registry again on every settings change would not make it
    /// appear.
    static KWIN: RefCell<Option<Kwin>> = const { RefCell::new(None) };
}

enum Kwin {
    /// No Wayland, or a compositor that does not blur.
    Deaf,
    Blurs(Manager),
}

impl Kwin {
    fn look_up() -> Self {
        bind().map_or(Self::Deaf, Self::Blurs)
    }
}

struct Manager {
    manager: OrgKdeKwinBlurManager,
    connection: Connection,
    /// Held open for as long as the blur is wanted: KWin drops the blur from a
    /// surface when the object that asked for it goes away.
    blurred: HashMap<ObjectId, OrgKdeKwinBlur>,
    /// Nothing here sends events, but the objects belong to the queue, so it
    /// outlives them.
    queue: EventQueue<Globals>,
}

fn bind() -> Option<Manager> {
    let display = gdk::Display::default()?.downcast::<WaylandDisplay>().ok()?;
    let connection = Connection::from_backend(display.wl_display()?.backend().upgrade()?);
    let (globals, queue) = registry_queue_init::<Globals>(&connection).ok()?;
    let manager = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
    Some(Manager {
        manager,
        connection,
        blurred: HashMap::new(),
        queue,
    })
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

delegate_noop!(Globals: ignore OrgKdeKwinBlurManager);
delegate_noop!(Globals: ignore OrgKdeKwinBlur);
