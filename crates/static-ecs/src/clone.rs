//! Cloning entities and subtrees: every `Clone` component is copied, chosen
//! by specialization; link lists are not `Clone`, so relations are rebuilt.
use std::collections::HashMap;

use crate::lookup::ComponentLookup;
use crate::relation::{Relates, Relation, Sources};
use crate::{Entity, Storage, World};

/// Clones `T` from one entity onto another if `T: Clone`; otherwise nothing.
#[doc(hidden)]
pub trait MaybeClone {
    fn clone_component<W: Storage>(world: &mut World<W>, from: Entity, to: Entity);
}
impl<T: Send + Sync + 'static> MaybeClone for T {
    default fn clone_component<W: Storage>(_: &mut World<W>, _: Entity, _: Entity) {}
}
impl<T: Clone + Send + Sync + 'static> MaybeClone for T {
    fn clone_component<W: Storage>(world: &mut World<W>, from: Entity, to: Entity) {
        if let Some(value) = world.get::<T>(from).cloned() {
            world.insert(to, value);
        }
    }
}

impl<S: Storage> World<S> {
    /// Spawn a copy of `entity` holding clones of its `Clone` components;
    /// other components (and relation links) are left out. Hooks run as for
    /// any insertion. `None` if `entity` is dead.
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn clone_entity(&mut self, entity: Entity) -> Option<Entity> {
        if !self.contains(entity) {
            return None;
        }
        let copy = self.spawn();
        S::clone_components(self, entity, copy);
        Some(copy)
    }
    /// Clone `root` and its descendants along `R` (a one-parent relation),
    /// as [`clone_entity`](Self::clone_entity) does, linking each copy to
    /// its parent's copy with a clone of the edge's value, in order. The
    /// root's copy is returned unlinked. A non-`Clone` marker (say, a
    /// `Disabling` `Template`) is how a prototype differs from its copies.
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn clone_subtree<R: Relation + Clone>(&mut self, root: Entity) -> Option<Entity>
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>> + ComponentLookup<crate::relation::PairOf<R>>,
    {
        if !self.contains(root) {
            return None;
        }
        let mut copies = HashMap::new();
        self.walk_subtree::<R>(root, |world, entity| {
            let copy = world.clone_entity(entity).unwrap();
            if entity != root {
                let relates = world.get::<Relates<R>>(entity).unwrap();
                let (parent, value) = (relates.entries()[0].0, R::clone(relates));
                world.relate(copy, value, copies[&parent]);
            }
            copies.insert(entity, copy);
            true
        });
        Some(copies[&root])
    }
}
