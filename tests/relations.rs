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
#[should_panic(expected = "install the relation")]
fn relating_without_installing_the_relation_panics() {
    let mut app = App::new()
        .register::<Relates<Likes>>()
        .register::<Sources<Likes>>()
        .build();
    let (a, b) = (app.world.spawn(), app.world.spawn());
    app.world.relate::<Likes>(a, b);
}

struct InSquad;
impl Relation for InSquad {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
}
struct Tagged;
impl Relation for Tagged {
    type Kind = ManyToMany;
    const FRAGMENTING: bool = true;
}

/// Sources of `target` through the table-level path, by name.
fn related<R: Relation>(
    world: &mut World<impl Has<Name> + Has<Relates<R>>>,
    target: Entity,
) -> Vec<u32> {
    let mut query = world.query_filtered::<&Name, Related<R>>();
    let mut names: Vec<_> = query.iter_related(target).map(|n| n.0).collect();
    names.sort_unstable();
    names
}

#[test]
fn fragmenting_one_to_many_groups_each_targets_sources_into_tables() {
    let mut app = App::new()
        .relation::<InSquad, _>()
        .register::<Name>()
        .register::<Elite>()
        .build();
    let world = &mut app.world;
    let [alpha, beta] = [100, 200].map(|i| world.spawn_bundle((Name(i),)));
    let members: Vec<_> = (0..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    for (i, &member) in members.iter().enumerate() {
        world.relate::<InSquad>(member, if i % 2 == 0 { alpha } else { beta });
    }
    assert_eq!(related::<InSquad>(world, alpha), [0, 2, 4]);
    assert_eq!(related::<InSquad>(world, beta), [1, 3, 5]);
    // Nothing besides the source's table key stores the relation.
    assert!(world.get::<Relates<InSquad>>(members[0]).is_none());
    assert!(world.get::<Sources<InSquad>>(alpha).is_none());
    assert_eq!(world.targets::<InSquad>(members[0]), [alpha]);
    // Reparenting is one move between the squads' tables.
    world.relate::<InSquad>(members[0], beta);
    assert_eq!(related::<InSquad>(world, alpha), [2, 4]);
    assert_eq!(related::<InSquad>(world, beta), [0, 1, 3, 5]);
    assert_eq!(world.targets::<InSquad>(members[0]), [beta]);
    assert!(world.unrelate::<InSquad>(members[1], beta));
    assert!(!world.unrelate::<InSquad>(members[1], beta));
    assert_eq!(related::<InSquad>(world, beta), [0, 3, 5]);
    assert_eq!(world.sources::<InSquad>(beta).len(), 3);
    // Plain iteration still sees every source, whatever its table.
    assert_eq!(
        world
            .query_filtered::<&Name, Related<InSquad>>()
            .iter_mut()
            .count(),
        5
    );
    // Components added later keep the entity in its target's table.
    world.insert(members[3], Elite);
    assert_eq!(related::<InSquad>(world, beta), [0, 3, 5]);
}
struct Elite;

#[test]
fn fragmenting_many_to_many_keys_tables_by_target_sets() {
    let mut app = App::new()
        .relation::<Tagged, _>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let [red, blue] = [100, 200].map(|i| world.spawn_bundle((Name(i),)));
    let things: Vec<_> = (0..4).map(|i| world.spawn_bundle((Name(i),))).collect();
    world.relate::<Tagged>(things[0], red);
    world.relate::<Tagged>(things[1], red);
    world.relate::<Tagged>(things[1], blue);
    world.relate::<Tagged>(things[2], blue);
    assert_eq!(related::<Tagged>(world, red), [0, 1]);
    assert_eq!(related::<Tagged>(world, blue), [1, 2]);
    world.unrelate::<Tagged>(things[1], red);
    assert_eq!(related::<Tagged>(world, red), [0]);
    assert_eq!(related::<Tagged>(world, blue), [1, 2]);
    // A despawned target leaves its sources in tables without its pair.
    world.despawn(blue);
    assert!(world.targets::<Tagged>(things[2]).is_empty());
    assert_eq!(world.targets::<Tagged>(things[1]), Vec::<Entity>::new());
    assert_eq!(related::<Tagged>(world, red), [0]);
    assert_eq!(
        world
            .query_filtered::<&Name, Related<Tagged>>()
            .iter_mut()
            .count(),
        1
    );
}

#[test]
fn churning_fragmenting_targets_reuses_dead_tables() {
    let mut app = App::new()
        .relation::<InSquad, _>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let mut tables_after_first = None;
    for round in 0..50 {
        let squad = world.spawn_bundle((Name(1000),));
        let members: Vec<_> = (0..3).map(|i| world.spawn_bundle((Name(i),))).collect();
        for &member in &members {
            world.relate::<InSquad>(member, squad);
        }
        assert_eq!(related::<InSquad>(world, squad), [0, 1, 2], "round {round}");
        for member in members {
            world.despawn(member);
        }
        world.despawn(squad);
        let tables = world.archetype_count();
        tables_after_first.get_or_insert(tables);
        assert_eq!(
            Some(tables),
            tables_after_first,
            "round {round}: tables grew"
        );
    }
}

#[test]
fn descendants_come_parents_first_for_both_storages() {
    let mut app = App::new()
        .relation::<ChildOf, _>()
        .relation::<InSquad, _>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let names: Vec<_> = (0..7).map(|i| world.spawn_bundle((Name(i),))).collect();
    // 0 -> {1, 2}; 1 -> {3, 4}; 2 -> {5}; 5 -> {6}
    for (child, parent) in [(1, 0), (2, 0), (3, 1), (4, 1), (5, 2), (6, 5)] {
        world.relate::<ChildOf>(names[child], names[parent]);
        world.relate::<InSquad>(names[child], names[parent]);
    }
    for order in [
        world.descendants::<ChildOf>(names[0]),
        world.descendants::<InSquad>(names[0]),
    ] {
        let position = |i: usize| order.iter().position(|&e| e == names[i]).unwrap();
        assert_eq!(order.len(), 6);
        for (child, parent) in [(3, 1), (4, 1), (5, 2), (6, 5)] {
            assert!(position(parent) < position(child));
        }
    }
}

struct Mount;
impl Relation for Mount {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
    const LINKED: bool = true;
}

#[test]
fn linked_fragmenting_relations_despawn_subtrees_and_terminate_on_cycles() {
    let mut app = App::new().relation::<Mount, _>().register::<Name>().build();
    let world = &mut app.world;
    let names: Vec<_> = (0..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    // 0 -> {1, 2}; 1 -> {3}; 3 -> {4}; 5 stays separate.
    for (child, parent) in [(1, 0), (2, 0), (3, 1), (4, 3)] {
        world.relate::<Mount>(names[child], names[parent]);
    }
    assert!(world.despawn(names[0]));
    for gone in &names[..5] {
        assert!(!world.contains(*gone));
    }
    assert!(world.contains(names[5]));
    // A cycle: each despawn reaches the other, and both go exactly once.
    let [a, b] = [7, 8].map(|i| world.spawn_bundle((Name(i),)));
    world.relate::<Mount>(a, b);
    world.relate::<Mount>(b, a);
    assert!(world.despawn(a));
    assert!(!world.contains(a) && !world.contains(b));
    assert_eq!(world.query::<&Name>().iter_mut().count(), 1);
}

#[test]
fn fragmenting_queries_read_targets_and_subtrees_inside_systems() {
    struct Update;
    struct Log(Vec<(u32, Option<u32>)>, usize);
    let mut app = App::new()
        .relation::<InSquad, _>()
        .add_systems(
            Update,
            |members: Query<(Entity, &Name), Related<InSquad>>,
             names: Query<&Name>,
             mut log: Single<&mut Log>| {
                let mut members = members;
                let mut names = names;
                let pairs: Vec<_> = members.iter_mut().map(|(e, n)| (e, n.0)).collect();
                for (entity, name) in pairs {
                    let target = members.target(entity);
                    let target = target.and_then(|t| names.get_mut(t)).map(|n| n.0);
                    log.0.push((name, target));
                }
                let root = members.iter_mut().find(|(_, n)| n.0 == 1).map(|(e, _)| e);
                let parent = root.and_then(|r| members.target(r)).unwrap();
                log.1 = members.related_descendants(parent).len();
            },
        )
        .build();
    let world = &mut app.world;
    let [top, mid, leaf] = [0, 1, 2].map(|i| world.spawn_bundle((Name(i),)));
    world.relate::<InSquad>(mid, top);
    world.relate::<InSquad>(leaf, mid);
    let log = world.spawn_bundle((Log(Vec::new(), 0),));
    app.emit(&Update);
    let mut seen = app.world.get::<Log>(log).unwrap().0.clone();
    seen.sort_unstable();
    assert_eq!(seen, [(1, Some(0)), (2, Some(1))]);
    assert_eq!(app.world.get::<Log>(log).unwrap().1, 2);
}
