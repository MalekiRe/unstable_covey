//! Development tools, compiled only with the `devtools` feature. Without it
//! none of this exists and nothing else in the crate changes.
//!
//! - **Change reporting.** Every change tick is unique and remembers who
//!   issued it ([`Writer`]): a system, the commands a system recorded, a
//!   direct `World` call (with its source location), or a task.
//! - **History.** Each `emit` records the values that changed and every
//!   structural event, so past values, entity timelines, and questions over
//!   time are answerable ([`World::history`](crate::World::history)).
//! - **Provenance.** Which system last wrote a value, what it reads, and who
//!   wrote those, frame by frame back.
//! - **Stepping.** Run one system of a label at a time.
//! - **Time travel.** Snapshots of the whole world, rewinding, and forks.
//! - **A task console**, **"why isn't this entity in my query?"**, and an
//!   **MCP server** exposing all of it to agents with progressive disclosure.
//! - **Assembly per system**, extracted from a binary built with line tables.

use std::any::TypeId;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::Entity;
use crate::change::SystemWorld;

pub mod asm;
pub(crate) mod history;
pub mod mcp;
pub(crate) mod snapshot;
pub(crate) mod step;
pub mod value;

pub use history::{
    Change, ChangeKind, Explanation, Frame, History, Record, Structural, StructuralKind,
    TimelineEntry, condition,
};
pub use snapshot::{Diff, Fork, Snapshot, SnapshotError};
pub use step::{Step, StepOutcome};
pub use value::Value;

/// Lock a devtools mutex; a panicking holder leaves consistent data behind.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Who issued a change tick, and so who wrote every value stamped with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// Nothing tracked was running: a remote bridge, or an internal update.
    Unknown,
    /// A system (see [`system`]).
    System(u32),
    /// Commands recorded by system `system` at `at`, applied at frame end.
    Commands {
        system: u32,
        at: &'static Location<'static>,
    },
    /// A direct `World` call at `at`.
    World(&'static Location<'static>),
    /// An async task (see [`task_info`]), including its bridges.
    Task(u32),
    /// An edit through devtools (`World::set_field`, the MCP `set` tool).
    Edit,
}
impl Writer {
    /// The system behind this writer, if any.
    pub fn system(self) -> Option<u32> {
        match self {
            Writer::System(id) | Writer::Commands { system: id, .. } => Some(id),
            _ => None,
        }
    }
    /// A one-line description with names and source locations.
    pub fn describe(self) -> String {
        let at = |l: &Location| format!("{}:{}", l.file(), l.line());
        match self {
            Writer::Unknown => "unknown (no tracked writer)".into(),
            Writer::System(id) => match system(id) {
                Some(s) => format!("system #{id} `{}` ({})", s.name, at(s.location)),
                None => format!("system #{id}"),
            },
            Writer::Commands { system: id, at: l } => match system(id) {
                Some(s) => format!("commands of system #{id} `{}` queued at {}", s.name, at(l)),
                None => format!("commands of system #{id} queued at {}", at(l)),
            },
            Writer::World(l) => format!("World call at {}", at(l)),
            Writer::Task(id) => match task_info(id) {
                Some(t) => format!("task #{id} `{}` ({})", t.name, at(t.location)),
                None => format!("task #{id}"),
            },
            Writer::Edit => "devtools edit".into(),
        }
    }
}

thread_local! {
    static WRITER: Cell<Writer> = const { Cell::new(Writer::Unknown) };
}
/// The writer ticks issued on this thread are attributed to.
pub(crate) fn writer() -> Writer {
    WRITER.get()
}
/// Restores the previous writer when dropped.
pub(crate) struct Writing(Writer);
impl Drop for Writing {
    fn drop(&mut self) {
        WRITER.set(self.0);
    }
}
/// Attribute ticks issued on this thread to `writer` until the guard drops.
pub(crate) fn writing(writer: Writer) -> Writing {
    Writing(WRITER.replace(writer))
}
/// A direct `World` call: attributed to its location unless something
/// tracked (a system, commands, a hook, a task) is already writing.
#[track_caller]
pub(crate) fn world_call() -> Writing {
    let current = WRITER.get();
    let writer = if current == Writer::Unknown {
        Writer::World(Location::caller())
    } else {
        current
    };
    Writing(WRITER.replace(writer))
}
/// The writer for commands queued at `at`: the queuing system's commands, a
/// task's own writes, or a direct call.
pub(crate) fn commands_writer(at: &'static Location<'static>) -> Writer {
    match WRITER.get() {
        Writer::System(system) | Writer::Commands { system, .. } => Writer::Commands { system, at },
        task @ Writer::Task(_) => task,
        _ => Writer::World(at),
    }
}

static TICK: AtomicU64 = AtomicU64::new(0);
static TICKS: Mutex<BTreeMap<u64, Writer>> = Mutex::new(BTreeMap::new());

/// Issue a tick after `latest`, unique across the process, and remember who
/// issued it. Replaces the Lamport increment: ticks stay ordered along every
/// path, and parallel writers never share one.
pub(crate) fn issue(latest: u64) -> u64 {
    assert!(latest < u64::MAX >> 1, "world change clock exhausted");
    let previous = TICK
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |t| {
            Some(t.max(latest) + 1)
        })
        .unwrap_or_else(|t| t);
    let tick = previous.max(latest) + 1;
    lock(&TICKS).insert(tick, writer());
    tick
}
/// Who issued `tick`.
pub fn writer_of(tick: u64) -> Writer {
    lock(&TICKS).get(&tick).copied().unwrap_or(Writer::Unknown)
}

/// How a system touches a component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchKind {
    Read,
    Write,
    /// Tests membership only (`Has<T>`, `With<T>`, ...).
    Mention,
    /// The whole world, exclusively.
    Exclusive,
}
/// One component a system touches, from its static access type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    pub component: &'static str,
    pub kind: TouchKind,
    /// Reached through a relation join, on other entities.
    pub joined: bool,
}

/// Where a system was registered, and how it runs.
#[derive(Clone, Debug)]
pub struct SystemInfo {
    pub id: u32,
    /// The function's type name: a path for `fn` items, `{{closure}}` inside
    /// its defining function for closures.
    pub name: &'static str,
    /// The schedule label it runs under.
    pub label: &'static str,
    /// A lifecycle hook, run inside structural operations.
    pub hook: bool,
    /// A run condition, deciding whether the systems it guards run.
    pub condition: bool,
    /// Exclusive world access.
    pub exclusive: bool,
    /// The `add_systems`/`schedule` call (or plugin) that registered it.
    pub location: &'static Location<'static>,
    /// Position inside the registration: `chain[1] > parallel[0]`.
    pub path: String,
    pub touches: Vec<Touch>,
}
impl SystemInfo {
    pub fn reads(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.touches
            .iter()
            .filter(|t| t.kind == TouchKind::Read)
            .map(|t| t.component)
    }
    pub fn writes(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.touches
            .iter()
            .filter(|t| t.kind == TouchKind::Write)
            .map(|t| t.component)
    }
}
/// A registered async task.
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: u32,
    pub name: &'static str,
    pub location: &'static Location<'static>,
}

static SYSTEMS: Mutex<Vec<SystemInfo>> = Mutex::new(Vec::new());
static TASKS: Mutex<Vec<TaskInfo>> = Mutex::new(Vec::new());
static LABELS: Mutex<BTreeMap<TypeId, &'static str>> = Mutex::new(BTreeMap::new());

pub(crate) fn register_system(mut info: SystemInfo) -> u32 {
    let mut systems = lock(&SYSTEMS);
    info.id = systems.len() as u32 + 1;
    let id = info.id;
    systems.push(info);
    id
}
pub(crate) fn register_task(name: &'static str, location: &'static Location<'static>) -> u32 {
    let mut tasks = lock(&TASKS);
    let id = tasks.len() as u32 + 1;
    tasks.push(TaskInfo { id, name, location });
    id
}
/// A registered system by id.
pub fn system(id: u32) -> Option<SystemInfo> {
    let systems = lock(&SYSTEMS);
    id.checked_sub(1)
        .and_then(|i| systems.get(i as usize))
        .cloned()
}
/// A registered task by id.
pub fn task_info(id: u32) -> Option<TaskInfo> {
    let tasks = lock(&TASKS);
    id.checked_sub(1)
        .and_then(|i| tasks.get(i as usize))
        .cloned()
}
/// Remember a label's name, for task console output.
pub(crate) fn name_label<E: 'static>() -> TypeId {
    let id = TypeId::of::<E>();
    lock(&LABELS).insert(id, std::any::type_name::<E>());
    id
}
/// A label's name, if it was ever emitted or awaited.
pub fn label_name(label: TypeId) -> Option<&'static str> {
    lock(&LABELS).get(&label).copied()
}

/// A builder call that stores a type: `register`, `relation`,
/// `insert_resource`, `add_plugins`, and the like.
#[derive(Clone, Debug)]
pub struct Registered {
    pub call: &'static str,
    pub type_name: &'static str,
    pub location: &'static Location<'static>,
}
#[track_caller]
pub(crate) fn noted<T: ?Sized>(
    mut registered: Vec<Registered>,
    call: &'static str,
) -> Vec<Registered> {
    registered.push(Registered {
        call,
        type_name: std::any::type_name::<T>(),
        location: Location::caller(),
    });
    registered
}

/// One registration under a label, as traversal meets it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Registration {
    pub label: TypeId,
    pub name: &'static str,
    pub location: &'static Location<'static>,
    pub hook: bool,
}
/// How a traversal's group nodes combine their children.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Children run in order.
    Sequence,
    /// Children may run at once.
    Parallel,
    /// A dependency graph, children in serial (topological) order.
    Graph,
    /// A run condition's own systems.
    Condition,
}

/// A system as traversal sees it.
#[doc(hidden)]
pub trait Leaf<W: SystemWorld> {
    /// Its registry id; 0 before `build` (or for systems devtools does not track).
    fn id(&self) -> u32;
    fn set_id(&mut self, id: u32);
    /// Has a registry id slot (opaque systems do not).
    fn tracked(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str;
    fn exclusive(&self) -> bool;
    fn touches(&self, out: &mut Vec<Touch>);
    /// Run once; whether it ran (parameters and conditions may skip it).
    /// # Safety
    /// The world is live and exclusively held; `input` is its label's input.
    unsafe fn run(&mut self, input: *const (), world: *mut W, clock: &mut u64) -> bool;
    /// Move recorded commands to the world's queue.
    /// # Safety
    /// The world is live and exclusively held.
    unsafe fn defer(&mut self, world: *mut W);
    /// For each query parameter that `entity` is not a row of, why.
    /// # Safety
    /// The world is live and exclusively held.
    unsafe fn explain(&mut self, world: *mut W, entity: Entity, out: &mut Vec<String>);
}
/// A run condition as traversal sees it.
#[doc(hidden)]
pub trait Guard<W: SystemWorld> {
    /// Evaluate the condition.
    /// # Safety
    /// As `Leaf::run`.
    unsafe fn eval(&mut self, input: *const (), world: *mut W, clock: &mut u64) -> bool;
    /// # Safety
    /// As `Leaf::defer`.
    unsafe fn defer(&mut self, world: *mut W);
}
/// An async task as traversal sees it.
#[doc(hidden)]
pub trait TaskView<W: SystemWorld> {
    fn id(&self) -> u32;
    fn set_id(&mut self, id: u32);
    fn name(&self) -> &'static str;
    fn location(&self) -> &'static Location<'static>;
    fn status(&self) -> TaskStatus;
    /// Poll if due at `label`; whether it ran.
    /// # Safety
    /// As `Leaf::run`.
    unsafe fn poll(&mut self, label: TypeId, world: *mut W, clock: &mut u64) -> bool;
}
/// Walks an app's systems in dispatch order.
#[doc(hidden)]
pub trait Visitor<W: SystemWorld> {
    /// A registration; descend into it?
    fn registration(&mut self, _registration: &Registration) -> bool {
        true
    }
    fn end_registration(&mut self) {}
    fn group(&mut self, _group: Group) {}
    fn end_group(&mut self) {}
    fn leaf(&mut self, _leaf: &mut dyn Leaf<W>) {}
    /// After a condition's own systems (inside `Group::Condition`): descend
    /// into the systems it guards?
    fn guard(&mut self, _guard: &mut dyn Guard<W>) -> bool {
        true
    }
    fn task(&mut self, _task: &mut dyn TaskView<W>) {}
}

/// What a task is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Registered; starts at the app's first `emit`.
    NotStarted,
    /// Waiting for a bridge's label to be emitted.
    WaitingFor(&'static str),
    /// Waiting to be woken (an external future, a timer, a channel).
    Pending,
    Finished,
    /// Its poll panicked; it never runs again.
    Panicked,
}
/// A task's console entry.
#[derive(Clone, Debug)]
pub struct TaskStatus {
    pub id: u32,
    pub name: &'static str,
    pub location: &'static Location<'static>,
    pub state: TaskState,
    pub polls: u64,
    pub wakes: u64,
    /// Emits since the task last made progress (was polled).
    pub idle_frames: u64,
    /// Woken but not polled yet.
    pub woken: bool,
}

/// Emit counts by label, and the frame counter, for the task console.
static EMITS: Mutex<BTreeMap<TypeId, u64>> = Mutex::new(BTreeMap::new());
static FRAMES: AtomicU64 = AtomicU64::new(0);
pub(crate) fn emitted<E: 'static>() {
    let id = name_label::<E>();
    *lock(&EMITS).entry(id).or_default() += 1;
    FRAMES.fetch_add(1, Ordering::Relaxed);
}
/// Times `label` was emitted, process-wide.
pub fn emits(label: TypeId) -> u64 {
    lock(&EMITS).get(&label).copied().unwrap_or(0)
}
/// Emits so far, process-wide.
pub fn frames() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}
