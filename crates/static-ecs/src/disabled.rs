//! Default query filters: queries skip entities holding a `Disabling`
//! component unless they mention it.

use std::marker::PhantomData;

use crate::lookup::ComponentLookup;
use crate::relation::{Cardinality, Relates, Relation, Side, Sources};
use crate::{Commands, Entity, Storage, World};

/// A default query filter. Queries skip entities holding a `Disabling`
/// component unless they mention it: fetch it (`&T`, `Option<&T>`, ...),
/// ask `Has<T>`, filter on it (`With<T>`, `Without<T>`, `Changed<T>`, ...),
/// or opt in with [`MayHave<T>`](crate::MayHave). Joins skip such targets
/// the same way, unless their inner query mentions the component. Direct
/// access (`World::get`, commands, hooks) still sees them. [`Disabled`] is
/// the built-in one; implement this for your own (prefabs, pooled entities).
/// ```
/// use static_ecs::prelude::*;
/// struct Prefab;
/// impl Disabling for Prefab {}
/// struct Hp(u32);
/// let mut world = App::new()
///     .register::<Hp>()
///     .register::<Prefab>()
///     .register::<Disabled>()
///     .build()
///     .world;
/// world.spawn_bundle((Hp(1),));
/// world.spawn_bundle((Hp(2), Prefab));
/// world.spawn_bundle((Hp(3), Disabled));
/// assert_eq!(world.query::<&Hp>().iter_mut().count(), 1);
/// assert_eq!(world.query_filtered::<&Hp, With<Prefab>>().iter_mut().count(), 1);
/// // Each disabling component needs its own mention.
/// assert_eq!(world.query_filtered::<&Hp, MayHave<Prefab>>().iter_mut().count(), 2);
/// assert_eq!(
///     world
///         .query_filtered::<&Hp, (MayHave<Prefab>, MayHave<Disabled>)>()
///         .iter_mut()
///         .count(),
///     3
/// );
/// ```
pub trait Disabling: 'static {}

/// Hides an entity from every query that does not mention `Disabled`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Disabled;
impl Disabling for Disabled {}

/// Whether `T` is `Disabling`, as a constant for component metadata.
#[doc(hidden)]
pub trait IsDisabling {
    const IS: bool;
}
impl<T: 'static> IsDisabling for T {
    default const IS: bool = false;
}
impl<T: Disabling> IsDisabling for T {
    const IS: bool = true;
}

/// Where [`World::disable_subtree`] started: the entity explicitly disabled
/// with `D`, as opposed to its descendants, which hold `D` because of it.
/// Opening an ancestor leaves such subtrees disabled. Not itself
/// `Disabling`; worlds using subtree toggles store it.
pub struct DisabledRoot<D>(PhantomData<fn() -> D>);
impl<D> Default for DisabledRoot<D> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<S: Storage> World<S> {
    /// Disable `root` and its descendants along `R` (a one-parent relation,
    /// e.g. `ChildOf`) by inserting `D` where it is missing. `root` is
    /// marked [`DisabledRoot<D>`]. Subtrees already disabled are not walked;
    /// entities related below `root` later are not disabled. Allocates
    /// nothing: the walk follows the link lists.
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn disable_subtree<D: Disabling + Default + Send + Sync, R: Relation>(&mut self, root: Entity)
    where
        S: ComponentLookup<D>
            + ComponentLookup<DisabledRoot<D>>
            + ComponentLookup<Relates<R>>
            + ComponentLookup<Sources<R>>,
    {
        if !self.contains(root) || self.get::<DisabledRoot<D>>(root).is_some() {
            return;
        }
        self.insert(root, DisabledRoot::<D>::default());
        self.walk_subtree::<R>(root, |world, entity| {
            if world.get::<D>(entity).is_some() {
                return entity == root;
            }
            world.insert(entity, D::default());
            true
        });
    }
    /// Undo [`disable_subtree`](Self::disable_subtree) at `root`: remove `D`
    /// from it and its descendants, except subtrees disabled at their own
    /// roots. While an ancestor is still disabled, only the mark goes, so
    /// `root` reopens with that ancestor.
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn enable_subtree<D: Disabling + Default + Send + Sync, R: Relation>(&mut self, root: Entity)
    where
        S: ComponentLookup<D>
            + ComponentLookup<DisabledRoot<D>>
            + ComponentLookup<Relates<R>>
            + ComponentLookup<Sources<R>>,
    {
        if self.remove::<DisabledRoot<D>>(root).is_none() {
            return;
        }
        let parent = self.get::<Relates<R>>(root).and_then(|links| links.entries().first());
        if parent.is_some_and(|&(parent, _)| self.get::<D>(parent).is_some()) {
            return;
        }
        self.walk_subtree::<R>(root, |world, entity| {
            if entity != root && world.get::<DisabledRoot<D>>(entity).is_some() {
                return false;
            }
            world.remove::<D>(entity);
            true
        });
    }
    /// Depth-first, parents before children, in link order: `visit` returns
    /// whether to descend. Moves up through each entity's parent link, so
    /// it needs no stack.
    fn walk_subtree<R: Relation>(&mut self, root: Entity, mut visit: impl FnMut(&mut Self, Entity) -> bool)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>,
    {
        const {
            assert!(
                !R::FRAGMENTING && <R::Kind as Cardinality>::Source::ONE,
                "subtree walks need a non-fragmenting relation with one target per source"
            )
        };
        let mut current = root;
        let mut descend = visit(self, root);
        loop {
            if descend
                && let Some(&(child, _)) = self.get::<Sources<R>>(current).and_then(|s| s.entries().first())
            {
                current = child;
                descend = visit(self, current);
                continue;
            }
            // Next sibling, or up a level.
            loop {
                if current == root {
                    return;
                }
                let (parent, slot) = self.get::<Relates<R>>(current).unwrap().entries()[0];
                let sibling = self.get::<Sources<R>>(parent).unwrap().entries().get(slot as usize + 1);
                if let Some(&(sibling, _)) = sibling {
                    current = sibling;
                    descend = visit(self, current);
                    break;
                }
                current = parent;
            }
        }
    }
}

impl<S: Storage> Commands<'_, S> {
    /// Deferred [`World::disable_subtree`].
    pub fn disable_subtree<D: Disabling + Default + Send + Sync, R: Relation>(&mut self, root: Entity)
    where
        S: ComponentLookup<D>
            + ComponentLookup<DisabledRoot<D>>
            + ComponentLookup<Relates<R>>
            + ComponentLookup<Sources<R>>,
    {
        self.queue(move |world| world.disable_subtree::<D, R>(root));
    }
    /// Deferred [`World::enable_subtree`].
    pub fn enable_subtree<D: Disabling + Default + Send + Sync, R: Relation>(&mut self, root: Entity)
    where
        S: ComponentLookup<D>
            + ComponentLookup<DisabledRoot<D>>
            + ComponentLookup<Relates<R>>
            + ComponentLookup<Sources<R>>,
    {
        self.queue(move |world| world.enable_subtree::<D, R>(root));
    }
}
