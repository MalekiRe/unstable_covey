use static_ecs::prelude::*;
use std::cell::RefCell;

#[derive(Debug, PartialEq)]
struct Value(i32);
struct Tag;
struct Gate;
type Schema = (
    (Components<Value>, (Components<Tag>, Components<Gate>)),
    (
        (BundleSlot<(Value,)>, BundleSlot<(Value, Tag)>),
        (BundleSlot<(Tag,)>, BundleSlot<(Gate,)>),
    ),
);

#[test]
fn reads_replacements_migration_and_reinsertion_preserve_change_semantics() {
    let mut world = World::<Schema>::default();
    let a = world.spawn_bundle((Value(1), Tag));
    let b = world.spawn_bundle((Value(2),));
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems(
        (),
        |mut added: Query<&Value, Added<Value>>, mut changed: Query<&Value, Changed<Value>>| {
            let mut a: Vec<_> = added.iter_mut().map(|v| v.0).collect();
            let mut c: Vec<_> = changed.iter_mut().map(|v| v.0).collect();
            a.sort_unstable();
            c.sort_unstable();
            log.borrow_mut().push((a, c));
        },
    );
    app.emit(&());
    // Merely fetching mutable data must not dirty it.
    assert_eq!(app.world.get_mut::<Value>(a).unwrap().0, 1);
    for value in &mut app.world.query::<&mut Value>() {
        assert!(value.0 > 0);
    }
    app.world.remove::<Tag>(a);
    app.world.insert(b, Tag);
    app.emit(&());
    app.world.get_mut::<Value>(a).unwrap().0 = 3;
    assert_eq!(app.world.insert(b, Value(4)), Some(Value(2)));
    app.emit(&());
    app.world.remove::<Value>(a);
    app.world.insert(a, Value(5));
    app.world.despawn(b); // swap-removal must keep timestamps aligned.
    app.emit(&());
    assert_eq!(
        *log.borrow(),
        vec![
            (vec![1, 2], vec![1, 2]),
            (vec![], vec![]),
            (vec![], vec![3, 4]),
            (vec![5], vec![5]),
        ]
    );
}

#[test]
fn skipped_single_and_conditions_do_not_consume_pending_changes() {
    let mut world = World::<Schema>::default();
    let value = world.spawn_bundle((Value(1),));
    let gate = world.spawn();
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems(
        (),
        (|mut q: Query<&Value, Changed<Value>>, _: Single<&Gate>| {
            log.borrow_mut()
                .push(q.iter_mut().map(|v| v.0).collect::<Vec<_>>());
        })
        .run_if(|mut q: Query<&Tag>| q.iter_mut().next().is_some()),
    );
    app.emit(&());
    app.world.insert(gate, Tag);
    app.emit(&()); // condition true, invalid Single
    app.world.get_mut::<Value>(value).unwrap().0 = 9;
    app.world.insert(gate, Gate);
    app.emit(&());
    app.emit(&());
    assert_eq!(*log.borrow(), vec![vec![9], vec![]]);
}

#[test]
fn row_filters_compose_and_single_counts_only_matching_rows() {
    type ChangedOrTagged = Or<(Changed<Value>, With<Tag>)>;
    let mut world = World::<Schema>::default();
    let a = world.spawn_bundle((Value(1), Tag));
    world.spawn_bundle((Value(2),));
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems(
        (),
        (
            |mut q: Query<&Value, ChangedOrTagged>| {
                let mut values: Vec<_> = q.iter_mut().map(|v| v.0).collect();
                values.sort_unstable();
                log.borrow_mut().push(values);
            },
            |mut value: Single<&mut Value, Changed<Value>>| {
                value.0 += 10;
            },
        )
            .chain(),
    );
    app.emit(&()); // two changed rows: Single skips
    app.world.get_mut::<Value>(a).unwrap().0 = 3;
    app.emit(&()); // still two changes since Single has never run
    assert_eq!(app.world.get::<Value>(a), Some(&Value(3)));
    assert_eq!(*log.borrow(), vec![vec![1, 2], vec![3]]);
}

#[test]
fn successful_changed_single_resumes_after_a_quiet_frame() {
    let mut world = World::<Schema>::default();
    let a = world.spawn_bundle((Value(1),));
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems((), |value: Single<&Value, Changed<Value>>| {
        log.borrow_mut().push(value.0);
    });
    app.emit(&());
    app.emit(&());
    app.world.get_mut::<Value>(a).unwrap().0 = 2;
    app.emit(&());
    assert_eq!(*log.borrow(), vec![1, 2]);
}
#[test]
fn replacing_the_world_resets_system_change_windows() {
    let mut world = World::<Schema>::default();
    world.spawn_bundle((Value(1),));
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems((), |mut values: Query<&Value, Added<Value>>| {
        log.borrow_mut().extend(values.iter_mut().map(|v| v.0));
    });
    for _ in 0..10 {
        app.emit(&());
    }
    let mut replacement = World::<Schema>::default();
    replacement.spawn_bundle((Tag,)); // Same table index now has a different signature.
    replacement.spawn_bundle((Value(2),));
    app.world = replacement;
    app.emit(&());
    assert_eq!(*log.borrow(), vec![1, 2]);
}

#[test]
fn cached_matches_follow_new_tables_growth_and_repopulated_empty_tables() {
    let mut world = World::<Schema>::default();
    let original = world.spawn_bundle((Value(1),));
    let log = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems((), |mut query: Query<&Value>| {
        let mut values = query.iter_mut().map(|v| v.0).collect::<Vec<_>>();
        values.sort_unstable();
        log.borrow_mut().push(values);
    });
    app.emit(&());
    app.world.despawn(original);
    app.emit(&());
    app.world.spawn_batch((2..34).map(|i| (Value(i),)));
    app.world.spawn_bundle((Value(34), Tag)); // Newly created matching archetype.
    app.world.spawn_bundle((Gate,)); // Newly created nonmatching archetype.
    app.emit(&());
    assert_eq!(
        *log.borrow(),
        vec![vec![1], vec![], (2..35).collect::<Vec<_>>()]
    );
}

#[test]
fn read_only_cursors_do_not_consume_subsequent_system_writes() {
    let mut world = World::<Schema>::default();
    world.spawn_bundle((Value(1),));
    let before = RefCell::new(Vec::new());
    let after = RefCell::new(Vec::new());
    let mut app = App::new(world).add_systems(
        (),
        (
            |mut q: Query<&Value, Changed<Value>>| {
                before
                    .borrow_mut()
                    .push(q.iter_mut().map(|v| v.0).collect::<Vec<_>>())
            },
            |mut value: Single<&mut Value>, _: Single<&Gate>| value.0 += 1,
            |mut q: Query<&Value, Changed<Value>>| {
                after
                    .borrow_mut()
                    .push(q.iter_mut().map(|v| v.0).collect::<Vec<_>>())
            },
        )
            .chain(),
    );
    app.emit(&());
    app.emit(&());
    let gate = app.world.spawn_bundle((Gate,));
    app.emit(&());
    app.emit(&());
    app.world.despawn(gate);
    app.emit(&());
    assert_eq!(
        *before.borrow(),
        vec![vec![1], vec![], vec![], vec![2], vec![3]]
    );
    assert_eq!(
        *after.borrow(),
        vec![vec![1], vec![], vec![2], vec![3], vec![]]
    );
}

#[test]
fn parallel_writers_are_seen_exactly_once_by_readers_before_and_after_the_join() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Update;
    let frames = 50;
    let (before, after) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (seen_before, seen_after) = (before.clone(), after.clone());
    WorkerPool::new(3).install(|| {
        let mut world = World::<(Components<u32>, Components<u64>)>::default();
        let entity = world.spawn();
        world.insert(entity, 0_u32);
        world.insert(entity, 0_u64);
        let mut app = App::new(world)
            .add_systems(Update, move |mut q: Query<&u32, Changed<u32>>| {
                seen_before.fetch_add(q.iter_mut().count(), Ordering::SeqCst);
            })
            .add_systems(
                Update,
                (
                    // The longer branch issues more ticks; the join must keep its clock.
                    (
                        |mut a: Single<&mut u32>| **a += 1,
                        |mut a: Single<&mut u32>| **a += 1,
                        |mut a: Single<&mut u32>| **a += 1,
                    ),
                    |mut b: Single<&mut u64>| **b += 1,
                )
                    .parallel(),
            )
            .add_systems(
                Update,
                move |mut a: Query<&u32, Changed<u32>>, mut b: Query<&u64, Changed<u64>>| {
                    seen_after.fetch_add(
                        a.iter_mut().count() + b.iter_mut().count(),
                        Ordering::SeqCst,
                    );
                },
            );
        for _ in 0..frames {
            app.emit(&Update);
        }
        assert_eq!(app.world.get::<u32>(entity), Some(&(3 * frames as u32)));
    });
    // The earlier reader sees the insertion, then each previous frame's write.
    assert_eq!(before.load(Ordering::SeqCst), frames);
    assert_eq!(after.load(Ordering::SeqCst), 2 * frames);
}
