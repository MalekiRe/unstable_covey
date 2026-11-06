//! The event loop: one wakeup reads the socket, applies what the compositor
//! said to the world (emitting an event label per change), emits [`Step`],
//! then sends what the world wants (new, changed and despawned windows).
//! With nothing animating and no deadline it sleeps in `poll`.
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use static_ecs::prelude::*;
use static_ecs::{Dispatch as Handles, Inbox, Stores, Wakeup};
use wayland_client::globals::{BindError, GlobalError, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{Connection, ConnectError, EventQueue, Proxy};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode as DecorationMode;

use crate::event::{CloseRequested, FocusChanged, FrameTime, KeyInput, PointerButton, PointerLeft, PointerMoved, Redraw, Resized, Scroll, Step, TextInput};
use crate::input::{Keys, Pointer};
use crate::wayland::{Globals, Proto, State, placeholder};
use crate::window::{Minimize, Surface, Window, WindowMode, WindowState};

/// The world's vocabulary the loop reads and writes: what [`WindowPlugin`]
/// declares.
///
/// [`WindowPlugin`]: crate::WindowPlugin
pub trait WindowWorld:
    Storage
    + Stores<Window>
    + Stores<WindowMode>
    + Stores<Minimize>
    + Stores<Surface>
    + Stores<WindowState>
    + Stores<Keys>
    + Stores<Pointer>
    + Stores<WakeAt>
    + Stores<FrameTime>
    + Stores<Wakeup>
    + Stores<Inbox<CloseRequested>>
    + Stores<Inbox<Resized>>
    + Stores<Inbox<FocusChanged>>
    + Stores<Inbox<PointerMoved>>
    + Stores<Inbox<PointerLeft>>
    + Stores<Inbox<PointerButton>>
    + Stores<Inbox<Scroll>>
    + Stores<Inbox<KeyInput>>
    + Stores<Inbox<TextInput>>
{
}
impl<S> WindowWorld for S where
    S: Storage
        + Stores<Window>
        + Stores<WindowMode>
        + Stores<Minimize>
        + Stores<Surface>
        + Stores<WindowState>
        + Stores<Keys>
        + Stores<Pointer>
        + Stores<WakeAt>
        + Stores<FrameTime>
        + Stores<Wakeup>
        + Stores<Inbox<CloseRequested>>
        + Stores<Inbox<Resized>>
        + Stores<Inbox<FocusChanged>>
        + Stores<Inbox<PointerMoved>>
        + Stores<Inbox<PointerLeft>>
        + Stores<Inbox<PointerButton>>
        + Stores<Inbox<Scroll>>
        + Stores<Inbox<KeyInput>>
        + Stores<Inbox<TextInput>>
{
}

/// Every label the loop emits (any app's listener list handles them all).
pub trait WindowLabels<S: Storage>:
    Handles<Step, World<S>>
    + Handles<Redraw, World<S>>
    + Handles<CloseRequested, World<S>>
    + Handles<Resized, World<S>>
    + Handles<FocusChanged, World<S>>
    + Handles<PointerMoved, World<S>>
    + Handles<PointerLeft, World<S>>
    + Handles<PointerButton, World<S>>
    + Handles<Scroll, World<S>>
    + Handles<KeyInput, World<S>>
    + Handles<TextInput, World<S>>
{
}
impl<S: Storage, L> WindowLabels<S> for L where
    L: Handles<Step, World<S>>
        + Handles<Redraw, World<S>>
        + Handles<CloseRequested, World<S>>
        + Handles<Resized, World<S>>
        + Handles<FocusChanged, World<S>>
        + Handles<PointerMoved, World<S>>
        + Handles<PointerLeft, World<S>>
        + Handles<PointerButton, World<S>>
        + Handles<Scroll, World<S>>
        + Handles<KeyInput, World<S>>
        + Handles<TextInput, World<S>>
{
}

/// The earliest moment the loop must wake even if nothing else happens (a
/// timer). Reset before each [`Step`]; write it with [`WakeAt::request`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakeAt(Option<Instant>);
impl WakeAt {
    /// Wake no later than `at`.
    pub fn request(&mut self, at: Instant) {
        self.0 = Some(self.0.map_or(at, |t| t.min(at)));
    }
    pub fn get(&self) -> Option<Instant> {
        self.0
    }
}

/// Why the loop could not start.
#[derive(Debug)]
pub enum Error {
    Connect(ConnectError),
    Registry(GlobalError),
    /// A required global (`wl_compositor`, `wl_shm`, `xdg_wm_base`) is
    /// missing.
    Missing(&'static str, BindError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "cannot connect to the Wayland compositor: {e}"),
            Error::Registry(e) => write!(f, "cannot read the compositor's globals: {e}"),
            Error::Missing(name, e) => write!(f, "the compositor lacks {name}: {e}"),
        }
    }
}
impl std::error::Error for Error {}

/// What a step decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// No windows remain.
    Done,
    /// The compositor went away.
    Disconnected,
}

/// The loop's own counters, for measuring an idle window.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Returns from `poll`.
    pub wakeups: u64,
    /// Surface commits.
    pub commits: u64,
}

/// A Wayland connection driving one app's windows.
pub struct WindowLoop {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    globals: Globals,
    /// Shared 1×1 placeholder (with a viewporter).
    placeholder: Option<WlBuffer>,
    /// Behaviors and other threads wake the loop through this eventfd.
    wake: Arc<OwnedFd>,
    woken: bool,
    started: bool,
    last_step: Option<Instant>,
    missing: Vec<&'static str>,
    pub stats: Stats,
}

impl WindowLoop {
    /// Connect to the compositor named by the environment.
    pub fn connect() -> Result<Self, Error> {
        Self::with(Connection::connect_to_env().map_err(Error::Connect)?)
    }
    /// Use an already connected socket (a test compositor, a socket handed
    /// over by a launcher).
    pub fn from_socket(socket: UnixStream) -> Result<Self, Error> {
        Self::with(Connection::from_socket(socket).map_err(Error::Connect)?)
    }
    fn with(conn: Connection) -> Result<Self, Error> {
        let (list, queue) = registry_queue_init::<State>(&conn).map_err(Error::Registry)?;
        let qh = queue.handle();
        let globals = Globals {
            compositor: list.bind(&qh, 4..=6, ()).map_err(|e| Error::Missing("wl_compositor", e))?,
            shm: list.bind(&qh, 1..=1, ()).map_err(|e| Error::Missing("wl_shm", e))?,
            wm: list.bind(&qh, 1..=6, ()).map_err(|e| Error::Missing("xdg_wm_base", e))?,
            decoration: list.bind(&qh, 1..=1, ()).ok(),
            viewporter: list.bind(&qh, 1..=1, ()).ok(),
            fractional: list.bind(&qh, 1..=1, ()).ok(),
        };
        let mut state = State::default();
        let registry = list.registry().clone();
        for global in list.contents().clone_list() {
            if global.interface == WlOutput::interface().name {
                state.bind_output(&registry, global.name, global.version, &qh);
            }
        }
        let mut missing = Vec::new();
        if globals.decoration.is_none() {
            missing.push("xdg-decoration: windows have no title bar");
        }
        if globals.fractional.is_none() || globals.viewporter.is_none() {
            missing.push("fractional-scale-v1 and viewporter: display scaling rounds to whole steps");
        }
        let wake = Arc::new(
            rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK).expect("eventfd"),
        );
        Ok(Self {
            conn,
            queue,
            state,
            globals,
            placeholder: None,
            wake,
            woken: false,
            started: false,
            last_step: None,
            missing,
            stats: Stats::default(),
        })
    }

    /// Optional globals the compositor lacks, and what each costs.
    pub fn missing(&self) -> &[&'static str] {
        &self.missing
    }

    /// Run until no windows remain or the compositor goes away.
    pub fn run<S: WindowWorld, L: WindowLabels<S>>(mut self, app: &mut App<S, L>) -> Stats {
        while self.step(app, None) == Flow::Continue {}
        self.stats
    }

    /// One wakeup: wait up to `timeout` (`None`: as long as nothing needs
    /// doing) unless work is pending, then apply events, emit [`Step`], and
    /// send the world's requests.
    pub fn step<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>, timeout: Option<Duration>) -> Flow {
        if !self.started {
            self.started = true;
            self.attach(&mut app.world);
        } else if !self.has_work(&app.world) && !self.wait(&app.world, timeout) {
            return Flow::Disconnected;
        }
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return Flow::Disconnected;
        }
        self.apply(app);
        let now = Instant::now();
        let dt = self.last_step.map_or(0.0, |t| (now - t).as_secs_f32());
        self.last_step = Some(now);
        if let Some(mut wake) = app.world.resource_mut::<WakeAt>() {
            *wake = WakeAt::default();
        }
        app.world.insert_resource(FrameTime { now_ns: now_ns(), display_ns: 0, dt });
        app.emit(&Step);
        self.sync(&mut app.world);
        if self.conn.flush().is_err() {
            return Flow::Disconnected;
        }
        if self.state.windows.is_empty() { Flow::Done } else { Flow::Continue }
    }

    /// Resources the loop needs, and the wakeup behaviors call.
    fn attach<S: WindowWorld>(&self, world: &mut World<S>) {
        world.init_resource::<Keys>();
        world.init_resource::<Pointer>();
        world.init_resource::<WakeAt>();
        let wake = self.wake.clone();
        world.insert_resource(Wakeup(Arc::new(move || {
            let _ = rustix::io::write(&*wake, &1u64.to_ne_bytes());
        })));
    }

    fn has_work<S: WindowWorld>(&self, world: &World<S>) -> bool {
        self.woken
            || self.state.windows.iter().any(|w| w.dirty || w.close_requested)
            || world.resource::<WakeAt>().and_then(WakeAt::get).is_some_and(|at| at <= Instant::now())
    }

    /// Sleep in `poll` on the socket and the wakeup eventfd until one is
    /// readable, the deadline, or `timeout`. False if the connection broke.
    fn wait<S: WindowWorld>(&mut self, world: &World<S>, timeout: Option<Duration>) -> bool {
        if self.conn.flush().is_err() {
            return false;
        }
        let Some(guard) = self.queue.prepare_read() else {
            // Events are already queued.
            return true;
        };
        let deadline = world.resource::<WakeAt>().and_then(WakeAt::get).map(|at| at.saturating_duration_since(Instant::now()));
        let timeout = match (timeout, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timespec = timeout.map(|t| Timespec { tv_sec: t.as_secs() as i64, tv_nsec: t.subsec_nanos() as i64 });
        let socket = guard.connection_fd();
        let mut fds = [PollFd::new(&socket, PollFlags::IN), PollFd::new(&*self.wake, PollFlags::IN)];
        match rustix::event::poll(&mut fds, timespec.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
        self.stats.wakeups += 1;
        let (socket, wake) = (fds[0].revents(), fds[1].revents());
        if socket.intersects(PollFlags::ERR | PollFlags::HUP) && !socket.contains(PollFlags::IN) {
            return false;
        }
        if socket.contains(PollFlags::IN) {
            if guard.read().is_err() {
                return false;
            }
        } else {
            drop(guard);
        }
        // The compositor hung up: whatever it sent last has been read.
        if socket.contains(PollFlags::HUP) {
            return false;
        }
        if wake.contains(PollFlags::IN) {
            let mut count = [0u8; 8];
            let _ = rustix::io::read(&*self.wake, &mut count);
            self.woken = true;
        }
        true
    }

    /// What the compositor said, into the world: sizes, scales, states,
    /// close requests, each change emitted as its label.
    fn apply<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>) {
        self.woken = false;
        if std::mem::take(&mut self.state.outputs_changed) {
            for window in &mut self.state.windows {
                window.dirty = true;
            }
        }
        // Windows the app despawned since the last sync are gone already.
        self.state.windows.retain(|w| app.world.get::<Window>(w.entity).is_some());
        for i in 0..self.state.windows.len() {
            let window = &mut self.state.windows[i];
            let entity = window.entity;
            if std::mem::take(&mut window.dirty) && (window.configured || window.configured_now) {
                let first = !window.configured;
                if std::mem::take(&mut window.configured_now) {
                    window.configured = true;
                    let (w, h) = window.pending_size;
                    if w > 0 && h > 0 {
                        window.size = (w as u32, h as u32);
                    }
                    window.state = window.pending_state;
                }
                let scale = window.scale(&self.state.outputs);
                let surface = Surface { width: window.size.0, height: window.size.1, scale };
                let state = window.state;
                self.present_placeholder(i, &surface);
                let world = &mut app.world;
                if world.get::<WindowState>(entity) != Some(&state) {
                    world.insert(entity, state);
                }
                let previous = world.get::<Surface>(entity).copied();
                if previous != Some(surface) {
                    world.insert(entity, surface);
                    if !first && previous.is_some() {
                        app.emit(&Resized { window: entity, surface });
                        app.world.deliver(entity, Resized { window: entity, surface });
                    }
                }
            }
            let window = &mut self.state.windows[i];
            if std::mem::take(&mut window.close_requested) {
                let close = app.world.get::<Window>(entity).is_some_and(|w| w.close_on_request);
                app.emit(&CloseRequested { window: entity });
                app.world.deliver(entity, CloseRequested { window: entity });
                if close {
                    app.world.despawn(entity);
                }
            }
        }
    }

    /// A window nothing draws into still needs a buffer to be shown: an
    /// opaque placeholder, stretched by the viewport where there is one.
    fn present_placeholder(&mut self, index: usize, surface: &Surface) {
        let qh = self.queue.handle();
        let window = &mut self.state.windows[index];
        if let Some(viewport) = &window.viewport {
            viewport.set_destination(surface.width as i32, surface.height as i32);
            if window.placeholder.is_none() {
                let shared = self.placeholder.get_or_insert_with(|| placeholder(&self.globals.shm, &qh, 1, 1)).clone();
                window.surface.attach(Some(&shared), 0, 0);
                window.surface.damage_buffer(0, 0, 1, 1);
                window.placeholder = Some((shared, (1, 1)));
            }
        } else {
            let factor = surface.scale.0.div_ceil(crate::Scale::UNIT);
            let size = (surface.width * factor, surface.height * factor);
            if window.placeholder.as_ref().is_none_or(|(_, s)| *s != size) {
                if let Some((old, _)) = window.placeholder.take() {
                    old.destroy();
                }
                let buffer = placeholder(&self.globals.shm, &qh, size.0, size.1);
                window.surface.attach(Some(&buffer), 0, 0);
                window.surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
                window.placeholder = Some((buffer, size));
            }
            window.surface.set_buffer_scale(factor as i32);
        }
        window.surface.commit();
        self.stats.commits += 1;
    }

    /// What the world wants, to the compositor: new windows, changed ones,
    /// and despawned ones.
    fn sync<S: WindowWorld>(&mut self, world: &mut World<S>) {
        // Despawned (or `Window` removed): dropping the protocol objects
        // destroys them.
        self.state.windows.retain(|w| world.get::<Window>(w.entity).is_some());
        let mut created = Vec::new();
        world.query::<(Entity, &Window)>().each(|(entity, window)| {
            if !self.state.windows.iter().any(|w| w.entity == entity) {
                created.push((entity, window.clone()));
            }
        });
        for (entity, window) in created {
            let mode = world.get::<WindowMode>(entity).copied().unwrap_or_default();
            self.create(entity, window, mode);
        }
        for window in &mut self.state.windows {
            let entity = window.entity;
            if let Some(wanted) = world.get::<Window>(entity)
                && *wanted != window.sent
            {
                let wanted = wanted.clone();
                update(window, wanted);
            }
            let mode = world.get::<WindowMode>(entity).copied().unwrap_or_default();
            if mode != window.sent_mode {
                set_mode(window, mode);
            }
            if world.remove::<Minimize>(entity).is_some() {
                window.toplevel.set_minimized();
            }
        }
    }

    fn create(&mut self, entity: Entity, window: Window, mode: WindowMode) {
        let qh = self.queue.handle();
        let g = &self.globals;
        let surface = g.compositor.create_surface(&qh, entity);
        let xdg = g.wm.get_xdg_surface(&surface, &qh, entity);
        let toplevel = xdg.get_toplevel(&qh, entity);
        let decoration = g.decoration.as_ref().map(|manager| manager.get_toplevel_decoration(&toplevel, &qh, entity));
        let viewport = g.viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
        let fractional = match (&g.fractional, &viewport) {
            (Some(manager), Some(_)) => Some(manager.get_fractional_scale(&surface, &qh, entity)),
            _ => None,
        };
        // Everything before the first commit, so the first configure
        // already reflects it.
        toplevel.set_title(window.title.clone());
        toplevel.set_app_id(app_id(&window));
        send_limits(&toplevel, &window);
        if let Some(decoration) = &decoration {
            decoration.set_mode(decoration_mode(&window));
        }
        let size = (window.width.max(1), window.height.max(1));
        let mut proto = Proto {
            entity,
            surface,
            xdg,
            toplevel,
            decoration,
            viewport,
            fractional,
            sent: window,
            sent_mode: WindowMode::Windowed,
            pending_size: (0, 0),
            pending_state: WindowState::default(),
            configured_now: false,
            configured: false,
            size,
            state: WindowState::default(),
            fractional_scale: None,
            preferred_buffer_scale: None,
            outputs: Vec::new(),
            dirty: false,
            close_requested: false,
            server_decorated: None,
            placeholder: None,
        };
        set_mode(&mut proto, mode);
        proto.surface.commit();
        self.state.windows.push(proto);
    }
}

fn app_id(window: &Window) -> String {
    if window.app_id.is_empty() { exe_name() } else { window.app_id.clone() }
}
fn send_limits(toplevel: &wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel, window: &Window) {
    let ((min_w, min_h), (max_w, max_h)) = window.limits();
    toplevel.set_min_size(min_w as i32, min_h as i32);
    toplevel.set_max_size(max_w as i32, max_h as i32);
}
/// Undecorated: neither side draws a frame.
fn decoration_mode(window: &Window) -> DecorationMode {
    if window.decorated { DecorationMode::ServerSide } else { DecorationMode::ClientSide }
}

/// Send what differs between `window.sent` and `wanted`.
fn update(window: &mut Proto, wanted: Window) {
    let sent = &window.sent;
    if wanted.title != sent.title {
        window.toplevel.set_title(wanted.title.clone());
    }
    if wanted.app_id != sent.app_id {
        window.toplevel.set_app_id(app_id(&wanted));
    }
    if wanted.limits() != sent.limits() {
        send_limits(&window.toplevel, &wanted);
    }
    if wanted.decorated != sent.decorated
        && let Some(decoration) = &window.decoration
    {
        decoration.set_mode(decoration_mode(&wanted));
    }
    // A floating window's size is ours to choose.
    if (wanted.width, wanted.height) != (sent.width, sent.height) && !window.state.maximized && !window.state.fullscreen {
        window.size = (wanted.width.max(1), wanted.height.max(1));
        window.dirty = true;
    }
    window.sent = wanted;
}

fn set_mode(window: &mut Proto, mode: WindowMode) {
    let toplevel = &window.toplevel;
    match (window.sent_mode, mode) {
        (WindowMode::Maximized, _) => toplevel.unset_maximized(),
        (WindowMode::Fullscreen, _) => toplevel.unset_fullscreen(),
        _ => {}
    }
    match mode {
        WindowMode::Maximized => toplevel.set_maximized(),
        WindowMode::Fullscreen => toplevel.set_fullscreen(None),
        WindowMode::Windowed => {}
    }
    window.sent_mode = mode;
}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "static-window".into())
}

pub(crate) fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Connect to the compositor and run `app` until its windows are gone.
/// Optional globals the compositor lacks are reported once on stderr.
pub fn run<S: WindowWorld, L: WindowLabels<S>>(app: &mut App<S, L>) -> Result<Stats, Error> {
    let window_loop = WindowLoop::connect()?;
    for missing in window_loop.missing() {
        eprintln!("static-window: the compositor lacks {missing}");
    }
    Ok(window_loop.run(app))
}
