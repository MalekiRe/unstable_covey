use crate::init::{Init, LocalState};
use std::ops::{Deref, DerefMut};

use crate::access::{Access, Filtered, QueryAccess, Read, Union, ValidAccess, Write};
use crate::change::{ChangeWindow, Mut, SystemWorld};
use crate::command::{CommandQueue, Commands};
use crate::filter::{FilterFor, QueryFilter};
use crate::immutable::IsImmutable;
use crate::lookup::ComponentLookup;
use crate::query::{FetchQuery, JoinCheck, QueryData, QueryState};
use crate::{Query, Remote, Single, Storage, World};

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
    impl<S: super::Storage> Sealed for super::Commands<'_, S> {}
    impl<S: super::Storage> Sealed for &super::Remote<S> {}
    impl<T> Sealed for super::Res<'_, T> {}
    impl<T> Sealed for super::ResMut<'_, T> {}
    impl<P: Sealed> Sealed for Option<P> {}
}

/// What a parameter accesses and yields, independent of any world type.
pub trait ParamShape: sealed::Sealed {
    type Access: Access;
    type State: Init;
    type Item<'a>;
}
/// Fetching a parameter from world type `S`. Implementations are sealed.
/// # Safety
/// Access must describe every world borrow including its structural filter.
/// Fetch may only borrow matching component rows, for the invocation lifetime.
pub unsafe trait Param<S: SystemWorld>: ParamShape {
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
    /// Move deferred work recorded in this parameter's state to the world's
    /// queue, after the system ran.
    fn defer(_state: &mut Self::State, _queue: &mut S::Queue) {}
    /// If `entity` is not this parameter's row, why (queries only).
    /// # Safety
    /// The world is live and nothing runs.
    #[cfg(feature = "devtools")]
    #[doc(hidden)]
    unsafe fn explain(
        _world: *mut S,
        _state: &mut Self::State,
        _entity: crate::Entity,
        _window: ChangeWindow,
        _out: &mut Vec<String>,
    ) {
    }
}
impl<T: Default + 'static> ParamShape for Local<'_, T> {
    type Access = ();
    type State = LocalState<T>;
    type Item<'a> = Local<'a, T>;
}
// SAFETY: Local only borrows its own per-system state, never world storage.
unsafe impl<S: SystemWorld, T: Default + 'static> Param<S> for Local<'_, T> {
    const VALID: bool = true;
    unsafe fn fetch<'a>(
        _: *mut S,
        local: &'a mut LocalState<T>,
        _: ChangeWindow,
    ) -> Option<Local<'a, T>> {
        Some(Local(&mut local.0))
    }
}
impl<S: Storage> ParamShape for &Remote<S> {
    type Access = ();
    type State = ();
    type Item<'a> = &'a Remote<S>;
}
// SAFETY: borrows only the world's remote handle, which never changes; it
// touches no component storage.
unsafe impl<S: Storage> Param<World<S>> for &Remote<S> {
    const VALID: bool = true;
    unsafe fn fetch(world: *mut World<S>, _: &mut (), _: ChangeWindow) -> Option<&Remote<S>> {
        Some(unsafe { &(*world).points.remote })
    }
}
impl<D: QueryData, F: QueryFilter> ParamShape for Query<'_, D, F> {
    /// The row filter narrows the rows' own access; joins reach other
    /// entities whatever the filter.
    type Access = Union<Filtered<QueryAccess<D::Access, F::Access>, F>, (D::Direct, D::Far)>;
    type State = QueryState;
    type Item<'a> = Query<'a, D, F>;
}
// SAFETY: D describes the component accesses, F precisely filters their tables.
// Iteration only yields distinct aligned rows within the query borrow lifetime.
unsafe impl<S: Storage, D: FetchQuery<S>, F: FilterFor<S>> Param<World<S>> for Query<'_, D, F>
where
    D::Access: ValidAccess,
    D: JoinCheck,
{
    const VALID: bool =
        D::VALID && F::VALID && D::Access::VALID && D::ROW_VALID && !D::WRITES_IMMUTABLE;
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        state: &'a mut QueryState,
        window: ChangeWindow,
    ) -> Option<Self::Item<'a>> {
        Some(unsafe { Query::from_cached(world, state, window) })
    }
    #[cfg(feature = "devtools")]
    unsafe fn explain(
        world: *mut World<S>,
        _: &mut QueryState,
        entity: crate::Entity,
        window: ChangeWindow,
        out: &mut Vec<String>,
    ) {
        out.extend(unsafe { Query::<D, F>::why_not(world, entity, window) });
    }
}
impl<D: QueryData, F: QueryFilter> ParamShape for Single<'_, D, F> {
    type Access = Union<Filtered<QueryAccess<D::Access, F::Access>, F>, (D::Direct, D::Far)>;
    type State = QueryState;
    type Item<'a> = Single<'a, D, F>;
}
// SAFETY: same access as Query; cardinality is checked before any row is fetched.
unsafe impl<S: Storage, D: FetchQuery<S>, F: FilterFor<S>> Param<World<S>> for Single<'_, D, F>
where
    D::Access: ValidAccess,
    D: JoinCheck,
{
    const VALID: bool =
        D::VALID && F::VALID && D::Access::VALID && D::ROW_VALID && !D::WRITES_IMMUTABLE;
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        state: &'a mut QueryState,
        window: ChangeWindow,
    ) -> Option<Self::Item<'a>> {
        unsafe { Query::<D, F>::from_cached(world, state, window) }.single()
    }
    #[cfg(feature = "devtools")]
    unsafe fn explain(
        world: *mut World<S>,
        _: &mut QueryState,
        entity: crate::Entity,
        window: ChangeWindow,
        out: &mut Vec<String>,
    ) {
        out.extend(unsafe { Query::<D, F>::why_not(world, entity, window) });
    }
}
impl<S: Storage> ParamShape for Commands<'_, S> {
    type Access = ();
    type State = CommandQueue<S>;
    type Item<'a> = Commands<'a, S>;
}
// SAFETY: Commands only reserves entities atomically during the run; every
// structural change waits for the world's queue, applied with exclusive access.
unsafe impl<S: Storage> Param<World<S>> for Commands<'_, S> {
    const VALID: bool = true;
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        queue: &'a mut CommandQueue<S>,
        _: ChangeWindow,
    ) -> Option<Commands<'a, S>> {
        Some(Commands {
            queue,
            reserve: unsafe { World::reserve(world) },
        })
    }
    fn defer(queue: &mut CommandQueue<S>, world: &mut CommandQueue<S>) {
        world.append(queue);
    }
}

/// Resource `T`, shared. A system whose resource is missing is skipped;
/// take `Option<Res<T>>` to run regardless.
pub struct Res<'a, T: 'static>(&'a T);
impl<T> Deref for Res<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        self.0
    }
}
/// Resource `T`, exclusive, with change detection. Missing skips the system,
/// as for `Res`. Not available for `Immutable` resources (states).
pub struct ResMut<'a, T: 'static>(Mut<'a, T>);
impl<T> Deref for ResMut<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> DerefMut for ResMut<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
impl<T: 'static> ParamShape for Res<'_, T> {
    type Access = Read<T>;
    type State = ();
    type Item<'a> = Res<'a, T>;
}
// SAFETY: reads only `T` on the resource entity, declared as `Read<T>`.
unsafe impl<S: Storage, T: 'static> Param<World<S>> for Res<'_, T> {
    const VALID: bool = <S as ComponentLookup<T>>::COUNT == 1;
    #[inline]
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        _: &'a mut (),
        _: ChangeWindow,
    ) -> Option<Res<'a, T>> {
        let (column, row) = unsafe { World::resource_cell::<T>(world) }?;
        Some(Res(unsafe { &*column.value::<T>(row) }))
    }
}
impl<T: 'static> ParamShape for ResMut<'_, T> {
    type Access = Write<T>;
    type State = ();
    type Item<'a> = ResMut<'a, T>;
}
// SAFETY: writes only `T` on the resource entity, declared as `Write<T>`.
unsafe impl<S: Storage, T: 'static> Param<World<S>> for ResMut<'_, T> {
    const VALID: bool = <S as ComponentLookup<T>>::COUNT == 1 && !<T as IsImmutable>::IS;
    #[inline]
    unsafe fn fetch<'a>(
        world: *mut World<S>,
        _: &'a mut (),
        window: ChangeWindow,
    ) -> Option<ResMut<'a, T>> {
        const {
            assert!(
                !<T as IsImmutable>::IS,
                "immutable resource: replace it with `insert_resource` (states: `set_state`)"
            );
        }
        let (column, row) = unsafe { World::resource_cell::<T>(world) }?;
        Some(ResMut(unsafe {
            column.touch(window.now);
            Mut::new(
                &mut *column.value::<T>(row),
                &mut *column.changed(row),
                window.now,
            )
        }))
    }
}
/// Any parameter, fetched when available: `Option<Res<T>>` runs the system
/// whether or not the resource exists.
impl<P: ParamShape> ParamShape for Option<P> {
    type Access = P::Access;
    type State = P::State;
    type Item<'a> = Option<P::Item<'a>>;
}
// SAFETY: exactly `P`'s access and fetch.
unsafe impl<S: SystemWorld, P: Param<S>> Param<S> for Option<P> {
    const VALID: bool = P::VALID;
    #[inline]
    unsafe fn fetch<'a>(
        world: *mut S,
        state: &'a mut P::State,
        window: ChangeWindow,
    ) -> Option<Option<P::Item<'a>>> {
        Some(unsafe { P::fetch(world, state, window) })
    }
    fn defer(state: &mut P::State, queue: &mut S::Queue) {
        P::defer(state, queue);
    }
}
