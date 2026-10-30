use std::ops::{Deref, DerefMut};

use crate::access::{Access, Filtered, QueryAccess, ValidAccess};
use crate::change::ChangeWindow;
use crate::filter::{FilterFor, QueryFilter};
use crate::query::{FetchQuery, QueryData, QueryState};
use crate::{Query, Single, Storage, World};

pub struct Local<'s, T: 'static>(&'s mut T);
impl<T> Deref for Local<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}
impl<T> DerefMut for Local<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0
    }
}
mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for super::Local<'_, T> {}
    impl<D: super::QueryData, F: super::QueryFilter> Sealed for super::Query<'_, D, F> {}
    impl<D: super::QueryData, F: super::QueryFilter> Sealed for super::Single<'_, D, F> {}
}

/// Built-in parameter description. Implementations are sealed.
/// # Safety
/// Access must describe every world borrow including its structural filter.
/// Fetch may only borrow matching component rows, for the invocation lifetime.
pub unsafe trait Param<S>: sealed::Sealed {
    type Access: Access;
    type State: Default;
    type Item<'a>;
    const VALID: bool;
    /// Returns None when this invocation cannot satisfy the parameter.
    /// # Safety
    /// World is live for 'a; VALID and combined access checks have succeeded.
    /// No conflicting access is live and structural mutation is excluded.
    unsafe fn fetch<'a>(
        world: *mut S,
        local: &'a mut Self::State,
        window: ChangeWindow,
    ) -> Option<Self::Item<'a>>;
}
// SAFETY: Local only borrows its own per-system state, never world storage.
unsafe impl<S, T: Default + 'static> Param<S> for Local<'_, T> {
    type Access = ();
    type State = T;
    type Item<'a> = Local<'a, T>;
    const VALID: bool = true;
    unsafe fn fetch<'a>(_: *mut S, local: &'a mut T, _: ChangeWindow) -> Option<Local<'a, T>> {
        Some(Local(local))
    }
}
// SAFETY: D describes the component accesses, F precisely filters their tables.
// Iteration only yields distinct aligned rows within the query borrow lifetime.
unsafe impl<S: Storage, D: FetchQuery<S>, F: FilterFor<S>> Param<World<S>> for Query<'_, D, F>
where
    D::Access: ValidAccess,
{
    type Access = Filtered<QueryAccess<D::Access, F::Access>, F>;
    type State = QueryState;
    type Item<'a> = Query<'a, D, F>;
    const VALID: bool = D::VALID && F::VALID && D::Access::VALID;
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        state: &'a mut QueryState,
        window: ChangeWindow,
    ) -> Option<Self::Item<'a>> {
        Some(unsafe { Query::from_cached(world, state, window) })
    }
}
// SAFETY: same access as Query; cardinality is checked before any row is fetched.
unsafe impl<S: Storage, D: FetchQuery<S>, F: FilterFor<S>> Param<World<S>> for Single<'_, D, F>
where
    D::Access: ValidAccess,
{
    type Access = Filtered<QueryAccess<D::Access, F::Access>, F>;
    type State = QueryState;
    type Item<'a> = Single<'a, D, F>;
    const VALID: bool = D::VALID && F::VALID && D::Access::VALID;
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        state: &'a mut QueryState,
        window: ChangeWindow,
    ) -> Option<Self::Item<'a>> {
        unsafe { Query::<D, F>::from_cached(world, state, window) }.single()
    }
}
