use static_ecs::prelude::*;

struct Position(f32);
struct Velocity(f32);
struct Health(u32);
struct Time(f32);
struct Ticks(u64);
struct Running(bool);
struct Active;
struct Global;
struct Update;

fn integrate(
    mut query: Query<(&mut Position, &Velocity), With<Active>>,
    time: Single<&Time, With<Global>>,
) {
    for (mut position, velocity) in &mut query {
        position.0 += velocity.0 * time.0;
    }
}

fn decay(
    mut query: Query<&mut Health, With<Active>>,
    mut ticks: Single<&mut Ticks, With<Global>>,
    mut local: Local<u64>,
) {
    for mut health in &mut query {
        health.0 = health.0.saturating_sub(1);
    }
    *local += 1;
    ticks.0 = *local;
}

fn validate(
    mut query: Query<(&Position, &Velocity), With<Active>>,
    ticks: Single<&Ticks, With<Global>>,
) {
    for (position, velocity) in &mut query {
        assert_eq!(position.0, velocity.0 * ticks.0 as f32);
    }
}

fn running(state: Single<&Running, With<Global>>) -> bool {
    state.0
}

fn main() {
    // No schema: the world stores exactly what these systems use.
    let mut app = App::new()
        .add_systems(
            Update,
            ((integrate, decay).parallel(), validate)
                .chain()
                .run_if(running),
        )
        .build();
    let world = &mut app.world;
    let global = world.spawn_bundle((Global, Time(1.0), Ticks(0), Running(true)));
    let entities: Vec<_> = (1..=32)
        .map(|speed| {
            world.spawn_bundle((Position(0.0), Velocity(speed as f32), Health(100), Active))
        })
        .collect();
    let inactive = world.spawn_bundle((Position(50.0), Velocity(3.0), Health(100)));
    for _ in 0..10 {
        app.emit(&Update);
    }
    for (i, &entity) in entities.iter().enumerate() {
        assert_eq!(
            app.world.get::<Position>(entity).unwrap().0,
            10.0 * (i + 1) as f32
        );
        assert_eq!(app.world.get::<Health>(entity).unwrap().0, 90);
    }
    assert_eq!(app.world.get::<Position>(inactive).unwrap().0, 50.0);
    assert_eq!(app.world.get::<Health>(inactive).unwrap().0, 100);
    assert_eq!(app.world.get::<Ticks>(global).unwrap().0, 10);
    println!(
        "10 updates: 32 active entities integrated, health=90, local ticks=10; inactive entity unchanged"
    );

    app.world.get_mut::<Running>(global).unwrap().0 = false;
    app.emit(&Update);
    assert_eq!(app.world.get::<Ticks>(global).unwrap().0, 10);
    app.world.get_mut::<Running>(global).unwrap().0 = true;
    assert!(app.world.despawn(entities[0]));
    app.world.remove::<Active>(entities[1]);
    app.emit(&Update);
    assert!(app.world.get::<Position>(entities[0]).is_none());
    assert_eq!(app.world.get::<Position>(entities[1]).unwrap().0, 20.0);
    assert_eq!(app.world.get::<Health>(entities[1]).unwrap().0, 90);
    assert_eq!(app.world.get::<Position>(entities[2]).unwrap().0, 33.0);
    assert_eq!(app.world.get::<Ticks>(global).unwrap().0, 11);
    println!(
        "condition skipped the group; resumed after despawn and filter removal with local ticks=11"
    );
}
