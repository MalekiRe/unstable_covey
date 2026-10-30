use static_ecs::prelude::*;
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug, PartialEq)]
struct Tag(u32);
struct Update;

#[test]
fn remaining_cardinality_and_count_span_archetypes() {
    type Schema = (Components<u32>, (Components<Tag>, BundleSlot<(u32,)>));
    let mut world = World::<Schema>::default();
    let entities = world.spawn_batch((0_u32..12).map(|x| (x,)));
    for &entity in &entities[3..7] {
        world.insert(entity, Tag(1));
    }
    let mut query = world.query::<&u32>();
    let mut iter = query.iter_mut();
    assert_eq!(iter.size_hint(), (12, Some(12)));
    let first = *iter.next().unwrap();
    assert!(first < 12);
    assert_eq!(iter.size_hint(), (11, Some(11)));
    assert_eq!(iter.count(), 11);
    let mut iter = query.iter_mut();
    while iter.next().is_some() {}
    assert_eq!(iter.size_hint(), (0, Some(0)));
    assert_eq!(iter.count(), 0);
    assert_eq!(
        world
            .query_filtered::<Entity, Without<Tag>>()
            .iter_mut()
            .count(),
        8
    );
}

#[test]
fn sparse_toggles_preserve_dense_rows_and_generations_reject_stale_handles() {
    type Schema = (Components<u64>, (Sparse<Tag>, BundleSlot<(u64,)>));
    let mut world = World::<Schema>::default();
    let entities = world.spawn_batch((0_u64..32).map(|x| (x,)));
    let addresses: Vec<_> = entities
        .iter()
        .map(|&e| world.get::<u64>(e).unwrap() as *const u64)
        .collect();
    for (i, &entity) in entities.iter().enumerate() {
        world.insert(entity, Tag(i as u32));
    }
    for (i, &entity) in entities.iter().enumerate() {
        assert_eq!(
            world.get::<u64>(entity).unwrap() as *const u64,
            addresses[i]
        );
        assert_eq!(world.remove::<Tag>(entity), Some(Tag(i as u32)));
        assert_eq!(
            world.get::<u64>(entity).unwrap() as *const u64,
            addresses[i]
        );
    }
    let stale = entities[7];
    assert!(world.despawn(stale));
    let fresh = world.spawn_bundle((100_u64,));
    assert_ne!(stale, fresh);
    assert!(!world.contains(stale));
    assert_eq!(world.get::<u64>(stale), None);
    assert_eq!(world.get::<u64>(fresh), Some(&100));
    let mut other = World::<Schema>::default();
    let foreign = other.spawn_bundle((200_u64,));
    assert!(!world.contains(foreign));
    assert!(!other.contains(fresh));
    let mut values: Vec<_> = world.query::<&u64>().iter_mut().copied().collect();
    values.sort_unstable();
    let mut expected: Vec<_> = (0..32).filter(|&x| x != 7).collect();
    expected.push(100);
    assert_eq!(values, expected);
}

#[test]
fn bundle_replacement_and_removal_preserve_unrelated_sparse_values() {
    type Schema = (
        (Components<String>, Components<u32>),
        (
            Sparse<Tag>,
            (BundleSlot<(String, Tag)>, BundleSlot<(String, u32)>),
        ),
    );
    let mut world = World::<Schema>::default();
    let entity = world.spawn_bundle((String::from("before"), Tag(9)));
    assert_eq!(
        world.insert_bundle(entity, (String::from("after"), 42_u32)),
        (Some(String::from("before")), None)
    );
    assert_eq!(world.get::<Tag>(entity), Some(&Tag(9)));
    assert_eq!(
        world.remove_bundle::<(String, u32)>(entity),
        (Some(String::from("after")), Some(42))
    );
    assert_eq!(world.remove_bundle::<(String, u32)>(entity), (None, None));
    assert_eq!(world.get::<Tag>(entity), Some(&Tag(9)));
    assert!(world.despawn(entity));
    assert_eq!(world.remove_bundle::<(String, Tag)>(entity), (None, None));
}

#[test]
fn summary_windows_survive_sparse_membership_changes_and_world_replacement() {
    type Schema = (
        Components<u32, true>,
        (Sparse<Tag, true>, BundleSlot<(u32,)>),
    );
    let mut world = World::<Schema>::default();
    let entities = world.spawn_batch((0_u32..6).map(|x| (x,)));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let output = seen.clone();
    let mut app = App::new(world)
        .with_execution_mode(ExecutionMode::Serial)
        .add_systems(Update, move |mut q: Query<&u32, Changed<u32>>| {
            let (lower, upper) = q.iter_mut().size_hint();
            let mut values: Vec<_> = q.iter_mut().copied().collect();
            values.sort_unstable();
            assert!(lower <= values.len() && upper.unwrap() >= values.len());
            output.borrow_mut().push(values);
        });
    app.emit(&Update);
    app.emit(&Update);
    app.world.insert(entities[1], Tag(1));
    *app.world.get_mut::<u32>(entities[3]).unwrap() = 30;
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(
        &*seen.borrow(),
        &[vec![0, 1, 2, 3, 4, 5], vec![], vec![30], vec![]]
    );
    assert!(matches!(
        app.world.query::<&u32>().contiguous_mut(),
        Err(ContiguousError::Fragmented)
    ));
    let mut replacement = World::<Schema>::default();
    replacement.spawn_bundle((99_u32,));
    app.world = replacement;
    app.emit(&Update);
    assert_eq!(seen.borrow().last().unwrap(), &vec![99]);
}

#[test]
fn tracked_chunks_only_mark_mutably_accessed_rows() {
    type Schema = (Components<u32, true>, BundleSlot<(u32,)>);
    let mut world = World::<Schema>::default();
    world.spawn_batch((0_u32..5).map(|x| (x,)));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let output = seen.clone();
    let mut app = App::new(world)
        .with_execution_mode(ExecutionMode::Serial)
        .add_systems(Update, move |mut q: Query<&u32, Changed<u32>>| {
            output
                .borrow_mut()
                .push(q.iter_mut().copied().collect::<Vec<_>>());
        });
    app.emit(&Update);
    {
        let mut query = app.world.query::<(Entity, &mut u32)>();
        let (entities, mut values) = query.contiguous_mut().unwrap().next().unwrap();
        assert_eq!(entities.len(), 5);
        assert_eq!(&*values, &[0, 1, 2, 3, 4]);
        *values.get_mut(2).unwrap() = 20;
    }
    app.emit(&Update);
    assert_eq!(seen.borrow()[1], vec![20]);
    {
        let mut query = app.world.query::<&mut u32>();
        for mut values in query.contiguous_mut().unwrap() {
            values.as_mut_slice().fill(7);
        }
    }
    app.emit(&Update);
    assert_eq!(seen.borrow()[2], vec![7; 5]);
    assert!(matches!(
        app.world
            .query_filtered::<&u32, Changed<u32>>()
            .contiguous_mut(),
        Err(ContiguousError::RowFilter)
    ));
}

#[test]
fn sparse_and_dense_destructors_finish_after_unwind() {
    struct Owned(Arc<AtomicUsize>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Bomb;
    impl Drop for Bomb {
        fn drop(&mut self) {
            panic!("drop");
        }
    }
    type Schema = ((Components<Bomb>, Sparse<Owned>), BundleSlot<(Bomb, Owned)>);
    let count = Arc::new(AtomicUsize::new(0));
    let mut world = World::<Schema>::default();
    let entity = world.spawn_bundle((Bomb, Owned(count.clone())));
    assert!(catch_unwind(AssertUnwindSafe(|| world.despawn(entity))).is_err());
    assert!(!world.contains(entity));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let fresh = world.spawn();
    assert!(world.contains(fresh));
    assert!(!world.contains(entity));
    drop(world);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn mixed_or_filters_do_not_count_structurally_missing_branches() {
    type Schema = (Components<u32, true>, (Sparse<Tag>, BundleSlot<(u32,)>));
    type Filter = Or<(With<Tag>, Changed<u32>)>;
    let mut world = World::<Schema>::default();
    let entities = world.spawn_batch((0_u32..6).map(|x| (x,)));
    world.insert(entities[1], Tag(1));
    world.insert(entities[4], Tag(4));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let output = seen.clone();
    let mut app = App::new(world)
        .with_execution_mode(ExecutionMode::Serial)
        .add_systems(Update, move |mut q: Query<&u32, Filter>| {
            let (lower, upper) = q.iter_mut().size_hint();
            let count = q.iter_mut().count();
            assert!(lower <= count && upper.unwrap() >= count);
            output.borrow_mut().push(count);
        });
    app.emit(&Update);
    app.emit(&Update);
    *app.world.get_mut::<u32>(entities[2]).unwrap() = 20;
    app.emit(&Update);
    assert_eq!(&*seen.borrow(), &[6, 2, 3]);
}
