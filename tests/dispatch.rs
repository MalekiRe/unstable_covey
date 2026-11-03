use static_ecs::{
    App, Bus, Entity, Has, Query, Single, Sparse, With, Without, World, exclusive, subscribe,
};
use std::cell::RefCell;

mod first {
    pub struct Event(pub u32);
}
mod second {
    pub struct Event(pub u32);
}
struct Unhandled;

#[test]
fn specialization_filters_types_preserving_order_and_captures() {
    let log = RefCell::new(Vec::new());
    let mut count = 0;
    let mut bus = Bus::new().install((
        subscribe(|e: &first::Event| {
            count += 1;
            log.borrow_mut().push(("first", e.0 * count));
        }),
        (
            subscribe(|e: &second::Event| log.borrow_mut().push(("other", e.0))),
            subscribe(|e: &first::Event| log.borrow_mut().push(("last", e.0))),
        ),
    ));
    bus.emit(&first::Event(3));
    bus.emit(&Unhandled);
    bus.emit(&second::Event(7));
    bus.emit(&first::Event(5));
    assert_eq!(
        *log.borrow(),
        vec![
            ("first", 3),
            ("last", 3),
            ("other", 7),
            ("first", 10),
            ("last", 5)
        ]
    );
}

#[test]
fn schedule_labels_and_event_payloads_select_only_matching_systems() {
    struct Update;
    struct Set(u32);
    let mut app = App::new()
        .add_systems(Update, |mut n: Single<&mut u32>| **n += 1)
        .on(|event: &Set, world: &mut World<_>| {
            for mut n in &mut world.query::<&mut u32>() {
                *n = event.0;
            }
        })
        .add_systems(Update, |mut n: Single<&mut u32>| **n *= 2)
        .build();
    let entity = app.world.spawn_bundle((0_u32,));
    app.emit(&Set(10));
    assert_eq!(app.world.get::<u32>(entity), Some(&10));
    app.emit(&Update);
    assert_eq!(app.world.get::<u32>(entity), Some(&22));
    app.emit(&Unhandled);
    assert_eq!(app.world.get::<u32>(entity), Some(&22));
    app.emit(&Update);
    assert_eq!(app.world.get::<u32>(entity), Some(&46));
}

/// A plugin's structural system, generic over whatever world the app infers.
fn tag_new<S: Has<u64> + Has<Tag>>() -> static_ecs::ExclusiveSystem<impl FnMut(&mut World<S>)> {
    exclusive(|world: &mut World<S>| {
        let untagged: Vec<_> = world
            .query_filtered::<Entity, (With<u64>, Without<Tag>)>()
            .iter_mut()
            .collect();
        for entity in untagged {
            world.insert(entity, Tag);
        }
    })
}
struct Tag;
struct Marker;

#[test]
fn the_world_stores_every_component_any_registration_uses_once() {
    struct Update;
    let mut app = App::new()
        .storage::<Sparse<Marker>>()
        // Queried by two systems, filtered by one, and inserted by a plugin.
        .add_systems(Update, |mut q: Query<&mut u64>| {
            q.iter_mut().for_each(|mut v| *v += 1)
        })
        .add_systems(Update, tag_new())
        .add_systems(
            Update,
            |mut q: Query<&u64, With<Tag>>, mut sum: Single<&mut u32>| {
                **sum = q.iter_mut().count() as u32;
            },
        )
        .register::<Tag>()
        .register::<Marker>()
        .register::<u64>()
        .build();
    let total = app.world.spawn_bundle((0_u32,));
    let a = app.world.spawn_bundle((10_u64,));
    app.emit(&Update);
    let b = app.world.spawn_bundle((20_u64, Marker));
    app.emit(&Update);
    assert_eq!(app.world.get::<u64>(a), Some(&12));
    assert_eq!(app.world.get::<u64>(b), Some(&21));
    assert!(app.world.get::<Tag>(a).is_some() && app.world.get::<Tag>(b).is_some());
    assert_eq!(app.world.get::<u32>(total), Some(&2));
    // Sparse membership leaves the dense u64 row in place.
    assert!(app.world.remove::<Marker>(b).is_some());
    assert_eq!(app.world.get::<u64>(b), Some(&21));
}
