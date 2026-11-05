//! Default query filters: queries skip entities holding a `Disabling`
//! component unless they mention it.

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
