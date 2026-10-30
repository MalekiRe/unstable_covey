use static_ecs::prelude::*;

// Interleave zero-column bundle declarations with component leaves to exercise
// component indexing and bundle-slot selection independently.
type Schema = (
    (BundleSlot<(u32,)>, Components<u32>),
    (
        (Components<i16>, BundleSlot<(i16,)>),
        (BundleSlot<(u32, i16)>, BundleSlot<(i16, u32)>),
    ),
);

#[test]
fn bundle_slots_are_world_local_and_survive_empty_tables_and_moves() {
    let mut first = World::<Schema>::default();
    let mut second = World::<Schema>::default();
    let first_single = first.spawn_bundle((11_u32,));
    let second_single = second.spawn_bundle((22_i16,));

    // Resolve the same bundle in worlds whose table creation orders differ.
    let second_number = second.spawn_bundle((33_u32,));
    // Build the pair archetype through structural mutation before bundle lookup.
    first.insert(first_single, 12_i16);
    let pair = first.spawn_bundle((44_u32, 45_i16));
    let reversed = first.spawn_bundle((56_i16, 55_u32));
    let second_pair = second.spawn_bundle((66_u32, 67_i16));
    assert_eq!(first.get::<u32>(first_single), Some(&11));
    assert_eq!(first.get::<i16>(first_single), Some(&12));
    assert_eq!(second.get::<i16>(second_single), Some(&22));
    assert_eq!(second.get::<u32>(second_number), Some(&33));
    assert_eq!(second.get::<u32>(second_pair), Some(&66));
    assert_eq!(second.get::<i16>(second_pair), Some(&67));

    // Both tuple orders address the same component population, without mixing
    // up their column writes; swapping rows and emptying it must not stale slots.
    let mut rows = first
        .query::<(&u32, &i16)>()
        .iter_mut()
        .map(|(a, b)| (*a, *b))
        .collect::<Vec<_>>();
    rows.sort_unstable();
    assert_eq!(rows, [(11, 12), (44, 45), (55, 56)]);
    assert!(first.despawn(pair));
    assert!(first.despawn(first_single));
    assert!(first.despawn(reversed));
    let mut moved = first;
    let revived = moved.spawn_batch([(78_i16, 77_u32), (80, 79)]);
    assert_eq!(moved.get::<u32>(revived[0]), Some(&77));
    assert_eq!(moved.get::<i16>(revived[0]), Some(&78));
    assert_eq!(moved.get::<u32>(revived[1]), Some(&79));
    assert_eq!(moved.get::<i16>(revived[1]), Some(&80));
}
