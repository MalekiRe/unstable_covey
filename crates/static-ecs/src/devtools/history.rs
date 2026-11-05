//! What happened, frame by frame: the values that changed, structural
//! events, who caused each, and questions over all of it.

use std::collections::VecDeque;
use std::fmt::Write as _;

use super::value::{Value, path_steps};
use super::{Writer, writer, writer_of};
use crate::{Entity, Storage, World};

/// One `emit`.
#[derive(Clone, Debug)]
pub struct Frame {
    pub index: u64,
    pub label: &'static str,
    /// Ticks issued since the previous frame record, inclusive.
    pub first_tick: u64,
    pub last_tick: u64,
}
/// A structural change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralKind {
    Spawned,
    /// Components added to a live entity.
    Inserted,
    /// Components removed from a live entity.
    Removed,
    Despawned,
}
/// A structural change, with the components it concerned.
#[derive(Clone, Debug)]
pub struct Structural {
    pub frame: u64,
    pub tick: u64,
    pub entity: Entity,
    pub kind: StructuralKind,
    pub components: Vec<&'static str>,
    pub writer: Writer,
}
/// A value recorded when a frame ended (it changed during or before it).
#[derive(Clone, Debug)]
pub struct Record {
    pub frame: u64,
    pub tick: u64,
    pub entity: Entity,
    pub component: &'static str,
    pub value: Value,
    pub writer: Writer,
}
/// A live row added or changed since some tick.
#[derive(Clone, Debug)]
pub struct Change {
    pub entity: Entity,
    pub component: &'static str,
    pub kind: ChangeKind,
    pub tick: u64,
    pub writer: Writer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Changed,
}
/// One entry of an entity's timeline.
#[derive(Clone, Debug)]
pub enum TimelineEntry {
    Structural(Structural),
    Value(Record),
}
impl TimelineEntry {
    pub fn tick(&self) -> u64 {
        match self {
            TimelineEntry::Structural(s) => s.tick,
            TimelineEntry::Value(r) => r.tick,
        }
    }
}

/// A raw structural event; table indices resolve to components on demand.
#[derive(Clone, Copy, Debug)]
struct Event {
    frame: u64,
    tick: u64,
    entity: Entity,
    from: Option<usize>,
    to: Option<usize>,
    writer: Writer,
}

/// A world's recorded history.
pub(crate) struct Recorder {
    frames: Vec<Frame>,
    events: Vec<Event>,
    records: VecDeque<Record>,
    /// Records dropped to stay within capacity.
    pub(crate) dropped: u64,
    last_tick: u64,
    pub(crate) capacity: usize,
    pub(crate) enabled: bool,
    /// Times the world's contents were replaced (rewind, fork swap): query
    /// caches key on it.
    pub(crate) epoch: u64,
}
impl Default for Recorder {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            events: Vec::new(),
            records: VecDeque::new(),
            dropped: 0,
            last_tick: 0,
            capacity: 1 << 20,
            enabled: true,
            epoch: 0,
        }
    }
}
impl Clone for Recorder {
    fn clone(&self) -> Self {
        Self {
            frames: self.frames.clone(),
            events: self.events.clone(),
            records: self.records.clone(),
            dropped: self.dropped,
            last_tick: self.last_tick,
            capacity: self.capacity,
            enabled: self.enabled,
            epoch: self.epoch,
        }
    }
}
impl Recorder {
    fn frame(&self) -> u64 {
        self.frames.len() as u64
    }
    /// `entity` moved from table `from` to `to` (None: spawned / despawned).
    pub(crate) fn moved(
        &mut self,
        entity: Entity,
        from: Option<usize>,
        to: Option<usize>,
        tick: u64,
    ) {
        if self.enabled {
            let frame = self.frame();
            self.events.push(Event {
                frame,
                tick,
                entity,
                from,
                to,
                writer: writer(),
            });
        }
    }
}

impl<S: Storage> World<S> {
    /// Close the current frame: record every value changed since the last
    /// frame. `emit` calls this after applying commands.
    pub fn record_frame(&mut self, label: &'static str) {
        let first_tick = self.devtools.last_tick + 1;
        let last_tick = self.latest_tick().max(self.devtools.last_tick);
        if self.devtools.enabled {
            let frame = self.devtools.frame();
            let since = self.devtools.last_tick;
            let mut fresh = Vec::new();
            self.scan_changes(since, |entity, component, value, tick, _| {
                fresh.push(Record {
                    frame,
                    tick,
                    entity,
                    component,
                    value: value(),
                    writer: writer_of(tick),
                });
            });
            fresh.sort_by_key(|r| r.tick);
            let recorder = &mut self.devtools;
            for record in fresh {
                if recorder.records.len() == recorder.capacity {
                    recorder.records.pop_front();
                    recorder.dropped += 1;
                }
                recorder.records.push_back(record);
            }
        }
        let index = self.devtools.frame();
        self.devtools.frames.push(Frame {
            index,
            label,
            first_tick,
            last_tick,
        });
        self.devtools.last_tick = last_tick;
    }
    /// Visit every live row whose component changed after `since`.
    fn scan_changes(
        &self,
        since: u64,
        mut visit: impl FnMut(Entity, &'static str, &dyn Fn() -> Value, u64, bool),
    ) {
        for table in &self.tables {
            for (index, info) in self.info.iter().enumerate() {
                if !table.present[index] {
                    continue;
                }
                let column = table.columns[index];
                for (row, &entity) in table.entities.iter().enumerate() {
                    // SAFETY: the row is live and the column holds `info`'s type.
                    let changed = unsafe { *column.changed(row) };
                    if changed > since {
                        let added = unsafe { *column.added(row) } > since;
                        let describe = || unsafe {
                            (info.describe)(column.raw(row, info.layout.size()).cast_const())
                        };
                        visit(entity, info.name, &describe, changed, added);
                    }
                }
            }
        }
    }
    /// Live rows added or changed after `tick`, oldest first, each with who
    /// wrote it.
    pub fn changes_since(&self, tick: u64) -> Vec<Change> {
        let mut changes = Vec::new();
        self.scan_changes(tick, |entity, component, _, tick, added| {
            changes.push(Change {
                entity,
                component,
                kind: if added {
                    ChangeKind::Added
                } else {
                    ChangeKind::Changed
                },
                tick,
                writer: writer_of(tick),
            });
        });
        changes.sort_by_key(|c| c.tick);
        changes
    }
    /// The latest tick issued in this world; pass it to `changes_since`.
    pub fn tick(&self) -> u64 {
        self.latest_tick()
    }
    /// Every component of `entity`, described.
    pub fn describe(&self, entity: Entity) -> Option<Vec<(&'static str, Value)>> {
        let (table, row) = self.locator().find(entity)?;
        let table = &self.tables[table];
        Some(
            self.info
                .iter()
                .enumerate()
                .filter(|&(index, _)| table.present[index])
                .map(|(index, info)| {
                    let column = table.columns[index];
                    // SAFETY: the row is live and holds `info`'s type.
                    let value = unsafe {
                        (info.describe)(column.raw(row, info.layout.size()).cast_const())
                    };
                    (info.name, value)
                })
                .collect(),
        )
    }
    /// One component of `entity` by type name (full or last segment).
    pub fn describe_component(&self, entity: Entity, component: &str) -> Option<Value> {
        let index = self.component_index(component)?;
        let (table, row) = self.locator().find(entity)?;
        let table = &self.tables[table];
        if !table.present[index] {
            return None;
        }
        let info = &self.info[index];
        Some(unsafe {
            (info.describe)(
                table.columns[index]
                    .raw(row, info.layout.size())
                    .cast_const(),
            )
        })
    }
    /// The schema index of a component named `name` (full path, or the last
    /// segment when that is unique).
    pub(crate) fn component_index(&self, name: &str) -> Option<usize> {
        if let Some(index) = self.info.iter().position(|i| i.name == name) {
            return Some(index);
        }
        let mut matches = self
            .info
            .iter()
            .enumerate()
            .filter(|(_, i)| super::value::short_name(i.name) == name);
        let (index, _) = matches.next()?;
        matches.next().is_none().then_some(index)
    }
    /// Components of the world's schema, in index order.
    pub fn component_names(&self) -> Vec<&'static str> {
        self.info.iter().map(|i| i.name).collect()
    }
    /// Write the scalar field `path` of `entity`'s `component` from a
    /// literal (`3`, `-1.5`, `true`, `'x'`), as a debugger writes memory:
    /// only scalars inside plain structs, never `Immutable` components.
    /// The row is marked changed. Returns the field's new value.
    pub fn set_field(
        &mut self,
        entity: Entity,
        component: &str,
        path: &str,
        literal: &str,
    ) -> Result<Value, String> {
        let index = self
            .component_index(component)
            .ok_or_else(|| format!("no component `{component}` in this world"))?;
        let info = self.info[index];
        if info.immutable {
            return Err(format!(
                "`{}` is Immutable: change it by inserting a new value",
                info.name
            ));
        }
        let (table, row) = self
            .locator()
            .find(entity)
            .ok_or_else(|| "the entity is not alive".to_string())?;
        if !self.tables[table].present[index] {
            return Err(format!("the entity has no `{}`", info.name));
        }
        let column = self.tables[table].columns[index];
        // SAFETY: the row is live, holds `info`'s type, and `&mut self`
        // excludes every other borrow.
        let value = unsafe { (info.edit)(column.raw(row, info.layout.size()), path, literal)? };
        let _writing = super::writing(Writer::Edit);
        let tick = self.change_tick();
        unsafe { column.mark(row, tick, false) };
        Ok(value)
    }
    /// The field paths of `component` that `set_field` accepts.
    pub fn editable_fields(&self, component: &str) -> Option<Vec<String>> {
        let index = self.component_index(component)?;
        Some((self.info[index].editable)())
    }
    /// The recorded history: frames, values, and structural events.
    pub fn history(&self) -> History<'_, S> {
        History { world: self }
    }
    /// Stop or resume recording values and structural events.
    pub fn set_recording(&mut self, enabled: bool) {
        self.devtools.enabled = enabled;
    }
    /// Keep at most `records` value records, oldest dropped first.
    pub fn set_history_capacity(&mut self, records: usize) {
        self.devtools.capacity = records.max(1);
        while self.devtools.records.len() > self.devtools.capacity {
            self.devtools.records.pop_front();
            self.devtools.dropped += 1;
        }
    }
    /// Who wrote `entity`'s `component`, what that writer reads, and who
    /// wrote those reads on the same entity before it, `depth` levels back.
    pub fn explain(&self, entity: Entity, component: &str, depth: usize) -> Option<Explanation> {
        let index = self.component_index(component)?;
        let (table, row) = self.locator().find(entity)?;
        if !self.tables[table].present[index] {
            return None;
        }
        let column = self.tables[table].columns[index];
        // SAFETY: the row is live.
        let tick = unsafe { *column.changed(row) };
        Some(self.explain_at(entity, index, tick, depth))
    }
    fn explain_at(&self, entity: Entity, index: usize, tick: u64, depth: usize) -> Explanation {
        let info = &self.info[index];
        let writer = writer_of(tick);
        let history = self.history();
        let value = history
            .records_of(entity, info.name)
            .filter(|r| r.tick <= tick)
            .last()
            .map(|r| r.value.clone());
        let mut inputs = Vec::new();
        if depth > 0
            && let Some(system) = writer.system().and_then(super::system)
        {
            for read in system.reads() {
                let Some(read_index) = self.component_index(read) else {
                    continue;
                };
                // The read value's last write before this one.
                let previous = history
                    .records_of(entity, self.info[read_index].name)
                    .filter(|r| r.tick < tick)
                    .last();
                if let Some(previous) = previous {
                    inputs.push(self.explain_at(entity, read_index, previous.tick, depth - 1));
                }
            }
        }
        Explanation {
            entity,
            component: info.name,
            tick,
            frame: history.frame_of(tick),
            value,
            writer,
            inputs,
        }
    }
    /// Every structural event and recorded value of `entity`, in order.
    pub fn timeline(&self, entity: Entity) -> Vec<TimelineEntry> {
        let history = self.history();
        let mut entries: Vec<_> = history
            .structural()
            .filter(|s| s.entity == entity)
            .map(TimelineEntry::Structural)
            .chain(
                self.devtools
                    .records
                    .iter()
                    .filter(|r| r.entity == entity)
                    .cloned()
                    .map(TimelineEntry::Value),
            )
            .collect();
        entries.sort_by_key(TimelineEntry::tick);
        entries
    }
}

/// Why a value is what it is.
#[derive(Clone, Debug)]
pub struct Explanation {
    pub entity: Entity,
    pub component: &'static str,
    /// The tick of the write explained.
    pub tick: u64,
    /// The frame it happened in (`None`: after the last recorded frame).
    pub frame: Option<u64>,
    /// The value written, if recorded.
    pub value: Option<Value>,
    pub writer: Writer,
    /// The writer's reads on the same entity, each explained in turn.
    pub inputs: Vec<Explanation>,
}
impl Explanation {
    /// An indented, human-readable tree.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out
    }
    fn render_into(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        let value = self
            .value
            .as_ref()
            .map_or("?".to_string(), Value::to_string);
        let frame = self
            .frame
            .map_or("pending".to_string(), |f| format!("frame {f}"));
        let _ = writeln!(
            out,
            "{pad}{} = {value}  ({frame}, tick {}) by {}",
            super::value::short_name(self.component),
            self.tick,
            self.writer.describe()
        );
        for input in &self.inputs {
            let _ = writeln!(out, "{pad}  reads:");
            input.render_into(out, indent + 2);
        }
    }
}

/// A world's recorded history.
pub struct History<'w, S: Storage> {
    world: &'w World<S>,
}
impl<'w, S: Storage> History<'w, S> {
    pub fn frames(&self) -> &'w [Frame] {
        &self.world.devtools.frames
    }
    /// Value records, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &'w Record> + use<'w, S> {
        let world = self.world;
        world.devtools.records.iter()
    }
    /// Records dropped to stay within capacity.
    pub fn dropped(&self) -> u64 {
        self.world.devtools.dropped
    }
    /// Structural events, oldest first.
    pub fn structural(&self) -> impl Iterator<Item = Structural> + 'w {
        let world = self.world;
        world.devtools.events.iter().filter_map(move |event| {
            let present = |table: Option<usize>| table.map(|t| &world.tables[t].present);
            let names = |indices: &mut dyn Iterator<Item = usize>| -> Vec<&'static str> {
                indices.map(|i| world.info[i].name).collect()
            };
            let (kind, components) = match (present(event.from), present(event.to)) {
                (None, Some(to)) => (
                    StructuralKind::Spawned,
                    names(&mut (0..to.len()).filter(|&i| to[i])),
                ),
                (Some(from), None) => (
                    StructuralKind::Despawned,
                    names(&mut (0..from.len()).filter(|&i| from[i])),
                ),
                (Some(from), Some(to)) => {
                    let added = names(&mut (0..to.len()).filter(|&i| to[i] && !from[i]));
                    if added.is_empty() {
                        (
                            StructuralKind::Removed,
                            names(&mut (0..to.len()).filter(|&i| from[i] && !to[i])),
                        )
                    } else {
                        (StructuralKind::Inserted, added)
                    }
                }
                (None, None) => return None,
            };
            Some(Structural {
                frame: event.frame,
                tick: event.tick,
                entity: event.entity,
                kind,
                components,
                writer: event.writer,
            })
        })
    }
    /// Records of one entity's component (full or short name), oldest first.
    pub fn records_of<'a>(
        &self,
        entity: Entity,
        component: &'a str,
    ) -> impl Iterator<Item = &'w Record> + use<'w, 'a, S> {
        let short = super::value::short_name(component);
        self.records().filter(move |r| {
            r.entity == entity
                && (r.component == component || super::value::short_name(r.component) == short)
        })
    }
    /// The frame `tick` fell in.
    pub fn frame_of(&self, tick: u64) -> Option<u64> {
        let frames = self.frames();
        let i = frames.partition_point(|f| f.last_tick < tick);
        frames.get(i).map(|f| f.index)
    }
    /// The value of `entity`'s `component` as recorded at the end of `frame`.
    pub fn value_at(&self, entity: Entity, component: &str, frame: u64) -> Option<&'w Value> {
        self.records_of(entity, component)
            .filter(|r| r.frame <= frame)
            .last()
            .map(|r| &r.value)
    }
    /// Frame by frame, whether `condition` held for `entity`'s `component`
    /// (or any entity's, with `None`), from the first record on: maximal
    /// runs of frames `(first, last)` where it held.
    pub fn when(
        &self,
        entity: Option<Entity>,
        component: &str,
        condition: &dyn Fn(&Value) -> bool,
    ) -> Vec<(Entity, u64, u64)> {
        let last_frame = self.frames().last().map_or(0, |f| f.index);
        let mut entities: Vec<Entity> = self
            .records()
            .filter(|r| {
                super::value::short_name(r.component) == super::value::short_name(component)
            })
            .map(|r| r.entity)
            .filter(|e| entity.is_none_or(|x| x == *e))
            .collect();
        entities.sort_unstable();
        entities.dedup();
        let mut runs = Vec::new();
        for e in entities {
            let mut start = None;
            let mut previous_frame = 0;
            let mut holding = false;
            for record in self.records_of(e, component) {
                // Values hold from their record until the next one.
                let now = condition(&record.value);
                if now && !holding {
                    start = Some(record.frame);
                } else if !now && holding {
                    runs.push((
                        e,
                        start.take().unwrap_or(record.frame),
                        record.frame.saturating_sub(1),
                    ));
                }
                holding = now;
                previous_frame = record.frame;
            }
            if holding {
                // A despawned entity's last value holds until its despawn frame.
                let end = self
                    .structural()
                    .find(|s| s.entity == e && s.kind == StructuralKind::Despawned)
                    .map_or(last_frame, |s| s.frame.saturating_sub(1));
                let start = start.unwrap_or(previous_frame);
                runs.push((e, start, end.max(start)));
            }
        }
        runs
    }
    /// The first record where `condition` holds.
    pub fn first(
        &self,
        entity: Option<Entity>,
        component: &str,
        condition: &dyn Fn(&Value) -> bool,
    ) -> Option<&'w Record> {
        let short = super::value::short_name(component).to_string();
        self.records().find(|r| {
            entity.is_none_or(|e| e == r.entity)
                && super::value::short_name(r.component) == short
                && condition(&r.value)
        })
    }
    /// Every violation of: whenever `trigger` holds for an entity, `then`
    /// holds for it within `within` frames. Returns (entity, trigger frame).
    pub fn violations_of_response(
        &self,
        component: &str,
        trigger: &dyn Fn(&Value) -> bool,
        then_component: &str,
        then: &dyn Fn(&Value) -> bool,
        within: u64,
    ) -> Vec<(Entity, u64)> {
        let last_frame = self.frames().last().map_or(0, |f| f.index);
        let mut out = Vec::new();
        for (entity, start, _) in self.when(None, component, trigger) {
            let answered = self
                .when(Some(entity), then_component, then)
                .iter()
                .any(|&(_, s, e)| e >= start && s <= start + within);
            // Unanswered windows that have not closed yet are not violations.
            if !answered && start + within <= last_frame {
                out.push((entity, start));
            }
        }
        out
    }
}

/// A condition over values, from [`condition`].
pub type Predicate = Box<dyn Fn(&Value) -> bool + Send + Sync>;
/// A condition over a value, parsed from text: `path op literal`, joined
/// with `&&` / `||` (no parentheses). Ops: `< <= > >= == != contains`.
/// The path may be empty (the whole value): `> 3`, `== Idle`, `x < 0.5`.
pub fn condition(text: &str) -> Result<Predicate, String> {
    let alternatives: Vec<Vec<Clause>> = text
        .split("||")
        .map(|alt| alt.split("&&").map(Clause::parse).collect::<Result<_, _>>())
        .collect::<Result<_, _>>()?;
    Ok(Box::new(move |value| {
        alternatives
            .iter()
            .any(|all| all.iter().all(|clause| clause.holds(value)))
    }))
}
struct Clause {
    path: String,
    op: &'static str,
    literal: String,
}
impl Clause {
    fn parse(text: &str) -> Result<Self, String> {
        const OPS: [&str; 7] = ["<=", ">=", "==", "!=", "<", ">", "contains"];
        let text = text.trim();
        for op in OPS {
            if let Some(at) = text.find(op) {
                return Ok(Clause {
                    path: text[..at].trim().to_string(),
                    op,
                    literal: text[at + op.len()..].trim().trim_matches('"').to_string(),
                });
            }
        }
        Err(format!(
            "no operator in `{text}` (use < <= > >= == != contains)"
        ))
    }
    fn holds(&self, value: &Value) -> bool {
        let mut target = value;
        for step in path_steps(&self.path) {
            match target.get(step) {
                Some(v) => target = v,
                None => return false,
            }
        }
        // Single-field tuple structs compare by their field: `Hp(3) < 5`.
        if let Value::Tuple { items, .. } = target
            && items.len() == 1
            && self.op != "contains"
        {
            target = &items[0];
        }
        match (self.op, target.as_f64(), self.literal.parse::<f64>()) {
            ("<", Some(a), Ok(b)) => a < b,
            ("<=", Some(a), Ok(b)) => a <= b,
            (">", Some(a), Ok(b)) => a > b,
            (">=", Some(a), Ok(b)) => a >= b,
            ("==", Some(a), Ok(b)) => a == b,
            ("!=", Some(a), Ok(b)) => a != b,
            ("contains", _, _) => target.to_string().contains(&self.literal),
            ("==", _, _) => text_of(target) == self.literal,
            ("!=", _, _) => text_of(target) != self.literal,
            _ => false,
        }
    }
}
fn text_of(value: &Value) -> String {
    value.as_text().unwrap_or_else(|| value.to_string())
}
