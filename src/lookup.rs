use crate::storage::{Components, Sparse, Storage};

/// # Safety
/// COUNT and INDEX identify matching typed leaves in the static schema.
pub unsafe trait ComponentLookup<T: 'static>: Storage {
    const COUNT: usize;
    const INDEX: usize;
}
// SAFETY: the fallback exposes no matching component.
unsafe impl<T: 'static, S: Storage> ComponentLookup<T> for S {
    default const COUNT: usize = 0;
    default const INDEX: usize = 0;
}
// SAFETY: each declaration identifies exactly its own component type.
unsafe impl<T: 'static, const SUMMARY: bool> ComponentLookup<T> for Components<T, SUMMARY> {
    const COUNT: usize = 1;
    const INDEX: usize = 0;
}
unsafe impl<T: 'static, const SUMMARY: bool> ComponentLookup<T> for Sparse<T, SUMMARY> {
    const COUNT: usize = 1;
    const INDEX: usize = 0;
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
}
