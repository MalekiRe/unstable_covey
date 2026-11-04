//! `Default` for the crate's per-system state and per-table fetch types,
//! extended to tuples of up to 16 elements (the standard library stops at 12).

/// The initial value of a state or fetch slot.
#[doc(hidden)]
pub trait Init {
    fn init() -> Self;
}
impl<T> Init for Option<T> {
    #[inline]
    fn init() -> Self {
        None
    }
}
impl<T> Init for *const T {
    #[inline]
    fn init() -> Self {
        std::ptr::null()
    }
}
impl Init for bool {
    #[inline]
    fn init() -> Self {
        false
    }
}
impl Init for () {
    #[inline]
    fn init() {}
}
macro_rules! init_tuple {
    ($($t:ident),+) => {
        impl<$($t: Init),+> Init for ($($t,)+) {
            #[inline]
            fn init() -> Self {
                ($($t::init(),)+)
            }
        }
    };
}
init_tuple!(T0);
init_tuple!(T0, T1);
init_tuple!(T0, T1, T2);
init_tuple!(T0, T1, T2, T3);
init_tuple!(T0, T1, T2, T3, T4);
init_tuple!(T0, T1, T2, T3, T4, T5);
init_tuple!(T0, T1, T2, T3, T4, T5, T6);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
init_tuple!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
init_tuple!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14
);
init_tuple!(
    T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
);

/// `Local<T>`'s state: any `Default` value.
#[doc(hidden)]
pub struct LocalState<T>(pub(crate) T);
impl<T: Default> Init for LocalState<T> {
    #[inline]
    fn init() -> Self {
        Self(T::default())
    }
}
