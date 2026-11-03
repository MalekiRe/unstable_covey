use crate::storage::{Components, Sparse, Storage};

/// A world schema that stores `T`. Use it to bound code generic over worlds:
/// `fn f<S: Has<Position>>(world: &mut World<S>)`.
/// # Safety
/// COUNT and INDEX identify matching typed leaves in the static schema.
pub unsafe trait ComponentLookup<T: 'static>: Storage {
    const COUNT: usize;
    const INDEX: usize;
    const SPARSE: bool;
    const SUMMARY: bool;
}
// SAFETY: the fallback exposes no matching component.
unsafe impl<T: 'static, S: Storage> ComponentLookup<T> for S {
    default const COUNT: usize = 0;
    default const INDEX: usize = 0;
    default const SPARSE: bool = false;
    default const SUMMARY: bool = false;
}
// SAFETY: each declaration identifies exactly its own component type.
unsafe impl<T: 'static, const SUMMARY: bool> ComponentLookup<T> for Components<T, SUMMARY> {
    const COUNT: usize = 1;
    const INDEX: usize = 0;
    const SPARSE: bool = false;
    const SUMMARY: bool = SUMMARY;
}
unsafe impl<T: 'static, const SUMMARY: bool> ComponentLookup<T> for Sparse<T, SUMMARY> {
    const COUNT: usize = 1;
    const INDEX: usize = 0;
    const SPARSE: bool = true;
    const SUMMARY: bool = SUMMARY;
}
// SAFETY: recursive field order agrees with flattened static indices.
unsafe impl<T: 'static, A: ComponentLookup<T>, B: ComponentLookup<T>> ComponentLookup<T>
    for (A, B)
{
    const COUNT: usize = A::COUNT + B::COUNT;
    const INDEX: usize = if A::COUNT != 0 {
        A::INDEX
    } else {
        A::LEN + B::INDEX
    };
    const SPARSE: bool = if A::COUNT != 0 { A::SPARSE } else { B::SPARSE };
    const SUMMARY: bool = if A::COUNT != 0 {
        A::SUMMARY
    } else {
        B::SUMMARY
    };
}
