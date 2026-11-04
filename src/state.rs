//! States: one type per state, grouped. A state is an `Immutable` resource;
//! `set_state` exits the group's active state (removing it, which runs its
//! `OnExit` hooks) and enters the new one (inserting it, which runs `OnEnter`
//! and `OnTransition`). No values are compared: changing a state's data
//! is a transition too. Substates and computed states hang off a parent
//! state: they exit before it (innermost first) and enter after it.
//!
//! A transition is atomic: commands its hooks queue apply after it, substates
//! included. Each group follows at most one parent state, so substates
//! cannot form a cycle: its first state could never become active.
use std::any::{TypeId, type_name};
use std::marker::PhantomData;

use crate::dispatch::{Lifecycle, Schedule};
use crate::hook::{Event, Insert, Replace};
use crate::immutable::Immutable;
use crate::param::Res;
use crate::require::Requires;
use crate::{Storage, World};

/// A state of group `Group`, which has at most one active state:
/// ```
/// use static_ecs::prelude::*;
/// struct Game;
/// struct Menu;
/// struct Playing { level: u32 }
/// impl State for Menu { type Group = Game; }
/// impl State for Playing { type Group = Game; }
/// ```
pub trait State: Send + Sync + 'static {
    type Group: 'static;
}
impl<T: State> Immutable for T {}
// Storing a state stores its group's record.
impl<T: State> Requires for T {
    type Required = (Current<T::Group>,);
}

/// Whether `T` is a `State`, as a constant for compile-time checks.
#[doc(hidden)]
pub trait IsState {
    const IS: bool;
}
impl<T: ?Sized> IsState for T {
    default const IS: bool = false;
}
impl<T: State> IsState for T {
    const IS: bool = true;
}

/// Group `G`'s active state and the one before it, on the resource entity.
#[doc(hidden)]
pub struct Current<G> {
    active: Option<Active>,
    previous: Option<TypeId>,
    group: PhantomData<fn() -> G>,
}
impl<G> Default for Current<G> {
    fn default() -> Self {
        Self {
            active: None,
            previous: None,
            group: PhantomData,
        }
    }
}
impl<G: 'static> Immutable for Current<G> {}

/// An active state: its type, and how to exit it (`exit::<S, T>`, erased:
/// the record never leaves its world).
#[derive(Clone, Copy)]
struct Active {
    id: TypeId,
    name: &'static str,
    exit: fn(),
}

/// A substate or computed state: group `group` follows parent state `parent`.
pub(crate) struct Link<S: Storage> {
    parent: TypeId,
    group: TypeId,
    parent_active: fn(&World<S>) -> bool,
    enter: fn(&mut World<S>, fn()),
    exit: fn(&mut World<S>),
    /// `enter`'s erased argument: a computed state's function.
    data: fn(),
}
impl<S: Storage> Clone for Link<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Storage> Copy for Link<S> {}

/// Exit state `T`: its substates first, newest first, then `T` itself.
fn exit<S: Storage, T: State>(world: &mut World<S>) {
    let parent = TypeId::of::<T>();
    for i in (0..world.state_links.len()).rev() {
        let link = world.state_links[i];
        if link.parent == parent {
            (link.exit)(world);
        }
    }
    if let Some(entity) = world.resources {
        world.remove::<T>(entity);
    }
}
fn nothing() {}

impl<S: Storage> World<S> {
    /// Make `state` its group's active state: exit the current one (its
    /// substates first), then enter `state` and then its substates. Ignored,
    /// with a debug panic, when `T` is a substate whose parent is inactive.
    pub fn set_state<T: State>(&mut self, state: T) {
        let group = TypeId::of::<T::Group>();
        if let Some(link) = self.state_links.iter().find(|link| link.group == group)
            && !(link.parent_active)(self)
        {
            debug_assert!(
                false,
                "set_state({}): its parent state is not active",
                type_name::<T>()
            );
            return;
        }
        self.transition(|world| {
            let previous = world.exit_active::<T::Group>();
            let exit: fn(&mut World<S>) = exit::<S, T>;
            world.put_current::<T::Group>(Current {
                active: Some(Active {
                    id: TypeId::of::<T>(),
                    name: type_name::<T>(),
                    // SAFETY: called back only as `fn(&mut World<S>)`, by this world.
                    exit: unsafe { std::mem::transmute::<fn(&mut World<S>), fn()>(exit) },
                }),
                previous,
                group: PhantomData,
            });
            let entity = world.resource_entity();
            world.insert(entity, state);
            let parent = TypeId::of::<T>();
            for i in 0..world.state_links.len() {
                let link = world.state_links[i];
                if link.parent == parent {
                    (link.enter)(world, link.data);
                }
            }
        });
    }
    /// Exit group `G`'s active state, leaving none.
    pub fn clear_state<G: 'static>(&mut self) {
        self.transition(|world| {
            if let Some(previous) = world.exit_active::<G>() {
                world.put_current::<G>(Current {
                    active: None,
                    previous: Some(previous),
                    group: PhantomData,
                });
            }
        });
    }
    /// The type name of group `G`'s active state.
    pub fn state_name<G: 'static>(&self) -> Option<&'static str> {
        Some(self.resource::<Current<G>>()?.active?.name)
    }
    /// Run a transition holding back what its hooks queue until it is done.
    fn transition(&mut self, run: impl FnOnce(&mut Self)) {
        if self.flushing {
            // The running flush applies them afterwards.
            return run(self);
        }
        struct Hold<'a, S: Storage>(&'a mut World<S>);
        impl<S: Storage> Drop for Hold<'_, S> {
            fn drop(&mut self) {
                self.0.flushing = false;
            }
        }
        self.flushing = true;
        let hold = Hold(self);
        run(&mut *hold.0);
        drop(hold);
        self.flush();
    }
    fn link(&mut self, link: Link<S>, group: &str) {
        assert!(
            !self
                .state_links
                .iter()
                .any(|other| other.group == link.group),
            "state group {group} already follows a parent state"
        );
        self.state_links.push(link);
    }
    /// Exit `G`'s active state, if any, returning its type.
    fn exit_active<G: 'static>(&mut self) -> Option<TypeId> {
        let active = self.resource::<Current<G>>()?.active?;
        // SAFETY: `set_state::<_, S>` erased exactly this type.
        let exit = unsafe { std::mem::transmute::<fn(), fn(&mut World<S>)>(active.exit) };
        exit(self);
        Some(active.id)
    }
    fn put_current<G: 'static>(&mut self, current: Current<G>) {
        let entity = self.resource_entity();
        self.insert(entity, current);
    }
    /// Make `C` (from its default) follow parent state `P`.
    pub(crate) fn link_sub_state<P: State, C: State + Default>(&mut self) {
        let link = Link {
            parent: TypeId::of::<P>(),
            group: TypeId::of::<C::Group>(),
            parent_active: |world| world.resource::<P>().is_some(),
            enter: |world, _| world.set_state(C::default()),
            exit: |world| world.clear_state::<C::Group>(),
            data: nothing,
        };
        self.link(link, type_name::<C::Group>());
    }
    /// Make `X` follow parent state `P` through `compute`.
    pub(crate) fn link_computed_state<P: State, X: State>(&mut self, compute: fn(&P) -> Option<X>) {
        fn enter<S: Storage, P: State, X: State>(world: &mut World<S>, data: fn()) {
            // SAFETY: erased from exactly this type below.
            let compute = unsafe { std::mem::transmute::<fn(), fn(&P) -> Option<X>>(data) };
            if let Some(state) = world.resource::<P>().and_then(compute) {
                world.set_state(state);
            }
        }
        let link = Link {
            parent: TypeId::of::<P>(),
            group: TypeId::of::<X::Group>(),
            parent_active: |world| world.resource::<P>().is_some(),
            enter: enter::<S, P, X>,
            exit: |world| world.clear_state::<X::Group>(),
            // SAFETY: called back only as `fn(&P) -> Option<X>`, by `enter`.
            data: unsafe { std::mem::transmute::<fn(&P) -> Option<X>, fn()>(compute) },
        };
        self.link(link, type_name::<X::Group>());
    }
}

/// Lifecycle label: after state `T` is entered, with no input.
pub struct OnEnter<T>(pub PhantomData<fn(T)>);
/// Lifecycle label: before state `T` is exited, while it is still readable.
pub struct OnExit<T>(pub PhantomData<fn(T)>);
/// Lifecycle label: after entering `To` directly from `From`.
pub struct OnTransition<From, To>(pub PhantomData<fn(From, To)>);
impl<T: State> Event for OnEnter<T> {
    type Component = T;
    type Kind = Insert;
}
impl<T: State> Event for OnExit<T> {
    type Component = T;
    type Kind = Replace;
}
impl<A: State, B: State> Event for OnTransition<A, B> {
    type Component = B;
    type Kind = Insert;
    #[inline]
    unsafe fn admits<S: Storage>(world: *mut World<S>) -> bool {
        unsafe { &*world }
            .resource::<Current<B::Group>>()
            .is_some_and(|current| current.previous == Some(TypeId::of::<A>()))
    }
}
macro_rules! labels {
    ($($label:ident<$($t:ident),+>),*) => {$(
        impl<$($t: State),+> Schedule for $label<$($t),+> {
            type Route = Lifecycle;
        }
        impl<$($t),+> $label<$($t),+> {
            /// The label, for `add_systems`.
            pub const fn new() -> Self {
                Self(PhantomData)
            }
        }
        impl<$($t),+> Default for $label<$($t),+> {
            fn default() -> Self {
                Self::new()
            }
        }
    )*};
}
labels!(OnEnter<T>, OnExit<T>, OnTransition<A, B>);

/// Run condition: state `T` is active. Systems taking `Res<T>` are gated
/// on it already.
pub fn in_state<T: State>() -> impl FnMut(Option<Res<'_, T>>) -> bool + Clone + Send + 'static {
    |state: Option<Res<T>>| state.is_some()
}
