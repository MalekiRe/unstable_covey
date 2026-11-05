use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use static_ecs::prelude::*;

struct Locomotion;
struct Life;

#[derive(Debug, Default, PartialEq)]
struct Walking(u32);
#[derive(Debug, PartialEq)]
struct Running(u32);
#[derive(Debug, PartialEq)]
struct Dead;
#[derive(Debug, PartialEq)]
struct Pos(i32);
impl OneOf for Walking {
    type Groups = (Locomotion, Life);
}
impl OneOf for Running {
    type Groups = (Locomotion,);
}
impl OneOf for Dead {
    type Groups = (Life,);
}

type Movers = (
    (Components<Walking>, Components<Running>),
    (Components<Dead>, Components<Pos>),
);

#[test]
fn inserting_a_member_removes_the_members_of_each_of_its_groups() {
    let mut world = World::<Movers>::default();
    // Running and Dead share no group, so they coexist.
    let e = world.spawn_bundle((Running(1), Dead, Pos(7)));
    assert_eq!(world.insert(e, Walking(2)), None);
    assert_eq!(world.get::<Walking>(e), Some(&Walking(2)));
    assert_eq!(world.get::<Running>(e), None);
    assert_eq!(world.get::<Dead>(e), None);
    assert_eq!(world.get::<Pos>(e), Some(&Pos(7)));
    // Dead shares only Life with Walking.
    world.insert(e, Dead);
    assert_eq!(world.get::<Walking>(e), None);
    world.insert(e, Running(3));
    assert_eq!(world.get::<Dead>(e), Some(&Dead));
    // Reinserting a member replaces it and removes nothing.
    assert_eq!(world.insert(e, Running(4)), Some(Running(3)));
    assert_eq!(world.get::<Dead>(e), Some(&Dead));
    // Queries see each entity once, in its new table only.
    let other = world.spawn_bundle((Running(5),));
    world.insert(other, Walking(6));
    let mut walking: Vec<_> = world.query::<&Walking>().iter_mut().map(|w| w.0).collect();
    walking.sort_unstable();
    assert_eq!(walking, [6]);
    assert_eq!(world.query::<(&Running, &Dead)>().iter_mut().count(), 1);
    assert_eq!(world.query::<&Running>().iter_mut().count(), 1);
}

#[test]
fn bundles_remove_every_member_their_members_exclude() {
    let mut world = World::<Movers>::default();
    let e = world.spawn_bundle((Walking(1),));
    // Running removes Walking; Dead, sharing no group with Running, stays.
    assert_eq!(world.insert_bundle(e, (Running(2), Dead)), (None, None));
    assert_eq!(world.get::<Walking>(e), None);
    assert_eq!(world.get::<Running>(e), Some(&Running(2)));
    assert_eq!(world.get::<Dead>(e), Some(&Dead));
}

struct Counted(Arc<AtomicUsize>);
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Slot;
struct A(#[expect(dead_code, reason = "held for its drop")] Counted);
struct B(#[expect(dead_code, reason = "held for its drop")] Counted);
impl OneOf for A {
    type Groups = (Slot,);
}
impl OneOf for B {
    type Groups = (Slot,);
}

#[test]
fn removed_values_drop_once_from_dense_and_sparse_columns() {
    let drops = Arc::new(AtomicUsize::new(0));
    let counted = || Counted(drops.clone());
    let mut world = World::<((Components<A>, Sparse<B>), Components<u32>)>::default();
    let entities: Vec<_> = (0..4)
        .map(|i| world.spawn_bundle((A(counted()), i as u32)))
        .collect();
    // Dense A to sparse B and back, every row of a shared table.
    for &e in &entities {
        world.insert(e, B(counted()));
    }
    assert_eq!(drops.load(Ordering::SeqCst), 4);
    world.insert(entities[1], A(counted()));
    assert_eq!(drops.load(Ordering::SeqCst), 5);
    for (i, &e) in entities.iter().enumerate() {
        assert_eq!(world.get::<u32>(e), Some(&(i as u32)));
        assert_eq!(world.get::<A>(e).is_some(), i == 1);
        assert_eq!(world.get::<B>(e).is_some(), i != 1);
    }
    drop(world);
    assert_eq!(drops.load(Ordering::SeqCst), 9);
}

struct Bomb(Arc<AtomicUsize>);
impl Drop for Bomb {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("bomb");
    }
}
struct Safe;
impl OneOf for Bomb {
    type Groups = (Slot,);
}
impl OneOf for Safe {
    type Groups = (Slot,);
}

#[test]
fn a_panicking_drop_still_completes_the_move() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::<((Components<Bomb>, Components<Safe>), Components<u32>)>::default();
    let e = world.spawn_bundle((Bomb(drops.clone()), 1_u32));
    let other = world.spawn_bundle((Bomb(drops.clone()), 2_u32));
    std::mem::forget(world.remove::<Bomb>(other));
    assert!(catch_unwind(AssertUnwindSafe(|| world.insert(e, Safe))).is_err());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(world.get::<Bomb>(e).is_none() && world.get::<Safe>(e).is_some());
    assert_eq!(world.get::<u32>(e), Some(&1));
    assert_eq!(world.get::<u32>(other), Some(&2));
    // The bomb left the world: dropping it does not drop the value again.
    drop(world);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[derive(Default)]
struct Log(Vec<String>);

#[test]
fn removed_members_fire_replace_and_remove_with_old_values_before_add() {
    let mut app = App::new()
        .add_systems(
            OnReplace::<Running>,
            |t: Trigger, mut q: Query<&Running>, mut log: ResMut<Log>| {
                log.0
                    .push(format!("replace running {}", q.get_mut(t[0]).unwrap().0))
            },
        )
        .add_systems(
            OnRemove::<Running>,
            |t: Trigger, mut q: Query<&Running>, mut log: ResMut<Log>| {
                log.0
                    .push(format!("remove running {}", q.get_mut(t[0]).unwrap().0))
            },
        )
        .add_systems(OnRemove::<Dead>, |_: Trigger, mut log: ResMut<Log>| {
            log.0.push("remove dead".into())
        })
        .add_systems(
            OnAdd::<Walking>,
            |t: Trigger, mut q: Query<(Has<Running>, Has<Dead>)>, mut log: ResMut<Log>| {
                log.0
                    .push(format!("add walking {:?}", q.get_mut(t[0]).unwrap()))
            },
        )
        .add_systems(OnInsert::<Walking>, |_: Trigger, mut log: ResMut<Log>| {
            log.0.push("insert walking".into())
        })
        .init_resource::<Log>()
        .build();
    let e = app.world.spawn_bundle((Running(3), Dead));
    app.world.insert(e, Walking(1));
    assert_eq!(
        app.world.resource::<Log>().unwrap().0,
        [
            "replace running 3",
            "remove running 3",
            "remove dead",
            "add walking (false, false)",
            "insert walking",
        ]
    );
}

struct Update;
impl Schedule for Update {}

#[test]
fn sibling_filters_are_disjoint_so_their_writers_run_together() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                // Both borrows live at once: no entity matches both filters.
                |mut walkers: Query<&mut u32, With<Walking>>,
                 mut runners: Query<&mut u32, With<Running>>| {
                    let mut walker = walkers.iter_mut().next().unwrap();
                    let mut runner = runners.iter_mut().next().unwrap();
                    *walker += 1;
                    *runner += *walker;
                },
                (
                    |mut q: Query<&mut u32, With<Walking>>| q.iter_mut().for_each(|mut n| *n *= 10),
                    |mut q: Query<&mut u32, (With<Running>, Without<Dead>)>| {
                        q.iter_mut().for_each(|mut n| *n *= 100)
                    },
                )
                    .parallel(),
            )
                .chain(),
        )
        .build();
    let walker = app.world.spawn_bundle((Walking(0), 1_u32));
    let runner = app.world.spawn_bundle((Running(0), 5_u32));
    app.emit(&Update);
    assert_eq!(app.world.get::<u32>(walker), Some(&20));
    assert_eq!(app.world.get::<u32>(runner), Some(&700));
}

/// A rider walks: its required Walking removes Running.
#[derive(Default)]
struct Rider;
impl Requires for Rider {
    type Required = (Walking,);
}

#[test]
fn required_members_remove_their_siblings() {
    let mut world = App::new()
        .register::<Rider>()
        .register::<Running>()
        .register::<Dead>()
        .build()
        .world;
    let e = world.spawn_bundle((Running(1),));
    world.insert(e, Rider);
    assert_eq!(world.get::<Walking>(e), Some(&Walking(0)));
    assert_eq!(world.get::<Running>(e), None);
}

#[test]
#[should_panic(
    expected = "would remove `one_of::Walking`, which the insert requires: they share a `OneOf` group"
)]
fn a_member_excluding_a_requirement_panics() {
    let mut world = App::new()
        .register::<Rider>()
        .register::<Running>()
        .register::<Dead>()
        .build()
        .world;
    world.spawn_bundle((Rider, Running(1)));
}
