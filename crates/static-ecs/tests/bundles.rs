use static_ecs::prelude::*;

type Schema = (
    (Components<u32>, Components<i16>),
    (Components<u8>, Components<u64>),
);

#[test]
fn undeclared_bundles_are_world_local_and_survive_empty_tables_and_moves() {
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
    // up their column writes; swapping rows and emptying it must not stale caches.
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

#[test]
fn many_bundle_transitions_stay_exact_while_the_cache_grows() {
    let mut world = World::<Schema>::default();
    // Every spawn, insert, and remove below is a distinct cached transition,
    // enough to force several rehashes; each must land in its exact archetype.
    for round in 0..3_u32 {
        let a = world.spawn_bundle((round, 1_i16));
        let b = world.spawn_bundle((round, 2_u8));
        let c = world.spawn_bundle((round, 3_u64));
        let d = world.spawn_bundle((4_i16, 5_u8));
        let e = world.spawn_bundle((6_i16, 7_u64));
        let f = world.spawn_bundle((8_u8, 9_u64));
        let g = world.spawn_bundle((round, 10_i16, 11_u8));
        let h = world.spawn_bundle((round, 12_i16, 13_u64));
        let i = world.spawn_bundle((14_i16, 15_u8, 16_u64));
        let j = world.spawn_bundle((round, 17_i16, 18_u8, 19_u64));
        assert_eq!(world.insert_bundle(a, (20_u8, 21_u64)), (None, None));
        assert_eq!(world.insert_bundle(d, (round, 22_u64)), (None, None));
        assert_eq!(
            world.remove_bundle::<(u32, i16)>(g),
            (Some(round), Some(10))
        );
        assert_eq!(world.remove_bundle::<(i16, u64)>(i), (Some(14), Some(16)));
        assert_eq!(world.remove_bundle::<(u8, u64)>(j), (Some(18), Some(19)));
        for (entity, expected) in [
            (a, (Some(round), Some(1), Some(20), Some(21))),
            (b, (Some(round), None, Some(2), None)),
            (c, (Some(round), None, None, Some(3))),
            (d, (Some(round), Some(4), Some(5), Some(22))),
            (e, (None, Some(6), None, Some(7))),
            (f, (None, None, Some(8), Some(9))),
            (g, (None, None, Some(11), None)),
            (h, (Some(round), Some(12), None, Some(13))),
            (i, (None, None, Some(15), None)),
            (j, (Some(round), Some(17), None, None)),
        ] {
            let actual = (
                world.get::<u32>(entity).copied(),
                world.get::<i16>(entity).copied(),
                world.get::<u8>(entity).copied(),
                world.get::<u64>(entity).copied(),
            );
            assert_eq!(actual, expected, "round {round}");
        }
    }
    assert_eq!(world.query::<&u32>().iter_mut().count(), 3 * 6);
}
