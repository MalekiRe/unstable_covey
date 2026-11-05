use std::sync::{Arc, Mutex};

use static_ecs::prelude::*;
use static_ecs::{From, To};

#[derive(Debug, PartialEq)]
struct Hp(u32);
struct Prefab;
impl Disabling for Prefab {}

/// The `Hp` values a query sees, sorted.
macro_rules! seen {
    ($world:expr, $data:ty, $filter:ty, $hp:expr) => {{
        let mut values: Vec<u32> = $world
            .query_filtered::<$data, $filter>()
            .iter_mut()
            .map($hp)
            .collect();
        values.sort_unstable();
        values
    }};
}

#[test]
fn queries_skip_disabled_rows_unless_they_mention_the_component() {
    let mut world = App::new()
        .register::<Hp>()
        .register::<Disabled>()
        .register::<Prefab>()
        .build()
        .world;
    let enabled = world.spawn_bundle((Hp(0),));
    let disabled = world.spawn_bundle((Hp(1), Disabled));
    let prefab = world.spawn_bundle((Hp(2), Prefab));
    assert_eq!(seen!(world, &Hp, (), |h| h.0), [0]);
    // Fetching, optionally fetching, or testing the component mentions it.
    assert_eq!(seen!(world, (&Hp, &Disabled), (), |(h, _)| h.0), [1]);
    assert_eq!(
        seen!(world, (&Hp, Option<&Disabled>), (), |(h, _)| h.0),
        [0, 1]
    );
    assert_eq!(seen!(world, (&Hp, Has<Disabled>), (), |(h, _)| h.0), [0, 1]);
    assert_eq!(seen!(world, &Hp, With<Disabled>, |h| h.0), [1]);
    assert_eq!(seen!(world, &Hp, Without<Disabled>, |h| h.0), [0]);
    assert_eq!(seen!(world, &Hp, MayHave<Disabled>, |h| h.0), [0, 1]);
    assert_eq!(
        seen!(world, &Hp, Or<(With<Disabled>, With<Prefab>)>, |h| h.0),
        [1, 2]
    );
    // Each disabling component needs its own mention.
    assert_eq!(seen!(world, &Hp, MayHave<Prefab>, |h| h.0), [0, 2]);
    assert_eq!(
        seen!(world, &Hp, (MayHave<Prefab>, MayHave<Disabled>), |h| h.0),
        [0, 1, 2]
    );
    // Lookups by entity agree; direct access ignores default filters.
    assert!(world.query::<&Hp>().get_mut(enabled).is_some());
    assert!(world.query::<&Hp>().get_mut(disabled).is_none());
    assert!(world.query::<&Hp>().get_mut(prefab).is_none());
    assert!(
        world
            .query_filtered::<&Hp, MayHave<Disabled>>()
            .get_mut(disabled)
            .is_some()
    );
    assert_eq!(world.get::<Hp>(disabled), Some(&Hp(1)));
    // Enabling is removing the component.
    world.remove::<Disabled>(disabled);
    assert_eq!(seen!(world, &Hp, (), |h| h.0), [0, 1]);
}

#[test]
fn cached_system_queries_follow_entities_in_and_out_of_disabled_tables() {
    struct Update;
    impl Schedule for Update {}
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let mut app = App::new()
        .register::<Disabled>()
        .add_systems(Update, move |mut q: Query<&Hp>| {
            let mut values: Vec<_> = q.iter_mut().map(|h| h.0).collect();
            values.sort_unstable();
            *log.lock().unwrap() = values;
        })
        .build();
    let a = app.world.spawn_bundle((Hp(0),));
    app.world.spawn_bundle((Hp(1),));
    app.emit(&Update);
    assert_eq!(*seen.lock().unwrap(), [0, 1]);
    // The disabled table appears after the query cached its tables.
    app.world.insert(a, Disabled);
    app.emit(&Update);
    assert_eq!(*seen.lock().unwrap(), [1]);
    app.world.remove::<Disabled>(a);
    app.emit(&Update);
    assert_eq!(*seen.lock().unwrap(), [0, 1]);
}

struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
}
struct InSquad;
impl Relation for InSquad {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
}

#[test]
fn joins_skip_disabled_targets_unless_the_inner_query_mentions_the_component() {
    let mut app = App::new()
        .relation::<ChildOf>()
        .relation::<InSquad>()
        .register::<Hp>()
        .register::<Disabled>()
        .build();
    let world = &mut app.world;
    let hidden = world.spawn_bundle((Hp(10), Disabled));
    let shown = world.spawn_bundle((Hp(20),));
    let a = world.spawn_bundle((Hp(1),));
    let b = world.spawn_bundle((Hp(2), Disabled));
    world.relate(a, ChildOf, hidden);
    world.relate(a, InSquad, hidden);
    world.relate(b, ChildOf, shown);
    world.relate(b, InSquad, shown);
    let c = world.spawn_bundle((Hp(3),));
    world.relate(c, ChildOf, shown);
    world.relate(c, InSquad, shown);

    // `To`: linked, and fragmenting (resolved once per table).
    let mut q = world.query::<To<ChildOf, &Hp>>();
    assert_eq!(q.get_mut(a).unwrap().map(|h| h.0), None);
    let mut q = world.query::<To<ChildOf, (&Hp, Has<Disabled>)>>();
    assert_eq!(
        q.get_mut(a).unwrap().map(|(h, d)| (h.0, d)),
        Some((10, true))
    );
    let mut q = world.query::<To<InSquad, &Hp>>();
    assert_eq!(q.get_mut(a).unwrap().map(|h| h.0), None);
    let mut q = world.query::<To<InSquad, (&Hp, Has<Disabled>)>>();
    assert_eq!(
        q.get_mut(a).unwrap().map(|(h, d)| (h.0, d)),
        Some((10, true))
    );

    // `From`: linked, and fragmenting (whole source tables).
    let mut q = world.query::<From<ChildOf, &Hp>>();
    let mut children: Vec<_> = q.get_mut(shown).unwrap().map(|h| h.0).collect();
    children.sort_unstable();
    assert_eq!(children, [3]);
    let mut q = world.query::<From<ChildOf, (&Hp, Has<Disabled>)>>();
    let mut children: Vec<_> = q.get_mut(shown).unwrap().map(|(h, _)| h.0).collect();
    children.sort_unstable();
    assert_eq!(children, [2, 3]);
    let mut q = world.query::<From<InSquad, &Hp>>();
    let mut members: Vec<_> = q.get_mut(shown).unwrap().map(|h| h.0).collect();
    members.sort_unstable();
    assert_eq!(members, [3]);
    let mut q = world.query::<From<InSquad, (&Hp, Option<&Disabled>)>>();
    let mut members: Vec<_> = q.get_mut(shown).unwrap().map(|(h, _)| h.0).collect();
    members.sort_unstable();
    assert_eq!(members, [2, 3]);
}
