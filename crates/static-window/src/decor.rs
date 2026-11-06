//! Title bars, sk_app's way: the compositor's, through `xdg-decoration`,
//! wherever it offers them; where it does not (GNOME), libdecor's, loaded
//! with `dlopen` at the first window that needs one. libdecor then owns the
//! `xdg_surface` and `xdg_toplevel`: its configure replaces ours and reports
//! the content size without the frame. Without libdecor, a window has no
//! frame. `STATIC_WINDOW_FORCE_CSD=1` takes the libdecor path anyway, to
//! try it on a desktop that decorates.
use std::cell::Cell;
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::OnceLock;

use static_ecs::Entity;
use wayland_client::Proxy;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{Mode, ZxdgToplevelDecorationV1};
use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

use crate::wayland::State;
use crate::window::{Window, WindowState};

/// Who draws title bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decorations {
    /// The compositor, asked through `xdg-decoration`.
    Server,
    /// libdecor, if it loads.
    Client,
}

/// The compositor's, unless it has no `xdg-decoration` or the environment
/// forces the libdecor path.
pub(crate) fn choose(has_xdg_decoration: bool, force_csd: bool) -> Decorations {
    if has_xdg_decoration && !force_csd { Decorations::Server } else { Decorations::Client }
}

/// `STATIC_WINDOW_FORCE_CSD` set to anything but `0`.
pub(crate) fn forced() -> bool {
    std::env::var_os("STATIC_WINDOW_FORCE_CSD").is_some_and(|v| !v.is_empty() && v != "0")
}

#[repr(C)]
struct Interface {
    error: unsafe extern "C" fn(*mut c_void, c_int, *const c_char),
    reserved: [usize; 10],
}
#[repr(C)]
struct FrameInterface {
    configure: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    close: unsafe extern "C" fn(*mut c_void, *mut c_void),
    commit: unsafe extern "C" fn(*mut c_void, *mut c_void),
    dismiss_popup: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void),
    reserved: [usize; 10],
}

type Frame = *mut c_void;

/// libdecor's functions, resolved once for the process.
struct Api {
    _lib: libloading::Library,
    new: unsafe extern "C" fn(*mut c_void, *const Interface) -> *mut c_void,
    unref: unsafe extern "C" fn(*mut c_void),
    dispatch: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    decorate: unsafe extern "C" fn(*mut c_void, *mut c_void, *const FrameInterface, *mut c_void) -> Frame,
    frame_unref: unsafe extern "C" fn(Frame),
    set_title: unsafe extern "C" fn(Frame, *const c_char),
    set_app_id: unsafe extern "C" fn(Frame, *const c_char),
    set_visibility: unsafe extern "C" fn(Frame, bool),
    set_min_content_size: unsafe extern "C" fn(Frame, c_int, c_int),
    set_max_content_size: unsafe extern "C" fn(Frame, c_int, c_int),
    set_maximized: unsafe extern "C" fn(Frame),
    unset_maximized: unsafe extern "C" fn(Frame),
    set_fullscreen: unsafe extern "C" fn(Frame, *mut c_void),
    unset_fullscreen: unsafe extern "C" fn(Frame),
    set_minimized: unsafe extern "C" fn(Frame),
    map: unsafe extern "C" fn(Frame),
    commit: unsafe extern "C" fn(Frame, *mut c_void, *mut c_void),
    state_new: unsafe extern "C" fn(c_int, c_int) -> *mut c_void,
    state_free: unsafe extern "C" fn(*mut c_void),
    content_size: unsafe extern "C" fn(*mut c_void, Frame, *mut c_int, *mut c_int) -> bool,
    window_state: unsafe extern "C" fn(*mut c_void, *mut u32) -> bool,
}

static API: OnceLock<Option<Api>> = OnceLock::new();

fn api() -> Option<&'static Api> {
    API.get_or_init(|| {
        // SAFETY: libdecor's documented entry points, typed as its header
        // declares them; the library stays loaded for the process.
        unsafe {
            let lib = libloading::Library::new("libdecor-0.so.0").ok()?;
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).ok()?
                };
            }
            Some(Api {
                new: sym!("libdecor_new"),
                unref: sym!("libdecor_unref"),
                dispatch: sym!("libdecor_dispatch"),
                decorate: sym!("libdecor_decorate"),
                frame_unref: sym!("libdecor_frame_unref"),
                set_title: sym!("libdecor_frame_set_title"),
                set_app_id: sym!("libdecor_frame_set_app_id"),
                set_visibility: sym!("libdecor_frame_set_visibility"),
                set_min_content_size: sym!("libdecor_frame_set_min_content_size"),
                set_max_content_size: sym!("libdecor_frame_set_max_content_size"),
                set_maximized: sym!("libdecor_frame_set_maximized"),
                unset_maximized: sym!("libdecor_frame_unset_maximized"),
                set_fullscreen: sym!("libdecor_frame_set_fullscreen"),
                unset_fullscreen: sym!("libdecor_frame_unset_fullscreen"),
                set_minimized: sym!("libdecor_frame_set_minimized"),
                map: sym!("libdecor_frame_map"),
                commit: sym!("libdecor_frame_commit"),
                state_new: sym!("libdecor_state_new"),
                state_free: sym!("libdecor_state_free"),
                content_size: sym!("libdecor_configuration_get_content_size"),
                window_state: sym!("libdecor_configuration_get_window_state"),
                _lib: lib,
            })
        }
    })
    .as_ref()
}

thread_local! {
    /// The loop's protocol state while libdecor dispatches, for its
    /// callbacks.
    static DISPATCHING: Cell<*mut State> = const { Cell::new(std::ptr::null_mut()) };
}

/// A libdecor context on our connection.
pub(crate) struct Libdecor(*mut c_void);

impl Libdecor {
    /// Load libdecor and attach it to `display` (a `wl_display*`).
    pub fn new(display: *mut c_void) -> Option<Self> {
        static INTERFACE: Interface = Interface { error, reserved: [0; 10] };
        let api = api()?;
        // SAFETY: a live display; the interface is static.
        let context = unsafe { (api.new)(display, &INTERFACE) };
        (!context.is_null()).then_some(Self(context))
    }
    /// Run libdecor's pending events (its objects are on libwayland's
    /// default queue, ours on wayland-rs's), its callbacks seeing `state`.
    pub fn dispatch(state: &mut State) {
        let (Some(api), Some(context)) = (api(), state.decor.as_ref().map(|d| d.0)) else { return };
        DISPATCHING.set(state);
        // SAFETY: a live context; callbacks reach `state` only through
        // `DISPATCHING`, and nothing else touches it meanwhile.
        unsafe { (api.dispatch)(context, 0) };
        DISPATCHING.set(std::ptr::null_mut());
    }
    /// Decorate `surface`, configured like `window`, and map it.
    pub fn decorate(&self, surface: &WlSurface, entity: Entity, window: &Window) -> Option<DecorFrame> {
        let api = api()?;
        static FRAME: FrameInterface = FrameInterface { configure, close, commit, dismiss_popup, reserved: [0; 10] };
        let data = Box::into_raw(Box::new(entity));
        // SAFETY: a live context and surface; `data` lives until the frame
        // is dropped.
        let frame = unsafe { (api.decorate)(self.0, surface.id().as_ptr().cast(), &FRAME, data.cast()) };
        if frame.is_null() {
            drop(unsafe { Box::from_raw(data) });
            return None;
        }
        let frame = DecorFrame { frame, data };
        frame.set_title(&window.title);
        frame.set_app_id(&crate::run::app_id(window));
        frame.set_limits(window);
        // SAFETY: a live frame.
        unsafe { (api.map)(frame.frame) };
        Some(frame)
    }
}

impl Drop for Libdecor {
    fn drop(&mut self) {
        if let Some(api) = api() {
            // SAFETY: the context made in `new`; its frames are gone.
            unsafe { (api.unref)(self.0) };
        }
    }
}

/// A libdecor frame around one window.
pub(crate) struct DecorFrame {
    frame: Frame,
    data: *mut Entity,
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

impl DecorFrame {
    fn api(&self) -> &'static Api {
        api().expect("a frame implies libdecor loaded")
    }
    pub fn set_title(&self, title: &str) {
        // SAFETY: a live frame; libdecor copies the string.
        unsafe { (self.api().set_title)(self.frame, c_string(title).as_ptr()) }
    }
    pub fn set_app_id(&self, app_id: &str) {
        // SAFETY: as above.
        unsafe { (self.api().set_app_id)(self.frame, c_string(app_id).as_ptr()) }
    }
    pub fn set_limits(&self, window: &Window) {
        let ((min_w, min_h), (max_w, max_h)) = window.limits();
        // SAFETY: a live frame.
        unsafe {
            (self.api().set_min_content_size)(self.frame, min_w as c_int, min_h as c_int);
            (self.api().set_max_content_size)(self.frame, max_w as c_int, max_h as c_int);
        }
    }
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: a live frame.
        unsafe { (self.api().set_visibility)(self.frame, visible) }
    }
    pub fn set_maximized(&self, maximized: bool) {
        // SAFETY: a live frame.
        unsafe { if maximized { (self.api().set_maximized)(self.frame) } else { (self.api().unset_maximized)(self.frame) } }
    }
    pub fn set_fullscreen(&self, fullscreen: bool) {
        // SAFETY: a live frame; no particular output.
        unsafe {
            if fullscreen { (self.api().set_fullscreen)(self.frame, std::ptr::null_mut()) } else { (self.api().unset_fullscreen)(self.frame) }
        }
    }
    pub fn set_minimized(&self) {
        // SAFETY: a live frame.
        unsafe { (self.api().set_minimized)(self.frame) }
    }
    /// The app chose a new content size for a floating window.
    pub fn resize(&self, width: u32, height: u32) {
        let api = self.api();
        // SAFETY: a live frame; the state is freed after the commit.
        unsafe {
            let state = (api.state_new)(width as c_int, height as c_int);
            (api.commit)(self.frame, state, std::ptr::null_mut());
            (api.state_free)(state);
        }
    }
}

impl Drop for DecorFrame {
    fn drop(&mut self) {
        // SAFETY: the frame and data made in `decorate`.
        unsafe {
            (self.api().frame_unref)(self.frame);
            drop(Box::from_raw(self.data));
        }
    }
}

/// A window's shell role: a plain toplevel (decorated by the compositor if
/// it can), or a libdecor frame.
pub(crate) enum Role {
    Xdg { xdg: XdgSurface, toplevel: XdgToplevel, decoration: Option<ZxdgToplevelDecorationV1> },
    Decor(DecorFrame),
}

impl Role {
    pub fn set_title(&self, title: &str) {
        match self {
            Role::Xdg { toplevel, .. } => toplevel.set_title(title.into()),
            Role::Decor(frame) => frame.set_title(title),
        }
    }
    pub fn set_app_id(&self, app_id: &str) {
        match self {
            Role::Xdg { toplevel, .. } => toplevel.set_app_id(app_id.into()),
            Role::Decor(frame) => frame.set_app_id(app_id),
        }
    }
    pub fn set_limits(&self, window: &Window) {
        match self {
            Role::Xdg { toplevel, .. } => {
                let ((min_w, min_h), (max_w, max_h)) = window.limits();
                toplevel.set_min_size(min_w as i32, min_h as i32);
                toplevel.set_max_size(max_w as i32, max_h as i32);
            }
            Role::Decor(frame) => frame.set_limits(window),
        }
    }
    /// Undecorated: neither side draws a frame.
    pub fn set_decorated(&self, decorated: bool) {
        match self {
            Role::Xdg { decoration: Some(decoration), .. } => {
                decoration.set_mode(if decorated { Mode::ServerSide } else { Mode::ClientSide });
            }
            Role::Xdg { decoration: None, .. } => {}
            Role::Decor(frame) => frame.set_visible(decorated),
        }
    }
    pub fn set_maximized(&self, maximized: bool) {
        match self {
            Role::Xdg { toplevel, .. } if maximized => toplevel.set_maximized(),
            Role::Xdg { toplevel, .. } => toplevel.unset_maximized(),
            Role::Decor(frame) => frame.set_maximized(maximized),
        }
    }
    pub fn set_fullscreen(&self, fullscreen: bool) {
        match self {
            Role::Xdg { toplevel, .. } if fullscreen => toplevel.set_fullscreen(None),
            Role::Xdg { toplevel, .. } => toplevel.unset_fullscreen(),
            Role::Decor(frame) => frame.set_fullscreen(fullscreen),
        }
    }
    pub fn set_minimized(&self) {
        match self {
            Role::Xdg { toplevel, .. } => toplevel.set_minimized(),
            Role::Decor(frame) => frame.set_minimized(),
        }
    }
    /// The app resized a floating window: libdecor must be told (it sizes
    /// the frame); a plain toplevel takes it from the next buffer.
    pub fn resized(&self, width: u32, height: u32) {
        if let Role::Decor(frame) = self {
            frame.resize(width, height);
        }
    }
}

impl Drop for Role {
    fn drop(&mut self) {
        if let Role::Xdg { xdg, toplevel, decoration } = self {
            // A decoration before its toplevel: the reverse is a protocol
            // error that ends the connection.
            if let Some(decoration) = decoration.take() {
                decoration.destroy();
            }
            toplevel.destroy();
            xdg.destroy();
        }
    }
}

/// The protocol state and window a callback is about.
fn dispatching<'a>(data: *mut c_void) -> Option<(&'a mut State, Entity)> {
    let state = DISPATCHING.get();
    // SAFETY: set for the duration of `Libdecor::dispatch`, with nothing
    // else borrowing the state; `data` is the frame's boxed entity.
    unsafe { (!state.is_null() && !data.is_null()).then(|| (&mut *state, *data.cast::<Entity>())) }
}

unsafe extern "C" fn error(_: *mut c_void, _: c_int, message: *const c_char) {
    // SAFETY: libdecor passes a C string.
    let message = unsafe { std::ffi::CStr::from_ptr(message) };
    eprintln!("static-window: libdecor: {}", message.to_string_lossy());
}

/// libdecor's configure: the content size (frame excluded) and state, which
/// it needs committed before this returns.
unsafe extern "C" fn configure(frame: Frame, configuration: *mut c_void, data: *mut c_void) {
    let (Some(api), Some((state, entity))) = (api(), dispatching(data)) else { return };
    let Some(window) = state.window(entity) else { return };
    let (mut width, mut height) = (0, 0);
    // SAFETY: the configuration libdecor handed us, valid during this call.
    unsafe {
        if !(api.content_size)(configuration, frame, &mut width, &mut height) || width <= 0 || height <= 0 {
            // The size is ours to choose.
            (width, height) = (window.size.0 as c_int, window.size.1 as c_int);
        }
        let mut bits = 0u32;
        if (api.window_state)(configuration, &mut bits) {
            window.pending_state = WindowState {
                activated: bits & 1 != 0,
                maximized: bits & 2 != 0,
                fullscreen: bits & 4 != 0,
                resizing: false,
                suspended: bits & 128 != 0,
            };
        }
        let libdecor_state = (api.state_new)(width, height);
        (api.commit)(frame, libdecor_state, configuration);
        (api.state_free)(libdecor_state);
    }
    window.pending_size = (width, height);
    window.configured_now = true;
    window.dirty = true;
}

unsafe extern "C" fn close(_: Frame, data: *mut c_void) {
    if let Some((state, entity)) = dispatching(data)
        && let Some(window) = state.window(entity)
    {
        window.close_requested = true;
    }
}

/// libdecor applied frame state that needs a surface commit to take effect.
unsafe extern "C" fn commit(_: Frame, data: *mut c_void) {
    if let Some((state, entity)) = dispatching(data)
        && let Some(window) = state.window(entity)
    {
        window.surface.commit();
    }
}

unsafe extern "C" fn dismiss_popup(_: Frame, _: *const c_char, _: *mut c_void) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compositor_draws_title_bars_unless_it_cannot_or_is_overruled() {
        assert_eq!(choose(true, false), Decorations::Server);
        assert_eq!(choose(false, false), Decorations::Client);
        assert_eq!(choose(true, true), Decorations::Client);
    }
}
