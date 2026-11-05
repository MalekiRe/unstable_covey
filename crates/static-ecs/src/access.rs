use std::marker::PhantomData;

use crate::filter::{Disjoint, Hides};

pub trait Same<T: 'static>: 'static {
    const VALUE: bool;
}
impl<A: 'static, B: 'static> Same<B> for A {
    default const VALUE: bool = false;
}
impl<A: 'static> Same<A> for A {
    const VALUE: bool = true;
}

pub struct Read<T>(PhantomData<fn() -> T>);
pub struct Write<T>(PhantomData<fn() -> T>);
pub struct Exclusive;
/// Component access restricted to archetypes matching `F`.
pub struct Filtered<A, F>(PhantomData<fn() -> (A, F)>);
/// Drives sync point `P`: no component access, so it conflicts with nothing;
/// it only adds `P` to the inferred schema.
pub struct Drives<P>(PhantomData<fn() -> P>);

pub trait Access {
    const WRITES: bool;
    const EXCLUSIVE: bool = false;
    /// The components this access touches, for devtools.
    #[cfg(feature = "devtools")]
    #[doc(hidden)]
    fn touches(_joined: bool, _out: &mut Vec<crate::devtools::Touch>) {}
}
#[cfg(feature = "devtools")]
fn touch<T: ?Sized>(
    kind: crate::devtools::TouchKind,
    joined: bool,
    out: &mut Vec<crate::devtools::Touch>,
) {
    out.push(crate::devtools::Touch {
        component: std::any::type_name::<T>(),
        kind,
        joined,
    });
}
impl Access for () {
    const WRITES: bool = false;
}
impl<T> Access for Read<T> {
    const WRITES: bool = false;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        touch::<T>(crate::devtools::TouchKind::Read, joined, out);
    }
}
impl<T> Access for Write<T> {
    const WRITES: bool = true;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        touch::<T>(crate::devtools::TouchKind::Write, joined, out);
    }
}
impl Access for Exclusive {
    const WRITES: bool = true;
    const EXCLUSIVE: bool = true;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        out.push(crate::devtools::Touch {
            component: "the whole world",
            kind: crate::devtools::TouchKind::Exclusive,
            joined,
        });
    }
}
impl<A: Access, B: Access> Access for (A, B) {
    const WRITES: bool = A::WRITES || B::WRITES;
    const EXCLUSIVE: bool = A::EXCLUSIVE || B::EXCLUSIVE;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        A::touches(joined, out);
        B::touches(joined, out);
    }
}
impl<A: Access, F> Access for Filtered<A, F> {
    const WRITES: bool = A::WRITES;
    const EXCLUSIVE: bool = A::EXCLUSIVE;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        A::touches(joined, out);
    }
}
impl<P> Access for Drives<P> {
    const WRITES: bool = false;
}

pub trait Against<T: 'static, const WRITE: bool> {
    const VALUE: bool;
}
impl<T: 'static, const W: bool> Against<T, W> for () {
    const VALUE: bool = false;
}
impl<T: 'static, U: 'static, const W: bool> Against<T, W> for Read<U> {
    const VALUE: bool = W && <T as Same<U>>::VALUE;
}
impl<T: 'static, U: 'static, const W: bool> Against<T, W> for Write<U> {
    const VALUE: bool = <T as Same<U>>::VALUE;
}
impl<T: 'static, const W: bool> Against<T, W> for Exclusive {
    const VALUE: bool = true;
}
impl<T: 'static, const W: bool, A: Against<T, W>, B: Against<T, W>> Against<T, W> for (A, B) {
    const VALUE: bool = A::VALUE || B::VALUE;
}
impl<T: 'static, const W: bool, A: Against<T, W>, F> Against<T, W> for Filtered<A, F> {
    const VALUE: bool = A::VALUE;
}
impl<T: 'static, const W: bool, P> Against<T, W> for Drives<P> {
    const VALUE: bool = false;
}

pub trait Conflict<B> {
    const VALUE: bool;
}
impl<B: Access> Conflict<B> for () {
    const VALUE: bool = B::EXCLUSIVE;
}
impl<T: 'static, B: Against<T, false>> Conflict<B> for Read<T> {
    const VALUE: bool = B::VALUE;
}
impl<T: 'static, B: Against<T, true>> Conflict<B> for Write<T> {
    const VALUE: bool = B::VALUE;
}
impl<B> Conflict<B> for Exclusive {
    const VALUE: bool = true;
}
impl<A: Conflict<R>, B: Conflict<R>, R> Conflict<R> for (A, B) {
    const VALUE: bool = A::VALUE || B::VALUE;
}
impl<A: Access, F> Conflict<()> for Filtered<A, F> {
    const VALUE: bool = A::EXCLUSIVE;
}
impl<T: 'static, A: Conflict<Read<T>>, F> Conflict<Read<T>> for Filtered<A, F> {
    const VALUE: bool = A::VALUE;
}
impl<T: 'static, A: Conflict<Write<T>>, F> Conflict<Write<T>> for Filtered<A, F> {
    const VALUE: bool = A::VALUE;
}
impl<A, F> Conflict<Exclusive> for Filtered<A, F> {
    const VALUE: bool = true;
}
impl<A, F, B, C> Conflict<(B, C)> for Filtered<A, F>
where
    Self: Conflict<B> + Conflict<C>,
{
    const VALUE: bool = <Self as Conflict<B>>::VALUE || <Self as Conflict<C>>::VALUE;
}
impl<A, B, F, G> Conflict<Filtered<B, G>> for Filtered<A, F>
where
    A: Access + Conflict<B>,
    B: Access,
    F: Disjoint<G> + Hides<(B, G)>,
    G: Disjoint<F> + Hides<(A, F)>,
{
    // Exclusive access borrows the whole world, regardless of filters. A
    // query that never names a `Disabling` component the other requires
    // skips all of the other's rows: an implicit `Without`.
    const VALUE: bool = A::EXCLUSIVE
        || B::EXCLUSIVE
        || (<A as Conflict<B>>::VALUE
            && !<F as Disjoint<G>>::VALUE
            && !<G as Disjoint<F>>::VALUE
            && !<F as Hides<(B, G)>>::VALUE
            && !<G as Hides<(A, F)>>::VALUE);
}
/// Mentions `T` without accessing its values (table membership only): the
/// world stores `T`, and only exclusive access conflicts.
pub struct Mentions<T>(PhantomData<fn() -> T>);
impl<T: 'static> Access for Mentions<T> {
    const WRITES: bool = false;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        touch::<T>(crate::devtools::TouchKind::Mention, joined, out);
    }
}
impl<T> ValidAccess for Mentions<T> {
    const VALID: bool = true;
}
impl<U: 'static, const W: bool, T> Against<U, W> for Mentions<T> {
    const VALUE: bool = false;
}
impl<B: Access, T> Conflict<B> for Mentions<T> {
    const VALUE: bool = B::EXCLUSIVE;
}
impl<A: Access, F, T> Conflict<Mentions<T>> for Filtered<A, F> {
    const VALUE: bool = A::EXCLUSIVE;
}
impl<B, P> Conflict<B> for Drives<P> {
    const VALUE: bool = false;
}
impl<A: Access, F, P> Conflict<Drives<P>> for Filtered<A, F> {
    const VALUE: bool = A::EXCLUSIVE;
}

pub trait ValidAccess {
    const VALID: bool;
}
impl ValidAccess for () {
    const VALID: bool = true;
}
impl<T> ValidAccess for Read<T> {
    const VALID: bool = true;
}
impl<T> ValidAccess for Write<T> {
    const VALID: bool = true;
}
impl<A: ValidAccess, F> ValidAccess for Filtered<A, F> {
    // Filtering cannot make aliases inside one query valid.
    const VALID: bool = A::VALID;
}
impl<A: ValidAccess + Conflict<B>, B: ValidAccess> ValidAccess for (A, B) {
    const VALID: bool = A::VALID && B::VALID && !<A as Conflict<B>>::VALUE;
}
impl<P> ValidAccess for Drives<P> {
    const VALID: bool = true;
}
impl ValidAccess for Exclusive {
    const VALID: bool = true;
}

/// A query's own access and its joins' access: one set for scheduling. The
/// two halves touch different entities, so no conflict between them makes
/// the pair invalid; the query's join checks decide which overlaps are sound.
pub struct Union<A, B>(PhantomData<fn() -> (A, B)>);
impl<A: Access, B: Access> Access for Union<A, B> {
    const WRITES: bool = A::WRITES || B::WRITES;
    const EXCLUSIVE: bool = A::EXCLUSIVE || B::EXCLUSIVE;
    /// The second half reaches other entities through joins.
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        A::touches(joined, out);
        B::touches(true, out);
    }
}
impl<A: ValidAccess, B: ValidAccess> ValidAccess for Union<A, B> {
    const VALID: bool = A::VALID && B::VALID;
}
impl<T: 'static, const W: bool, A: Against<T, W>, B: Against<T, W>> Against<T, W> for Union<A, B> {
    const VALUE: bool = A::VALUE || B::VALUE;
}
impl<A: Conflict<R>, B: Conflict<R>, R> Conflict<R> for Union<A, B> {
    const VALUE: bool = A::VALUE || B::VALUE;
}
impl<A, F, B, C> Conflict<Union<B, C>> for Filtered<A, F>
where
    Self: Conflict<B> + Conflict<C>,
{
    const VALUE: bool = <Self as Conflict<B>>::VALUE || <Self as Conflict<C>>::VALUE;
}

/// Query data and filter reads may overlap inside one query (e.g.
/// Query<&mut T, Changed<T>>), but both participate in external conflicts.
pub struct QueryAccess<A, B>(PhantomData<fn() -> (A, B)>);
impl<A: Access, B: Access> Access for QueryAccess<A, B> {
    const WRITES: bool = A::WRITES || B::WRITES;
    #[cfg(feature = "devtools")]
    fn touches(joined: bool, out: &mut Vec<crate::devtools::Touch>) {
        A::touches(joined, out);
        B::touches(joined, out);
    }
}
impl<A: ValidAccess, B> ValidAccess for QueryAccess<A, B> {
    const VALID: bool = A::VALID;
}
impl<T: 'static, const W: bool, A: Against<T, W>, B: Against<T, W>> Against<T, W>
    for QueryAccess<A, B>
{
    const VALUE: bool = A::VALUE || B::VALUE;
}
impl<A: Conflict<R>, B: Conflict<R>, R> Conflict<R> for QueryAccess<A, B> {
    const VALUE: bool = A::VALUE || B::VALUE;
}
