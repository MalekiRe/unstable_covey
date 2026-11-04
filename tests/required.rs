use static_ecs::prelude::*;

struct Update;
impl Schedule for Update {}

#[derive(Debug, Default, PartialEq)]
struct Pos(i32);
#[derive(Debug, Default, PartialEq)]
struct Vel(i32);
#[derive(Debug, Default, PartialEq)]
struct Mass(i32);
struct Bird;
impl Requires for Bird {
    type Required = (Pos, Vel);
}
// Recursive: a bird's velocity brings mass.
impl Requires for Vel {
    type Required = (Mass,);
}
// A cycle terminates: each side is added once.
#[derive(Default)]
struct Left;
#[derive(Default)]
struct Right;
impl Requires for Left {
    type Required = (Right,);
}
impl Requires for Right {
    type Required = (Left,);
}

/// Only `Bird` is named; `Pos`, `Vel`, and `Mass` are stored because it
/// requires them.
fn birds() -> World<impl Stores<Bird> + Stores<Pos> + Stores<Vel> + Stores<Mass>> {
    App::new().register::<Bird>().build().world
}

#[test]
fn spawning_adds_missing_required_components_recursively_keeping_given_values() {
    let mut world = birds();
    let plain = world.spawn_bundle((Bird,));
    assert_eq!(world.get::<Pos>(plain), Some(&Pos(0)));
    assert_eq!(world.get::<Vel>(plain), Some(&Vel(0)));
    assert_eq!(world.get::<Mass>(plain), Some(&Mass(0)));
    let given = world.spawn_bundle((Vel(5), Bird));
    assert_eq!(world.get::<Vel>(given), Some(&Vel(5)));
    assert_eq!(world.get::<Mass>(given), Some(&Mass(0)));
    let batch = world.spawn_batch((0..3).map(|i| (Bird, Pos(i))));
    for (i, &bird) in batch.iter().enumerate() {
        assert_eq!(world.get::<Pos>(bird), Some(&Pos(i as i32)));
        assert_eq!(world.get::<Mass>(bird), Some(&Mass(0)));
    }
    // One table holds every bird spawned without values, with or without.
    assert_eq!(
        world
            .query::<(&Bird, &Pos, &Vel, &Mass)>()
            .iter_mut()
            .count(),
        5
    );
}

#[test]
fn inserting_keeps_components_the_entity_already_has_per_source_table() {
    let mut world = birds();
    let fresh = world.spawn_bundle((Mass(1),));
    let moving = world.spawn_bundle((Mass(2), Pos(7)));
    world.insert(fresh, Bird);
    world.insert(moving, Bird);
    assert_eq!(world.get::<Mass>(fresh), Some(&Mass(1)));
    assert_eq!(world.get::<Pos>(fresh), Some(&Pos(0)));
    assert_eq!(world.get::<Pos>(moving), Some(&Pos(7)));
    assert_eq!(world.get::<Vel>(moving), Some(&Vel(0)));
    // Reinserting changes nothing else; removing a required one is allowed.
    world.insert(moving, Pos(8));
    world.insert(moving, Bird);
    assert_eq!(world.get::<Pos>(moving), Some(&Pos(8)));
    assert_eq!(world.remove::<Vel>(moving), Some(Vel(0)));
    assert!(world.get::<Bird>(moving).is_some());
}

#[test]
fn cycles_add_each_member_once() {
    let mut world = App::new().register::<Left>().build().world;
    let e = world.spawn_bundle((Left,));
    assert!(world.get::<Right>(e).is_some());
    let e = world.spawn_bundle((Right,));
    assert!(world.get::<Left>(e).is_some());
}

#[derive(Default)]
struct Log(Vec<&'static str>);

#[test]
fn required_components_fire_add_then_insert_hooks_only_when_added() {
    let mut app = App::new()
        .add_systems(OnAdd::<Bird>, |_: Trigger, mut log: Single<&mut Log>| {
            log.0.push("add bird")
        })
        .add_systems(OnAdd::<Pos>, |_: Trigger, mut log: Single<&mut Log>| {
            log.0.push("add pos")
        })
        .add_systems(OnInsert::<Bird>, |_: Trigger, mut log: Single<&mut Log>| {
            log.0.push("insert bird")
        })
        .add_systems(OnInsert::<Mass>, |t: Trigger, mut log: Single<&mut Log>| {
            log.0.push(if t.len() == 2 {
                "insert 2 mass"
            } else {
                "insert mass"
            })
        })
        .build();
    let log = app.world.spawn_bundle((Log::default(),));
    app.world.spawn_bundle((Bird,));
    let placed = app.world.spawn_bundle((Pos(1), Mass(1)));
    app.world.insert(placed, Bird);
    app.world.spawn_batch([(Bird,), (Bird,)]);
    assert_eq!(
        app.world.get::<Log>(log).unwrap().0,
        [
            "add bird",
            "add pos",
            "insert bird",
            "insert mass",
            // Spawned with Pos and Mass: their hooks ran then, not again.
            "add pos",
            "insert mass",
            "add bird",
            "insert bird",
            "add bird",
            "add pos",
            "insert bird",
            "insert 2 mass",
        ]
    );
}

#[test]
fn commands_spawn_and_insert_with_requirements() {
    let mut app = App::new()
        .add_systems(
            Update,
            |mut commands: Commands<_>, mut done: Single<&mut Vec<Entity>>| {
                let spawned = commands.spawn((Bird,));
                let target = commands.spawn((Vel(3),));
                commands.insert(target, (Bird,));
                done.extend([spawned, target]);
            },
        )
        .register::<Bird>()
        .build();
    let done = app.world.spawn_bundle((Vec::<Entity>::new(),));
    app.emit(&Update);
    let &[spawned, target] = app.world.get::<Vec<Entity>>(done).unwrap().as_slice() else {
        panic!("two entities");
    };
    assert_eq!(app.world.get::<Pos>(spawned), Some(&Pos(0)));
    assert_eq!(app.world.get::<Vel>(target), Some(&Vel(3)));
    assert_eq!(app.world.get::<Mass>(target), Some(&Mass(0)));
}

struct Flock;
impl Plugin for Flock {
    type Components = (Bird,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        schedule(Update, |birds: Query<&Bird>| {
            let _ = birds;
        })
    }
}

#[test]
fn plugin_components_bring_their_requirements() {
    let mut world = App::new().add_plugins(Flock).build().world;
    let e = world.spawn_bundle((Bird,));
    assert_eq!(world.get::<Mass>(e), Some(&Mass(0)));
}
