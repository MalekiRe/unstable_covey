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

struct InPanel;
impl Relation for InPanel {
    type Kind = OneToMany;
    const LINKED: bool = true;
    const ACYCLIC: bool = true;
    const ORDERED: bool = true;
}
#[derive(Default)]
struct Closed;
impl Disabling for Closed {}

#[test]
fn subtree_toggles_hide_descendants_and_keep_nested_closes() {
    let mut app = App::new()
        .relation::<InPanel>()
        .register::<Hp>()
        .register::<Closed>()
        .register::<DisabledRoot<Closed>>()
        .build();
    let world = &mut app.world;
    // panel(1) -> [row(2) -> [menu(3) -> item(4)], row(5)]; other(6)
    let panel = world.spawn_bundle((Hp(1),));
    let row = world.spawn_bundle((Hp(2), InPanel.to(panel)));
    let menu = world.spawn_bundle((Hp(3), InPanel.to(row)));
    world.spawn_bundle((Hp(4), InPanel.to(menu)));
    world.spawn_bundle((Hp(5), InPanel.to(panel)));
    world.spawn_bundle((Hp(6),));
    let all = |world: &mut World<_>| seen!(world, &Hp, (), |h: &Hp| h.0);
    let closed = |world: &mut World<_>| seen!(world, &Hp, With<Closed>, |h: &Hp| h.0);

    world.disable_subtree::<Closed, InPanel>(menu);
    assert_eq!(all(world), [1, 2, 5, 6]);
    world.disable_subtree::<Closed, InPanel>(panel);
    assert_eq!(all(world), [6]);
    // Closing again, or a node inside, changes nothing visible.
    world.disable_subtree::<Closed, InPanel>(panel);
    world.disable_subtree::<Closed, InPanel>(row);
    assert_eq!(closed(world), [1, 2, 3, 4, 5]);
    // Opening `row` while `panel` is closed only drops its own mark.
    world.enable_subtree::<Closed, InPanel>(row);
    assert_eq!(all(world), [6]);
    // Opening `panel` reopens everything but the menu closed on its own.
    world.enable_subtree::<Closed, InPanel>(panel);
    assert_eq!(all(world), [1, 2, 5, 6]);
    world.enable_subtree::<Closed, InPanel>(menu);
    assert_eq!(all(world), [1, 2, 3, 4, 5, 6]);
    // Opening what is open is a no-op.
    world.enable_subtree::<Closed, InPanel>(panel);
    assert_eq!(all(world), [1, 2, 3, 4, 5, 6]);
    assert_eq!(seen!(world, &Hp, With<DisabledRoot<Closed>>, |h: &Hp| h.0), Vec::<u32>::new());
}

/// Panels: hierarchy roots, open or closed.
type Roots<'w> = Query<'w, (Entity, Has<Closed>), (Without<Relates<InPanel>>, MayHave<Closed>)>;

#[test]
fn subtree_toggles_through_commands_apply_after_the_frame() {
    struct Toggle;
    impl Schedule for Toggle {}
    let mut app = App::new()
        .relation::<InPanel>()
        .register::<Hp>()
        .add_systems(Toggle, |mut commands: Commands<_>, mut roots: Roots| {
            roots.each(|(root, closed)| {
                if closed {
                    commands.enable_subtree::<Closed, InPanel>(root);
                } else {
                    commands.disable_subtree::<Closed, InPanel>(root);
                }
            });
        })
        .register::<DisabledRoot<Closed>>()
        .build();
    let world = &mut app.world;
    let root = world.spawn_bundle((Hp(1),));
    for i in 0..100 {
        world.spawn_bundle((Hp(10 + i), InPanel.to(root)));
    }
    app.emit(&Toggle);
    assert_eq!(seen!(app.world, &Hp, (), |h: &Hp| h.0).len(), 0);
    app.emit(&Toggle);
    assert_eq!(seen!(app.world, &Hp, (), |h: &Hp| h.0).len(), 101);
}

#[test]
fn queries_that_never_see_a_disabling_component_run_beside_ones_requiring_it() {
    // The first query cannot see `Prefab` rows; the second sees only those.
    let mut app = App::new()
        .add_systems(
            (),
            (
                |mut live: Query<&mut Hp>| live.iter_mut().for_each(|mut hp| hp.0 += 1),
                |mut prefabs: Query<&mut Hp, With<Prefab>>| prefabs.iter_mut().for_each(|mut hp| hp.0 += 100),
            )
                .parallel(),
        )
        .build();
    let live = app.world.spawn_bundle((Hp(0),));
    let prefab = app.world.spawn_bundle((Hp(0), Prefab));
    app.emit(&());
    assert_eq!(app.world.get::<Hp>(live), Some(&Hp(1)));
    assert_eq!(app.world.get::<Hp>(prefab), Some(&Hp(100)));
}
