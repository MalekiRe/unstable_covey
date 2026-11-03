use static_ecs::prelude::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, PartialEq)]
struct Position(i32);
#[derive(Debug, PartialEq)]
struct Velocity(i32);
struct Tag;

#[test]
fn table_moves_swap_rows_and_reallocation_preserve_entity_alignment() {
    type State = (
        Components<Position>,
        (Components<Velocity>, Components<Tag>),
    );
    let mut world = World::<State>::default();
    let entities: Vec<_> = (0..96).map(|_| world.spawn()).collect();
    for (i, &entity) in entities.iter().enumerate() {
        world.insert(entity, Position(i as i32));
        if i % 2 == 0 {
            world.insert(entity, Velocity(1000 + i as i32));
        }
        if i % 3 == 0 {
            world.insert(entity, Tag);
        }
    }
    // Move non-last rows out of several tables, then move rows back into them.
    for (i, &entity) in entities.iter().enumerate() {
        if i % 5 == 0 {
            assert!(world.despawn(entity));
        } else {
            if i % 2 == 0 {
                assert_eq!(
                    world.remove::<Velocity>(entity),
                    Some(Velocity(1000 + i as i32))
                );
            }
            world.insert(entity, Velocity(2000 + i as i32));
            if i % 3 == 0 {
                assert!(world.remove::<Tag>(entity).is_some());
            } else {
                world.insert(entity, Tag);
            }
            assert_eq!(
                world.insert(entity, Position(10 * i as i32)),
                Some(Position(i as i32))
            );
        }
    }
    let mut seen = [false; 96];
    let mut query = world.query::<(Entity, (&mut Position, &Velocity))>();
    for (entity, (mut position, velocity)) in &mut query {
        let i = entities
            .iter()
            .position(|&candidate| candidate == entity)
            .unwrap();
        assert!(!seen[i]);
        seen[i] = true;
        assert_ne!(i % 5, 0);
        assert_eq!(position.0, 10 * i as i32);
        assert_eq!(velocity.0, 2000 + i as i32);
        position.0 += velocity.0;
    }
    for (i, &entity) in entities.iter().enumerate() {
        assert_eq!(seen[i], i % 5 != 0);
        assert_eq!(world.contains(entity), i % 5 != 0);
        if i % 5 == 0 {
            assert_eq!(world.get::<Position>(entity), None);
            assert_eq!(world.get::<Velocity>(entity), None);
        } else {
            assert_eq!(
                world.get::<Position>(entity),
                Some(&Position(2000 + 11 * i as i32))
            );
            assert_eq!(world.get::<Tag>(entity).is_some(), i % 3 != 0);
        }
    }
}

#[test]
fn stale_and_foreign_handles_cannot_address_live_rows() {
    let mut first = World::<Components<u32>>::default();
    let mut second = World::<Components<u32>>::default();
    let a = first.spawn();
    let b = second.spawn();
    first.insert(a, 10_u32);
    second.insert(b, 20_u32);
    assert_ne!(a, b);
    assert!(!second.contains(a));
    assert_eq!(second.get::<u32>(a), None);
    assert_eq!(second.remove::<u32>(a), None);
    assert!(!second.despawn(a));
    assert!(catch_unwind(AssertUnwindSafe(|| second.insert(a, 30_u32))).is_err());
    assert!(first.despawn(a));
    assert!(!first.despawn(a));
    let fresh = first.spawn();
    assert_ne!(a, fresh);
    assert!(catch_unwind(AssertUnwindSafe(|| first.insert(a, 40_u32))).is_err());
    first.insert(fresh, 50_u32);
    assert_eq!(first.get::<u32>(fresh), Some(&50));
    assert_eq!(second.get::<u32>(b), Some(&20));
}

#[test]
fn moved_replaced_removed_and_despawned_values_drop_exactly_once() {
    struct Owned {
        id: usize,
        drops: Arc<Vec<AtomicUsize>>,
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            self.drops[self.id].fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new((0..65).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    let mut world = World::<(Components<Owned>, Components<Tag>)>::default();
    let entities: Vec<_> = (0..64)
        .map(|id| {
            let entity = world.spawn();
            world.insert(
                entity,
                Owned {
                    id,
                    drops: drops.clone(),
                },
            );
            entity
        })
        .collect();
    for &entity in &entities {
        world.insert(entity, Tag);
    }
    for &entity in entities.iter().step_by(2) {
        world.remove::<Tag>(entity);
    }
    assert!(drops.iter().all(|n| n.load(Ordering::SeqCst) == 0));
    let replaced = world
        .insert(
            entities[0],
            Owned {
                id: 64,
                drops: drops.clone(),
            },
        )
        .unwrap();
    let removed = world.remove::<Owned>(entities[1]).unwrap();
    assert_eq!(replaced.id, 0);
    assert_eq!(removed.id, 1);
    assert_eq!(drops[0].load(Ordering::SeqCst), 0);
    assert_eq!(drops[1].load(Ordering::SeqCst), 0);
    assert!(world.despawn(entities[2]));
    assert_eq!(drops[2].load(Ordering::SeqCst), 1);
    drop(world);
    for id in 2..65 {
        assert_eq!(drops[id].load(Ordering::SeqCst), 1);
    }
    assert_eq!(drops[0].load(Ordering::SeqCst), 0);
    assert_eq!(drops[1].load(Ordering::SeqCst), 0);
    drop((replaced, removed));
    assert!(drops.iter().all(|n| n.load(Ordering::SeqCst) == 1));
}

#[test]
fn empty_schema_tracks_entity_lifetimes() {
    let mut world = World::new(());
    let a = world.spawn();
    let b = world.spawn();
    assert!(world.despawn(a));
    assert!(world.contains(b));
    assert_eq!(
        world.query::<Entity>().iter_mut().collect::<Vec<_>>(),
        vec![b]
    );
    assert!(world.despawn(b));
    assert_eq!(world.query::<Entity>().iter_mut().count(), 0);
}

#[test]
fn panicking_destructor_leaves_surviving_rows_addressable() {
    struct Bomb(bool);
    impl Drop for Bomb {
        fn drop(&mut self) {
            assert!(!self.0, "component destructor panic");
        }
    }
    let mut world = World::<(Components<Bomb>, Components<u32>)>::default();
    let removed = world.spawn();
    let survivor = world.spawn();
    for (entity, armed, value) in [(removed, true, 10_u32), (survivor, false, 20)] {
        world.insert(entity, Bomb(armed));
        world.insert(entity, value);
    }
    assert!(catch_unwind(AssertUnwindSafe(|| world.despawn(removed))).is_err());
    assert!(!world.contains(removed));
    assert_eq!(world.get::<u32>(survivor), Some(&20));
    *world.get_mut::<u32>(survivor).unwrap() += 1;
    assert_eq!(
        world.single::<(Entity, &u32)>().unwrap().into_inner(),
        (survivor, &21)
    );
    assert!(world.remove::<Bomb>(survivor).is_some());
    assert_eq!(world.get::<u32>(survivor), Some(&21));
}

#[test]
fn batch_iteration_panic_keeps_prefix_and_allows_further_growth_and_migration() {
    type Schema = (Components<u32>, Components<Tag>);
    let mut world = World::<Schema>::default();
    let first = world.spawn_batch((0_u32..17).map(|n| (n, Tag)));
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            world.spawn_batch((17_u32..25).map(|n| {
                assert_ne!(n, 20, "iterator panic");
                (n, Tag)
            }));
        }))
        .is_err()
    );
    assert!(
        world
            .spawn_batch(std::iter::empty::<(u32, Tag)>())
            .is_empty()
    );
    // An iterator with no useful size hint must grow both column buffers safely.
    let rest = world.spawn_batch((20_u32..100).filter(|_| true).map(|n| (n, Tag)));
    for &entity in Iterator::chain(first.iter(), &rest) {
        world.remove::<Tag>(entity);
    }
    for &entity in Iterator::chain(first.iter(), &rest) {
        world.insert(entity, Tag);
    }
    let mut values = world
        .query::<&u32>()
        .iter_mut()
        .copied()
        .collect::<Vec<_>>();
    values.sort_unstable();
    assert_eq!(values, (0..100).collect::<Vec<_>>());
    for (n, &entity) in first.iter().enumerate() {
        assert_eq!(world.get::<u32>(entity), Some(&(n as u32)));
    }
}

#[test]
fn nested_destructor_unwinding_finishes_remaining_columns() {
    struct Owned(usize, Arc<Vec<AtomicUsize>>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.1[self.0].fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Bomb(bool);
    impl Drop for Bomb {
        fn drop(&mut self) {
            assert!(!self.0, "nested destructor panic");
        }
    }
    let drops = Arc::new((0..4).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    type Schema = (
        (Components<Owned>, Components<Bomb>),
        (Components<String>, Components<u32>),
    );
    let mut world = World::<Schema>::default();
    let entities = world.spawn_batch((0..4).map(|i| {
        (
            Owned(i, drops.clone()),
            Bomb(i == 1),
            format!("row {i}"),
            i as u32,
        )
    }));
    assert!(catch_unwind(AssertUnwindSafe(|| world.despawn(entities[1]))).is_err());
    assert!(!world.contains(entities[1]));
    for i in [0, 2, 3] {
        assert_eq!(
            world.get::<String>(entities[i]).unwrap(),
            &format!("row {i}")
        );
        assert_eq!(world.get::<u32>(entities[i]), Some(&(i as u32)));
    }
    assert_eq!(drops[1].load(Ordering::SeqCst), 1);
    drop(world);
    assert!(drops.iter().all(|count| count.load(Ordering::SeqCst) == 1));
}
