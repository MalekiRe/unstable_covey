//! Plugins name their components and relations, then build their systems for
//! the app's final world: everything a system can take works inside one.
use static_ecs::prelude::*;
use std::sync::{Arc, Mutex};

struct Update;
impl Schedule for Update {}
struct Never;
impl Schedule for Never {}

#[derive(Debug, PartialEq, Clone, Copy)]
struct Pos(i32);
#[derive(Debug, PartialEq, Clone, Copy)]
struct Vel(i32);
struct Spawned(u32);
struct Hp;

struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
    const LINKED: bool = true;
}

/// Moves things; spawns one mover per frame through `Commands<S>`.
struct Physics;
impl Plugin for Physics {
    type Components = (Pos, Vel);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            schedule(
                Update,
                (
                    |mut q: Query<(&mut Pos, &Vel)>| {
                        for (mut pos, vel) in &mut q {
                            pos.0 += vel.0;
                        }
                    },
                    // Disjoint access: runs beside the mover.
                    |mut spawned: Local<u32>, mut commands: Commands<S>| {
                        *spawned += 1;
                        commands.spawn((Pos(0), Vel(*spawned as i32)));
                    },
                )
                    .parallel(),
            ),
            // `_` works too: the plugin's world fixes it.
            schedule(Update, |q: Query<&Pos>, _: Commands<_>| {
                let _ = q;
            }),
        )
    }
}

/// Uses another plugin's systems, a relation, a hook, an exclusive system,
/// and a task.
struct Game {
    log: Arc<Mutex<Vec<String>>>,
}
impl Plugin for Game {
    type Components = (Pos, Vel, Spawned, Hp);
    type Relations = (ChildOf,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        let (log, hook_log) = (self.log.clone(), self.log);
        (
            Physics.systems::<S>(),
            // Hooks belong to the world, so they own what they capture.
            schedule(OnAdd::<Hp>, move |trigger: Trigger| {
                let log = &hook_log;
                log.lock()
                    .unwrap()
                    .push(format!("hp added x{}", trigger.len()));
            }),
            schedule(
                Update,
                exclusive(move |world: &mut World<S>| {
                    let parent = world.spawn_bundle((Hp,));
                    let child = world.spawn_bundle((Hp, ChildOf.to(parent)));
                    world.despawn(parent);
                    log.lock()
                        .unwrap()
                        .push(format!("child alive: {}", world.contains(child)));
                }),
            ),
            task(move |world: AsyncWorld<S>| async move {
                let mut ticks = 0;
                loop {
                    let n = world
                        .bridge(Update, |mut q: Query<&mut Spawned>| {
                            for mut s in &mut q {
                                s.0 += 1;
                            }
                            q.iter_mut().count()
                        })
                        .await
                        .unwrap();
                    ticks += 1;
                    let _ = (n, ticks);
                }
            }),
        )
    }
}

#[test]
fn plugins_build_systems_of_every_kind_for_the_app_world() {
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let mut app = App::new()
            // The app already installs the relation: installing it again is a no-op.
            .relation::<ChildOf>()
            .add_plugins(Game { log: log.clone() })
            .build();
        let counter = app.world.spawn_bundle((Spawned(0),));
        app.emit(&Update);
        app.emit(&Update);
        app.emit(&Never);
        app.emit(&Update);
        // Three frames: three movers spawned, each moved every later frame.
        let mut q = app.world.query::<(&Pos, &Vel)>();
        let mut movers: Vec<_> = q.iter_mut().map(|(p, v)| (p.0, v.0)).collect();
        movers.sort_unstable();
        assert_eq!(movers, [(0, 3), (2, 1), (2, 2)]);
        // The task's bridge ran once per Update frame.
        assert_eq!(app.world.get::<Spawned>(counter).unwrap().0, 3);
    }
    let log = Arc::into_inner(log).unwrap().into_inner().unwrap();
    // Each frame: the hook saw both spawns of the exclusive system, and the
    // linked relation took the child with its parent.
    assert_eq!(
        log,
        ["hp added x1", "hp added x1", "child alive: false"].repeat(3)
    );
}

#[test]
fn plugins_choose_storage_for_their_components() {
    struct Hot(u32);
    struct Cold(u32);
    struct Flag;
    struct Tuned;
    impl Plugin for Tuned {
        type Components = (Hot, Cold, Flag);
        type Storage = (Components<Hot, true>, Sparse<Flag>);
        fn systems<S: Storage>(self) -> impl Systems<S> {
            schedule((), |mut q: Query<&mut Hot, Changed<Cold>>| q.iter_mut().for_each(|mut h| h.0 += 1))
        }
    }
    fn storage<S: Storage + Stores<Hot> + Stores<Cold> + Stores<Flag>>(_: &World<S>) -> [bool; 3] {
        [<S as Stores<Hot>>::SUMMARY, <S as Stores<Cold>>::SUMMARY, <S as Stores<Flag>>::SPARSE]
    }
    let mut app = App::new().add_plugins(Tuned).build();
    assert_eq!(storage(&app.world), [true, false, true]);
    let e = app.world.spawn_bundle((Hot(0), Cold(0), Flag));
    app.emit(&());
    assert_eq!(app.world.get::<Hot>(e).unwrap().0, 1);
}

#[test]
fn a_plugin_may_only_declare_components() {
    struct Marker;
    struct Count(u32);
    struct Vocabulary;
    impl Plugin for Vocabulary {
        type Components = (Marker, Count);
        type Storage = (Sparse<Marker>,);
        fn systems<S: Storage>(self) -> impl Systems<S> {}
    }
    let mut app = App::new()
        .add_plugins(Vocabulary)
        .add_systems((), |mut q: Query<&mut Count, With<Marker>>| q.iter_mut().for_each(|mut c| c.0 += 1))
        .build();
    let e = app.world.spawn_bundle((Count(0), Marker));
    app.emit(&());
    assert_eq!(app.world.get::<Count>(e).unwrap().0, 1);
}

#[test]
fn payload_labels_with_named_fields_are_named_by_type() {
    struct Hit {
        target: Entity,
        amount: u32,
    }
    impl Schedule for Hit {
        type In = Ref<Self>;
    }
    struct Health(u32);
    let mut app = App::new()
        .add_systems(label::<Hit>(), |hit: &Hit, mut q: Query<&mut Health>| {
            if let Some(mut health) = q.get_mut(hit.target) {
                health.0 -= hit.amount;
            }
        })
        .build();
    let target = app.world.spawn_bundle((Health(10),));
    app.emit(&Hit { target, amount: 4 });
    assert_eq!(app.world.get::<Health>(target).unwrap().0, 6);
}
