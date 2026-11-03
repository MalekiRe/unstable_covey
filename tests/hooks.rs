use static_ecs::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Health(u32);
struct Marker;
#[derive(Default)]
struct Log(Vec<String>);

fn value_of(query: &mut Query<(Entity, &Health)>, entity: Entity) -> u32 {
    query.get_mut(entity).unwrap().1.0
}

#[test]
fn hooks_compose_in_registration_order_and_distinguish_add_insert_and_replace() {
    let mut app = App::new()
        .on_add::<Health, _, _>(|trigger: Trigger, mut log: Single<&mut Log>| {
            log.0.push(format!("add {}", trigger.len()));
        })
        .on_insert::<Health, _, _>(
            |trigger: Trigger, mut health: Query<(Entity, &Health)>, mut log: Single<&mut Log>| {
                let value = value_of(&mut health, trigger[0]);
                log.0.push(format!("insert {value}"));
            },
        )
        .on_insert::<Health, _, _>(|mut log: Single<&mut Log>| log.0.push("insert again".into()))
        .on_replace::<Health, _, _>(
            |trigger: Trigger, mut health: Query<(Entity, &Health)>, mut log: Single<&mut Log>| {
                let value = value_of(&mut health, trigger[0]);
                log.0.push(format!("replace {value}"));
            },
        )
        .build();
    let log = app.world.spawn_bundle((Log::default(),));
    let entity = app.world.spawn_bundle((Health(1),));
    app.world.insert(entity, Health(2));
    assert_eq!(
        app.world.get::<Log>(log).unwrap().0,
        [
            "add 1",
            "insert 1",
            "insert again",
            "replace 1",
            "insert 2",
            "insert again"
        ]
    );
}

#[test]
fn removal_and_despawn_hooks_see_the_value_before_it_goes() {
    let mut app = App::new()
        .on_remove::<Health, _, _>(
            |trigger: Trigger, mut health: Query<(Entity, &Health)>, mut log: Single<&mut Log>| {
                for &entity in trigger.iter() {
                    let value = value_of(&mut health, entity);
                    log.0.push(format!("remove {value}"));
                }
            },
        )
        .register::<Marker>()
        .build();
    let log = app.world.spawn_bundle((Log::default(),));
    let a = app.world.spawn_bundle((Health(3),));
    let b = app.world.spawn_bundle((Health(4), Marker));
    let plain = app.world.spawn_bundle((Marker,));
    assert_eq!(app.world.remove::<Health>(a), Some(Health(3)));
    assert_eq!(app.world.remove::<Health>(a), None); // absent: no hook
    assert!(app.world.despawn(b));
    assert!(app.world.despawn(plain)); // no Health: fires nothing
    assert_eq!(
        app.world.get::<Log>(log).unwrap().0,
        ["remove 3", "remove 4"]
    );
}

#[test]
fn batch_spawns_fire_each_hook_once_with_every_entity() {
    let mut app = App::new()
        .on_add::<Health, _, _>(|trigger: Trigger, mut log: Single<&mut Log>| {
            log.0.push(format!("add {}", trigger.len()));
        })
        .build();
    let log = app.world.spawn_bundle((Log::default(),));
    let spawned = app.world.spawn_batch((0..100).map(|i| (Health(i),)));
    assert_eq!(spawned.len(), 100);
    assert_eq!(app.world.get::<Log>(log).unwrap().0, ["add 100"]);
}

#[test]
fn hook_commands_apply_before_the_structural_operation_returns() {
    struct Update;
    let mut app = App::new()
        .on_add::<Health, _, _>(|trigger: Trigger, mut commands: Commands<_>| {
            for &entity in trigger.iter() {
                commands.insert(entity, (Marker,));
            }
        })
        // A hook reacting to the first hook's commands, in the same flush.
        .on_add::<Marker, _, _>(|trigger: Trigger, mut log: Single<&mut Log>| {
            log.0.push(format!("marked {}", trigger.len()));
        })
        .add_systems(Update, |mut commands: Commands<_>| {
            commands.spawn((Health(9),));
        })
        .build();
    let log = app.world.spawn_bundle((Log::default(),));
    let direct = app.world.spawn_bundle((Health(1),));
    assert!(app.world.get::<Marker>(direct).is_some());
    app.emit(&Update);
    let mut marked: Vec<_> = app
        .world
        .query::<(&Health, &Marker)>()
        .iter_mut()
        .map(|(health, _)| health.0)
        .collect();
    marked.sort_unstable();
    assert_eq!(marked, [1, 9]);
    assert_eq!(
        app.world.get::<Log>(log).unwrap().0,
        ["marked 1", "marked 1"]
    );
}

#[test]
fn worlds_without_hooks_fire_nothing_and_keep_working() {
    let mut world = World::<(Components<Health>, Components<Marker>)>::default();
    let entity = world.spawn_bundle((Health(1),));
    world.insert(entity, Marker);
    world.remove::<Health>(entity);
    assert!(world.despawn(entity));
}
