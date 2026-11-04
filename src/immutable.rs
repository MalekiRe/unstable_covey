//! Immutable components: changed only by inserting a new value or removing
//! the old one, so hooks (`OnReplace`, `OnInsert`) see every change and can
//! keep invariants that a write in place would break.

/// A component that is never written in place: `&mut T` in queries,
/// `World::get_mut`, and `ResMut<T>` do not compile for it. Insert a new
/// value to change it.
/// ```compile_fail
/// use static_ecs::prelude::*;
/// struct Level(u32);
/// impl Immutable for Level {}
/// let mut world = App::new().register::<Level>().build().world;
/// let e = world.spawn_bundle((Level(1),));
/// world.get_mut::<Level>(e);
/// ```
pub trait Immutable: 'static {}

/// Whether `T` is `Immutable`, as a constant for compile-time checks.
#[doc(hidden)]
pub trait IsImmutable {
    const IS: bool;
}
impl<T: ?Sized> IsImmutable for T {
    default const IS: bool = false;
}
impl<T: Immutable> IsImmutable for T {
    const IS: bool = true;
}
