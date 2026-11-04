//! Infers a world's component vocabulary from the systems an app composes.
//! The result is an ordinary nested-pair schema, deduplicated by component type.
use crate::access::{Drives, Exclusive, Filtered, Mentions, QueryAccess, Read, Union, Write};
use crate::filter::{Added, Changed, Or, With, Without};
use crate::lookup::{ComponentLookup, PointLookup};
use crate::plugin::ComponentList;
use crate::require::{HasRequired, Requires};
use crate::storage::{Components, Point, Sparse, Storage};

/// A storage declaration for one component type.
pub trait Declaration: Storage {
    type Component: 'static;
    const SPARSE: bool;
    const SUMMARY: bool;
}
impl<T: 'static, const SUMMARY: bool> Declaration for Components<T, SUMMARY> {
    type Component = T;
    const SPARSE: bool = false;
    const SUMMARY: bool = SUMMARY;
}
impl<T: 'static, const SUMMARY: bool> Declaration for Sparse<T, SUMMARY> {
    type Component = T;
    const SPARSE: bool = true;
    const SUMMARY: bool = SUMMARY;
}

#[doc(hidden)]
pub trait Present<T: 'static> {
    const VALUE: bool;
}
impl<T: 'static, S: ComponentLookup<T>> Present<T> for S {
    const VALUE: bool = <S as ComponentLookup<T>>::COUNT != 0;
}
#[doc(hidden)]
pub struct If<const B: bool>;
#[doc(hidden)]
pub trait Choose<A, B> {
    type Out;
}
impl<A, B> Choose<A, B> for If<true> {
    type Out = A;
}
impl<A, B> Choose<A, B> for If<false> {
    type Out = B;
}

/// `Self` plus declaration `D` and everything its component requires, unless
/// `Self` already stores the component. New components append, so existing
/// indices never change.
#[doc(hidden)]
pub trait Insert<D> {
    type Out: Storage;
}
impl<C: Storage + Present<D::Component>, D: Declaration> Insert<D> for C
where
    If<{ core::gca!(<C as Present<D::Component>>::VALUE) }>: Append<C, D>,
{
    type Out = <If<{ core::gca!(<C as Present<D::Component>>::VALUE) }> as Append<C, D>>::Out;
}
/// Selected by whether `C` already stores `D`'s component.
#[doc(hidden)]
pub trait Append<C, D> {
    type Out: Storage;
}
impl<C: Storage, D> Append<C, D> for If<true> {
    type Out = C;
}
impl<C: Storage, D: Declaration> Append<C, D> for If<false>
where
    (C, D): Storage,
    If<{ core::gca!(<D::Component as HasRequired>::HAS) }>: AppendRequired<(C, D), D::Component>,
{
    type Out = <If<{ core::gca!(<D::Component as HasRequired>::HAS) }> as AppendRequired<
        (C, D),
        D::Component,
    >>::Out;
}
/// Selected by whether `T` requires components: `C` plus `T::Required`.
#[doc(hidden)]
pub trait AppendRequired<C, T> {
    type Out: Storage;
}
impl<C: Storage, T> AppendRequired<C, T> for If<false> {
    type Out = C;
}
impl<C: Storage, T: Requires> AppendRequired<C, T> for If<true>
where
    T::Required: ComponentList<C>,
{
    type Out = <T::Required as ComponentList<C>>::Out;
}

/// `C` extended with every component an access or filter type mentions.
#[doc(hidden)]
pub trait Uses<C> {
    type Out: Storage;
}
macro_rules! nothing {
    ($($ty:ty),*) => {$(
        impl<C: Storage> Uses<C> for $ty {
            type Out = C;
        }
    )*};
}
nothing!((), Exclusive);
// Relation components come from `.relation::<R>()`; filtering alone does
// not create storage.
impl<C: Storage, R> Uses<C> for crate::filter::Related<R> {
    type Out = C;
}
impl<C: Storage, R> Uses<C> for crate::filter::Relinked<R> {
    type Out = C;
}
macro_rules! component {
    ($($name:ident),*) => {$(
        impl<C: Insert<Components<T>>, T: 'static> Uses<C> for $name<T> {
            type Out = <C as Insert<Components<T>>>::Out;
        }
    )*};
}
component!(Read, Write, Mentions, With, Without, Added, Changed);
impl<C, A: Uses<C>, B: Uses<A::Out>> Uses<C> for (A, B) {
    type Out = B::Out;
}
/// Longer tuples (filters): their head, then the rest as one tuple.
macro_rules! uses_tuple {
    ($a:ident, $($t:ident),+) => {
        impl<C, $a: Uses<C>, $($t),+> Uses<C> for ($a, $($t),+)
        where
            ($($t,)+): Uses<$a::Out>,
        {
            type Out = <($($t,)+) as Uses<$a::Out>>::Out;
        }
    };
}
uses_tuple!(U0, U1, U2);
uses_tuple!(U0, U1, U2, U3);
uses_tuple!(U0, U1, U2, U3, U4);
uses_tuple!(U0, U1, U2, U3, U4, U5);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8, U9);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10, U11);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10, U11, U12);
uses_tuple!(U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10, U11, U12, U13);
uses_tuple!(
    U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10, U11, U12, U13, U14
);
uses_tuple!(
    U0, U1, U2, U3, U4, U5, U6, U7, U8, U9, U10, U11, U12, U13, U14, U15
);

impl<C, A: Uses<C>, F: Uses<A::Out>> Uses<C> for Filtered<A, F> {
    type Out = F::Out;
}
impl<C, A: Uses<C>, B: Uses<A::Out>> Uses<C> for QueryAccess<A, B> {
    type Out = B::Out;
}
impl<C, A: Uses<C>, B: Uses<A::Out>> Uses<C> for Union<A, B> {
    type Out = B::Out;
}
impl<C, F: Uses<C>> Uses<C> for Or<F> {
    type Out = F::Out;
}
#[doc(hidden)]
pub trait Placed<P: 'static> {
    const VALUE: bool;
}
impl<P: 'static, S: PointLookup<P>> Placed<P> for S {
    const VALUE: bool = <S as PointLookup<P>>::COUNT != 0;
}
/// A driven sync point adds its `Point` declaration, once.
impl<C: Storage + Placed<P>, P: 'static> Uses<C> for Drives<P>
where
    If<{ core::gca!(<C as Placed<P>>::VALUE) }>: Choose<C, (C, Point<P>), Out: Storage>,
{
    type Out = <If<{ core::gca!(<C as Placed<P>>::VALUE) }> as Choose<C, (C, Point<P>)>>::Out;
}
