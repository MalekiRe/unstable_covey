use static_ecs::prelude::*;
use std::any::type_name;
use std::panic::{AssertUnwindSafe, catch_unwind};

struct Update;
impl Schedule for Update {}

struct Game;
struct Menu;
#[derive(Debug, PartialEq)]
struct Playing {
    level: u32,
}
impl State for Menu {
    type Group = Game;
}
impl State for Playing {
    type Group = Game;
}
// Substate group of Playing.
struct Run;
#[derive(Default)]
struct Running;
struct Paused;
impl State for Running {
    type Group = Run;
}
impl State for Paused {
    type Group = Run;
}
// Computed from Playing.
struct Difficulty;
struct Hard;
impl State for Hard {
    type Group = Difficulty;
}

#[derive(Default)]
struct Log(Vec<String>);
fn log(entry: &'static str) -> impl FnMut(ResMut<Log>) + Clone + Send {
    move |mut log: ResMut<Log>| log.0.push(entry.into())
}
fn take(world: &mut World<impl Stores<Log>>) -> Vec<String> {
    std::mem::take(&mut world.resource_mut::<Log>().unwrap().0)
}

#[test]
fn transitions_exit_innermost_first_and_enter_outermost_first() {
    let mut app = App::new()
        .add_systems(OnEnter::<Menu>, log("enter menu"))
        .add_systems(OnExit::<Menu>, log("exit menu"))
        .add_systems(
            OnEnter::<Playing>,
            |mut log: ResMut<Log>, playing: Res<Playing>| {
                log.0.push(format!("enter playing {}", playing.level))
            },
        )
        .add_systems(
            OnExit::<Playing>,
            // The state is still readable while it exits.
            |mut log: ResMut<Log>, playing: Res<Playing>| {
                log.0.push(format!("exit playing {}", playing.level))
            },
        )
        .add_systems(OnTransition::<Menu, Playing>, log("menu -> playing"))
        .add_systems(OnTransition::<Playing, Playing>, log("playing -> playing"))
        .add_systems(OnEnter::<Running>, log("enter running"))
        .add_systems(OnExit::<Running>, log("exit running"))
        .add_systems(OnEnter::<Paused>, log("enter paused"))
        .add_systems(OnExit::<Paused>, log("exit paused"))
        .add_systems(OnEnter::<Hard>, log("enter hard"))
        .add_systems(OnExit::<Hard>, log("exit hard"))
        .init_resource::<Log>()
        .init_state(Menu)
        .add_sub_state::<Playing, Running>()
        .add_computed_state(|playing: &Playing| (playing.level >= 3).then_some(Hard))
        .build();
    let world = &mut app.world;
    assert_eq!(take(world), ["enter menu"]);
    assert_eq!(world.state_name::<Game>(), Some(type_name::<Menu>()));

    world.set_state(Playing { level: 1 });
    assert_eq!(
        take(world),
        [
            "exit menu",
            "enter playing 1",
            "menu -> playing",
            "enter running"
        ]
    );
    world.set_state(Paused);
    assert_eq!(take(world), ["exit running", "enter paused"]);

    // New data is a transition: substates exit first and re-enter after.
    world.set_state(Playing { level: 3 });
    assert_eq!(
        take(world),
        [
            "exit paused",
            "exit playing 1",
            "enter playing 3",
            "playing -> playing",
            "enter running",
            "enter hard"
        ]
    );
    // Links exit newest first: the computed state, then the substate.
    world.set_state(Menu);
    assert_eq!(
        take(world),
        ["exit hard", "exit running", "exit playing 3", "enter menu"]
    );
    assert!(world.resource::<Playing>().is_none());
    assert_eq!(world.state_name::<Run>(), None);
    assert_eq!(world.state_name::<Difficulty>(), None);

    // A substate whose parent is inactive is ignored; debug builds panic.
    let ignored = catch_unwind(AssertUnwindSafe(|| world.set_state(Paused)));
    assert_eq!(ignored.is_err(), cfg!(debug_assertions));
    assert!(world.resource::<Paused>().is_none());
    assert_eq!(take(world), Vec::<String>::new());

    // Clearing exits; entering from nothing is no transition from Menu.
    world.clear_state::<Game>();
    world.set_state(Playing { level: 1 });
    assert_eq!(
        take(world),
        ["exit menu", "enter playing 1", "enter running"]
    );
    world.clear_state::<Game>();
    assert_eq!(take(world), ["exit running", "exit playing 1"]);
    assert_eq!(world.state_name::<Game>(), None);
}

#[test]
fn hook_commands_apply_after_the_whole_transition() {
    let mut app = App::new()
        .add_systems(OnEnter::<Playing>, |mut commands: Commands<_>| {
            commands.queue(|world| world.resource_mut::<Log>().unwrap().0.push("queued".into()));
        })
        .add_systems(OnEnter::<Running>, log("enter running"))
        .add_systems(OnEnter::<Menu>, log("enter menu"))
        .init_resource::<Log>()
        .add_sub_state::<Playing, Running>()
        .build();
    app.world.set_state(Playing { level: 1 });
    assert_eq!(take(&mut app.world), ["enter running", "queued"]);
    // A hook may redirect: it applies after the transition it ran in.
    let mut app = App::new()
        .add_systems(OnEnter::<Running>, |mut commands: Commands<_>| {
            commands.set_state(Menu)
        })
        .add_systems(OnEnter::<Menu>, log("enter menu"))
        .add_systems(OnExit::<Running>, log("exit running"))
        .init_resource::<Log>()
        .add_sub_state::<Playing, Running>()
        .build();
    app.world.set_state(Playing { level: 1 });
    assert_eq!(take(&mut app.world), ["exit running", "enter menu"]);
    assert_eq!(app.world.state_name::<Game>(), Some(type_name::<Menu>()));
}

#[test]
fn states_gate_systems_and_change_through_commands() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut commands: Commands<_>, _: Res<Menu>| commands.set_state(Playing { level: 1 }),
                (|mut log: ResMut<Log>| log.0.push("playing".into())).run_if(in_state::<Playing>()),
                |mut commands: Commands<_>, playing: Res<Playing>| {
                    if playing.level == 2 {
                        commands.clear_state::<Game>();
                    } else {
                        commands.set_state(Playing { level: 2 });
                    }
                },
            ),
        )
        .init_resource::<Log>()
        .init_state(Menu)
        .build();
    // Menu -> Playing 1: commands apply after the emit.
    app.emit(&Update);
    assert_eq!(take(&mut app.world), Vec::<String>::new());
    app.emit(&Update);
    assert_eq!(take(&mut app.world), ["playing"]);
    assert_eq!(app.world.resource::<Playing>(), Some(&Playing { level: 2 }));
    app.emit(&Update);
    assert_eq!(take(&mut app.world), ["playing"]);
    assert_eq!(app.world.state_name::<Game>(), None);
    app.emit(&Update);
    assert_eq!(take(&mut app.world), Vec::<String>::new());
}

#[test]
#[should_panic(expected = "already follows a parent state")]
fn a_state_group_follows_one_parent() {
    App::new()
        .add_sub_state::<Playing, Running>()
        .add_sub_state::<Menu, Running>()
        .build();
}
