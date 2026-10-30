use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};

/// A mutable component borrow. Reading it does not mark the component changed;
/// mutable dereferencing does, even if the eventual value compares equal.
pub struct Mut<'a, T> {
    value: &'a mut T,
    changed: &'a mut u64,
    summary: Option<&'a AtomicU64>,
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
    pub(crate) fn new(
        value: &'a mut T,
        changed: &'a mut u64,
        summary: Option<&'a AtomicU64>,
        tick: u64,
    ) -> Self {
        Self {
            value,
            changed,
            summary,
            tick,
        }
    }
    fn mark_changed(&mut self) {
        *self.changed = self.tick;
        if let Some(summary) = self.summary {
            summary.fetch_max(self.tick, Ordering::Relaxed);
        }
    }
    pub fn into_inner(mut self) -> &'a mut T {
        self.mark_changed();
        self.value
    }
}

/// A contiguous mutable component slice with lazy change tracking.
/// Reading the slice does not change timestamps. `get_mut` tracks only the
/// accessed element; `as_mut_slice` and `into_inner` mark every element changed.
pub struct MutSlice<'a, T> {
    values: &'a mut [T],
    changed: &'a mut [u64],
    summary: Option<&'a AtomicU64>,
    tick: u64,
}
impl<'a, T> MutSlice<'a, T> {
    pub(crate) fn new(
        values: &'a mut [T],
        changed: &'a mut [u64],
        summary: Option<&'a AtomicU64>,
        tick: u64,
    ) -> Self {
        Self {
            values,
            changed,
            summary,
            tick,
        }
    }
    pub fn get_mut(&mut self, index: usize) -> Option<Mut<'_, T>> {
        let value = self.values.get_mut(index)?;
        Some(Mut::new(
            value,
            &mut self.changed[index],
            self.summary,
            self.tick,
        ))
    }
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = Mut<'_, T>> + DoubleEndedIterator {
        let summary = self.summary;
        let tick = self.tick;
        self.values
            .iter_mut()
            .zip(self.changed.iter_mut())
            .map(move |(value, changed)| Mut::new(value, changed, summary, tick))
    }
    fn mark_all(&mut self) {
        self.changed.fill(self.tick);
        if !self.values.is_empty()
            && let Some(summary) = self.summary
        {
            summary.fetch_max(self.tick, Ordering::Relaxed);
        }
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

#[doc(hidden)]
pub trait SystemWorld {
    /// # Safety
    /// The pointer is live; only immutable world identity is accessed.
    unsafe fn world_id(world: *mut Self) -> u64;
    /// # Safety
    /// The pointer is live; only the world's atomic clock is accessed.
    unsafe fn next_tick(world: *mut Self) -> u64;
    /// # Safety
    /// The pointer is live; only the world's atomic clock is accessed.
    unsafe fn read_tick(world: *mut Self) -> u64;
}
