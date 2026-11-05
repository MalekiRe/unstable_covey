//! Required components: inserting a component brings along the components it
//! requires, each `Default`, unless the entity already has them or the same
//! insert provides them. Requirements are recursive; the world must store
//! every required component (register it, or list it in a plugin).
use crate::lookup::ComponentLookup;
use crate::storage::{Storage, Table};

/// Components inserted alongside `Self`: `type Required = (Pos, Vel);`, each
/// `Default`. Spawning `Bird` alone then yields `(Bird, Pos::default(),
/// Vel::default())` in one table move; values given in the same bundle, or
/// already on the entity, are kept.
pub trait Requires: 'static {
    type Required;
}

/// One required component to initialize: its index, and a writer of its
/// default value into a table row.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct RequiredInit {
    pub(crate) index: usize,
    pub(crate) init: unsafe fn(&Table, usize, usize, u64),
}

/// Write `T::default()` into column `index` at `row`, as newly added.
/// # Safety
/// Column `index` stores `T`; `row` is reserved and uninitialized.
unsafe fn init<T: Default + 'static>(table: &Table, index: usize, row: usize, tick: u64) {
    let column = table.columns[index];
    unsafe {
        column.value::<T>(row).write(T::default());
        column.mark(row, tick, true);
    }
}

/// A tuple of required component types.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a tuple of `Default` components",
    note = "required components are inserted as `Default::default()`: `type Required = (Pos, Vel);`"
)]
pub trait RequiredList<S: Storage> {
    /// Append each member not already listed, then what it requires.
    fn collect(out: &mut Vec<RequiredInit>);
}
impl<S: Storage> RequiredList<S> for () {
    fn collect(_: &mut Vec<RequiredInit>) {}
}
fn push<S: Storage, T: Default + 'static>(out: &mut Vec<RequiredInit>) {
    // Checked per instantiation, so requirement cycles terminate.
    const {
        assert!(
            <S as ComponentLookup<T>>::COUNT == 1,
            "a required component is not stored by this world: register it"
        );
    }
    let index = <S as ComponentLookup<T>>::INDEX;
    if out.iter().any(|r| r.index == index) {
        return;
    }
    out.push(RequiredInit {
        index,
        init: init::<T>,
    });
    <T as MaybeRequires<S>>::collect(out);
}
macro_rules! required_list {
    ($($t:ident),+) => {
        impl<S: Storage, $($t: Default + 'static),+> RequiredList<S> for ($($t,)+) {
            fn collect(out: &mut Vec<RequiredInit>) {
                $(push::<S, $t>(out);)+
            }
        }
    };
}
required_list!(T0);
required_list!(T0, T1);
required_list!(T0, T1, T2);
required_list!(T0, T1, T2, T3);
required_list!(T0, T1, T2, T3, T4);
required_list!(T0, T1, T2, T3, T4, T5);
required_list!(T0, T1, T2, T3, T4, T5, T6);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
required_list!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
required_list!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14
);
required_list!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
);

/// Whether a component implements `Requires`; drives vocabulary inference.
/// One final impl: type-level `gca!` reveals no specialized `default`
/// items, but evaluating this body does.
#[doc(hidden)]
pub trait HasRequired {
    const HAS: bool;
}
impl<T: ?Sized> HasRequired for T {
    const HAS: bool = <T as Specialized>::HAS;
}
trait Specialized {
    const HAS: bool;
}
impl<T: ?Sized> Specialized for T {
    default const HAS: bool = false;
}
impl<T: Requires> Specialized for T {
    const HAS: bool = true;
}

/// What any component requires in world `S`: nothing, unless it implements
/// `Requires`.
#[doc(hidden)]
pub trait MaybeRequires<S: Storage> {
    fn collect(out: &mut Vec<RequiredInit>);
}
impl<S: Storage, T: ?Sized> MaybeRequires<S> for T {
    #[inline(always)]
    default fn collect(_: &mut Vec<RequiredInit>) {}
}
impl<S: Storage, T: Requires> MaybeRequires<S> for T
where
    T::Required: RequiredList<S>,
{
    fn collect(out: &mut Vec<RequiredInit>) {
        <T::Required as RequiredList<S>>::collect(out);
    }
}
