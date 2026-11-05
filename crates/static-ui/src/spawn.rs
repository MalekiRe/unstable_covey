//! Spawning UI trees from nested values, without macros: the tree's shape
//! is a type, so spawning it is a fixed sequence of calls.
//! ```ignore
//! world.spawn_tree(el((PANEL, Background(GRAY))).children((
//!     el((Node::row(), Text::new("Clay - UI Library", 24))),
//!     el((Node::row(),)).children((el((Text::new("ok", 16),)),)),
//! )));
//! ```
use static_ecs::prelude::*;
use static_ecs::Bundle;

use crate::ChildOf;

/// An element: a bundle, and children spawned under it in order.
pub struct El<B, C = ()> {
    bundle: B,
    children: C,
}
/// An element from a bundle; add children with [`El::children`].
pub fn el<B>(bundle: B) -> El<B> {
    El { bundle, children: () }
}
impl<B> El<B> {
    pub fn children<C>(self, children: C) -> El<B, C> {
        El { bundle: self.bundle, children }
    }
}

/// Something that spawns as a UI subtree.
pub trait SpawnTree<S: Storage> {
    /// Spawn under `parent` (or as a root); returns the subtree's root.
    fn spawn_tree(self, world: &mut World<S>, parent: Option<Entity>) -> Entity;
}
impl<S: Storage, B: Bundle<S>, C: SpawnList<S>> SpawnTree<S> for El<B, C>
where
    S: Stores<Relates<ChildOf>> + Stores<Sources<ChildOf>>,
{
    fn spawn_tree(self, world: &mut World<S>, parent: Option<Entity>) -> Entity {
        let entity = world.spawn_bundle(self.bundle);
        if let Some(parent) = parent {
            world.relate(entity, ChildOf, parent);
        }
        self.children.spawn_list(world, entity);
        entity
    }
}

/// A tuple of subtrees spawned in order under one parent.
pub trait SpawnList<S: Storage> {
    fn spawn_list(self, world: &mut World<S>, parent: Entity);
}
impl<S: Storage> SpawnList<S> for () {
    fn spawn_list(self, _: &mut World<S>, _: Entity) {}
}
macro_rules! spawn_list {
    ($($t:ident $i:tt),+) => {
        impl<S: Storage, $($t: SpawnTree<S>),+> SpawnList<S> for ($($t,)+) {
            fn spawn_list(self, world: &mut World<S>, parent: Entity) {
                $(self.$i.spawn_tree(world, Some(parent));)+
            }
        }
    };
}
spawn_list!(A 0);
spawn_list!(A 0, B 1);
spawn_list!(A 0, B 1, C 2);
spawn_list!(A 0, B 1, C 2, D 3);
spawn_list!(A 0, B 1, C 2, D 3, E 4);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
spawn_list!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);
/// A vector of subtrees, for dynamic lists.
impl<S: Storage, T: SpawnTree<S>> SpawnList<S> for Vec<T> {
    fn spawn_list(self, world: &mut World<S>, parent: Entity) {
        for child in self {
            child.spawn_tree(world, Some(parent));
        }
    }
}

/// `world.spawn_ui(tree)` and `world.spawn_ui_under(parent, tree)`.
pub trait SpawnUi<S: Storage> {
    fn spawn_ui(&mut self, tree: impl SpawnTree<S>) -> Entity;
    fn spawn_ui_under(&mut self, parent: Entity, tree: impl SpawnTree<S>) -> Entity;
}
impl<S: Storage> SpawnUi<S> for World<S> {
    fn spawn_ui(&mut self, tree: impl SpawnTree<S>) -> Entity {
        tree.spawn_tree(self, None)
    }
    fn spawn_ui_under(&mut self, parent: Entity, tree: impl SpawnTree<S>) -> Entity {
        tree.spawn_tree(self, Some(parent))
    }
}
