use std::ops::{Deref, DerefMut};

/// A mutable component borrow. Reading it does not mark the component changed;
/// mutable dereferencing does, even if the eventual value compares equal.
/// Its column's change summary was raised when the borrow was fetched.
pub struct Mut<'a, T> {
    value: &'a mut T,
    changed: &'a mut u64,
    tick: u64,
}
impl<T> Deref for Mut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}
impl<T> DerefMut for Mut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.mark_changed();
        self.value
    }
}
impl<'a, T> Mut<'a, T> {
    pub(crate) fn new(value: &'a mut T, changed: &'a mut u64, tick: u64) -> Self {
        Self {
            value,
            changed,
            tick,
        }
    }
    fn mark_changed(&mut self) {
        *self.changed = self.tick;
    }
    pub fn into_inner(mut self) -> &'a mut T {
        self.mark_changed();
        self.value
    }
    /// The borrow without marking: for bookkeeping invisible to readers.
    pub(crate) fn into_unmarked(self) -> &'a mut T {
        self.value
    }
}

/// A contiguous mutable component slice with lazy change tracking.
/// Reading the slice does not change timestamps. `get_mut` tracks only the
/// accessed element; `as_mut_slice` and `into_inner` mark every element changed.
pub struct MutSlice<'a, T> {
    values: &'a mut [T],
    changed: &'a mut [u64],
    tick: u64,
}
impl<'a, T> MutSlice<'a, T> {
    pub(crate) fn new(values: &'a mut [T], changed: &'a mut [u64], tick: u64) -> Self {
        Self {
            values,
            changed,
            tick,
        }
    }
    pub fn get_mut(&mut self, index: usize) -> Option<Mut<'_, T>> {
        let value = self.values.get_mut(index)?;
        Some(Mut::new(value, &mut self.changed[index], self.tick))
    }
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = Mut<'_, T>> + DoubleEndedIterator {
        let tick = self.tick;
        self.values
            .iter_mut()
            .zip(self.changed.iter_mut())
            .map(move |(value, changed)| Mut::new(value, changed, tick))
    }
    fn mark_all(&mut self) {
        self.changed.fill(self.tick);
    }
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.mark_all();
        self.values
    }
    pub fn into_inner(mut self) -> &'a mut [T] {
        self.mark_all();
        self.values
    }
}
impl<T> Deref for MutSlice<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.values
    }
}

/// Shared query window for all parameters of one successful system invocation.
#[doc(hidden)]
#[derive(Clone, Copy, Default)]
pub struct ChangeWindow {
    pub(crate) last: u64,
    pub(crate) now: u64,
}
impl ChangeWindow {
    pub(crate) fn contains(self, tick: u64) -> bool {
        tick > self.last && tick <= self.now
    }
}

/// Issue the tick after `latest`. Ticks stay below 2^63 so a frame's flow can
/// pack its executed bit beside the clock.
#[inline]
pub(crate) fn next(latest: u64) -> u64 {
    assert!(latest < u64::MAX >> 1, "world change clock exhausted");
    latest + 1
}

/// Change ticks form a Lamport clock: systems receive the clock along their
/// continuation, writers issue the next tick, and joins take the maximum.
/// Conflicting systems are always ordered, so no shared counter is needed.
#[doc(hidden)]
pub trait SystemWorld {
    /// # Safety
    /// The pointer is live; only immutable world identity is accessed.
    unsafe fn world_id(world: *mut Self) -> u64;
    /// The latest issued tick.
    /// # Safety
    /// The pointer is live and no system holds the world.
    unsafe fn clock(world: *mut Self) -> u64;
    /// # Safety
    /// The pointer is live and no system holds the world.
    unsafe fn set_clock(world: *mut Self, latest: u64);
    /// Where systems move their deferred commands.
    type Queue;
    /// # Safety
    /// The pointer is live; the result borrows only the queue field.
    unsafe fn queue(world: *mut Self) -> *mut Self::Queue;
    /// Apply every queued command.
    fn flush(&mut self);
}
impl SystemWorld for () {
    unsafe fn world_id(_: *mut Self) -> u64 {
        u64::MAX
    }
    unsafe fn clock(_: *mut Self) -> u64 {
        0
    }
    unsafe fn set_clock(_: *mut Self, _: u64) {}
    type Queue = ();
    unsafe fn queue(_: *mut Self) -> *mut () {
        std::ptr::NonNull::dangling().as_ptr()
    }
    fn flush(&mut self) {}
}
