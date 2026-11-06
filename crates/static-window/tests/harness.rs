//! The scripted compositor itself, driven by a raw client: what the window
//! tests rely on.
mod support;

use support::compositor::{Compositor, Globals};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Client {
    configured: Option<u32>,
}
impl Dispatch<WlRegistry, GlobalListContents> for Client {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(Client: WlCompositor);
delegate_noop!(Client: ignore WlSurface);
delegate_noop!(Client: ignore xdg_wm_base::XdgWmBase);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(client: &mut Self, surface: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            client.configured = Some(serial);
        }
    }
}

#[test]
fn a_raw_client_binds_makes_a_toplevel_and_is_configured() {
    let (compositor, socket) = Compositor::spawn(Globals { fractional_scale: false, ..Globals::default() });
    let conn = Connection::from_socket(socket).unwrap();
    let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
    let names: Vec<String> = globals.contents().clone_list().into_iter().map(|g| g.interface).collect();
    assert!(names.iter().any(|n| n == "xdg_wm_base") && names.iter().any(|n| n == "zxdg_decoration_manager_v1"));
    assert!(!names.iter().any(|n| n == "wp_fractional_scale_manager_v1"), "{names:?}");
    let qh = queue.handle();
    let compositor_global: WlCompositor = globals.bind(&qh, 4..=6, ()).unwrap();
    let wm: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=6, ()).unwrap();
    let surface = compositor_global.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("hello".into());
    surface.commit();
    conn.flush().unwrap();

    let serial = compositor.with(|server, _| {
        assert_eq!(server.toplevel(0).title, "hello");
        assert_eq!(server.surface_of(0).commits.len(), 1);
        server.configure(0, 320, 200, &[])
    });
    let mut client = Client::default();
    while client.configured.is_none() {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    assert_eq!(client.configured, Some(serial));
    conn.flush().unwrap();
    assert_eq!(compositor.with(|server, _| server.toplevel(0).acked.clone()), [serial]);
}
