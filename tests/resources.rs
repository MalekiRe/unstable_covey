use static_ecs::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Update;
impl Schedule for Update {}

#[derive(Debug, Default, PartialEq)]
struct Score(u32);
#[derive(Debug, Default, PartialEq)]
struct Speed(u32);
#[derive(Default)]
struct Log(Vec<String>);

#[test]
fn world_resources_insert_replace_read_write_and_remove() {
    let mut world = App::new().register::<Score>().build().world;
    assert_eq!(world.resource::<Score>(), None);
    assert_eq!(world.remove_resource::<Score>(), None);
    assert_eq!(world.insert_resource(Score(1)), None);
    assert_eq!(world.insert_resource(Score(2)), Some(Score(1)));
    world.resource_mut::<Score>().unwrap().0 += 1;
    assert_eq!(world.resource::<Score>(), Some(&Score(3)));
    world.init_resource::<Score>();
    assert_eq!(world.resource::<Score>(), Some(&Score(3)));
    assert_eq!(world.remove_resource::<Score>(), Some(Score(3)));
    world.init_resource::<Score>();
    assert_eq!(world.resource::<Score>(), Some(&Score(0)));
}

#[test]
fn queries_never_see_resources_even_with_identical_components() {
    let mut world = App::new()
        .add_systems(Update, |_: Query<(Entity, &Score)>| {})
        .build()
        .world;
    world.insert_resource(Score(1));
    let entity = world.spawn_bundle((Score(2),));
    let rows: Vec<_> = world
        .query::<(Entity, &Score)>()
        .iter_mut()
        .map(|(e, s)| (e, s.0))
        .collect();
    assert_eq!(rows, [(entity, 2)]);
    // Component-less queries scan every table; the resource table is skipped.
    assert_eq!(
        world.query::<Entity>().iter_mut().collect::<Vec<_>>(),
        [entity]
    );
    assert_eq!(world.resource::<Score>(), Some(&Score(1)));
}

#[test]
fn missing_resources_skip_systems_and_options_run_regardless() {
    static RAN: AtomicUsize = AtomicUsize::new(0);
    static SAW: AtomicUsize = AtomicUsize::new(0);
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |score: Res<Score>, mut speed: ResMut<Speed>| {
                    RAN.fetch_add(1, Ordering::Relaxed);
                    speed.0 += score.0;
                },
                |score: Option<Res<Score>>| {
                    SAW.fetch_add(score.map_or(100, |s| s.0 as usize), Ordering::Relaxed);
                },
            ),
        )
        .build();
    app.emit(&Update);
    assert_eq!(
        (RAN.load(Ordering::Relaxed), SAW.load(Ordering::Relaxed)),
        (0, 100)
    );
    app.world.insert_resource(Score(2));
    app.emit(&Update);
    // Speed is still missing.
    assert_eq!(
        (RAN.load(Ordering::Relaxed), SAW.load(Ordering::Relaxed)),
        (0, 102)
    );
    app.world.insert_resource(Speed(1));
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(RAN.load(Ordering::Relaxed), 2);
    assert_eq!(app.world.resource::<Speed>(), Some(&Speed(5)));
}

#[test]
fn builder_resources_apply_in_order_after_hooks_are_installed() {
    let app = App::new()
        .add_systems(
            OnInsert::<Score>,
            |_: Trigger, score: Res<Score>, log: Option<ResMut<Log>>| {
                if let Some(mut log) = log {
                    log.0.push(format!("score {}", score.0));
                }
            },
        )
        .init_resource::<Log>()
        .insert_resource(Score(4))
        // Already provided: keeps 4.
        .init_resource::<Score>()
        .insert_resource(Speed(9))
        .build();
    assert_eq!(app.world.resource::<Log>().unwrap().0, ["score 4"]);
    assert_eq!(app.world.resource::<Score>(), Some(&Score(4)));
    assert_eq!(app.world.resource::<Speed>(), Some(&Speed(9)));
}

#[test]
fn commands_insert_and_remove_resources_when_applied() {
    let mut app =
        App::new()
            .add_systems(
                Update,
                |mut commands: Commands<_>,
                 score: Option<Res<Score>>,
                 speed: Option<Res<Speed>>| {
                    match (score.is_some(), speed.is_some()) {
                        (false, _) => commands.insert_resource(Score(1)),
                        (true, false) => commands.insert_resource(Speed(2)),
                        (true, true) => commands.remove_resource::<Score>(),
                    }
                },
            )
            .build();
    app.emit(&Update);
    assert_eq!(app.world.resource::<Score>(), Some(&Score(1)));
    app.emit(&Update);
    assert_eq!(app.world.resource::<Speed>(), Some(&Speed(2)));
    app.emit(&Update);
    assert_eq!(app.world.resource::<Score>(), None);
    assert_eq!(app.world.resource::<Speed>(), Some(&Speed(2)));
}
