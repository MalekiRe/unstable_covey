use std::marker::PhantomData;

use crate::access::{Access, Read};
use crate::change::ChangeWindow;
use crate::one_of::Shares;
use crate::relation::{PairOf, Relates, Relation};
use crate::storage::Table;
use crate::{Storage, access::Same, lookup::ComponentLookup};

/// Matches archetypes containing `T`.
pub struct With<T>(PhantomData<fn() -> T>);

/// Matches archetypes without `T`.
pub struct Without<T>(PhantomData<fn() -> T>);

/// Matches any alternative in a two- or three-element filter tuple.
pub struct Or<F>(PhantomData<fn() -> F>);

mod sealed {
    pub trait Sealed {}
}

/// Structural predicates match tables; change predicates additionally inspect rows.
/// Tuples combine predicates with AND; [`Or`] combines alternatives with OR.
/// Table-wide outcome. Maybe requires checking individual row timestamps.
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TableMatch {
    None,
    Maybe,
    All,
}
impl TableMatch {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, _) | (_, Self::None) => Self::None,
            (Self::All, Self::All) => Self::All,
            _ => Self::Maybe,
        }
    }
    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, Self::None) => Self::None,
            _ => Self::Maybe,
        }
    }
}
pub trait QueryFilter: sealed::Sealed {
    type State: Copy;
    type Access: Access;
    const ARCHETYPAL: bool = true;
    /// Whether this component is required in every matching archetype.
    #[doc(hidden)]
    fn requires(_: &Self::State, _: usize) -> bool {
        false
    }
    /// # Safety
    /// The row is live and filter accesses are granted.
    unsafe fn row(state: &Self::State, table: &Table, row: usize, window: ChangeWindow) -> bool;
    fn matches(state: &Self::State, present: &[bool]) -> bool;
    /// # Safety
    /// Filter access is granted and table metadata and summaries are live.
    unsafe fn classify(state: &Self::State, table: &Table, _: ChangeWindow) -> TableMatch {
        if Self::matches(state, &table.present) {
            TableMatch::All
        } else {
            TableMatch::None
        }
    }
}

/// Resolves a structural filter against a world's typed component schema.
pub trait FilterFor<S: Storage>: QueryFilter {
    const VALID: bool;
    const STATE: Self::State;
}

impl sealed::Sealed for () {}
impl QueryFilter for () {
    type State = ();
    type Access = ();
    unsafe fn row(_: &(), _: &Table, _: usize, _: ChangeWindow) -> bool {
        true
    }

    fn matches(_: &(), _: &[bool]) -> bool {
        true
    }
}
impl<S: Storage> FilterFor<S> for () {
    const VALID: bool = true;
    const STATE: () = ();
}

impl<T: 'static> sealed::Sealed for With<T> {}
impl<T: 'static> QueryFilter for With<T> {
    type State = usize;
    type Access = ();
    fn requires(state: &usize, index: usize) -> bool {
        *state == index
    }
    unsafe fn row(state: &usize, table: &Table, _: usize, _: ChangeWindow) -> bool {
        Self::matches(state, &table.present)
    }

    fn matches(state: &usize, present: &[bool]) -> bool {
        present[*state]
    }
}
impl<T: 'static, S: Storage + ComponentLookup<T>> FilterFor<S> for With<T> {
    const VALID: bool = <S as ComponentLookup<T>>::COUNT == 1;
    const STATE: usize = <S as ComponentLookup<T>>::INDEX;
}

impl<T: 'static> sealed::Sealed for Without<T> {}
impl<T: 'static> QueryFilter for Without<T> {
    type State = usize;
    type Access = ();
    unsafe fn row(state: &usize, table: &Table, _: usize, _: ChangeWindow) -> bool {
        Self::matches(state, &table.present)
    }

    fn matches(state: &usize, present: &[bool]) -> bool {
        !present[*state]
    }
}
impl<T: 'static, S: Storage + ComponentLookup<T>> FilterFor<S> for Without<T> {
    const VALID: bool = <S as ComponentLookup<T>>::COUNT == 1;
    const STATE: usize = <S as ComponentLookup<T>>::INDEX;
}

/// Matches sources of relation `R`, through `Relates<R>` or, for fragmenting
/// relations, the table's pairs.
pub struct Related<R>(PhantomData<fn() -> R>);
impl<R: Relation> Related<R> {
    /// The component whose presence marks a source table.
    const fn index<S: ComponentLookup<PairOf<R>> + ComponentLookup<Relates<R>>>() -> usize {
        if R::FRAGMENTING {
            <S as ComponentLookup<PairOf<R>>>::INDEX
        } else {
            <S as ComponentLookup<Relates<R>>>::INDEX
        }
    }
}
impl<R: Relation> sealed::Sealed for Related<R> {}
impl<R: Relation> QueryFilter for Related<R> {
    type State = usize;
    type Access = ();
    fn requires(state: &usize, index: usize) -> bool {
        *state == index
    }
    unsafe fn row(state: &usize, table: &Table, _: usize, _: ChangeWindow) -> bool {
        Self::matches(state, &table.present)
    }
    fn matches(state: &usize, present: &[bool]) -> bool {
        present[*state]
    }
}
impl<R: Relation, S: Storage + ComponentLookup<PairOf<R>> + ComponentLookup<Relates<R>>>
    FilterFor<S> for Related<R>
{
    const VALID: bool = if R::FRAGMENTING {
        <S as ComponentLookup<PairOf<R>>>::COUNT == 1
    } else {
        <S as ComponentLookup<Relates<R>>>::COUNT == 1
    };
    const STATE: usize = Self::index::<S>();
}
/// Matches sources whose links along `R` changed since this system last ran
/// (related, retargeted, a target gained or lost, or edge data replaced), for
/// either storage.
pub struct Relinked<R>(PhantomData<fn() -> R>);
impl<R: Relation> sealed::Sealed for Relinked<R> {}
impl<R: Relation> QueryFilter for Relinked<R> {
    type State = usize;
    type Access = Read<Relates<R>>;
    const ARCHETYPAL: bool = false;
    fn requires(state: &usize, index: usize) -> bool {
        *state == index
    }
    unsafe fn classify(index: &usize, table: &Table, window: ChangeWindow) -> TableMatch {
        unsafe { <Changed<Relates<R>> as QueryFilter>::classify(index, table, window) }
    }
    fn matches(index: &usize, present: &[bool]) -> bool {
        present[*index]
    }
    unsafe fn row(index: &usize, table: &Table, row: usize, window: ChangeWindow) -> bool {
        unsafe { <Changed<Relates<R>> as QueryFilter>::row(index, table, row, window) }
    }
}
impl<R: Relation, S: Storage + ComponentLookup<PairOf<R>> + ComponentLookup<Relates<R>>>
    FilterFor<S> for Relinked<R>
{
    const VALID: bool = <Related<R> as FilterFor<S>>::VALID;
    const STATE: usize = Related::<R>::index::<S>();
}
/// The component a `Related<R>` table is guaranteed to contain.
const fn marks<T: 'static, R: Relation>() -> bool {
    if R::FRAGMENTING {
        <T as Same<PairOf<R>>>::VALUE
    } else {
        <T as Same<Relates<R>>>::VALUE
    }
}

/// Whether a matching archetype is guaranteed to contain `T`.
#[doc(hidden)]
pub trait Required<T: 'static> {
    const VALUE: bool;
}

/// Whether a matching archetype is guaranteed not to contain `T`.
#[doc(hidden)]
pub trait Excluded<T: 'static> {
    const VALUE: bool;
}

/// Holding `U` excludes `T` when they are distinct and share a `OneOf` group.
const fn excludes<T: 'static, U: 'static>() -> bool {
    <U as Shares<T>>::VALUE && !<T as Same<U>>::VALUE
}

/// A conservative, directional proof that two filters cannot both match.
/// Access proofs check both directions: conjunctions can expose a useful
/// requirement on either side, while every OR alternative must be disjoint.
#[doc(hidden)]
pub trait Disjoint<F> {
    const VALUE: bool;
}

impl<T: 'static> Required<T> for () {
    const VALUE: bool = false;
}
impl<T: 'static> Excluded<T> for () {
    const VALUE: bool = false;
}
impl<F> Disjoint<F> for () {
    const VALUE: bool = false;
}
impl<T: 'static, U: 'static> Required<T> for With<U> {
    const VALUE: bool = <T as Same<U>>::VALUE;
}
impl<T: 'static, U: 'static> Excluded<T> for With<U> {
    const VALUE: bool = excludes::<T, U>();
}
impl<T: 'static, U: 'static> Required<T> for Without<U> {
    const VALUE: bool = false;
}
impl<T: 'static, U: 'static> Excluded<T> for Without<U> {
    const VALUE: bool = <T as Same<U>>::VALUE;
}
impl<T: 'static, F: Excluded<T>> Disjoint<F> for With<T> {
    const VALUE: bool = F::VALUE;
}
impl<T: 'static, F: Required<T>> Disjoint<F> for Without<T> {
    const VALUE: bool = F::VALUE;
}
impl<T: 'static, R: Relation> Required<T> for Related<R> {
    const VALUE: bool = marks::<T, R>();
}
impl<T: 'static, R: Relation> Excluded<T> for Related<R> {
    const VALUE: bool = false;
}
impl<R: Relation, F: Excluded<PairOf<R>> + Excluded<Relates<R>>> Disjoint<F> for Related<R> {
    const VALUE: bool = if R::FRAGMENTING {
        <F as Excluded<PairOf<R>>>::VALUE
    } else {
        <F as Excluded<Relates<R>>>::VALUE
    };
}
impl<T: 'static, R: Relation> Required<T> for Relinked<R> {
    const VALUE: bool = marks::<T, R>();
}
impl<T: 'static, R: Relation> Excluded<T> for Relinked<R> {
    const VALUE: bool = false;
}
impl<R: Relation, F: Excluded<PairOf<R>> + Excluded<Relates<R>>> Disjoint<F> for Relinked<R> {
    const VALUE: bool = <Related<R> as Disjoint<F>>::VALUE;
}

macro_rules! filter_access {
    ($a:ident) => { $a::Access };
    ($a:ident, $($rest:ident),+) => { ($a::Access, filter_access!($($rest),+)) };
}
macro_rules! filter_tuple {
    ($($filter:ident: $index:tt),+) => {
        impl<$($filter: QueryFilter),+> sealed::Sealed for ($($filter,)+) {}
        impl<$($filter: QueryFilter),+> QueryFilter for ($($filter,)+) {
            type State = ($($filter::State,)+);
            type Access = filter_access!($($filter),+);
            const ARCHETYPAL: bool = true $(&& $filter::ARCHETYPAL)+;
            fn requires(state: &Self::State, index: usize) -> bool {
                false $(|| $filter::requires(&state.$index, index))+
            }
            unsafe fn classify(state: &Self::State, table: &Table, window: ChangeWindow) -> TableMatch {
                TableMatch::All $(.and(unsafe { $filter::classify(&state.$index, table, window) }))+
            }

            fn matches(state: &Self::State, present: &[bool]) -> bool {
                true $(&& $filter::matches(&state.$index, present))+
            }
            unsafe fn row(state: &Self::State, table: &Table, row: usize, window: ChangeWindow) -> bool {
                true $(&& unsafe { $filter::row(&state.$index, table, row, window) })+
            }
        }
        impl<S: Storage, $($filter: FilterFor<S>),+> FilterFor<S> for ($($filter,)+) {
            const VALID: bool = true $(&& $filter::VALID)+;
            const STATE: Self::State = ($($filter::STATE,)+);
        }
        impl<T: 'static, $($filter: Required<T>),+> Required<T> for ($($filter,)+) {
            const VALUE: bool = false $(|| $filter::VALUE)+;
        }
        impl<T: 'static, $($filter: Excluded<T>),+> Excluded<T> for ($($filter,)+) {
            const VALUE: bool = false $(|| $filter::VALUE)+;
        }
        impl<F, $($filter: Disjoint<F>),+> Disjoint<F> for ($($filter,)+) {
            const VALUE: bool = false $(|| $filter::VALUE)+;
        }

        impl<$($filter: QueryFilter),+> sealed::Sealed for Or<($($filter,)+)> {}
        impl<$($filter: QueryFilter),+> QueryFilter for Or<($($filter,)+)> {
            type State = ($($filter::State,)+);
            type Access = filter_access!($($filter),+);
            const ARCHETYPAL: bool = true $(&& $filter::ARCHETYPAL)+;
            fn requires(state: &Self::State, index: usize) -> bool {
                true $(&& $filter::requires(&state.$index, index))+
            }
            unsafe fn classify(state: &Self::State, table: &Table, window: ChangeWindow) -> TableMatch {
                TableMatch::None $(.or(unsafe { $filter::classify(&state.$index, table, window) }))+
            }

            fn matches(state: &Self::State, present: &[bool]) -> bool {
                false $(|| $filter::matches(&state.$index, present))+
            }
            unsafe fn row(state: &Self::State, table: &Table, row: usize, window: ChangeWindow) -> bool {
                false $(|| unsafe { $filter::row(&state.$index, table, row, window) })+
            }
        }
        impl<S: Storage, $($filter: FilterFor<S>),+> FilterFor<S> for Or<($($filter,)+)> {
            const VALID: bool = true $(&& $filter::VALID)+;
            const STATE: Self::State = ($($filter::STATE,)+);
        }
        impl<T: 'static, $($filter: Required<T>),+> Required<T> for Or<($($filter,)+)> {
            const VALUE: bool = true $(&& $filter::VALUE)+;
        }
        impl<T: 'static, $($filter: Excluded<T>),+> Excluded<T> for Or<($($filter,)+)> {
            const VALUE: bool = true $(&& $filter::VALUE)+;
        }
        impl<F, $($filter: Disjoint<F>),+> Disjoint<F> for Or<($($filter,)+)> {
            const VALUE: bool = true $(&& $filter::VALUE)+;
        }
    };
}

filter_tuple!(F0: 0, F1: 1);
filter_tuple!(F0: 0, F1: 1, F2: 2);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10, F11: 11);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10, F11: 11, F12: 12);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10, F11: 11, F12: 12, F13: 13);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10, F11: 11, F12: 12, F13: 13, F14: 14);
filter_tuple!(F0: 0, F1: 1, F2: 2, F3: 3, F4: 4, F5: 5, F6: 6, F7: 7, F8: 8, F9: 9, F10: 10, F11: 11, F12: 12, F13: 13, F14: 14, F15: 15);
/// Matches components inserted since this system last ran successfully.
pub struct Added<T>(PhantomData<fn() -> T>);
/// Matches inserted, replaced, or mutably dereferenced components since last run.
pub struct Changed<T>(PhantomData<fn() -> T>);
macro_rules! change_filter {
    ($name:ident, $field:ident, $summary:ident) => {
        impl<T: 'static> sealed::Sealed for $name<T> {}
        impl<T: 'static> QueryFilter for $name<T> {
            type State = usize;
            type Access = Read<T>;
            const ARCHETYPAL: bool = false;
            fn requires(state: &usize, index: usize) -> bool {
                *state == index
            }
            unsafe fn classify(index: &usize, table: &Table, window: ChangeWindow) -> TableMatch {
                if !table.present[*index] {
                    return TableMatch::None;
                }
                // A newest timestamp beyond now is not proof that older rows
                // miss this ordered window. Only <=last can reject the table.
                if let Some(summary) = unsafe { table.columns[*index].$summary() } {
                    if summary.load(std::sync::atomic::Ordering::Relaxed) <= window.last {
                        return TableMatch::None;
                    }
                }
                TableMatch::Maybe
            }
            fn matches(index: &usize, present: &[bool]) -> bool {
                present[*index]
            }
            unsafe fn row(index: &usize, table: &Table, row: usize, window: ChangeWindow) -> bool {
                table.present[*index]
                    && window.contains(unsafe { *table.columns[*index].$field(row) })
            }
        }
        impl<T: 'static, S: ComponentLookup<T>> FilterFor<S> for $name<T> {
            const VALID: bool = S::COUNT == 1;
            const STATE: usize = S::INDEX;
        }
        impl<T: 'static, U: 'static> Required<T> for $name<U> {
            const VALUE: bool = <T as Same<U>>::VALUE;
        }
        impl<T: 'static, U: 'static> Excluded<T> for $name<U> {
            const VALUE: bool = excludes::<T, U>();
        }
        impl<T: 'static, F: Excluded<T>> Disjoint<F> for $name<T> {
            const VALUE: bool = F::VALUE;
        }
    };
}
change_filter!(Added, added, added_summary);
change_filter!(Changed, changed, changed_summary);
