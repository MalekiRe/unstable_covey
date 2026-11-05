//! Traversals over an app's systems: registration, listing, stepping one
//! system at a time, and explaining why an entity is not a query's row.

use std::any::TypeId;

use super::history::Change;
use super::{
    Group, Guard, Leaf, Registration, SystemInfo, TaskStatus, TaskView, Touch, Visitor, Writer,
};
use crate::change::SystemWorld;
use crate::dispatch::{Fits, FromEvent};
use crate::{App, Entity, Schedule, Storage, System, World};

/// The writer for system `id` (0: not registered).
pub(crate) fn system_writer(id: u32) -> Writer {
    if id == 0 {
        Writer::Unknown
    } else {
        Writer::System(id)
    }
}

/// A system without its own view: one leaf, untracked.
pub struct Opaque<'a, T>(pub(crate) &'a mut T);
impl<W: SystemWorld, T: System<W>> Leaf<W> for Opaque<'_, T> {
    fn id(&self) -> u32 {
        0
    }
    fn tracked(&self) -> bool {
        false
    }
    fn set_id(&mut self, _: u32) {}
    fn name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
    fn exclusive(&self) -> bool {
        <T::Access as crate::access::Access>::EXCLUSIVE
    }
    fn touches(&self, out: &mut Vec<Touch>) {
        <T::Access as crate::access::Access>::touches(false, out);
    }
    unsafe fn run(&mut self, input: *const (), world: *mut W, clock: &mut u64) -> bool {
        unsafe { self.0.run_unchecked(input, world, clock) }.is_some()
    }
    unsafe fn defer(&mut self, world: *mut W) {
        self.0.defer(unsafe { &mut *W::queue(world) });
    }
    unsafe fn explain(&mut self, _: *mut W, _: Entity, _: &mut Vec<String>) {}
}

/// A run condition, evaluated on its own.
pub struct Condition<'a, C>(pub(crate) &'a mut C);
impl<W: SystemWorld, C: System<W, Output = bool>> Guard<W> for Condition<'_, C> {
    unsafe fn eval(&mut self, input: *const (), world: *mut W, clock: &mut u64) -> bool {
        (unsafe { self.0.run_unchecked(input, world, clock) }) == Some(true)
    }
    unsafe fn defer(&mut self, world: *mut W) {
        self.0.defer(unsafe { &mut *W::queue(world) });
    }
}

/// Any system tree, as an object.
#[doc(hidden)]
pub trait Inspect<W: SystemWorld> {
    fn inspect(&mut self, visitor: &mut dyn Visitor<W>);
}
impl<W: SystemWorld, T: System<W>> Inspect<W> for T {
    fn inspect(&mut self, visitor: &mut dyn Visitor<W>) {
        <T as System<W>>::inspect(self, visitor);
    }
}

/// Tracks where a traversal is: the registration and the group path.
#[derive(Default)]
struct Cursor {
    registration: Option<Registration>,
    /// (group, children seen, same-kind groups merged into this frame)
    frames: Vec<(Group, usize, usize)>,
    /// Inside a run condition's own systems.
    condition: usize,
}
impl Cursor {
    fn group(&mut self, group: Group) {
        if group == Group::Condition {
            self.condition += 1;
        }
        match self.frames.last_mut() {
            Some((kind, _, merged)) if *kind == group && group != Group::Condition => *merged += 1,
            _ => {
                if let Some((_, count, _)) = self.frames.last_mut() {
                    *count += 1;
                }
                self.frames.push((group, 0, 0));
            }
        }
    }
    fn end_group(&mut self) {
        if let Some((group, _, merged)) = self.frames.last_mut() {
            if *group == Group::Condition {
                self.condition -= 1;
            }
            if *merged > 0 {
                *merged -= 1;
            } else {
                self.frames.pop();
            }
        }
    }
    fn leaf(&mut self) -> String {
        if let Some((_, count, _)) = self.frames.last_mut() {
            *count += 1;
        }
        self.path()
    }
    fn path(&self) -> String {
        self.frames
            .iter()
            .map(|&(group, count, _)| {
                let name = match group {
                    Group::Sequence => "chain",
                    Group::Parallel => "parallel",
                    Group::Graph => "graph",
                    Group::Condition => "condition",
                };
                format!("{name}[{}]", count.saturating_sub(1))
            })
            .collect::<Vec<_>>()
            .join(" > ")
    }
    fn info(&self, leaf: &dyn Leaf<impl SystemWorld>, path: String) -> SystemInfo {
        let registration = self.registration.expect("leaves sit inside registrations");
        let mut touches = Vec::new();
        leaf.touches(&mut touches);
        SystemInfo {
            id: leaf.id(),
            name: leaf.name(),
            label: registration.name,
            hook: registration.hook,
            condition: self.condition > 0,
            exclusive: leaf.exclusive(),
            location: registration.location,
            path,
            touches,
        }
    }
}

/// Assigns ids to new systems and tasks, and registers them.
struct Registrar(Cursor);
impl<W: SystemWorld> Visitor<W> for Registrar {
    fn registration(&mut self, registration: &Registration) -> bool {
        self.0.registration = Some(*registration);
        true
    }
    fn group(&mut self, group: Group) {
        self.0.group(group);
    }
    fn end_group(&mut self) {
        self.0.end_group();
    }
    fn leaf(&mut self, leaf: &mut dyn Leaf<W>) {
        let path = self.0.leaf();
        if leaf.id() == 0 && leaf.tracked() {
            let info = self.0.info(&*leaf, path);
            let id = super::register_system(info);
            leaf.set_id(id);
        }
    }
    fn task(&mut self, task: &mut dyn TaskView<W>) {
        if task.id() == 0 {
            let id = super::register_task(task.name(), task.location());
            task.set_id(id);
        }
    }
}
/// Assign registry ids to every system and task of a freshly built app.
pub(crate) fn register<S: Storage, L: Fits<World<S>>>(app: &mut App<S, L>) {
    let mut registrar = Registrar(Cursor::default());
    app.listeners.inspect(&mut registrar);
    if let Some(hooks) = &mut app.world.hooks {
        // SAFETY: no hook runs while the app is being built.
        unsafe { hooks.inspect(&mut registrar) };
    }
}

/// Collects every system and task.
struct Lister {
    cursor: Cursor,
    systems: Vec<SystemInfo>,
    tasks: Vec<TaskStatus>,
}
impl<W: SystemWorld> Visitor<W> for Lister {
    fn registration(&mut self, registration: &Registration) -> bool {
        self.cursor.registration = Some(*registration);
        true
    }
    fn group(&mut self, group: Group) {
        self.cursor.group(group);
    }
    fn end_group(&mut self) {
        self.cursor.end_group();
    }
    fn leaf(&mut self, leaf: &mut dyn Leaf<W>) {
        let path = self.cursor.leaf();
        let info = super::system(leaf.id()).unwrap_or_else(|| self.cursor.info(&*leaf, path));
        self.systems.push(info);
    }
    fn task(&mut self, task: &mut dyn TaskView<W>) {
        self.tasks.push(task.status());
    }
}

/// Finds one system and explains an entity against its queries.
struct Explainer {
    system: u32,
    entity: Entity,
    world: *mut (),
    found: Option<Vec<String>>,
}
impl<W: SystemWorld> Visitor<W> for Explainer {
    fn leaf(&mut self, leaf: &mut dyn Leaf<W>) {
        if self.found.is_none() && leaf.id() == self.system {
            let mut reasons = Vec::new();
            // SAFETY: the app holds the world exclusively; nothing runs.
            unsafe { leaf.explain(self.world.cast(), self.entity, &mut reasons) };
            self.found = Some(reasons);
        }
    }
}

/// What one step did.
#[derive(Clone, Debug)]
pub enum Step {
    /// A system ran, or was skipped by its parameters (`ran: false`).
    System {
        id: u32,
        name: &'static str,
        ran: bool,
    },
    /// A run condition was evaluated; false skips the systems it guards.
    Condition { name: &'static str, result: bool },
    /// A task was reached; `polled` if it was due.
    Task {
        id: u32,
        name: &'static str,
        polled: bool,
    },
}
/// The result of `App::step`.
#[derive(Clone, Debug)]
pub struct StepOutcome {
    /// What ran, unless the pass had nothing left.
    pub step: Option<Step>,
    /// Position of this step within the label's pass.
    pub index: usize,
    /// The pass finished: commands were applied and the frame recorded.
    pub finished: bool,
    /// Rows the step added or changed.
    pub changes: Vec<Change>,
}

/// An app's stepping position within one label's pass.
#[derive(Default)]
pub(crate) struct Stepping {
    label: Option<TypeId>,
    cursor: usize,
    /// Guard results so far this pass, by unit index.
    guards: Vec<(usize, bool)>,
}

struct Stepper<W> {
    label: TypeId,
    target: usize,
    index: usize,
    input: *const (),
    world: *mut W,
    guards: Vec<(usize, bool)>,
    condition: usize,
    condition_name: &'static str,
    step: Option<Step>,
}
impl<W: SystemWorld> Stepper<W> {
    fn run<T>(&mut self, body: impl FnOnce(*const (), *mut W, &mut u64) -> T) -> T {
        // SAFETY: the app holds the world exclusively for the step.
        unsafe {
            let mut clock = W::clock(self.world);
            let out = crate::executor::serially(|| body(self.input, self.world, &mut clock));
            W::set_clock(self.world, clock);
            out
        }
    }
}
impl<W: SystemWorld> Visitor<W> for Stepper<W> {
    fn registration(&mut self, registration: &Registration) -> bool {
        !registration.hook && registration.label == self.label && self.index <= self.target
    }
    fn group(&mut self, group: Group) {
        if group == Group::Condition {
            self.condition += 1;
        }
    }
    fn end_group(&mut self) {}
    fn leaf(&mut self, leaf: &mut dyn Leaf<W>) {
        if self.condition > 0 {
            // A condition's own system: the guard that follows is the step.
            self.condition_name = leaf.name();
            return;
        }
        if self.index == self.target {
            let world = self.world;
            let ran = self.run(|input, world, clock| unsafe { leaf.run(input, world, clock) });
            unsafe { leaf.defer(world) };
            self.step = Some(Step::System {
                id: leaf.id(),
                name: leaf.name(),
                ran,
            });
        }
        self.index += 1;
    }
    fn guard(&mut self, guard: &mut dyn Guard<W>) -> bool {
        self.condition -= 1;
        let index = self.index;
        self.index += 1;
        if index == self.target {
            let world = self.world;
            let result = self.run(|input, world, clock| unsafe { guard.eval(input, world, clock) });
            unsafe { guard.defer(world) };
            self.guards.push((index, result));
            self.step = Some(Step::Condition {
                name: self.condition_name,
                result,
            });
            // The guarded systems are later steps.
            return false;
        }
        index < self.target
            && self
                .guards
                .iter()
                .find(|&&(i, _)| i == index)
                .is_some_and(|&(_, result)| result)
    }
    fn task(&mut self, task: &mut dyn TaskView<W>) {
        if self.index == self.target {
            let label = self.label;
            let polled = self.run(|_, world, clock| unsafe { task.poll(label, world, clock) });
            self.step = Some(Step::Task {
                id: task.id(),
                name: task.name(),
                polled,
            });
        }
        self.index += 1;
    }
}

impl<S: Storage, L: Fits<World<S>>> App<S, L> {
    /// Every system (hooks included) in dispatch order, with registration
    /// sites, paths, and accesses.
    pub fn systems(&mut self) -> Vec<SystemInfo> {
        self.inspect_all().0
    }
    /// The task console: every task's state.
    pub fn tasks(&mut self) -> Vec<TaskStatus> {
        self.inspect_all().1
    }
    fn inspect_all(&mut self) -> (Vec<SystemInfo>, Vec<TaskStatus>) {
        let mut lister = Lister {
            cursor: Cursor::default(),
            systems: Vec::new(),
            tasks: Vec::new(),
        };
        self.listeners.inspect(&mut lister);
        if let Some(hooks) = &mut self.world.hooks {
            // SAFETY: no hook runs between frames.
            unsafe { hooks.inspect(&mut lister) };
        }
        (lister.systems, lister.tasks)
    }
    /// Why `entity` is not a row of system `system`'s queries: one reason
    /// per mismatching query parameter, empty if it matches them all. `None`
    /// when no such system exists.
    pub fn why_not(&mut self, system: u32, entity: Entity) -> Option<Vec<String>> {
        let world: *mut World<S> = &raw mut self.world;
        let mut explainer = Explainer {
            system,
            entity,
            world: world.cast(),
            found: None,
        };
        self.listeners.inspect(&mut explainer);
        if explainer.found.is_none() {
            // SAFETY: no hook runs between frames; explaining reads tables,
            // never the hook list borrowed here.
            if let Some(hooks) = unsafe { (*world).hooks.as_mut() } {
                unsafe { hooks.inspect(&mut explainer) };
            }
        }
        explainer.found
    }
    /// Run the next system (or run condition, or task) of `E`'s pass, alone.
    /// When the pass ends, its commands apply and the frame is recorded, as
    /// `emit` would. Stepping another label abandons the current pass.
    pub fn step<E: Schedule>(&mut self, event: &E) -> StepOutcome
    where
        E::In: FromEvent<E>,
    {
        let label = TypeId::of::<E>();
        if self.stepping.label != Some(label) {
            self.abandon_step();
            self.stepping.label = Some(label);
        }
        if self.stepping.cursor == 0 {
            super::emitted::<E>();
        }
        let before = self.world.tick();
        let input = <E::In as FromEvent<E>>::of(event);
        let mut stepper = Stepper {
            label,
            target: self.stepping.cursor,
            index: 0,
            input: (&raw const input).cast(),
            world: &raw mut self.world,
            guards: std::mem::take(&mut self.stepping.guards),
            condition: 0,
            condition_name: "",
            step: None,
        };
        self.listeners.inspect(&mut stepper);
        let index = self.stepping.cursor;
        self.stepping.guards = stepper.guards;
        let step = stepper.step;
        let changes = self.world.changes_since(before);
        let finished = step.is_none();
        if finished {
            self.world.flush();
            self.world.record_frame(std::any::type_name::<E>());
            self.stepping = Stepping::default();
        } else {
            self.stepping.cursor += 1;
        }
        StepOutcome {
            step,
            index,
            finished,
            changes,
        }
    }
    /// Abandon a pass in progress: apply its queued commands and record the
    /// frame. Nothing happens if no pass is in progress.
    pub fn abandon_step(&mut self) {
        if self.stepping.cursor > 0 {
            self.world.flush();
            self.world.record_frame("(abandoned step)");
        }
        self.stepping = Stepping::default();
    }
}

impl<S: Storage, L: Fits<World<S>>> App<S, L> {
    /// Why the world stores `component` (full or short name): the systems
    /// and hooks that use it, the components that require it, and the
    /// builder calls that registered it.
    pub fn why_component(&mut self, component: &str) -> Option<Vec<String>> {
        use super::value::short_name;
        let index = self.world.component_index(component)?;
        let name = self.world.info[index].name;
        let at = |l: &std::panic::Location| format!("{}:{}", l.file(), l.line());
        let mut reasons = Vec::new();
        for system in self.systems() {
            let uses: Vec<_> = system
                .touches
                .iter()
                .filter(|t| t.component == name)
                .map(|t| {
                    format!(
                        "{:?}{}",
                        t.kind,
                        if t.joined { " (through a join)" } else { "" }
                    )
                })
                .collect();
            if !uses.is_empty() {
                reasons.push(format!(
                    "system #{} `{}` under `{}` ({}) uses it: {}",
                    system.id,
                    system.name,
                    short_name(system.label),
                    at(system.location),
                    uses.join(", ")
                ));
            } else if system.hook && system.label.contains(short_name(name)) {
                reasons.push(format!(
                    "hook `{}` on `{}` ({}) stores it",
                    system.name,
                    short_name(system.label),
                    at(system.location)
                ));
            }
        }
        let mut requirements = Vec::new();
        S::requirements::<S>(&mut requirements);
        for (requirer, required) in requirements.iter().enumerate() {
            if required.contains(&index) {
                reasons.push(format!(
                    "`{}` requires it (`Requires`), directly or through another requirement",
                    short_name(self.world.info[requirer].name)
                ));
            }
        }
        for registered in &self.registered {
            if registered.type_name == name {
                reasons.push(format!(
                    "`{}::<{}>` at {}",
                    registered.call,
                    short_name(name),
                    at(registered.location)
                ));
            }
        }
        if reasons.is_empty() {
            let plugins: Vec<_> = self
                .registered
                .iter()
                .filter(|r| r.call == "add_plugins")
                .map(|r| format!("`{}` ({})", short_name(r.type_name), at(r.location)))
                .collect();
            reasons.push(if plugins.is_empty() {
                "a relation, state, or internal component the app installs".to_string()
            } else {
                format!(
                    "declared by a plugin's `Components`: one of {}",
                    plugins.join(", ")
                )
            });
        }
        Some(reasons)
    }
    /// Why system `first` runs before system `second` (or does not).
    pub fn why_order(&mut self, first: u32, second: u32) -> Option<String> {
        let systems = self.systems();
        let position = |id| systems.iter().position(|s| s.id == id);
        let (a, b) = (position(first)?, position(second)?);
        let (sa, sb) = (&systems[a], &systems[b]);
        let at = |s: &SystemInfo| format!("{}:{}", s.location.file(), s.location.line());
        let label = super::value::short_name;
        if sa.hook || sb.hook {
            return Some(format!(
                "hooks run inside structural operations (insert, remove, despawn), not in emit order: `{}` and `{}` follow the operations that fire them",
                sa.name, sb.name
            ));
        }
        if sa.label != sb.label {
            return Some(format!(
                "they run under different labels (`{}` and `{}`): each runs when its label is emitted",
                label(sa.label),
                label(sb.label)
            ));
        }
        let (earlier, later) = if a < b { (sa, sb) } else { (sb, sa) };
        if sa.location != sb.location {
            return Some(format!(
                "registration order: `{}` was added at {} before `{}` at {}; registrations under one label run in the order added",
                earlier.name,
                at(earlier),
                later.name,
                at(later)
            ));
        }
        // One registration: the first group where their paths part decides.
        let pa: Vec<&str> = sa.path.split(" > ").collect();
        let pb: Vec<&str> = sb.path.split(" > ").collect();
        let split = pa
            .iter()
            .zip(&pb)
            .position(|(x, y)| x != y)
            .unwrap_or(pa.len().min(pb.len()));
        let group = pa
            .get(split)
            .map_or("", |s| s.split('[').next().unwrap_or(""));
        Some(match group {
            "parallel" => format!(
                "no order: they are in one parallel group (at {}) and may run at the same time; `{}` merely comes first when serial",
                at(sa),
                earlier.name
            ),
            "graph" => format!(
                "the dependency graph at {} ranks `{}` before `{}`",
                at(sa),
                earlier.name,
                later.name
            ),
            _ => format!(
                "`{}` precedes `{}` in a chain (tuple or `.chain()`) at {}",
                earlier.name,
                later.name,
                at(sa)
            ),
        })
    }
}
