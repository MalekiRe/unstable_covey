use static_ecs::prelude::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

struct Update;
impl Schedule for Update {}
struct Tag;
struct Other;
struct Global;

fn with_workers(task: impl FnOnce() + Send) {
    WorkerPool::new(2).install(task);
}

#[derive(Debug, PartialEq)]
struct Position(i32);
#[derive(Debug, PartialEq)]
struct Velocity(i32);

#[test]
fn heterogeneous_queries_skip_tables_missing_any_requested_component() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut query: Query<(&mut Position, &Velocity)>| {
                    for (mut p, v) in &mut query {
                        p.0 += v.0;
                    }
                },
                |mut query: Query<(&mut Position, &Velocity, &i32)>| {
                    for (mut p, v, a) in &mut query {
                        p.0 += v.0 * *a;
                    }
                },
            ),
        )
        .build();
    let world = &mut app.world;
    let complete = world.spawn();
    let pair = world.spawn();
    let position_only = world.spawn();
    let removed = world.spawn();
    let dead = world.spawn();
    for entity in [complete, pair, position_only, removed, dead] {
        world.insert(entity, Position(10));
    }
    for entity in [dead, pair, removed, complete] {
        world.insert(entity, Velocity(2));
    }
    for entity in [removed, complete, dead] {
        world.insert(entity, 3_i32);
    }
    assert_eq!(world.remove::<Velocity>(removed), Some(Velocity(2)));
    assert!(world.despawn(dead));
    let no_position = world.spawn();
    world.insert(no_position, Velocity(100));
    world.insert(no_position, 100_i32);
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(complete), Some(&Position(18)));
    assert_eq!(app.world.get::<Position>(pair), Some(&Position(12)));
    assert_eq!(
        app.world.get::<Position>(position_only),
        Some(&Position(10))
    );
    assert_eq!(app.world.get::<Position>(removed), Some(&Position(10)));
    assert_eq!(app.world.get::<Position>(dead), None);
    assert_eq!(app.world.get::<Position>(no_position), None);
    app.world.insert(removed, Velocity(4));
    app.world.despawn(complete);
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(removed), Some(&Position(26)));
    assert_eq!(app.world.get::<Position>(pair), Some(&Position(14)));
}

#[test]
fn retained_mutable_items_span_multiple_tables() {
    fn retain(mut query: Query<(Entity, &mut Position, &mut Velocity)>) {
        let mut items: Vec<_> = query.iter_mut().collect();
        for (_, position, velocity) in &mut items {
            position.0 += 1;
            velocity.0 += position.0;
        }
    }
    let mut app = App::new()
        .add_systems(Update, retain)
        .register::<Tag>()
        .build();
    let world = &mut app.world;
    let entities: Vec<_> = (0..48)
        .map(|i| {
            let entity = world.spawn();
            world.insert(entity, Position(i));
            world.insert(entity, Velocity(i * 10));
            if i % 2 == 0 {
                world.insert(entity, Tag);
            }
            entity
        })
        .collect();
    app.emit(&Update);
    for (i, &entity) in entities.iter().enumerate() {
        assert_eq!(
            app.world.get::<Position>(entity),
            Some(&Position(i as i32 + 1))
        );
        assert_eq!(
            app.world.get::<Velocity>(entity),
            Some(&Velocity(11 * i as i32 + 1))
        );
    }
}

#[test]
fn singleton_requires_exactly_one_matching_row_across_all_tables() {
    type State = (
        Components<Position>,
        (Components<Velocity>, Components<Tag>),
    );
    let mut world = World::<State>::default();
    assert!(world.single::<&Position>().is_none());
    let a = world.spawn();
    world.insert(a, Position(1));
    world.insert(a, Velocity(2));
    world.single::<&mut Position>().unwrap().into_inner().0 += 3;
    assert_eq!(world.single::<&Position>().unwrap().0, 4);
    let b = world.spawn();
    world.insert(b, Position(10));
    // a and b match the data but occupy different tables.
    assert!(world.single::<&Position>().is_none());
    assert!(world.single_filtered::<&Position, With<Tag>>().is_none());
    world.insert(a, Tag);
    {
        let mut single = world
            .single_filtered::<(&mut Position, &Velocity), With<Tag>>()
            .unwrap();
        let (position, velocity) = &mut *single;
        position.0 += velocity.0;
    }
    assert_eq!(world.get::<Position>(a), Some(&Position(6)));
    world.insert(b, Tag);
    assert!(world.single_filtered::<&Position, With<Tag>>().is_none());
    world.remove::<Tag>(b);
    assert_eq!(
        world.single_filtered::<&Position, With<Tag>>().unwrap().0,
        6
    );
    world.remove::<Position>(b);
    assert_eq!(world.single::<&Position>().unwrap().0, 6);
    world.insert(b, Position(10));
    world.insert(b, Velocity(20));
    world.insert(b, Tag);
    // Multiple rows in the same table are also rejected.
    assert!(world.single::<&Position>().is_none());
    world.despawn(b);
    assert_eq!(world.single::<&Position>().unwrap().0, 6);
}

#[test]
fn singleton_skips_preserve_body_state_and_resume_when_all_parameters_match() {
    fn update(
        mut calls: Local<usize>,
        mut p: Single<&mut Position>,
        v: Single<&Velocity>,
        mut log: Single<&mut Vec<usize>>,
    ) {
        *calls += 1;
        p.0 += v.0;
        log.push(*calls);
    }
    let siblings = std::cell::Cell::new(0);
    let mut app = App::new()
        .add_systems(
            Update,
            (update, || siblings.set(siblings.get() + 1)).chain(),
        )
        .build();
    let log = app.world.spawn_bundle((Vec::<usize>::new(),));
    app.emit(&Update); // First Single is missing.
    let p = app.world.spawn();
    app.world.insert(p, Position(1));
    app.emit(&Update); // A later Single is missing after the mutable one was fetched.
    assert_eq!(app.world.get::<Position>(p), Some(&Position(1)));
    let v = app.world.spawn();
    app.world.insert(v, Velocity(2));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(p), Some(&Position(3)));
    let duplicate = app.world.spawn();
    app.world.insert(duplicate, Position(20));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(duplicate), Some(&Position(20)));
    assert_eq!(app.world.get::<Position>(p), Some(&Position(3)));
    app.world.remove::<Position>(duplicate);
    app.emit(&Update);
    app.world.insert(duplicate, Velocity(50));
    app.emit(&Update); // Later parameter has multiple matches.
    assert_eq!(app.world.get::<Position>(p), Some(&Position(5)));
    app.world.despawn(duplicate);
    app.emit(&Update);
    app.world.remove::<Velocity>(v);
    app.emit(&Update);
    app.world.insert(v, Velocity(2));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(p), Some(&Position(9)));
    assert_eq!(app.world.get::<Vec<usize>>(log).unwrap(), &[1, 2, 3, 4]);
    assert_eq!(siblings.get(), 9);
}

#[test]
fn invalid_singleton_condition_skips_group_without_advancing_locals() {
    fn condition(mut calls: Local<usize>, enabled: Single<&bool, With<Tag>>) -> bool {
        *calls += 1;
        **enabled && *calls % 2 == 1
    }
    fn record(mut calls: Local<usize>, mut log: Single<&mut Vec<usize>>) {
        *calls += 1;
        log.push(*calls);
    }
    let mut app = App::new()
        .add_systems(Update, (record, record).chain().run_if(condition))
        .build();
    let log = app.world.spawn_bundle((Vec::<usize>::new(),));
    app.emit(&Update);
    let enabled = app.world.spawn();
    app.world.insert(enabled, true);
    app.emit(&Update); // Does not match the filter yet.
    app.world.insert(enabled, Tag);
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(log).unwrap(), &[1, 1]);
    let duplicate = app.world.spawn();
    app.world.insert(duplicate, true);
    app.world.insert(duplicate, Tag);
    app.emit(&Update);
    app.world.remove::<Tag>(duplicate);
    app.emit(&Update); // Second valid condition call returns false.
    assert_eq!(app.world.get::<Vec<usize>>(log).unwrap(), &[1, 1]);
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(log).unwrap(), &[1, 1, 2, 2]);
    app.world.remove::<bool>(enabled);
    app.emit(&Update);
    app.world.insert(enabled, false);
    app.emit(&Update);
    *app.world.get_mut::<bool>(enabled).unwrap() = true;
    app.emit(&Update);
    assert_eq!(
        app.world.get::<Vec<usize>>(log).unwrap(),
        &[1, 1, 2, 2, 3, 3]
    );
}

#[test]
fn parallel_singleton_skips_leave_siblings_running() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut p: Single<&mut Position>| p.0 += 10,
                |mut q: Query<&mut Velocity>| {
                    for mut v in &mut q {
                        v.0 += 1;
                    }
                },
            )
                .parallel(),
        )
        .build();
    let velocity = app.world.spawn_bundle((Velocity(2),));
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Velocity>(velocity), Some(&Velocity(3)));
    let a = app.world.spawn();
    let b = app.world.spawn();
    app.world.insert(a, Position(1));
    app.world.insert(b, Position(2));
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Velocity>(velocity), Some(&Velocity(4)));
    assert_eq!(app.world.get::<Position>(a), Some(&Position(1)));
    assert_eq!(app.world.get::<Position>(b), Some(&Position(2)));
    app.world.despawn(b);
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Velocity>(velocity), Some(&Velocity(5)));
    assert_eq!(app.world.get::<Position>(a), Some(&Position(11)));
}

#[test]
fn structural_filters_apply_and_or_without_and_required_data() {
    type State = (
        Components<Position>,
        (Components<Tag>, (Components<Other>, Components<Global>)),
    );
    let mut world = World::<State>::default();
    for mask in 0..8 {
        let entity = world.spawn();
        world.insert(entity, Position(mask));
        if mask & 1 != 0 {
            world.insert(entity, Tag);
        }
        if mask & 2 != 0 {
            world.insert(entity, Other);
        }
        if mask & 4 != 0 {
            world.insert(entity, Global);
        }
    }
    let marker_only = world.spawn();
    world.insert(marker_only, Tag);
    macro_rules! values {
        ($filter:ty) => {{
            let mut values: Vec<_> = world
                .query_filtered::<&Position, $filter>()
                .iter_mut()
                .map(|p| p.0)
                .collect();
            values.sort_unstable();
            values
        }};
    }
    assert_eq!(values!(With<Tag>), vec![1, 3, 5, 7]);
    assert_eq!(values!(Without<Tag>), vec![0, 2, 4, 6]);
    assert_eq!(values!((With<Tag>, Without<Other>)), vec![1, 5]);
    assert_eq!(values!((With<Tag>, With<Other>, Without<Global>)), vec![3]);
    assert_eq!(
        values!(Or<(With<Tag>, With<Other>)>),
        vec![1, 2, 3, 5, 6, 7]
    );
    assert_eq!(
        values!((Or<(With<Tag>, With<Other>)>, Without<Global>)),
        vec![1, 2, 3]
    );
    assert_eq!(values!((With<Tag>, Without<Tag>)), Vec::<i32>::new());
}

#[test]
fn complementary_parallel_queries_follow_structural_changes_between_emissions() {
    fn tagged(mut query: Query<&mut Position, With<Tag>>) {
        for mut value in &mut query {
            value.0 += 10;
        }
    }
    fn untagged(mut query: Query<&mut Position, Without<Tag>>) {
        for mut value in &mut query {
            value.0 += 1;
        }
    }
    let mut app = App::new()
        .add_systems(Update, (tagged, untagged).parallel())
        .build();
    let a = app.world.spawn_bundle((Position(0), Tag));
    let b = app.world.spawn_bundle((Position(100),));
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Position>(a), Some(&Position(10)));
    assert_eq!(app.world.get::<Position>(b), Some(&Position(101)));
    app.world.remove::<Tag>(a);
    app.world.insert(b, Tag);
    let c = app.world.spawn();
    app.world.insert(c, Position(200));
    app.world.insert(c, Tag);
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Position>(a), Some(&Position(11)));
    assert_eq!(app.world.get::<Position>(b), Some(&Position(111)));
    assert_eq!(app.world.get::<Position>(c), Some(&Position(210)));
    app.world.remove::<Position>(b);
    app.world.despawn(c);
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Position>(a), Some(&Position(12)));
    assert_eq!(app.world.get::<Position>(b), None);
    assert_eq!(app.world.get::<Position>(c), None);
}

#[test]
fn complementary_access_survives_parameter_and_nested_group_composition() {
    type EitherTag = Or<(With<Tag>, With<Other>)>;
    fn tagged(mut values: Query<&mut i32, EitherTag>) {
        for mut value in &mut values {
            *value += 10;
        }
    }
    fn neither(mut values: Query<&mut i32, (Without<Tag>, Without<Other>)>) {
        for mut value in &mut values {
            *value += 1;
        }
    }
    fn together(
        mut tagged: Query<&mut i32, With<Tag>>,
        mut untagged: Query<&mut i32, Without<Tag>>,
    ) {
        for mut value in &mut tagged {
            *value *= 2;
        }
        for mut value in &mut untagged {
            *value *= 3;
        }
    }
    let mut app = App::new()
        .add_systems(
            Update,
            (
                ((tagged, tagged).chain(), (neither, neither).chain()).parallel(),
                together,
            )
                .chain(),
        )
        .build();
    let entities = [
        app.world.spawn_bundle((0_i32, Tag)),
        app.world.spawn_bundle((0_i32, Other)),
        app.world.spawn_bundle((0_i32,)),
    ];
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<i32>(entities[0]), Some(&40));
    assert_eq!(app.world.get::<i32>(entities[1]), Some(&60));
    assert_eq!(app.world.get::<i32>(entities[2]), Some(&6));
}

#[test]
fn filtered_single_and_query_can_write_complementary_populations() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                |mut value: Single<&mut i32, With<Global>>| **value *= 3,
                |mut values: Query<&mut i32, Without<Global>>| {
                    for mut value in &mut values {
                        *value += 2;
                    }
                },
            )
                .parallel(),
        )
        .build();
    let global = app.world.spawn_bundle((5_i32, Global));
    let ordinary = app.world.spawn_bundle((1_i32,));
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<i32>(global), Some(&15));
    assert_eq!(app.world.get::<i32>(ordinary), Some(&3));
}

#[test]
fn filtered_parameters_retain_disjoint_mutable_items_together() {
    let mut app = App::new()
        .add_systems(
            Update,
            |value: Single<&mut i32, With<Global>>,
             mut values: Query<&mut i32, Without<Global>>| {
                let mut global = value.into_inner();
                let mut ordinary = values.iter_mut().next().unwrap();
                *global += *ordinary;
                *ordinary += *global;
            },
        )
        .build();
    let global = app.world.spawn_bundle((5_i32, Global));
    let ordinary = app.world.spawn_bundle((1_i32,));
    app.emit(&Update);
    assert_eq!(app.world.get::<i32>(global), Some(&6));
    assert_eq!(app.world.get::<i32>(ordinary), Some(&7));
}

#[test]
fn parallel_read_singletons_join_before_dependents() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                (
                    |step: Single<&i32>, mut query: Query<&mut Position>| {
                        for mut p in &mut query {
                            p.0 += **step;
                        }
                    },
                    |step: Single<&i32>, mut query: Query<&mut Velocity>| {
                        for mut v in &mut query {
                            v.0 *= **step;
                        }
                    },
                )
                    .parallel(),
                |mut query: Query<(&mut Position, &Velocity)>| {
                    for (mut p, v) in &mut query {
                        p.0 += v.0;
                    }
                },
            )
                .chain(),
        )
        .build();
    let global = app.world.spawn_bundle((3_i32,));
    let entity = app.world.spawn_bundle((Position(1), Velocity(2)));
    with_workers(|| app.emit(&Update));
    assert_eq!(app.world.get::<Position>(entity), Some(&Position(10)));
    assert_eq!(app.world.get::<Velocity>(entity), Some(&Velocity(6)));
    assert_eq!(app.world.get::<i32>(global), Some(&3));
}

#[test]
fn local_state_is_independent_per_registration_and_persists() {
    fn record(mut local: Local<usize>, mut log: Single<&mut Vec<usize>>) {
        *local += 1;
        log.push(*local);
    }
    let mut app = App::new().add_systems(Update, (record, record)).build();
    let log = app.world.spawn_bundle((Vec::<usize>::new(),));
    for _ in 0..3 {
        app.emit(&Update);
    }
    assert_eq!(
        app.world.get::<Vec<usize>>(log).unwrap(),
        &[1, 1, 2, 2, 3, 3]
    );
}

#[test]
fn condition_skips_nested_group_without_advancing_system_locals() {
    fn every_other(
        mut calls: Local<usize>,
        enabled: Single<&bool>,
        mut query: Query<&i32>,
    ) -> bool {
        *calls += 1;
        *calls % 2 == 0 && **enabled && query.iter_mut().any(|v| *v > 0)
    }
    fn record(mut calls: Local<usize>, mut log: Single<&mut Vec<usize>>) {
        *calls += 1;
        log.push(*calls);
    }
    let mut app = App::new()
        .add_systems(
            Update,
            (record, (record, record).chain()).run_if(every_other),
        )
        .build();
    let global = app.world.spawn_bundle((Vec::<usize>::new(), true));
    let entity = app.world.spawn_bundle((1_i32,));
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(global).unwrap(), &[]);
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(global).unwrap(), &[1, 1, 1]);
    *app.world.get_mut::<bool>(global).unwrap() = false;
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(global).unwrap(), &[1, 1, 1]);
    *app.world.get_mut::<bool>(global).unwrap() = true;
    *app.world.get_mut::<i32>(entity).unwrap() = 0;
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(global).unwrap(), &[1, 1, 1]);
    *app.world.get_mut::<i32>(entity).unwrap() = 1;
    app.emit(&Update);
    assert_eq!(app.world.get::<Vec<usize>>(global).unwrap(), &[1, 1, 1]);
    app.emit(&Update);
    assert_eq!(
        app.world.get::<Vec<usize>>(global).unwrap(),
        &[1, 1, 1, 2, 2, 2]
    );
}

#[test]
fn tuples_and_chains_run_conflicting_systems_in_registration_order() {
    fn add(mut value: Single<&mut i32>) {
        **value += 2;
    }
    fn multiply(mut value: Single<&mut i32>) {
        **value *= 3;
    }
    fn subtract(mut value: Single<&mut i32>) {
        **value -= 1;
    }
    let mut app = App::new()
        .add_systems(Update, (add, multiply))
        .add_systems(Update, (subtract, (add, multiply).chain()).chain())
        .build();
    let entity = app.world.spawn_bundle((1_i32,));
    app.emit(&Update);
    assert_eq!(app.world.get::<i32>(entity), Some(&30));
    app.emit(&Update);
    assert_eq!(app.world.get::<i32>(entity), Some(&291));
}

#[test]
fn panic_propagates_only_after_parallel_sibling_finishes() {
    let rendezvous = std::sync::Arc::new(std::sync::Barrier::new(2));
    let left = rendezvous.clone();
    let mut app = App::new()
        .add_systems(
            Update,
            (
                move |_: Single<&mut Position>| {
                    left.wait();
                    panic!("system failed");
                },
                move |mut right: Single<&mut Velocity>| {
                    rendezvous.wait();
                    right.0 = 7;
                },
            )
                .parallel(),
        )
        .build();
    let entity = app.world.spawn_bundle((Position(0), Velocity(0)));
    with_workers(|| {
        assert!(catch_unwind(AssertUnwindSafe(|| app.emit(&Update))).is_err());
    });
    assert_eq!(app.world.get::<Velocity>(entity), Some(&Velocity(7)));
}

#[test]
fn exclusive_system_can_change_structure_before_a_typed_system() {
    let mut app = App::new()
        .add_systems(
            Update,
            (
                exclusive(|world: &mut World<_>| {
                    let entity = world.single::<(Entity, &Position)>().unwrap().0;
                    if world.remove::<Tag>(entity).is_none() {
                        world.insert(entity, Tag);
                    }
                }),
                |mut query: Query<&mut Position, With<Tag>>| {
                    for mut p in &mut query {
                        p.0 += 10;
                    }
                },
            )
                .chain(),
        )
        .build();
    let entity = app.world.spawn_bundle((Position(1),));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(entity), Some(&Position(11)));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(entity), Some(&Position(11)));
    app.emit(&Update);
    assert_eq!(app.world.get::<Position>(entity), Some(&Position(21)));
}

#[test]
#[cfg(not(miri))] // Host compiler subprocesses are not part of Miri runtime coverage.
fn invalid_parameter_access_is_rejected_at_compile_time() {
    use std::{path::PathBuf, process::Command};
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "static-ecs-ui-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(scratch.0.join("Cargo.toml"), format!(
        "[package]\nname = \"access-check\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\nstatic-ecs = {{ path = {:?} }}\n[[bin]]\nname = \"access-check\"\npath = \"main.rs\"\n", root,
    )).unwrap();
    for (fixture, error) in [
        ("valid", None),
        ("single_alias", Some("E0080")),
        ("query_alias", Some("E0080")),
        ("parallel_conflict", Some("E0080")),
        ("change_conflict", Some("E0080")),
        ("duplicate_bundle", Some("E0080")),
        ("mutable_condition", Some("E0080")),
        ("missing_component", Some("E0080")),
        ("late_storage", Some("E0080")),
        ("duplicate_component", Some("E0080")),
        ("missing_filter", Some("E0080")),
        ("overlapping_filters", Some("E0080")),
        ("overlapping_or", Some("E0080")),
        ("impossible_filter_alias", Some("E0080")),
        ("exclusive_parallel", Some("E0080")),
        ("query_borrow", Some("E0499")),
        ("world_borrow", Some("E0499")),
        ("missing_dependency", Some("E0080")),
        ("duplicate_node", Some("E0080")),
        ("bridge_outside_envelope", Some("E0080")),
        ("unplaced_sync_point", Some("E0080")),
        ("join_shared_iter", Some("E0080")),
        ("join_many_to_many_write", Some("E0080")),
        ("join_direct_alias", Some("E0080")),
        ("join_nested_alias", Some("E0080")),
        ("join_parallel_conflict", Some("E0080")),
        ("link_write_query", Some("E0080")),
        ("link_write_world", Some("E0080")),
        ("cascade_cyclic", Some("E0080")),
        ("plugin_missing_component", Some("E0080")),
        ("immutable_write", Some("E0080")),
        ("required_unstored", Some("E0080")),
    ] {
        std::fs::copy(
            root.join("tests/ui").join(format!("{fixture}.rs")),
            scratch.0.join("main.rs"),
        )
        .unwrap();
        let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command
            .arg(if error.is_none() { "run" } else { "build" })
            .arg("--offline")
            .arg("--manifest-path")
            .arg(scratch.0.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(scratch.0.join("target"));
        if !cfg!(debug_assertions) {
            command.arg("--release");
        }
        let output = command.output().unwrap();
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            error.is_none(),
            "{fixture}: {diagnostics}"
        );
        if let Some(code) = error {
            assert!(
                diagnostics.contains(&format!("error[{code}]")),
                "{fixture}: {diagnostics}"
            );
        }
    }
}

#[test]
fn get_mut_resolves_one_entity_through_data_archetype_and_row_filters() {
    let mut world = World::<(
        (Components<Position>, Components<Velocity>),
        (Sparse<Tag>, Components<Other>),
    )>::default();
    let moving = world.spawn_bundle((Position(1), Velocity(2)));
    let tagged = world.spawn_bundle((Position(3), Velocity(4), Tag));
    let still = world.spawn_bundle((Position(5),));
    let gone = world.spawn_bundle((Position(6), Velocity(7)));
    world.despawn(gone);
    let elsewhere = World::<(Components<Position>, Components<Velocity>)>::default()
        .spawn_bundle((Position(8), Velocity(9)));

    let mut query = world.query::<(&mut Position, &Velocity)>();
    let (mut position, velocity) = query.get_mut(tagged).unwrap();
    position.0 += velocity.0;
    assert!(
        query.get_mut(still).is_none(),
        "missing a queried component"
    );
    assert!(query.get_mut(gone).is_none(), "despawned");
    assert!(query.get_mut(elsewhere).is_none(), "another world's entity");
    assert_eq!(world.get::<Position>(tagged), Some(&Position(7)));

    let mut untagged = world.query_filtered::<&Position, Without<Tag>>();
    assert_eq!(untagged.get_mut(moving), Some(&Position(1)));
    assert!(untagged.get_mut(tagged).is_none(), "archetype filter");

    // Row filter: get_mut sees the watched entity only in windows where it changed.
    struct Watch(Entity, Vec<Option<i32>>);
    let mut app = App::new()
        .add_systems(
            Update,
            |mut positions: Query<&Position, Changed<Position>>, mut watch: Single<&mut Watch>| {
                let seen = positions.get_mut(watch.0).map(|p| p.0);
                watch.1.push(seen);
            },
        )
        .build();
    let watched = app.world.spawn_bundle((Position(10),));
    let log = app.world.spawn_bundle((Watch(watched, Vec::new()),));
    app.emit(&Update); // first run: everything is new
    app.emit(&Update); // unchanged since
    app.world.get_mut::<Position>(watched).unwrap().0 = 11;
    app.emit(&Update);
    assert_eq!(
        app.world.get::<Watch>(log).unwrap().1,
        [Some(10), None, Some(11)]
    );
}

#[derive(Debug, PartialEq, Clone, Copy)]
struct Shield(u32);
struct Stunned;

#[test]
fn optional_items_and_presence_checks_follow_each_row() {
    let mut app = App::new()
        .add_systems(
            Update,
            |mut q: Query<(&Position, Option<&mut Shield>, Has<Stunned>)>| {
                for (pos, shield, stunned) in &mut q {
                    // Shields charge unless stunned; rows without one still match.
                    if let Some(mut shield) = shield
                        && !stunned
                    {
                        shield.0 += pos.0 as u32;
                    }
                }
            },
        )
        .build();
    let world = &mut app.world;
    let bare = world.spawn_bundle((Position(1),));
    let shielded = world.spawn_bundle((Position(2), Shield(10)));
    let stunned = world.spawn_bundle((Position(3), Shield(10), Stunned));
    app.emit(&Update);
    let world = &mut app.world;
    assert_eq!(world.get::<Shield>(shielded), Some(&Shield(12)));
    assert_eq!(world.get::<Shield>(stunned), Some(&Shield(10)));
    let mut q = world.query::<(Entity, Option<&Shield>, Has<Stunned>)>();
    let mut rows: Vec<_> = q.iter_mut().map(|(e, s, h)| (e, s.copied(), h)).collect();
    rows.sort_by_key(|row| row.0);
    assert_eq!(
        rows,
        [
            (bare, None, false),
            (shielded, Some(Shield(12)), false),
            (stunned, Some(Shield(10)), true)
        ]
    );
    // `Has` alone stores its component: `Stunned` above came only from it.
    world.remove::<Stunned>(stunned);
    assert!(!world.query::<Has<Stunned>>().iter_mut().any(|has| has));
}

#[test]
fn presence_checks_run_beside_writers_and_options_conflict_like_writes() {
    struct Count(u32);
    with_workers(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    // Reads only membership: runs beside the writer of `Stunned`.
                    |mut q: Query<Has<Stunned>>, mut count: Single<&mut Count>| {
                        count.0 += q.iter_mut().filter(|&has| has).count() as u32;
                    },
                    |mut q: Query<&mut Stunned>| {
                        for _ in &mut q {}
                    },
                )
                    .parallel(),
            )
            .build();
        app.world.spawn_bundle((Count(0),));
        app.world.spawn_bundle((Stunned,));
        app.emit(&Update);
        assert_eq!(app.world.query::<&Count>().iter_mut().next().unwrap().0, 1);
    });
}

#[derive(Debug, PartialEq, Clone, Copy)]
struct C<const N: usize>(u32);

#[test]
fn tuples_reach_sixteen_for_queries_filters_bundles_params_and_chains() {
    type Sixteen = (
        &'static C<0>,
        &'static C<1>,
        &'static C<2>,
        &'static C<3>,
        &'static C<4>,
        &'static C<5>,
        &'static C<6>,
        &'static C<7>,
        &'static C<8>,
        &'static C<9>,
        &'static C<10>,
        &'static C<11>,
        &'static C<12>,
        &'static C<13>,
        &'static C<14>,
        &'static mut C<15>,
    );
    type Filters = (
        With<C<0>>,
        With<C<1>>,
        With<C<2>>,
        With<C<3>>,
        With<C<4>>,
        With<C<5>>,
        With<C<6>>,
        With<C<7>>,
        With<C<8>>,
        With<C<9>>,
        With<C<10>>,
        With<C<11>>,
        With<C<12>>,
        With<C<13>>,
        With<C<14>>,
        Without<Tag>,
    );
    let log = std::sync::Mutex::new(Vec::new());
    let push = |i: u32| {
        let log = &log;
        move || log.lock().unwrap().push(i)
    };
    let mut app = App::new()
        .register::<Tag>()
        .add_systems(Update, |mut q: Query<Sixteen, Filters>| {
            for row in &mut q {
                let (c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, c13, c14, mut c15) =
                    row;
                c15.0 = c0.0
                    + c1.0
                    + c2.0
                    + c3.0
                    + c4.0
                    + c5.0
                    + c6.0
                    + c7.0
                    + c8.0
                    + c9.0
                    + c10.0
                    + c11.0
                    + c12.0
                    + c13.0
                    + c14.0;
            }
        })
        .add_systems(
            Update,
            // Sixteen parameters in one system.
            |a: Query<&C<0>>,
             b: Query<&C<1>>,
             c: Query<&C<2>>,
             d: Query<&C<3>>,
             e: Query<&C<4>>,
             f: Query<&C<5>>,
             g: Query<&C<6>>,
             h: Query<&C<7>>,
             i: Query<&C<8>>,
             j: Query<&C<9>>,
             k: Query<&C<10>>,
             l: Query<&C<11>>,
             m: Query<&C<12>>,
             n: Query<&C<13>>,
             o: Query<&C<14>>,
             mut p: Query<&C<15>>| {
                let _ = (a, b, c, d, e, f, g, h, i, j, k, l, m, n, o);
                assert_eq!(p.iter_mut().filter(|c| c.0 == 105).count(), 1);
            },
        )
        .add_systems(
            Update,
            (
                push(0),
                push(1),
                push(2),
                push(3),
                push(4),
                push(5),
                push(6),
                push(7),
                push(8),
                push(9),
                push(10),
                push(11),
                push(12),
                push(13),
                push(14),
                push(15),
            )
                .chain(),
        )
        .build();
    let bundle = (
        C::<0>(0),
        C::<1>(1),
        C::<2>(2),
        C::<3>(3),
        C::<4>(4),
        C::<5>(5),
        C::<6>(6),
        C::<7>(7),
        C::<8>(8),
        C::<9>(9),
        C::<10>(10),
        C::<11>(11),
        C::<12>(12),
        C::<13>(13),
        C::<14>(14),
        C::<15>(0),
    );
    let entity = app.world.spawn_bundle(bundle);
    let tagged = app.world.spawn_bundle(bundle);
    app.world.insert(tagged, Tag);
    app.emit(&Update);
    assert_eq!(app.world.get::<C<15>>(entity), Some(&C(105)));
    assert_eq!(app.world.get::<C<15>>(tagged), Some(&C(0)));
    drop(app);
    assert_eq!(log.into_inner().unwrap(), (0..16).collect::<Vec<_>>());
}

#[test]
fn cached_queries_keep_every_table_as_tables_appear_between_runs() {
    struct Seen(usize);
    struct T<const N: usize>;
    let mut app = App::new()
        .register::<T<0>>()
        .register::<T<1>>()
        .register::<T<2>>()
        .register::<T<3>>()
        .register::<T<4>>()
        .register::<T<5>>()
        .register::<T<6>>()
        .add_systems(
            Update,
            |mut q: Query<&Position>, mut seen: Single<&mut Seen>| {
                seen.0 = q.iter_mut().count();
            },
        )
        .build();
    let seen = app.world.spawn_bundle((Seen(0),));
    let count = |app: &mut App<_, _>| {
        app.emit(&Update);
        app.world.get::<Seen>(seen).unwrap().0
    };
    // Three tables, cached inline...
    app.world.spawn_bundle((Position(0),));
    app.world.spawn_bundle((Position(0), T::<0>));
    app.world.spawn_bundle((Position(0), T::<1>));
    assert_eq!(count(&mut app), 3);
    // ...then seven more appear between runs: the cache crosses its inline
    // capacity while scanning only the new tables.
    app.world.spawn_bundle((Position(0), T::<2>));
    app.world.spawn_bundle((Position(0), T::<3>));
    assert_eq!(count(&mut app), 5);
    for _ in 0..2 {
        app.world.spawn_bundle((Position(0), T::<4>));
        app.world.spawn_bundle((Position(0), T::<5>));
        app.world.spawn_bundle((Position(0), T::<6>));
        app.world.spawn_bundle((Position(0), T::<0>, T::<6>));
    }
    assert_eq!(count(&mut app), 13);
    // A new world restarts the cache from scratch.
    let mut fresh = World::default();
    let seen = fresh.spawn_bundle((Seen(0),));
    fresh.spawn_bundle((Position(0), T::<5>));
    app.world = fresh;
    app.emit(&Update);
    assert_eq!(app.world.get::<Seen>(seen).unwrap().0, 1);
}
