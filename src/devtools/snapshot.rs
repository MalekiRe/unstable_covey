//! Time travel: snapshots of a whole world, rewinding to them, and forks
//! that run ahead in a copy and diff against the original.

use std::collections::VecDeque;
use std::fmt;
use std::mem::needs_drop;

use super::value::Value;
use crate::dispatch::Fits;
use crate::{App, Entity, Storage, World};

/// How a component type's values are copied into a snapshot.
pub(crate) trait CloneFn {
    fn clone_fn() -> Option<unsafe fn(*const u8, *mut u8)>;
}
/// Types without drop glue own nothing to free: copied bitwise.
impl<T: 'static> CloneFn for T {
    default fn clone_fn() -> Option<unsafe fn(*const u8, *mut u8)> {
        if needs_drop::<T>() {
            None
        } else {
            Some(copy_bits::<T>)
        }
    }
}
impl<T: Clone + 'static> CloneFn for T {
    fn clone_fn() -> Option<unsafe fn(*const u8, *mut u8)> {
        Some(clone_into::<T>)
    }
}
unsafe fn copy_bits<T>(from: *const u8, to: *mut u8) {
    unsafe { std::ptr::copy_nonoverlapping(from.cast::<T>(), to.cast::<T>(), 1) };
}
unsafe fn clone_into<T: Clone>(from: *const u8, to: *mut u8) {
    unsafe { to.cast::<T>().write((*from.cast::<T>()).clone()) };
}

/// Why a snapshot could not be taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Rows of this component exist, and it is neither `Clone` nor free of
    /// drop glue.
    NotClonable(&'static str),
    /// No snapshot at or before the requested frame.
    NoSnapshot(u64),
}
impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::NotClonable(name) => write!(
                f,
                "`{name}` has rows and is not Clone (and owns resources): derive Clone to snapshot"
            ),
            SnapshotError::NoSnapshot(frame) => write!(f, "no snapshot at or before frame {frame}"),
        }
    }
}
impl std::error::Error for SnapshotError {}

/// A whole world at the end of a frame.
pub struct Snapshot<S: Storage> {
    world: World<S>,
    /// Frames recorded when it was taken.
    pub frame: u64,
    pub tick: u64,
}
impl<S: Storage> Snapshot<S> {
    /// The world as it was, read-only.
    pub fn world(&self) -> &World<S> {
        &self.world
    }
}

/// A copy of the app's world that runs ahead on its own.
pub struct Fork<S: Storage> {
    world: World<S>,
}
impl<S: Storage> Fork<S> {
    pub fn world(&self) -> &World<S> {
        &self.world
    }
    pub fn world_mut(&mut self) -> &mut World<S> {
        &mut self.world
    }
}

/// Snapshots taken automatically every few frames.
pub(crate) struct Snapshots<S: Storage> {
    every: u64,
    keep: usize,
    taken: VecDeque<Snapshot<S>>,
    /// The last failure, reported once.
    pub(crate) error: Option<SnapshotError>,
}
impl<S: Storage> Default for Snapshots<S> {
    fn default() -> Self {
        Self {
            every: 0,
            keep: 0,
            taken: VecDeque::new(),
            error: None,
        }
    }
}
impl<S: Storage> Snapshots<S> {
    /// After a frame: snapshot if due.
    pub(crate) fn after_frame(&mut self, world: &World<S>) {
        if self.every == 0 {
            return;
        }
        let frame = world.history().frames().len() as u64;
        if !frame.is_multiple_of(self.every) {
            return;
        }
        match world.duplicate() {
            Ok(copy) => {
                if self.taken.len() == self.keep {
                    self.taken.pop_front();
                }
                self.taken.push_back(Snapshot {
                    tick: copy.tick(),
                    world: copy,
                    frame,
                });
            }
            Err(name) => self.error = Some(SnapshotError::NotClonable(name)),
        }
    }
}

/// Differences between two worlds sharing entity ids.
#[derive(Clone, Debug, Default)]
pub struct Diff {
    /// Live only in the second world.
    pub spawned: Vec<Entity>,
    /// Live only in the first world.
    pub despawned: Vec<Entity>,
    /// (entity, component, first value, second value); `None` where absent.
    pub changed: Vec<(Entity, &'static str, Option<Value>, Option<Value>)>,
}

impl<S: Storage> World<S> {
    /// A copy of this world as it is now.
    pub fn snapshot(&self) -> Result<Snapshot<S>, SnapshotError> {
        let world = self.duplicate().map_err(SnapshotError::NotClonable)?;
        Ok(Snapshot {
            frame: self.history().frames().len() as u64,
            tick: self.tick(),
            world,
        })
    }
    /// Make this world's contents those of `snapshot` (which stays usable).
    /// Hooks, remote handles, and recorded history stay; the history notes
    /// the rewind as a frame.
    pub fn restore(&mut self, snapshot: &Snapshot<S>) -> Result<(), SnapshotError> {
        let copy = snapshot
            .world
            .duplicate()
            .map_err(SnapshotError::NotClonable)?;
        self.replace_contents(copy);
        self.record_frame("(restored snapshot)");
        Ok(())
    }
    /// Entities and component values that differ between this world and
    /// `other` (a snapshot or fork of it).
    pub fn diff(&self, other: &World<S>) -> Diff {
        let mut diff = Diff::default();
        let live = |world: &World<S>| {
            let mut entities: Vec<Entity> = world
                .tables
                .iter()
                .filter(|t| !t.resource)
                .flat_map(|t| t.entities.iter().copied())
                .collect();
            entities.sort_unstable();
            entities
        };
        let (mine, theirs) = (live(self), live(other));
        for &entity in &mine {
            if !other.contains(entity) {
                diff.despawned.push(entity);
                continue;
            }
            let a = self.describe(entity).unwrap_or_default();
            let b = other.describe(entity).unwrap_or_default();
            for (name, value) in &a {
                let name = *name;
                match b.iter().find(|(n, _)| *n == name) {
                    Some((_, other_value)) if other_value == value => {}
                    found => diff.changed.push((
                        entity,
                        name,
                        Some(value.clone()),
                        found.map(|(_, v)| v.clone()),
                    )),
                }
            }
            for (name, value) in &b {
                if !a.iter().any(|(n, _)| n == name) {
                    diff.changed.push((entity, name, None, Some(value.clone())));
                }
            }
        }
        diff.spawned = theirs.into_iter().filter(|&e| !self.contains(e)).collect();
        diff
    }
}

impl<S: Storage, L> App<S, L> {
    /// Snapshot every `every` frames, keeping the latest `keep` (0 stops).
    pub fn set_auto_snapshots(&mut self, every: u64, keep: usize) {
        self.snapshots.every = every;
        self.snapshots.keep = keep.max(1);
        while self.snapshots.taken.len() > self.snapshots.keep {
            self.snapshots.taken.pop_front();
        }
    }
    /// Frames of the automatic snapshots held, oldest first.
    pub fn snapshot_frames(&self) -> Vec<u64> {
        self.snapshots.taken.iter().map(|s| s.frame).collect()
    }
    /// Why the last automatic snapshot failed, if it did.
    pub fn snapshot_error(&self) -> Option<&SnapshotError> {
        self.snapshots.error.as_ref()
    }
    /// Rewind the world to the latest automatic snapshot at or before
    /// `frame`; returns the snapshot's frame. Later snapshots are dropped:
    /// running forward records new ones.
    pub fn rewind(&mut self, frame: u64) -> Result<u64, SnapshotError> {
        let index = self
            .snapshots
            .taken
            .iter()
            .rposition(|s| s.frame <= frame)
            .ok_or(SnapshotError::NoSnapshot(frame))?;
        self.snapshots.taken.truncate(index + 1);
        let snapshot = &self.snapshots.taken[index];
        let at = snapshot.frame;
        let copy = snapshot
            .world
            .duplicate()
            .map_err(SnapshotError::NotClonable)?;
        self.abandon_step_state();
        self.world.replace_contents(copy);
        self.world.record_frame("(rewound)");
        Ok(at)
    }
    /// A copy of the world to run ahead in, with `in_fork`.
    pub fn fork(&self) -> Result<Fork<S>, SnapshotError> {
        Ok(Fork {
            world: self.world.duplicate().map_err(SnapshotError::NotClonable)?,
        })
    }
    /// Run `body` against the fork's world, with this app's systems, hooks,
    /// and remote handles; then swap back. Systems' local state (`Local`,
    /// query caches aside) is shared between both timelines.
    pub fn in_fork<R>(&mut self, fork: &mut Fork<S>, body: impl FnOnce(&mut Self) -> R) -> R {
        self.abandon_step_state();
        self.world.swap_contents(&mut fork.world);
        struct Back<'a, S: Storage, L>(&'a mut App<S, L>, &'a mut Fork<S>);
        impl<S: Storage, L> Drop for Back<'_, S, L> {
            fn drop(&mut self) {
                self.0.abandon_step_state();
                self.0.world.swap_contents(&mut self.1.world);
            }
        }
        let back = Back(self, fork);
        body(&mut *back.0)
    }
    /// Forget a step pass in progress, applying its commands.
    pub(crate) fn abandon_step_state(&mut self) {
        self.world.flush();
        self.stepping = Default::default();
    }
}

impl<S: Storage, L: Fits<World<S>>> App<S, L> {
    /// Snapshot the world now.
    pub fn snapshot(&self) -> Result<Snapshot<S>, SnapshotError> {
        self.world.snapshot()
    }
}
