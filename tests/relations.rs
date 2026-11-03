use static_ecs::prelude::*;
use std::collections::BTreeSet;

struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
    const LINKED: bool = true;
}
struct Likes;
impl Relation for Likes {
    type Kind = ManyToMany;
}
struct Partner;
impl Relation for Partner {
    type Kind = OneToOne;
}
struct Name(u32);

#[test]
fn many_to_many_matches_a_set_model_through_relates_unrelates_and_despawns() {
    let mut app = App::new().relation::<Likes, _>().register::<Name>().build();
    let world = &mut app.world;
    let mut alive: Vec<Entity> = (0..40).map(|i| world.spawn_bundle((Name(i),))).collect();
    let mut model = BTreeSet::<(usize, usize)>::new(); // indices into `ids`
    let ids = alive.clone();
    let index = |e: Entity| ids.iter().position(|&x| x == e).unwrap();
    let mut seed = 0x9e3779b97f4a7c15_u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    // Miri runs a prefix: every unsafe path, at interpretable cost.
    let steps = if cfg!(miri) { 200 } else { 3000 };
    for step in 0..steps {
        let (a, b) = (alive[next(alive.len())], alive[next(alive.len())]);
        match next(10) {
            0..=5 => {
                world.relate::<Likes>(a, b);
                model.insert((index(a), index(b)));
            }
            6..=8 => {
                let existed = model.remove(&(index(a), index(b)));
                assert_eq!(world.unrelate::<Likes>(a, b), existed, "step {step}");
            }
            _ if alive.len() > 10 => {
                world.despawn(a);
                alive.retain(|&e| e != a);
                model.retain(|&(s, t)| s != index(a) && t != index(a));
            }
            _ => {}
        }
        for &e in &alive {
            let i = index(e);
            let mut targets: Vec<_> = world
                .get::<Relates<Likes>>(e)
                .map_or_else(Vec::new, |r| r.targets().map(index).collect());
            targets.sort_unstable();
            let expected: Vec<_> = model.range((i, 0)..(i + 1, 0)).map(|p| p.1).collect();
            assert_eq!(targets, expected, "targets of {i} at step {step}");
            let mut sources: Vec<_> = world
                .get::<Sources<Likes>>(e)
                .map_or_else(Vec::new, |s| s.sources().map(index).collect());
            sources.sort_unstable();
            let expected: Vec<_> = model.iter().filter(|p| p.1 == i).map(|p| p.0).collect();
            assert_eq!(sources, expected, "sources of {i} at step {step}");
        }
    }
}

#[test]
fn linked_hierarchies_reparent_and_despawn_recursively() {
    let mut app = App::new()
        .relation::<ChildOf, _>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let root = world.spawn_bundle((Name(0),));
    let other = world.spawn_bundle((Name(1),));
    let children: Vec<_> = (2..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    for &child in &children {
        world.relate::<ChildOf>(child, root);
    }
    let grandchild = world.spawn_bundle((Name(9),));
    world.relate::<ChildOf>(grandchild, children[1]);
    assert_eq!(
        world.get::<Relates<ChildOf>>(grandchild).unwrap().target(),
        children[1]
    );
    assert_eq!(world.get::<Sources<ChildOf>>(root).unwrap().len(), 4);

    // Reparent: one target per source, so the old parent loses the child.
    world.relate::<ChildOf>(children[0], other);
    assert_eq!(world.get::<Sources<ChildOf>>(root).unwrap().len(), 3);
    assert_eq!(
        world
            .get::<Sources<ChildOf>>(other)
            .unwrap()
            .sources()
            .collect::<Vec<_>>(),
        [children[0]]
    );

    // Despawning the root despawns its subtree, but not the reparented child.
    world.despawn(root);
    for &gone in &children[1..] {
        assert!(!world.contains(gone));
    }
    assert!(!world.contains(grandchild));
    assert!(world.contains(children[0]) && world.contains(other));
    assert_eq!(world.query::<&Name>().iter_mut().count(), 2);
}

#[test]
fn one_to_one_displaces_previous_partners_and_unlinks_on_removal() {
    let mut app = App::new()
        .relation::<Partner, _>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let [a, b, c] = [0, 1, 2].map(|i| world.spawn_bundle((Name(i),)));
    world.relate::<Partner>(a, b);
    world.relate::<Partner>(c, b); // b already had a source: a is dropped
    assert!(world.get::<Relates<Partner>>(a).is_none());
    assert_eq!(world.get::<Sources<Partner>>(b).unwrap().source(), c);
    world.relate::<Partner>(c, a); // c moves to a; b is left without sources
    assert!(world.get::<Sources<Partner>>(b).is_none());
    assert_eq!(world.get::<Sources<Partner>>(a).unwrap().source(), c);
    // Removing the component by hand still unlinks both sides.
    world.remove::<Relates<Partner>>(c);
    assert!(world.get::<Sources<Partner>>(a).is_none());
    // Unlinked target despawn leaves sources alive.
    world.relate::<Partner>(a, b);
    world.despawn(b);
    assert!(world.contains(a) && world.get::<Relates<Partner>>(a).is_none());
}

#[test]
fn commands_relate_freshly_spawned_entities() {
    struct Update;
    let mut app = App::new()
        .relation::<ChildOf, _>()
        .add_systems(Update, |mut commands: Commands<_>| {
            let parent = commands.spawn((Name(0),));
            for i in 1..=3 {
                let child = commands.spawn((Name(i),));
                commands.relate::<ChildOf>(child, parent);
            }
        })
        .register::<Name>()
        .build();
    app.emit(&Update);
    let mut parents = app.world.query::<(&Name, &Sources<ChildOf>)>();
    let (name, children) = parents.iter_mut().next().unwrap();
    assert_eq!((name.0, children.len()), (0, 3));
}

#[test]
#[should_panic(expected = "install the relation's hooks")]
fn relating_without_installed_hooks_panics() {
    let mut app = App::new()
        .register::<Relates<Likes>>()
        .register::<Sources<Likes>>()
        .build();
    let (a, b) = (app.world.spawn(), app.world.spawn());
    app.world.relate::<Likes>(a, b);
}
