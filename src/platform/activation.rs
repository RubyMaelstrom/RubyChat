//! Raising the window with an activation token (the `xdg_activation_v1`
//! Wayland protocol, wayland-protocols staging). A notification's click, a
//! tray host or a later launch can hand RubyChat a token; `activate`
//! passes it to the compositor with the window's surface, which is how a
//! Wayland client may take focus. Whether it does is the compositor's
//! call. A new window takes its token at creation instead (winit's
//! `with_activation_token`).
//!
//! Like the clipboard, it shares winit's connection with an event queue of
//! its own, dispatched without blocking whenever the event loop wakes.

use std::ffi::c_void;
use std::ptr::NonNull;

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::{self, XdgActivationV1};

pub struct Activation {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

#[derive(Default)]
struct State {
    activation: Option<XdgActivationV1>,
}

impl Activation {
    /// Activation on the Wayland connection `display`.
    ///
    /// # Safety
    ///
    /// `display` must be a live `wl_display` that outlives this: the event
    /// loop's own connection. Drop it before that connection closes.
    pub unsafe fn new(display: NonNull<c_void>) -> Self {
        // SAFETY: the caller keeps the display alive.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let _ = connection.flush();
        Self {
            connection,
            queue,
            state: State::default(),
        }
    }

    /// Handles what has arrived; call it whenever the event loop wakes.
    pub fn dispatch(&mut self) {
        if let Err(error) = self.queue.dispatch_pending(&mut self.state) {
            eprintln!("rubychat: activation: {error}");
        }
    }

    /// Asks the compositor to activate the window whose `wl_surface` is
    /// `surface`; false if it doesn't offer activation.
    ///
    /// # Safety
    ///
    /// `surface` must be the live `wl_surface` of a window on this
    /// connection.
    pub unsafe fn activate(&mut self, token: &str, surface: NonNull<c_void>) -> bool {
        self.dispatch();
        let Some(activation) = &self.state.activation else {
            return false;
        };
        // SAFETY: the caller vouches for the surface.
        let id =
            match unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) } {
                Ok(id) => id,
                Err(_) => return false,
            };
        let Ok(surface) = WlSurface::from_id(&self.connection, id) else {
            return false;
        };
        activation.activate(token.to_string(), &surface);
        let _ = self.connection.flush();
        true
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "xdg_activation_v1"
            && state.activation.is_none()
        {
            state.activation = Some(registry.bind(name, 1, handle, ()));
        }
    }
}

impl Dispatch<XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The global has no events.
    }
}

#[cfg(test)]
mod tests {
    use wayland_client::protocol::wl_compositor::WlCompositor;
    use wayland_client::protocol::wl_registry;

    use super::*;

    #[derive(Default)]
    struct Globals {
        compositor: Option<WlCompositor>,
    }

    impl Dispatch<WlRegistry, ()> for Globals {
        fn event(
            state: &mut Self,
            registry: &WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            handle: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name, interface, ..
            } = event
                && interface == "wl_compositor"
            {
                state.compositor = Some(registry.bind(name, 1, handle, ()));
            }
        }
    }

    wayland_client::delegate_noop!(Globals: ignore WlCompositor);
    wayland_client::delegate_noop!(Globals: ignore WlSurface);

    /// The running compositor: hands a surface that is never mapped to
    /// `activate` with a made-up token, which compositors may ignore, and
    /// checks the connection survives (`cargo test --lib activates_a_surface
    /// -- --ignored`).
    #[test]
    #[ignore]
    fn activates_a_surface() {
        let connection = Connection::connect_to_env().unwrap();
        let mut queue = connection.new_event_queue();
        let mut globals = Globals::default();
        connection.display().get_registry(&queue.handle(), ());
        queue.roundtrip(&mut globals).unwrap();
        let surface = globals
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&queue.handle(), ());
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        // SAFETY: the connection outlives the activation, dropped first.
        let mut activation = unsafe { Activation::new(display) };
        activation.queue.roundtrip(&mut activation.state).unwrap();
        let pointer = NonNull::new(surface.id().as_ptr().cast()).unwrap();
        // SAFETY: the surface is alive on this connection.
        assert!(unsafe { activation.activate("rubychat-test-token", pointer) });
        queue.roundtrip(&mut globals).unwrap();
        drop(activation);
        surface.destroy();
    }
}
