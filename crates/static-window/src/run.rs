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

use crate::decor::{Decorations, Libdecor, Role};

use crate::clipboard::{Clipboard, ClipboardText};
use crate::cursor::{CursorIcon, PointerLock, Theme};
use crate::event::{
    CloseRequested, FocusChanged, FrameTime, KeyInput, PointerButton, PointerDelta, PointerLeft, PointerMoved, Redraw, Resized, Scroll, Step,
    TextInput,
};
use crate::framebuffer::{Framebuffer, Shm};
use crate::input::{Key, Keys, Pointer};
use crate::wayland::{Feedback, Globals, InputEvent, OwnedSurface, Proto, State, placeholder};
use crate::window::{Animating, KeyboardFocus, Minimize, PointerOver, Surface, Window, WindowMode, WindowState};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::Lifetime;

/// Emit a window event's label, then deliver it to its window entity for
/// behaviors.
macro_rules! emit {
    ($app:expr, $event:expr) => {{
        let event = $event;
        $app.emit(&event);
        $app.world.deliver(event.window, event);
    }};
}

/// The window still exists (the app may have despawned it mid-batch).
fn alive<S: WindowWorld>(world: &World<S>, window: Entity) -> bool {
    world.get::<Window>(window).is_some()
}

/// The world's vocabulary the loop reads and writes: what [`WindowPlugin`]
/// declares.
///
/// [`WindowPlugin`]: crate::WindowPlugin
pub trait WindowWorld:
    Storage
    + Stores<Window>
    + Stores<WindowMode>
    + Stores<Minimize>
    + Stores<Framebuffer>
    + Stores<Animating>
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
    + Stores<Inbox<PointerDelta>>
    + Stores<CursorIcon>
    + Stores<PointerLock>
    + Stores<Clipboard>
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
        + Stores<Framebuffer>
        + Stores<Animating>
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
        + Stores<Inbox<PointerDelta>>
        + Stores<CursorIcon>
        + Stores<PointerLock>
        + Stores<Clipboard>
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
    + Handles<PointerDelta, World<S>>
    + Handles<ClipboardText, World<S>>
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
        + Handles<PointerDelta, World<S>>
        + Handles<ClipboardText, World<S>>
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

/// The loop's own counters, for measuring an idle window and latency.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Returns from `poll`.
    pub wakeups: u64,
    /// Wakeups that ran a frame ([`Step`]), and what started them.
    pub frames: u64,
    pub input_frames: u64,
    pub callback_frames: u64,
    pub woken_frames: u64,
    pub timer_frames: u64,
    /// Surface commits.
    pub commits: u64,
    /// Nanoseconds from the loop's creation (or [`WindowLoop::launched_at`])
    /// to the first buffer commit leaving the process; 0 before it.
    pub first_commit_ns: u64,
    /// With [`WindowLoop::record_latency`]: for each commit that answered
    /// input or a wakeup, the nanoseconds from its arrival to the commit
    /// leaving the process (our share), and to the screen, as
    /// `wp_presentation` reports it.
    pub input_to_commit_ns: Vec<u64>,
    pub input_to_present_ns: Vec<u64>,
}

/// A Wayland connection driving one app's windows. Fields drop in order:
/// windows, libdecor and globals while the connection still stands, the
/// connection last (libdecor destroys its objects on its way out).
pub struct WindowLoop {
    state: State,
    globals: Globals,
    /// Shared 1×1 placeholder (with a viewporter).
    placeholder: Option<WlBuffer>,
    /// Behaviors and other threads wake the loop through this eventfd.
    wake: Arc<OwnedFd>,
    woken: bool,
    started: bool,
    last_step: Option<Instant>,
    /// When the launch began, for `Stats::first_commit_ns`.
    launched_ns: u64,
    /// When this wakeup's input (or wakeup) arrived.
    input_at: Option<u64>,
    record_latency: bool,
    /// Who draws title bars, and whether libdecor was tried yet.
    decorations: Decorations,
    decor_tried: bool,
    missing: Vec<&'static str>,
    /// Numbers framebuffer allocations, so a late release of a replaced
    /// buffer is ignored.
    generation: u32,
    pub stats: Stats,
    queue: EventQueue<State>,
    conn: Connection,
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
            presentation: list.bind(&qh, 1..=2, ()).ok(),
            cursor_shape: list.bind(&qh, 1..=1, ()).ok(),
            constraints: list.bind(&qh, 1..=1, ()).ok(),
            relative: list.bind(&qh, 1..=1, ()).ok(),
            data_device: list.bind(&qh, 1..=3, ()).ok(),
        };
        let mut state = State::default();
        state.seat.seat = list.bind(&qh, 1..=8, ()).ok();
        if let (Some(manager), Some(seat)) = (&globals.data_device, &state.seat.seat) {
            state.selection.device = Some(manager.get_data_device(seat, &qh, ()));
        }
        let registry = list.registry().clone();
        for global in list.contents().clone_list() {
            if global.interface == WlOutput::interface().name {
                state.bind_output(&registry, global.name, global.version, &qh);
            }
        }
        let mut missing = Vec::new();
        if globals.decoration.is_none() {
            missing.push("xdg-decoration: title bars come from libdecor, where it is installed");
        }
        if globals.fractional.is_none() || globals.viewporter.is_none() {
            missing.push("fractional-scale-v1 and viewporter: display scaling rounds to whole steps");
        }
        let wake = Arc::new(
            rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK).expect("eventfd"),
        );
        let decorations = crate::decor::choose(globals.decoration.is_some(), crate::decor::forced());
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
            launched_ns: now_ns(),
            input_at: None,
            record_latency: false,
            decorations,
            decor_tried: false,
            missing,
            generation: 0,
            stats: Stats::default(),
        })
    }

    /// Optional globals the compositor lacks, and what each costs.
    pub fn missing(&self) -> &[&'static str] {
        &self.missing
    }
    /// Measure `Stats::first_commit_ns` from `ns` (CLOCK_MONOTONIC), a
    /// launcher's timestamp from before the process started.
    pub fn launched_at(&mut self, ns: u64) {
        self.launched_ns = ns;
    }
    /// Keep every input-to-commit and input-to-screen latency in `Stats`
    /// (a growing list: for benchmarks).
    pub fn record_latency(&mut self, record: bool) {
        self.record_latency = record;
        self.state.presented = record.then(Vec::new);
    }
    /// The stats, with the presentation latencies reported so far.
    pub fn stats(&mut self) -> &Stats {
        if let Some(presented) = &mut self.state.presented {
            self.stats.input_to_present_ns.append(presented);
        }
        &self.stats
    }

    /// Run until no windows remain or the compositor goes away.
    pub fn run<S: WindowWorld, L: WindowLabels<S>>(self, app: &mut App<S, L>) -> Stats {
        self.run_with(app, |_| {})
    }
    /// [`run`](Self::run), calling `frame` after each [`Step`]: where a
    /// layer on top emits its own labels (a UI's update and layout) before
    /// windows are drawn.
    pub fn run_with<S: WindowWorld, L: WindowLabels<S>>(mut self, app: &mut App<S, L>, mut frame: impl FnMut(&mut App<S, L>)) -> Stats {
        while self.step_with(app, None, &mut frame) == Flow::Continue {}
        self.stats();
        self.stats
    }

    /// One wakeup: wait up to `timeout` (`None`: as long as nothing needs
    /// doing) unless work is pending, then apply events, emit [`Step`], and
    /// send the world's requests.
    pub fn step<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>, timeout: Option<Duration>) -> Flow {
        self.step_with(app, timeout, |_| {})
    }
    /// [`step`](Self::step), calling `frame` after [`Step`].
    pub fn step_with<S: WindowWorld, L: WindowLabels<S>>(
        &mut self,
        app: &mut App<S, L>,
        timeout: Option<Duration>,
        mut frame: impl FnMut(&mut App<S, L>),
    ) -> Flow {
        if !self.started {
            self.started = true;
            self.attach(&mut app.world);
        } else {
            // Wait through traffic that changes nothing (a ping, a buffer
            // back that nobody waits for) until there is work or `timeout`
            // runs out; either way, then, a frame.
            let deadline = timeout.map(|t| Instant::now() + t);
            while !self.has_work(&app.world) {
                let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
                match self.wait(&app.world, remaining) {
                    None => return Flow::Disconnected,
                    Some(true) => break,
                    Some(false) => {}
                }
                if !self.dispatch() {
                    return Flow::Disconnected;
                }
            }
        }
        if !self.dispatch() {
            return Flow::Disconnected;
        }
        self.state.pump_repeat(now_ns());
        self.count_frame(&app.world);
        self.apply(app);
        self.apply_input(app);
        if let Some(text) = self.state.selection.received.take() {
            app.emit(&ClipboardText { text });
        }
        let now = Instant::now();
        let wall = self.last_step.map_or(0.0, |t| (now - t).as_secs_f32());
        self.last_step = Some(now);
        if let Some(mut wake) = app.world.resource_mut::<WakeAt>() {
            *wake = WakeAt::default();
        }
        let now_ns = now_ns();
        let paced = self.pace(now_ns);
        let dt = if paced.dt_ns > 0 { paced.dt_ns as f32 / 1e9 } else { wall };
        app.world.insert_resource(FrameTime { now_ns, display_ns: paced.display_ns, dt });
        app.emit(&Step);
        frame(app);
        self.sync(&mut app.world);
        self.present(app);
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
        world.init_resource::<Clipboard>();
        let wake = self.wake.clone();
        world.insert_resource(Wakeup(Arc::new(move || {
            let _ = rustix::io::write(&*wake, &1u64.to_ne_bytes());
        })));
    }

    fn has_work<S: WindowWorld>(&self, world: &World<S>) -> bool {
        self.woken
            || !self.state.input.is_empty()
            || self.state.selection.received.is_some()
            || self.state.seat.repeat.deadline().is_some_and(|at| at <= now_ns())
            || self.state.windows.iter().any(|w| w.dirty || w.close_requested || w.frame_done)
            || !self.state.released.is_empty() && self.state.windows.iter().any(|w| w.waiting)
            || world.resource::<WakeAt>().and_then(WakeAt::get).is_some_and(|at| at <= Instant::now())
    }

    /// Sleep in `poll` on the socket and the wakeup eventfd until one is
    /// readable, the deadline, or `timeout`. `Some(true)` if the timeout
    /// expired with nothing to read; `None` if the connection broke.
    fn wait<S: WindowWorld>(&mut self, world: &World<S>, timeout: Option<Duration>) -> Option<bool> {
        if self.conn.flush().is_err() {
            return None;
        }
        let Some(guard) = self.queue.prepare_read() else {
            // Events are already queued.
            return Some(false);
        };
        let wake_at = world.resource::<WakeAt>().and_then(WakeAt::get).map(|at| at.saturating_duration_since(Instant::now()));
        let repeat = self.state.seat.repeat.deadline().map(|at| Duration::from_nanos(at.saturating_sub(now_ns())));
        let timeout = [timeout, wake_at, repeat].into_iter().flatten().min();
        let timespec = timeout.map(|t| Timespec { tv_sec: t.as_secs() as i64, tv_nsec: t.subsec_nanos() as i64 });
        let socket = guard.connection_fd();
        let pipe = self.state.selection.reading.as_ref().map(|(pipe, _)| pipe);
        let mut fds = [
            PollFd::new(&socket, PollFlags::IN),
            PollFd::new(&*self.wake, PollFlags::IN),
            PollFd::new(pipe.unwrap_or(&*self.wake), PollFlags::IN),
        ];
        let polled = if pipe.is_some() { &mut fds[..] } else { &mut fds[..2] };
        let ready = match rustix::event::poll(polled, timespec.as_ref()) {
            Ok(ready) => ready,
            Err(rustix::io::Errno::INTR) => 1,
            Err(_) => return None,
        };
        self.stats.wakeups += 1;
        let (socket, wake) = (fds[0].revents(), fds[1].revents());
        if socket.intersects(PollFlags::ERR | PollFlags::HUP) && !socket.contains(PollFlags::IN) {
            return None;
        }
        if socket.contains(PollFlags::IN) {
            if guard.read().is_err() {
                return None;
            }
        } else {
            drop(guard);
        }
        // The compositor hung up: whatever it sent last has been read.
        if socket.contains(PollFlags::HUP) {
            return None;
        }
        self.state.selection.drain();
        if wake.contains(PollFlags::IN) {
            let mut count = [0u8; 8];
            let _ = rustix::io::read(&*self.wake, &mut count);
            self.woken = true;
        }
        Some(ready == 0)
    }

    /// What the compositor said, into the world: sizes, scales, states,
    /// close requests, each change emitted as its label.
    fn apply<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>) {
        self.woken = false;
        for window in &mut self.state.windows {
            window.frame_done = false;
        }
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
                // A window the app draws into is mapped by its first frame.
                if app.world.get::<Framebuffer>(entity).is_none() {
                    self.present_placeholder(i, &surface);
                }
                let world = &mut app.world;
                if world.get::<WindowState>(entity) != Some(&state) {
                    world.insert(entity, state);
                }
                let previous = world.get::<Surface>(entity).copied();
                if previous != Some(surface) {
                    world.insert(entity, surface);
                    if !first && previous.is_some() {
                        emit!(app, Resized { window: entity, surface });
                    }
                }
            }
            let window = &mut self.state.windows[i];
            if std::mem::take(&mut window.close_requested) {
                let close = app.world.get::<Window>(entity).is_some_and(|w| w.close_on_request);
                emit!(app, CloseRequested { window: entity });
                if close {
                    app.world.despawn(entity);
                }
            }
        }
    }

    /// Input, in order, into the world: `Keys`/`Pointer` and the focus
    /// markers first, then each event's label and delivery.
    fn apply_input<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>) {
        let mut input = std::mem::take(&mut self.state.input);
        for event in input.drain(..) {
            let world = &mut app.world;
            match event {
                InputEvent::KeyboardEnter { window, held } => {
                    if !alive(world, window) {
                        continue;
                    }
                    world.insert(window, KeyboardFocus);
                    // Modifiers held while focus arrives count, so a
                    // shortcut begun elsewhere still works here.
                    for code in held.into_iter().flatten() {
                        let keys = app.world.resource_mut::<Keys>().map(Mut::into_inner);
                        if let Some(keys) = keys
                            && keys.set(code, true)
                        {
                            let modifiers = keys.modifiers();
                            let key = self.state.seat.keymap().map_or(Key::Unidentified, |k| k.key(code.0 as u32));
                            emit!(app, KeyInput { window, code, key, pressed: true, repeat: false, modifiers });
                        }
                    }
                    emit!(app, FocusChanged { window, focused: true });
                }
                InputEvent::KeyboardLeave { window } => {
                    // Keys held as focus leaves would otherwise stay down.
                    while let Some(keys) = app.world.resource_mut::<Keys>().map(Mut::into_inner)
                        && let Some(code) = keys.release_one()
                    {
                        let modifiers = keys.modifiers();
                        if alive(&app.world, window) {
                            let key = self.state.seat.keymap().map_or(Key::Unidentified, |k| k.key(code.0 as u32));
                            emit!(app, KeyInput { window, code, key, pressed: false, repeat: false, modifiers });
                        }
                    }
                    if alive(&app.world, window) {
                        app.world.remove::<KeyboardFocus>(window);
                        emit!(app, FocusChanged { window, focused: false });
                    }
                }
                InputEvent::Key { window, code, pressed, repeat, key, text } => {
                    let Some(keys) = world.resource_mut::<Keys>().map(Mut::into_inner) else { continue };
                    keys.set(code, pressed);
                    let modifiers = keys.modifiers();
                    if !alive(world, window) {
                        continue;
                    }
                    emit!(app, KeyInput { window, code, key, pressed, repeat, modifiers });
                    if let Some(text) = text {
                        emit!(app, text);
                    }
                }
                InputEvent::PointerEnter { window, x, y } => {
                    if let Some(pointer) = world.resource_mut::<Pointer>().map(Mut::into_inner) {
                        pointer.window = Some(window);
                        pointer.x = x;
                        pointer.y = y;
                    }
                    if alive(world, window) {
                        world.insert(window, PointerOver);
                        emit!(app, PointerMoved { window, x, y });
                    }
                }
                InputEvent::PointerLeave { window } => {
                    if let Some(pointer) = world.resource_mut::<Pointer>().map(Mut::into_inner) {
                        pointer.window = None;
                    }
                    if alive(world, window) {
                        world.remove::<PointerOver>(window);
                        emit!(app, PointerLeft { window });
                    }
                }
                InputEvent::Motion { window, x, y } => {
                    if let Some(pointer) = world.resource_mut::<Pointer>().map(Mut::into_inner) {
                        pointer.moved(x, y);
                    }
                    if alive(world, window) {
                        emit!(app, PointerMoved { window, x, y });
                    }
                }
                InputEvent::Button { window, button, pressed } => {
                    if let Some(pointer) = world.resource_mut::<Pointer>().map(Mut::into_inner) {
                        pointer.set_button(button, pressed);
                    }
                    if alive(world, window) {
                        emit!(app, PointerButton { window, button, pressed });
                    }
                }
                InputEvent::Delta { window, dx, dy } => {
                    if let Some(pointer) = world.resource_mut::<Pointer>().map(Mut::into_inner) {
                        pointer.add_delta(dx, dy);
                    }
                    if alive(world, window) {
                        emit!(app, PointerDelta { window, dx, dy });
                    }
                }
                InputEvent::Scroll { window, dx, dy, lines } => {
                    if alive(world, window) {
                        emit!(app, Scroll { window, dx, dy, lines });
                    }
                }
            }
        }
        // Keep the allocation for the next wakeup.
        self.state.input = input;
    }

    /// A window nothing draws into still needs a buffer to be shown: an
    /// opaque placeholder, stretched by the viewport where there is one.
    fn present_placeholder(&mut self, index: usize, surface: &Surface) {
        let qh = self.queue.handle();
        let window = &mut self.state.windows[index];
        if window.viewport.is_some() {
            window.set_geometry(surface.width, surface.height, surface.scale);
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
            window.set_geometry(surface.width, surface.height, surface.scale);
        }
        window.surface.commit();
        self.stats.commits += 1;
    }

    /// Draw and commit the framebuffers with something to show: emit
    /// [`Redraw`] for each, then attach the back buffer with its damage.
    fn present<S: WindowWorld, L: WindowLabels<S>>(&mut self, app: &mut App<S, L>) {
        for id in self.state.released.drain(..) {
            if let Some(fb) = app.world.get_mut::<Framebuffer>(id.window).map(Mut::into_inner)
                && let Some(shm) = &mut fb.shm
                && shm.generation == id.generation
            {
                shm.busy[id.index as usize] = false;
            }
        }
        let qh = self.queue.handle();
        for i in 0..self.state.windows.len() {
            let window = &mut self.state.windows[i];
            let entity = window.entity;
            let animating = app.world.get::<Animating>(entity).is_some();
            let Some(fb) = app.world.get_mut::<Framebuffer>(entity).map(Mut::into_inner) else { continue };
            // Before the first configure, the size asked for: the first
            // frame is drawn while the compositor answers.
            let scale = fb.fixed_scale.unwrap_or_else(|| window.scale(&self.state.outputs));
            let (w, h) = if window.configured { window.size } else { (window.sent.width.max(1), window.sent.height.max(1)) };
            let pixels = (scale.to_pixels(w), scale.to_pixels(h));
            if fb.shm.as_ref().is_none_or(|s| (s.width, s.height, s.scale) != (pixels.0, pixels.1, scale)) {
                self.generation += 1;
                fb.shm = Some(Shm::new(&self.globals.shm, &qh, entity, self.generation, pixels.0, pixels.1, scale));
                fb.wanted = true;
                window.predrawn = None;
            }
            fb.take_requests();
            if !window.configured {
                if window.predrawn.is_none()
                    && let Some(index) = fb.begin()
                {
                    window.predrawn = Some(index);
                    app.emit(&Redraw { window: entity });
                    if let Some(fb) = app.world.get_mut::<Framebuffer>(entity).map(Mut::into_inner) {
                        fb.back = None;
                    }
                }
                continue;
            }
            let index = match window.predrawn.take() {
                // Drawn during the configure roundtrip at the size that came
                // back: show it as it is.
                Some(index) => {
                    fb.regions.clear();
                    fb.regions.push(crate::PixelRect::new(0, 0, pixels.0, pixels.1));
                    Some(index)
                }
                // The display hasn't shown the last frame yet: the next is
                // drawn when its callback fires.
                None if window.frame_requested => continue,
                None => {
                    let index = fb.begin();
                    window.waiting = index.is_none() && fb.wanted;
                    if index.is_some() {
                        app.emit(&Redraw { window: entity });
                    }
                    index
                }
            };
            let window = &mut self.state.windows[i];
            let Some(fb) = app.world.get_mut::<Framebuffer>(entity).map(Mut::into_inner) else { continue };
            fb.back = None;
            let mut new_buffer = false;
            match (index, &mut fb.shm) {
                (Some(index), Some(shm)) => {
                    new_buffer = true;
                    window.surface.attach(Some(&shm.buffers[index]), 0, 0);
                    for r in &fb.regions {
                        window.surface.damage_buffer(r.x as i32, r.y as i32, r.width as i32, r.height as i32);
                    }
                    window.set_geometry(w, h, scale);
                    shm.busy[index] = true;
                    window.commits += 1;
                    if let Some(presentation) = &self.globals.presentation {
                        let feedback = Feedback { window: entity, seq: window.commits, input_at: self.input_at };
                        presentation.feedback(&window.surface, &qh, feedback);
                    }
                }
                // Nothing new to show, but an animation wants the next frame.
                _ if animating && !window.frame_requested => {}
                _ => continue,
            }
            if animating && !window.frame_requested {
                window.surface.frame(&qh, entity);
                window.frame_requested = true;
                window.frame_seq = window.commits;
            }
            window.surface.commit();
            self.stats.commits += 1;
            // Our share of the latency ends when the commit leaves.
            if new_buffer && (self.stats.first_commit_ns == 0 || self.record_latency && self.input_at.is_some()) {
                let _ = self.conn.flush();
                let now = now_ns();
                if self.stats.first_commit_ns == 0 {
                    self.stats.first_commit_ns = now.saturating_sub(self.launched_ns).max(1);
                }
                if let (true, Some(at)) = (self.record_latency, self.input_at) {
                    self.stats.input_to_commit_ns.push(now.saturating_sub(at));
                }
            }
        }
    }

    /// Count a frame and why it ran; note when its input arrived.
    fn count_frame<S: WindowWorld>(&mut self, world: &World<S>) {
        let s = &mut self.stats;
        s.frames += 1;
        let input = !self.state.input.is_empty();
        s.input_frames += input as u64;
        s.woken_frames += self.woken as u64;
        s.callback_frames += self.state.windows.iter().any(|w| w.frame_done) as u64;
        s.timer_frames += world.resource::<WakeAt>().and_then(WakeAt::get).is_some_and(|at| at <= Instant::now()) as u64;
        self.input_at = (input || self.woken).then(now_ns);
    }

    /// This wakeup's display timing, from the main (oldest) window's pacer,
    /// as StereoKit's main frame pacer drives its time step.
    fn pace(&mut self, now: u64) -> crate::pacer::Paced {
        let outputs = &self.state.outputs;
        let Some(main) = self.state.windows.first_mut() else { return Default::default() };
        if main.pacer.refresh_ns() == 0 {
            // No presentation report yet: the outputs' current mode.
            let millihertz = outputs.iter().filter(|o| main.outputs.contains(&o.name)).map(|o| o.refresh_mhz).max();
            if let Some(mhz) = millihertz.filter(|&m| m > 0) {
                main.pacer.set_refresh(1_000_000_000_000 / mhz as u64);
            }
        }
        main.pacer.next(main.commits + 1, now)
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
                if wanted.passthrough != window.sent.passthrough {
                    set_passthrough(&self.globals, &self.queue.handle(), &window.surface, wanted.passthrough);
                }
                update(window, wanted);
            }
            let mode = world.get::<WindowMode>(entity).copied().unwrap_or_default();
            if mode != window.sent_mode {
                set_mode(window, mode);
            }
            if world.remove::<Minimize>(entity).is_some() {
                window.role.set_minimized();
            }
        }
        self.sync_pointer(world);
        let qh = self.queue.handle();
        if let Some(clipboard) = world.resource_mut::<Clipboard>().map(Mut::into_inner) {
            if let (Some(text), Some(manager)) = (clipboard.set.take(), &self.globals.data_device) {
                self.state.selection.set(manager, &qh, text, self.state.seat.input_serial);
            }
            if std::mem::take(&mut clipboard.requested) {
                self.state.selection.request(&self.conn);
            }
        }
    }

    /// Pointer locks (with raw motion while any holds), and the cursor of
    /// the window under the pointer.
    fn sync_pointer<S: WindowWorld>(&mut self, world: &World<S>) {
        let qh = self.queue.handle();
        let seat = &mut self.state.seat;
        let Some(pointer) = &seat.pointer else { return };
        for window in &mut self.state.windows {
            let wanted = world.get::<PointerLock>(window.entity).is_some();
            match (wanted, window.lock.take(), &self.globals.constraints) {
                (true, None, Some(constraints)) => {
                    window.lock = Some(constraints.lock_pointer(&window.surface, pointer, None, Lifetime::Persistent, &qh, ()));
                }
                (true, held, _) => window.lock = held,
                (false, Some(lock), _) => lock.destroy(),
                (false, None, _) => {}
            }
        }
        let locked = self.state.windows.iter().any(|w| w.lock.is_some());
        match (locked, seat.relative.take(), &self.globals.relative) {
            (true, None, Some(manager)) => seat.relative = Some(manager.get_relative_pointer(pointer, &qh, ())),
            (true, held, _) => seat.relative = held,
            (false, Some(relative), _) => relative.destroy(),
            (false, None, _) => {}
        }
        let Some(window) = seat.pointer_focus else { return };
        let icon = if world.get::<PointerLock>(window).is_some() {
            CursorIcon::Hidden
        } else {
            world.get::<CursorIcon>(window).copied().unwrap_or_default()
        };
        if seat.cursor == Some(icon) {
            return;
        }
        seat.cursor = Some(icon);
        let serial = seat.pointer_serial;
        match (icon.shape(), &self.globals.cursor_shape) {
            (None, _) => pointer.set_cursor(serial, None, 0, 0),
            (Some(shape), Some(manager)) => {
                seat.cursor_device.get_or_insert_with(|| manager.get_pointer(pointer, &qh, ())).set_shape(serial, shape);
            }
            // The compositor won't draw it: the theme's image, at the
            // window's (whole) scale.
            (Some(_), None) => {
                let outputs = &self.state.outputs;
                let scale = self.state.windows.iter().find(|w| w.entity == window).map_or(1, |w| w.scale(outputs).0.div_ceil(crate::Scale::UNIT));
                let theme = self.state.theme.get_or_insert_with(|| Theme::new(&self.globals.compositor, &qh));
                let hotspot = theme.show(icon, scale, &self.globals.shm, &qh).or_else(|| theme.show(CursorIcon::Default, scale, &self.globals.shm, &qh));
                if let Some((x, y)) = hotspot {
                    pointer.set_cursor(serial, Some(&theme.surface), x, y);
                }
            }
        }
    }

    /// Our queue's events, then libdecor's (its objects sit on libwayland's
    /// default queue). False if the connection broke.
    fn dispatch(&mut self) -> bool {
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return false;
        }
        if self.state.decor.is_some() {
            Libdecor::dispatch(&mut self.state);
        }
        true
    }

    /// The libdecor context, loaded at the first window that needs it.
    fn libdecor(&mut self) -> Option<&Libdecor> {
        if !std::mem::replace(&mut self.decor_tried, true) {
            self.state.decor = Libdecor::new(self.conn.backend().display_ptr().cast());
            if self.state.decor.is_none() {
                eprintln!("static-window: no xdg-decoration and no libdecor: windows have no title bar");
            }
        }
        self.state.decor.as_ref()
    }

    fn create(&mut self, entity: Entity, window: Window, mode: WindowMode) {
        let qh = self.queue.handle();
        let surface = self.globals.compositor.create_surface(&qh, entity);
        let client_side = window.decorated && self.decorations == Decorations::Client;
        let frame = if client_side { self.libdecor().and_then(|decor| decor.decorate(&surface, entity, &window)) } else { None };
        let g = &self.globals;
        let role = match frame {
            Some(frame) => Role::Decor(frame),
            None => {
                let xdg = g.wm.get_xdg_surface(&surface, &qh, entity);
                let toplevel = xdg.get_toplevel(&qh, entity);
                let decoration = match self.decorations {
                    Decorations::Server => g.decoration.as_ref().map(|manager| manager.get_toplevel_decoration(&toplevel, &qh, entity)),
                    Decorations::Client => None,
                };
                // Everything before the first commit, so the first
                // configure already reflects it.
                let role = Role::Xdg { xdg, toplevel, decoration };
                role.set_title(&window.title);
                role.set_app_id(&app_id(&window));
                role.set_limits(&window);
                role.set_decorated(window.decorated);
                role
            }
        };
        let viewport = g.viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
        if window.passthrough {
            set_passthrough(g, &qh, &surface, true);
        }
        let fractional = match (&g.fractional, &viewport) {
            (Some(manager), Some(_)) => Some(manager.get_fractional_scale(&surface, &qh, entity)),
            _ => None,
        };
        let size = (window.width.max(1), window.height.max(1));
        let mut proto = Proto {
            entity,
            role,
            surface: OwnedSurface(surface),
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
            destination: None,
            // wl_surface's initial buffer scale.
            buffer_scale: 1,
            frame_requested: false,
            frame_done: false,
            predrawn: None,
            waiting: false,
            pacer: Default::default(),
            commits: 0,
            frame_seq: 0,
            lock: None,
        };
        set_mode(&mut proto, mode);
        // libdecor maps its frame itself.
        if let Role::Xdg { .. } = proto.role {
            proto.surface.commit();
        }
        self.state.windows.push(proto);
    }
}

pub(crate) fn app_id(window: &Window) -> String {
    if window.app_id.is_empty() { exe_name() } else { window.app_id.clone() }
}

/// An empty input region lets the pointer through; none restores the
/// whole surface.
fn set_passthrough(globals: &Globals, qh: &wayland_client::QueueHandle<State>, surface: &wayland_client::protocol::wl_surface::WlSurface, on: bool) {
    if on {
        let region = globals.compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
    } else {
        surface.set_input_region(None);
    }
}

/// Send what differs between `window.sent` and `wanted`.
fn update(window: &mut Proto, wanted: Window) {
    let sent = &window.sent;
    let role = &window.role;
    if wanted.title != sent.title {
        role.set_title(&wanted.title);
    }
    if wanted.app_id != sent.app_id {
        role.set_app_id(&app_id(&wanted));
    }
    if wanted.limits() != sent.limits() {
        role.set_limits(&wanted);
    }
    if wanted.decorated != sent.decorated {
        role.set_decorated(wanted.decorated);
    }
    // A floating window's size is ours to choose.
    if (wanted.width, wanted.height) != (sent.width, sent.height) && !window.state.maximized && !window.state.fullscreen {
        window.size = (wanted.width.max(1), wanted.height.max(1));
        role.resized(window.size.0, window.size.1);
        window.dirty = true;
    }
    window.sent = wanted;
}

fn set_mode(window: &mut Proto, mode: WindowMode) {
    let role = &window.role;
    match window.sent_mode {
        WindowMode::Maximized => role.set_maximized(false),
        WindowMode::Fullscreen => role.set_fullscreen(false),
        WindowMode::Windowed => {}
    }
    match mode {
        WindowMode::Maximized => role.set_maximized(true),
        WindowMode::Fullscreen => role.set_fullscreen(true),
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
