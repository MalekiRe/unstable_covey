//! Mutually exclusive components: an entity holds at most one member of each
//! group. Inserting a member removes the entity's members of every group it
//! belongs to, in the same table move: their `OnReplace` and `OnRemove` hooks
//! run first, while the old values are readable.
use std::any::TypeId;

use crate::access::Same;

/// The groups `Self` belongs to, as a tuple of marker types (up to 8):
/// ```
/// use static_ecs::prelude::*;
/// struct Locomotion;
/// struct Life;
/// struct Walking;
/// struct Running;
/// struct Dead;
/// impl OneOf for Walking { type Groups = (Locomotion, Life); }
/// impl OneOf for Running { type Groups = (Locomotion,); }
/// impl OneOf for Dead { type Groups = (Life,); }
/// ```
/// Inserting `Walking` removes `Running` and `Dead`; inserting `Dead` removes
/// `Walking` only, so `Running` and `Dead` may coexist.
pub trait OneOf: 'static {
    type Groups: GroupList;
}

/// A tuple of group marker types.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a tuple of group types",
    note = "a component's groups are a tuple: `type Groups = (Locomotion,);`"
)]
pub trait GroupList: 'static {
    #[doc(hidden)]
    fn ids(out: &mut Vec<TypeId>);
}

/// Whether the tuple `Self` lists group `G`.
#[doc(hidden)]
pub trait Contains<G> {
    const VALUE: bool;
}
impl<T: ?Sized, G> Contains<G> for T {
    default const VALUE: bool = false;
}

/// Whether the tuples `Self` and `L` list a common group.
#[doc(hidden)]
pub trait Intersects<L> {
    const VALUE: bool;
}
impl<T: ?Sized, L> Intersects<L> for T {
    default const VALUE: bool = false;
}

macro_rules! group_list {
    ($($g:ident),+) => {
        impl<$($g: 'static),+> GroupList for ($($g,)+) {
            fn ids(out: &mut Vec<TypeId>) {
                $(out.push(TypeId::of::<$g>());)+
            }
        }
        impl<G: 'static, $($g: 'static),+> Contains<G> for ($($g,)+) {
            const VALUE: bool = false $(|| <G as Same<$g>>::VALUE)+;
        }
        impl<L, $($g: 'static),+> Intersects<L> for ($($g,)+) {
            const VALUE: bool = false $(|| <L as Contains<$g>>::VALUE)+;
        }
    };
}
group_list!(G0);
group_list!(G0, G1);
group_list!(G0, G1, G2);
group_list!(G0, G1, G2, G3);
group_list!(G0, G1, G2, G3, G4);
group_list!(G0, G1, G2, G3, G4, G5);
group_list!(G0, G1, G2, G3, G4, G5, G6);
group_list!(G0, G1, G2, G3, G4, G5, G6, G7);

/// Whether components `Self` and `U` share a group (a member of any group
/// shares it with itself).
#[doc(hidden)]
pub trait Shares<U> {
    const VALUE: bool;
}
impl<T: ?Sized, U> Shares<U> for T {
    default const VALUE: bool = false;
}
impl<T: OneOf, U: OneOf> Shares<U> for T {
    const VALUE: bool = <T::Groups as Intersects<U::Groups>>::VALUE;
}

/// How many members of the tuple `Self` share a group with `U`.
#[doc(hidden)]
pub trait Sharing<U> {
    const COUNT: usize;
}
macro_rules! sharing {
    ($($t:ident),+) => {
        impl<U, $($t),+> Sharing<U> for ($($t,)+) {
            const COUNT: usize = 0 $(+ <$t as Shares<U>>::VALUE as usize)+;
        }
    };
}
sharing!(T0);
sharing!(T0, T1);
sharing!(T0, T1, T2);
sharing!(T0, T1, T2, T3);
sharing!(T0, T1, T2, T3, T4);
sharing!(T0, T1, T2, T3, T4, T5);
sharing!(T0, T1, T2, T3, T4, T5, T6);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
sharing!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
sharing!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14
);
sharing!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
);

/// Any component's groups: none, unless it implements `OneOf`.
#[doc(hidden)]
pub trait MaybeOneOf {
    const IS: bool;
    fn groups(out: &mut Vec<TypeId>);
}
impl<T: ?Sized> MaybeOneOf for T {
    default const IS: bool = false;
    #[inline(always)]
    default fn groups(_: &mut Vec<TypeId>) {}
}
impl<T: OneOf> MaybeOneOf for T {
    const IS: bool = true;
    fn groups(out: &mut Vec<TypeId>) {
        <T::Groups as GroupList>::ids(out);
    }
}
