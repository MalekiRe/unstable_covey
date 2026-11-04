//! Infers a world's component vocabulary from the systems an app composes.
//! The result is an ordinary nested-pair schema, deduplicated by component type.
use crate::access::{Drives, Exclusive, Filtered, QueryAccess, Read, Union, Write};
use crate::filter::{Added, Changed, Or, With, Without};
use crate::lookup::{ComponentLookup, PointLookup};
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

/// `Self` plus declaration `D`, unless `Self` already stores its component.
/// New components append, so existing indices never change.
#[doc(hidden)]
pub trait Insert<D> {
    type Out: Storage;
}
impl<C: Storage + Present<D::Component>, D: Declaration> Insert<D> for C
where
    If<{ core::gca!(<C as Present<D::Component>>::VALUE) }>: Choose<C, (C, D), Out: Storage>,
{
    type Out = <If<{ core::gca!(<C as Present<D::Component>>::VALUE) }> as Choose<C, (C, D)>>::Out;
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
// Relation components come from `.relation::<R, _>()`; filtering alone does
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
component!(Read, Write, With, Without, Added, Changed);
impl<C, A: Uses<C>, B: Uses<A::Out>> Uses<C> for (A, B) {
    type Out = B::Out;
}
impl<C, A: Uses<C>, B: Uses<A::Out>, D: Uses<B::Out>> Uses<C> for (A, B, D) {
    type Out = D::Out;
}
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
