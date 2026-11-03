use static_ecs::prelude::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Update;
struct Spawner;
#[derive(Debug, PartialEq)]
struct Value(u32);
struct Link(Entity);

#[test]
fn reserved_entities_are_usable_by_later_commands_and_apply_after_the_emit() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut commands: Commands<_>, spawner: Single<&Spawner>| {
                    let _ = spawner;
                    let child = commands.spawn((Value(1),));
                    let parent = commands.spawn((Value(2),));
                    commands.insert(child, (Link(parent),));
                },
                // Runs in the same emit: the spawns are not applied yet.
                |values: Query<&Value>, mut seen: Single<&mut Vec<usize>>| {
                    let mut values = values;
                    seen.push(values.iter_mut().count());
                },
            )
                .chain(),
        )
        .register::<Link>()
        .build();
    app.world.spawn_bundle((Spawner, Vec::<usize>::new()));
    app.emit(&Update);
    let mut links: Vec<_> = app
        .world
        .query::<(Entity, &Link)>()
        .iter_mut()
        .map(|(child, link)| (child, link.0))
        .collect();
    assert_eq!(links.len(), 1);
    let (child, parent) = links.pop().unwrap();
    assert_eq!(app.world.get::<Value>(child), Some(&Value(1)));
    assert_eq!(app.world.get::<Value>(parent), Some(&Value(2)));
    app.emit(&Update);
    let seen = app
        .world
        .query::<&Vec<usize>>()
        .iter_mut()
        .next()
        .unwrap()
        .clone();
    assert_eq!(seen, [0, 2]);
}

#[test]
fn parallel_systems_reserve_distinct_entities_and_apply_in_system_order() {
    static ORDER: AtomicUsize = AtomicUsize::new(0);
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut commands: Commands<_>| {
                    for i in 0..100 {
                        commands.spawn((Value(i),));
                    }
                    commands.queue(|_| assert_eq!(ORDER.fetch_add(1, Ordering::Relaxed), 0));
                },
                |mut commands: Commands<_>| {
                    for i in 100..200 {
                        commands.spawn((Value(i),));
                    }
                    commands.queue(|_| assert_eq!(ORDER.fetch_add(1, Ordering::Relaxed), 1));
                },
            )
                .parallel(),
        )
        .register::<Value>()
        .build();
    WorkerPool::new(2).install(|| app.emit(&Update));
    let mut values: Vec<_> = app
        .world
        .query::<&Value>()
        .iter_mut()
        .map(|v| v.0)
        .collect();
    values.sort_unstable();
    assert_eq!(values, (0..200).collect::<Vec<_>>());
    assert_eq!(ORDER.load(Ordering::Relaxed), 2);
    // Freed and reserved slots coexist with direct spawns afterwards.
    let direct = app.world.spawn_bundle((Value(999),));
    assert_eq!(app.world.get::<Value>(direct), Some(&Value(999)));
}

#[test]
fn commands_on_despawned_entities_do_nothing() {
    let mut app = App::new()
        .add_systems(
            Update,
            |mut commands: Commands<_>, query: Query<(Entity, &Value)>| {
                let mut query = query;
                for (entity, _) in query.iter_mut() {
                    commands.despawn(entity);
                    commands.insert(entity, (Link(entity),));
                    commands.remove::<(Value,)>(entity);
                }
            },
        )
        .register::<Link>()
        .build();
    let entity = app.world.spawn_bundle((Value(1),));
    app.emit(&Update);
    assert!(!app.world.contains(entity));
    assert_eq!(app.world.query::<&Link>().iter_mut().count(), 0);
}

#[test]
fn a_panicking_command_drops_every_remaining_command_exactly_once() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }
    let mut system = static_ecs::IntoSystems::into_systems(|mut commands: Commands<_>| {
        let first = Counted;
        commands.queue(move |_| drop(first));
        commands.queue(|_| panic!("command failed"));
        let (second, third) = (Counted, Counted);
        commands.queue(move |_| drop(second));
        commands.queue(move |_| drop(third));
    });
    let mut world = World::<()>::default();
    let result = catch_unwind(AssertUnwindSafe(|| {
        static_ecs::System::run(&mut system, &mut world)
    }));
    assert!(result.is_err());
    assert_eq!(DROPS.load(Ordering::Relaxed), 3);
    drop(system);
    assert_eq!(DROPS.load(Ordering::Relaxed), 3);
}
