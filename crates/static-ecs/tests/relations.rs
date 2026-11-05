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
    let mut app = App::new().relation::<Likes>().register::<Name>().build();
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
                world.relate(a, Likes, b);
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
        // The joins see exactly the model's edges, both ways.
        let mut q = world.query::<(
            Entity,
            static_ecs::To<Likes, Entity>,
            static_ecs::From<Likes, Entity>,
        )>();
        let mut seen = (BTreeSet::new(), BTreeSet::new());
        for (e, to, from) in q.iter_mut() {
            seen.0.extend(to.map(|t| (index(e), index(t))));
            seen.1.extend(from.map(|s| (index(s), index(e))));
        }
        assert_eq!((&seen.0, &seen.1), (&model, &model), "joins at step {step}");
    }
}

#[test]
fn linked_hierarchies_reparent_and_despawn_recursively() {
    let mut app = App::new().relation::<ChildOf>().register::<Name>().build();
    let world = &mut app.world;
    let root = world.spawn_bundle((Name(0),));
    let other = world.spawn_bundle((Name(1),));
    let children: Vec<_> = (2..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    for &child in &children {
        world.relate(child, ChildOf, root);
    }
    let grandchild = world.spawn_bundle((Name(9),));
    world.relate(grandchild, ChildOf, children[1]);
    assert_eq!(
        world.get::<Relates<ChildOf>>(grandchild).unwrap().target(),
        children[1]
    );
    assert_eq!(world.get::<Sources<ChildOf>>(root).unwrap().len(), 4);

    // Reparent: one target per source, so the old parent loses the child.
    world.relate(children[0], ChildOf, other);
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
    let mut app = App::new().relation::<Partner>().register::<Name>().build();
    let world = &mut app.world;
    let [a, b, c] = [0, 1, 2].map(|i| world.spawn_bundle((Name(i),)));
    world.relate(a, Partner, b);
    world.relate(c, Partner, b); // b already had a source: a is dropped
    assert!(world.get::<Relates<Partner>>(a).is_none());
    assert_eq!(world.get::<Sources<Partner>>(b).unwrap().source(), c);
    world.relate(c, Partner, a); // c moves to a; b is left without sources
    assert!(world.get::<Sources<Partner>>(b).is_none());
    assert_eq!(world.get::<Sources<Partner>>(a).unwrap().source(), c);
    // Removing the component by hand still unlinks both sides.
    world.remove::<Relates<Partner>>(c);
    assert!(world.get::<Sources<Partner>>(a).is_none());
    // Unlinked target despawn leaves sources alive.
    world.relate(a, Partner, b);
    world.despawn(b);
    assert!(world.contains(a) && world.get::<Relates<Partner>>(a).is_none());
}

#[test]
fn commands_relate_freshly_spawned_entities() {
    struct Update;
    impl Schedule for Update {}
    let mut app = App::new()
        .relation::<ChildOf>()
        .add_systems(Update, |mut commands: Commands<_>| {
            let parent = commands.spawn((Name(0),));
            for i in 1..=3 {
                let child = commands.spawn((Name(i),));
                commands.relate(child, ChildOf, parent);
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
    app.world.relate(a, Likes, b);
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

/// Sources of `target` by name, through a `From` join.
macro_rules! related {
    ($relation:ty, $world:expr, $target:expr) => {{
        let mut query = $world.query::<static_ecs::From<$relation, &Name>>();
        let names = query.get_mut($target).into_iter().flatten().map(|n| n.0);
        sorted(names.collect::<Vec<_>>())
    }};
}

fn ids(entities: impl Iterator<Item = Entity>) -> Vec<Entity> {
    entities.collect()
}

#[test]
fn fragmenting_one_to_many_groups_each_targets_sources_into_tables() {
    let mut app = App::new()
        .relation::<InSquad>()
        .register::<Name>()
        .register::<Elite>()
        .build();
    let world = &mut app.world;
    let [alpha, beta] = [100, 200].map(|i| world.spawn_bundle((Name(i),)));
    let members: Vec<_> = (0..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    for (i, &member) in members.iter().enumerate() {
        world.relate(member, InSquad, if i % 2 == 0 { alpha } else { beta });
    }
    assert_eq!(related!(InSquad, world, alpha), [0, 2, 4]);
    assert_eq!(related!(InSquad, world, beta), [1, 3, 5]);
    // Nothing besides the source's table key stores the relation.
    assert!(world.get::<Relates<InSquad>>(members[0]).is_none());
    assert!(world.get::<Sources<InSquad>>(alpha).is_none());
    assert_eq!(ids(world.targets::<InSquad>(members[0])), [alpha]);
    // Reparenting is one move between the squads' tables.
    world.relate(members[0], InSquad, beta);
    assert_eq!(related!(InSquad, world, alpha), [2, 4]);
    assert_eq!(related!(InSquad, world, beta), [0, 1, 3, 5]);
    assert_eq!(ids(world.targets::<InSquad>(members[0])), [beta]);
    assert!(world.unrelate::<InSquad>(members[1], beta));
    assert!(!world.unrelate::<InSquad>(members[1], beta));
    assert_eq!(related!(InSquad, world, beta), [0, 3, 5]);
    assert_eq!(ids(world.sources::<InSquad>(beta)).len(), 3);
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
    assert_eq!(related!(InSquad, world, beta), [0, 3, 5]);
}
struct Elite;

#[test]
fn fragmenting_many_to_many_keys_tables_by_target_sets() {
    let mut app = App::new().relation::<Tagged>().register::<Name>().build();
    let world = &mut app.world;
    let [red, blue] = [100, 200].map(|i| world.spawn_bundle((Name(i),)));
    let things: Vec<_> = (0..4).map(|i| world.spawn_bundle((Name(i),))).collect();
    world.relate(things[0], Tagged, red);
    world.relate(things[1], Tagged, red);
    world.relate(things[1], Tagged, blue);
    world.relate(things[2], Tagged, blue);
    assert_eq!(related!(Tagged, world, red), [0, 1]);
    assert_eq!(related!(Tagged, world, blue), [1, 2]);
    world.unrelate::<Tagged>(things[1], red);
    assert_eq!(related!(Tagged, world, red), [0]);
    assert_eq!(related!(Tagged, world, blue), [1, 2]);
    // A despawned target leaves its sources in tables without its pair.
    world.despawn(blue);
    assert!(ids(world.targets::<Tagged>(things[2])).is_empty());
    assert_eq!(
        ids(world.targets::<Tagged>(things[1])),
        Vec::<Entity>::new()
    );
    assert_eq!(related!(Tagged, world, red), [0]);
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
    let mut app = App::new().relation::<InSquad>().register::<Name>().build();
    let world = &mut app.world;
    let mut tables_after_first = None;
    for round in 0..50 {
        let squad = world.spawn_bundle((Name(1000),));
        let members: Vec<_> = (0..3).map(|i| world.spawn_bundle((Name(i),))).collect();
        for &member in &members {
            world.relate(member, InSquad, squad);
        }
        assert_eq!(related!(InSquad, world, squad), [0, 1, 2], "round {round}");
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
        .relation::<ChildOf>()
        .relation::<InSquad>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let names: Vec<_> = (0..7).map(|i| world.spawn_bundle((Name(i),))).collect();
    // 0 -> {1, 2}; 1 -> {3, 4}; 2 -> {5}; 5 -> {6}
    for (child, parent) in [(1, 0), (2, 0), (3, 1), (4, 1), (5, 2), (6, 5)] {
        world.relate(names[child], ChildOf, names[parent]);
        world.relate(names[child], InSquad, names[parent]);
    }
    for order in [
        ids(world.descendants::<ChildOf>(names[0])),
        ids(world.descendants::<InSquad>(names[0])),
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
    let mut app = App::new().relation::<Mount>().register::<Name>().build();
    let world = &mut app.world;
    let names: Vec<_> = (0..6).map(|i| world.spawn_bundle((Name(i),))).collect();
    // 0 -> {1, 2}; 1 -> {3}; 3 -> {4}; 5 stays separate.
    for (child, parent) in [(1, 0), (2, 0), (3, 1), (4, 3)] {
        world.relate(names[child], Mount, names[parent]);
    }
    assert!(world.despawn(names[0]));
    for gone in &names[..5] {
        assert!(!world.contains(*gone));
    }
    assert!(world.contains(names[5]));
    // A cycle: each despawn reaches the other, and both go exactly once.
    let [a, b] = [7, 8].map(|i| world.spawn_bundle((Name(i),)));
    world.relate(a, Mount, b);
    world.relate(b, Mount, a);
    assert!(world.despawn(a));
    assert!(!world.contains(a) && !world.contains(b));
    assert_eq!(world.query::<&Name>().iter_mut().count(), 1);
}

#[test]
fn fragmenting_joins_read_targets_and_subtrees_inside_systems() {
    use static_ecs::{From, To};
    struct Update;
    impl Schedule for Update {}
    struct Log(Vec<(u32, Option<u32>)>, usize);
    type Grandchildren = From<InSquad, From<InSquad, Entity>>;
    let mut app = App::new()
        .relation::<InSquad>()
        .add_systems(
            Update,
            |mut members: Query<(&Name, To<InSquad, &Name>), Related<InSquad>>,
             mut trees: Query<(&Name, Grandchildren)>,
             mut log: Single<&mut Log>| {
                for (name, squad) in &mut members {
                    log.0.push((name.0, squad.map(|s| s.0)));
                }
                // Two levels below the root, through nested joins.
                for (name, children) in &mut trees {
                    if name.0 == 0 {
                        log.1 = children
                            .map(|grandchildren| 1 + grandchildren.count())
                            .sum();
                    }
                }
            },
        )
        .build();
    let world = &mut app.world;
    let [top, mid, leaf] = [0, 1, 2].map(|i| world.spawn_bundle((Name(i),)));
    world.relate(mid, InSquad, top);
    world.relate(leaf, InSquad, mid);
    let log = world.spawn_bundle((Log(Vec::new(), 0),));
    app.emit(&Update);
    let mut seen = app.world.get::<Log>(log).unwrap().0.clone();
    seen.sort_unstable();
    assert_eq!(seen, [(1, Some(0)), (2, Some(1))]);
    assert_eq!(app.world.get::<Log>(log).unwrap().1, 2);
}

#[derive(Debug, PartialEq)]
struct DockedAt {
    berth: u8,
}
impl Relation for DockedAt {
    type Kind = OneToMany;
}

#[test]
fn bundle_values_relate_retarget_and_unrelate_with_edge_data() {
    let mut app = App::new().relation::<DockedAt>().register::<Name>().build();
    let world = &mut app.world;
    let (kestrel, osprey) = (
        world.spawn_bundle((Name(1),)),
        world.spawn_bundle((Name(2),)),
    );
    let ship = world.spawn_bundle((Name(3), DockedAt { berth: 3 }.to(kestrel)));
    let docked = world.get::<Relates<DockedAt>>(ship).unwrap();
    assert_eq!((docked.target(), docked.berth), (kestrel, 3));
    assert_eq!(ids(world.sources::<DockedAt>(kestrel)), [ship]);

    // Inserting again retargets: the old link goes, the new value arrives.
    world.insert(ship, DockedAt { berth: 5 }.to(osprey));
    let docked = world.get::<Relates<DockedAt>>(ship).unwrap();
    assert_eq!((docked.target(), docked.berth), (osprey, 5));
    assert!(world.get::<Sources<DockedAt>>(kestrel).is_none());
    assert_eq!(ids(world.sources::<DockedAt>(osprey)), [ship]);

    // Removing the link unrelates.
    world.remove::<Relates<DockedAt>>(ship);
    assert!(world.get::<Sources<DockedAt>>(osprey).is_none());

    // A dead target leaves no link behind.
    world.despawn(kestrel);
    world.insert(ship, DockedAt { berth: 1 }.to(kestrel));
    assert!(world.get::<Relates<DockedAt>>(ship).is_none());
}

#[test]
fn commands_spawn_related_entities_and_many_to_many_values_replace_edges() {
    struct Update;
    impl Schedule for Update {}
    struct Station(Entity);
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<Likes>()
        .register::<Name>()
        .add_systems(Update, |s: Single<&Station>, mut commands: Commands<_>| {
            commands.spawn((Name(20), DockedAt { berth: 7 }.to(s.0)));
        })
        .build();
    let dock = app.world.spawn_bundle((Name(9),));
    app.world.spawn_bundle((Station(dock),));
    app.emit(&Update);
    let ship = ids(app.world.sources::<DockedAt>(dock))[0];
    assert_eq!(app.world.get::<Relates<DockedAt>>(ship).unwrap().berth, 7);

    let world = &mut app.world;
    let station = world.spawn_bundle((Name(10),));
    let fan = world.spawn_bundle((Name(11),));
    let (a, b) = (
        world.spawn_bundle((Name(12),)),
        world.spawn_bundle((Name(13),)),
    );
    world.relate(fan, Likes, a);
    world.relate(fan, Likes, b);
    assert_eq!(ids(world.targets::<Likes>(fan)).len(), 2);
    // A value replaces every edge of a many-to-many source; `relate` adds one.
    world.insert(fan, Likes.to(station));
    assert_eq!(ids(world.targets::<Likes>(fan)), [station]);
    assert!(world.get::<Sources<Likes>>(a).is_none());
    world.relate(fan, Likes, a);
    let mut targets = ids(world.targets::<Likes>(fan));
    targets.sort();
    let mut expected = vec![station, a];
    expected.sort();
    assert_eq!(targets, expected);
}

#[test]
#[should_panic(expected = "cannot relate to itself")]
fn one_sided_relations_refuse_self_edges() {
    let mut app = App::new().relation::<DockedAt>().build();
    let ship = app.world.spawn();
    app.world.relate(ship, DockedAt { berth: 0 }, ship);
}

#[test]
fn many_to_many_relations_allow_self_edges() {
    let mut app = App::new().relation::<Likes>().build();
    let narcissus = app.world.spawn();
    app.world.relate(narcissus, Likes, narcissus);
    assert_eq!(ids(app.world.targets::<Likes>(narcissus)), [narcissus]);
}

#[test]
fn fragmenting_bundle_values_become_pairs() {
    let mut app = App::new().relation::<InSquad>().register::<Name>().build();
    let world = &mut app.world;
    let (alpha, beta) = (
        world.spawn_bundle((Name(1),)),
        world.spawn_bundle((Name(2),)),
    );
    let member = world.spawn_bundle((Name(3), InSquad.to(alpha)));
    assert_eq!(ids(world.targets::<InSquad>(member)), [alpha]);
    assert!(world.get::<Relates<InSquad>>(member).is_none());
    world.insert(member, InSquad.to(beta));
    assert_eq!(ids(world.targets::<InSquad>(member)), [beta]);
    assert_eq!(ids(world.sources::<InSquad>(beta)), [member]);
    assert!(ids(world.sources::<InSquad>(alpha)).is_empty());
}

#[derive(Debug, PartialEq)]
struct Fuel(u32);

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort_unstable();
    v
}

#[test]
fn non_fragmenting_joins_follow_relations_by_cardinality() {
    use static_ecs::{From, To};
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<Partner>()
        .relation::<Likes>()
        .register::<Name>()
        .register::<Fuel>()
        .build();
    let world = &mut app.world;
    let [kestrel, osprey] = [10, 11].map(|i| world.spawn_bundle((Name(i), Fuel(0))));
    let ships = [1, 2, 3].map(|i| world.spawn_bundle((Name(i), Fuel(i))));
    world.insert(ships[0], DockedAt { berth: 1 }.to(kestrel));
    world.insert(ships[1], DockedAt { berth: 2 }.to(kestrel));
    world.insert(ships[2], DockedAt { berth: 7 }.to(osprey));
    // A target without the joined data is skipped.
    let bare = world.spawn();
    let drifter = world.spawn_bundle((Name(4), DockedAt { berth: 0 }.to(bare)));

    // One target per source: an Option, with the edge data beside it.
    let mut q = world.query::<(&Name, &Relates<DockedAt>, To<DockedAt, &Name>)>();
    let docked = sorted(
        q.iter_mut()
            .map(|(n, d, s)| (n.0, d.berth, s.map(|s| s.0)))
            .collect(),
    );
    assert_eq!(
        docked,
        [
            (1, 1, Some(10)),
            (2, 2, Some(10)),
            (3, 7, Some(11)),
            (4, 0, None)
        ]
    );
    // Many sources per target: an iterator; rows without sources yield none.
    let mut q = world.query::<(&Name, From<DockedAt, &Name>)>();
    let crews = sorted(
        q.iter_mut()
            .map(|(n, s)| (n.0, sorted(s.map(|s| s.0).collect())))
            .collect(),
    );
    let none = Vec::new;
    assert_eq!(
        crews,
        [
            (1, none()),
            (2, none()),
            (3, none()),
            (4, none()),
            (10, vec![1, 2]),
            (11, vec![3])
        ]
    );

    // Sources write their shared target one row at a time.
    world
        .query::<(&Fuel, To<DockedAt, &mut Fuel>)>()
        .each(|(own, station)| {
            if let Some(mut station) = station {
                station.0 += own.0;
            }
        });
    let mut q = world.query::<(&Fuel, To<DockedAt, &mut Fuel>)>();
    let mut rows = q.iter_lending();
    while let Some((own, station)) = rows.fetch_next() {
        if let Some(mut station) = station {
            station.0 += own.0 * 100;
        }
    }
    assert_eq!(world.get::<Fuel>(kestrel), Some(&Fuel(303)));
    assert_eq!(world.get::<Fuel>(osprey), Some(&Fuel(303)));
    // A target writes its sources: distinct per row, so items coexist.
    let mut q = world.query::<(&Name, From<DockedAt, &mut Fuel>)>();
    let all: Vec<_> = q.iter_mut().flat_map(|(_, crew)| crew).collect();
    assert_eq!(all.len(), 3);
    for mut fuel in all {
        fuel.0 = 0;
    }
    assert!(
        ships
            .iter()
            .all(|&s| world.get::<Fuel>(s) == Some(&Fuel(0)))
    );
    assert_eq!(world.get::<Fuel>(drifter), None);

    // One-to-one both ways, writing the partner.
    world.relate(ships[0], Partner, ships[1]);
    let mut q = world.query::<(&Name, To<Partner, &mut Fuel>)>();
    for (name, partner) in q.iter_mut() {
        if let Some(mut partner) = partner {
            partner.0 = name.0;
        }
    }
    let mut q = world.query::<(&Name, From<Partner, &Fuel>)>();
    let back = sorted(
        q.iter_mut()
            .filter_map(|(n, p)| Some((n.0, p?.0)))
            .collect(),
    );
    assert_eq!(back, [(2, 0)]);
    assert_eq!(world.get::<Fuel>(ships[1]), Some(&Fuel(1)));

    // Many-to-many reads both ways, self-edges included.
    world.relate(ships[0], Likes, ships[0]);
    world.relate(ships[0], Likes, kestrel);
    world.relate(ships[2], Likes, kestrel);
    let mut q = world.query::<(&Name, To<Likes, &Name>, From<Likes, &Name>)>();
    let likes = sorted(
        q.iter_mut()
            .map(|(n, to, from)| {
                (
                    n.0,
                    sorted(to.map(|t| t.0).collect()),
                    sorted(from.map(|f| f.0).collect()),
                )
            })
            .filter(|(_, to, from)| !to.is_empty() || !from.is_empty())
            .collect(),
    );
    assert_eq!(
        likes,
        [
            (1, vec![1, 10], vec![1]),
            (3, vec![10], vec![]),
            (10, vec![], vec![1, 3])
        ]
    );
}

#[test]
fn fragmenting_joins_run_in_systems_and_infer_joined_components() {
    use static_ecs::{From, To};
    struct Update;
    impl Schedule for Update {}
    // `Fuel` is never registered: the joins alone place it in the world.
    let mut app = App::new()
        .relation::<InSquad>()
        .relation::<Tagged>()
        .register::<Name>()
        .add_systems(
            Update,
            |mut members: Query<(&Name, To<InSquad, &mut Fuel>)>| {
                members.each(|(name, squad)| {
                    if let Some(mut squad) = squad {
                        squad.0 += name.0;
                    }
                })
            },
        )
        .add_systems(
            Update,
            |mut squads: Query<From<InSquad, &mut Fuel>, With<Name>>| {
                for members in &mut squads {
                    for mut fuel in members {
                        fuel.0 += 1000;
                    }
                }
            },
        )
        .build();
    let world = &mut app.world;
    let [alpha, beta] = [10, 11].map(|i| world.spawn_bundle((Name(i), Fuel(0))));
    let members = [1, 2, 3].map(|i| world.spawn_bundle((Name(i), Fuel(0), InSquad.to(alpha))));
    world.insert(members[2], InSquad.to(beta));
    world.relate(members[0], Tagged, alpha);
    world.relate(members[0], Tagged, beta);
    world.relate(members[1], Tagged, beta);
    app.emit(&Update);
    let world = &mut app.world;
    let fuel = |world: &mut World<_>, e| world.get::<Fuel>(e).unwrap().0;
    // The two systems both write `Fuel`, so they ran one after the other.
    assert_eq!(fuel(world, alpha), 3);
    assert_eq!(fuel(world, beta), 3);
    assert!(members.iter().all(|&m| fuel(world, m) == 1000));

    // One target resolved per table; sources gathered by whole tables.
    let mut q = world.query::<(&Name, To<InSquad, &Name>, From<InSquad, &Name>)>();
    let rows = sorted(
        q.iter_mut()
            .map(|(n, to, from)| (n.0, to.map(|t| t.0), sorted(from.map(|f| f.0).collect())))
            .collect(),
    );
    let none = Vec::new;
    assert_eq!(
        rows,
        [
            (1, Some(10), none()),
            (2, Some(10), none()),
            (3, Some(11), none()),
            (10, None, vec![1, 2]),
            (11, None, vec![3]),
        ]
    );
    let mut q = world.query::<(&Name, To<Tagged, &Name>, From<Tagged, &Name>)>();
    let tags = sorted(
        q.iter_mut()
            .map(|(n, to, from)| {
                (
                    n.0,
                    sorted(to.map(|t| t.0).collect()),
                    sorted(from.map(|f| f.0).collect()),
                )
            })
            .collect(),
    );
    assert_eq!(
        tags,
        [
            (1, vec![10, 11], none()),
            (2, vec![11], none()),
            (3, none(), none()),
            (10, none(), vec![1]),
            (11, none(), vec![1, 2]),
        ]
    );
}

#[test]
fn relinked_matches_changed_links_for_both_storages() {
    struct Update;
    impl Schedule for Update {}
    #[derive(Default)]
    struct Seen(Vec<Vec<u32>>);
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<InSquad>()
        .add_systems(
            Update,
            |mut docked: Query<&Name, Relinked<DockedAt>>,
             mut squads: Query<&Name, Relinked<InSquad>>,
             mut seen: Single<&mut Seen>| {
                let docked = docked.iter_mut().map(|n| n.0);
                let squads = squads.iter_mut().map(|n| n.0 + 100);
                seen.0
                    .push(sorted(Iterator::chain(docked, squads).collect()));
            },
        )
        .build();
    let world = &mut app.world;
    let seen = world.spawn_bundle((Seen::default(),));
    let [a, b] = [10, 11].map(|i| world.spawn_bundle((Name(i),)));
    let ships = [1, 2].map(|i| world.spawn_bundle((Name(i), DockedAt { berth: 0 }.to(a))));
    let members = [1, 2].map(|i| world.spawn_bundle((Name(i), InSquad.to(a))));
    app.emit(&Update);
    app.emit(&Update);
    // Retarget one of each, and replace one edge's data in place.
    app.world.relate(ships[0], DockedAt { berth: 0 }, b);
    app.world.relate(members[1], InSquad, b);
    app.emit(&Update);
    app.world.relate(ships[1], DockedAt { berth: 4 }, a);
    // Relating an existing fragmenting pair changes nothing.
    app.world.relate(members[1], InSquad, b);
    app.emit(&Update);
    // Losing a target unrelates: nothing left to match.
    app.world.despawn(b);
    app.emit(&Update);
    let seen = &app.world.get::<Seen>(seen).unwrap().0;
    assert_eq!(seen[0], [1, 2, 101, 102]);
    assert_eq!(seen[1], Vec::<u32>::new());
    assert_eq!(seen[2], [1, 102]);
    assert_eq!(seen[3], [2]);
    assert_eq!(seen[4], Vec::<u32>::new());
}

struct Attached;
impl Relation for Attached {
    type Kind = OneToMany;
    const ACYCLIC: bool = true;
}
struct Carried;
impl Relation for Carried {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
    const ACYCLIC: bool = true;
}
struct Precedes;
impl Relation for Precedes {
    type Kind = ManyToMany;
    const ACYCLIC: bool = true;
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Local(i64);
#[derive(Clone, Copy, Debug, PartialEq)]
struct Global(i64);
struct Skip;

/// Random forests: relate (only edges the model allows), unrelate, and
/// exclude rows; after each step a cascade propagates `Global` and must
/// visit every row once, after its parent when the parent is a row.
macro_rules! cascade_matches_model {
    ($name:ident, $relation:ident) => {
        #[test]
        fn $name() {
            use static_ecs::To;
            let mut app = App::new()
                .relation::<$relation>()
                .register::<Local>()
                .register::<Global>()
                .register::<Skip>()
                .build();
            let world = &mut app.world;
            let n = if cfg!(miri) { 24 } else { 120 };
            let nodes: Vec<Entity> = (0..n)
                .map(|i| world.spawn_bundle((Local(i as i64 + 1), Global(0))))
                .collect();
            let mut parent: Vec<Option<usize>> = vec![None; n];
            let mut skipped = vec![false; n];
            let mut seed = 0x2545f4914f6cdd1d_u64;
            let mut next = |n: usize| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed % n as u64) as usize
            };
            let reaches = |parent: &[Option<usize>], mut at: usize, to: usize| loop {
                if at == to {
                    return true;
                }
                match parent[at] {
                    Some(up) => at = up,
                    None => return false,
                }
            };
            let steps = if cfg!(miri) { 60 } else { 1500 };
            for step in 0..steps {
                let (a, b) = (next(n), next(n));
                match next(10) {
                    0..=5 if !reaches(&parent, b, a) => {
                        world.relate(nodes[a], $relation, nodes[b]);
                        parent[a] = Some(b);
                    }
                    6 if parent[a].is_some() => {
                        assert!(world.unrelate::<$relation>(nodes[a], nodes[parent[a].unwrap()]));
                        parent[a] = None;
                    }
                    7 => {
                        if skipped[a] {
                            world.remove::<Skip>(nodes[a]);
                        } else {
                            world.insert(nodes[a], Skip);
                        }
                        skipped[a] = !skipped[a];
                    }
                    _ => {}
                }
                let mut order = Vec::new();
                world
                    .query_filtered::<(To<$relation, &Global>, (Entity, &Local, &mut Global)), Without<Skip>>()
                    .cascade(|(up, (entity, local, mut global))| {
                        global.0 = up.map_or(0, |up| up.0) + local.0;
                        order.push(entity);
                    });
                let index = |e: Entity| nodes.iter().position(|&x| x == e).unwrap();
                let order: Vec<usize> = order.into_iter().map(index).collect();
                let mut position = vec![None; n];
                for (at, &i) in order.iter().enumerate() {
                    assert!(position[i].replace(at).is_none(), "{i} twice at step {step}");
                }
                for i in 0..n {
                    assert_eq!(position[i].is_some(), !skipped[i], "{i} at step {step}");
                    if let (Some(at), Some(up)) = (position[i], parent[i])
                        && let Some(up_at) = position[up]
                    {
                        assert!(up_at < at, "{i} before its parent {up} at step {step}");
                    }
                    if !skipped[i] {
                        let up = parent[i].map_or(0, |up| world.get::<Global>(nodes[up]).unwrap().0);
                        let global = world.get::<Global>(nodes[i]).unwrap().0;
                        assert_eq!(global, up + i as i64 + 1, "{i} at step {step}");
                    }
                }
            }
        }
    };
}
cascade_matches_model!(cascade_orders_parents_first_through_relates, Attached);
cascade_matches_model!(cascade_orders_parents_first_through_pairs, Carried);

#[test]
#[should_panic(expected = "would close a cycle")]
fn acyclic_relations_refuse_closing_a_chain() {
    let mut app = App::new().relation::<Attached>().build();
    let world = &mut app.world;
    let [a, b, c] = [(); 3].map(|()| world.spawn());
    world.relate(a, Attached, b);
    world.relate(b, Attached, c);
    world.relate(c, Attached, a);
}

#[test]
#[should_panic(expected = "would close a cycle")]
fn acyclic_fragmenting_bundles_refuse_cycles() {
    let mut app = App::new().relation::<Carried>().build();
    let world = &mut app.world;
    let a = world.spawn();
    let b = world.spawn_bundle((Carried.to(a),));
    world.insert(a, Carried.to(b));
}

#[test]
fn acyclic_many_to_many_allows_diamonds_and_refuses_back_edges() {
    let mut app = App::new().relation::<Precedes>().build();
    let world = &mut app.world;
    let [top, left, right, bottom] = [(); 4].map(|()| world.spawn());
    for (source, target) in [(left, top), (right, top), (bottom, left), (bottom, right)] {
        world.relate(source, Precedes, target);
    }
    for (source, target) in [(top, bottom), (left, bottom), (top, top)] {
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            world.relate(source, Precedes, target);
        }));
        assert!(refused.is_err());
    }
    assert_eq!(ids(world.targets::<Precedes>(bottom)).len(), 2);
    assert!(ids(world.targets::<Precedes>(top)).is_empty());
}

#[test]
fn cascade_inside_a_system_propagates_every_frame() {
    use static_ecs::To;
    struct Update;
    impl Schedule for Update {}
    type Transforms = (
        To<Attached, &'static Global>,
        (&'static Local, &'static mut Global),
    );
    let mut app = App::new()
        .relation::<Attached>()
        .add_systems(Update, |mut tree: Query<Transforms>| {
            tree.cascade(|(up, (local, mut global))| {
                global.0 = up.map_or(0, |up| up.0) + local.0;
            });
        })
        .build();
    let world = &mut app.world;
    // Children spawned before their parents: plain iteration would see stale parents.
    let [leaf, mid, root] = [100, 10, 1].map(|i| world.spawn_bundle((Local(i), Global(0))));
    world.relate(leaf, Attached, mid);
    world.relate(mid, Attached, root);
    let globals = |world: &World<_>| [leaf, mid, root].map(|e| world.get::<Global>(e).unwrap().0);
    app.emit(&Update);
    assert_eq!(globals(&app.world), [111, 11, 1]);
    app.world.insert(root, Local(2));
    app.emit(&Update);
    assert_eq!(globals(&app.world), [112, 12, 2]);
    // Reparent the leaf to the root.
    app.world.relate(leaf, Attached, root);
    app.emit(&Update);
    assert_eq!(globals(&app.world), [102, 12, 2]);
}

#[test]
fn joins_follow_retargets_and_despawns_in_both_storages() {
    use static_ecs::{From, To};
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<InSquad>()
        .register::<Name>()
        .build();
    let world = &mut app.world;
    let [a, b] = [10, 11].map(|i| world.spawn_bundle((Name(i),)));
    let unit = world.spawn_bundle((Name(1), DockedAt { berth: 0 }.to(a), InSquad.to(a)));
    let read = |world: &mut World<_>| {
        let mut q = world.query::<(To<DockedAt, &Name>, To<InSquad, &Name>)>();
        let (docked, squad) = q.get_mut(unit).unwrap();
        (docked.map(|n| n.0), squad.map(|n| n.0))
    };
    let crew = |world: &mut World<_>, e| {
        let mut q = world.query::<(From<DockedAt, Entity>, From<InSquad, Entity>)>();
        let (docked, squad) = q.get_mut(e).unwrap();
        (docked.count(), squad.count())
    };
    assert_eq!(read(world), (Some(10), Some(10)));
    // Retargeting moves the fragmenting source to another table: the table's
    // resolved target follows.
    world.insert(unit, InSquad.to(b));
    world.relate(unit, DockedAt { berth: 1 }, b);
    assert_eq!(read(world), (Some(11), Some(11)));
    assert_eq!((crew(world, a), crew(world, b)), ((0, 0), (1, 1)));
    // A despawned target unlinks: both joins come back empty.
    world.despawn(b);
    assert_eq!(read(world), (None, None));
}

#[test]
fn spawning_toward_a_target_despawned_in_the_same_flush_links_nothing() {
    struct Update;
    impl Schedule for Update {}
    struct Station(Entity);
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<InSquad>()
        .register::<Name>()
        .add_systems(Update, |s: Single<&Station>, mut commands: Commands<_>| {
            commands.spawn((Name(1), DockedAt { berth: 0 }.to(s.0), InSquad.to(s.0)));
            commands.despawn(s.0);
            commands.spawn((Name(2), DockedAt { berth: 0 }.to(s.0), InSquad.to(s.0)));
        })
        .build();
    let station = app.world.spawn_bundle((Name(9),));
    app.world.spawn_bundle((Station(station),));
    app.emit(&Update);
    let world = &mut app.world;
    assert!(!world.contains(station));
    let ships: Vec<_> = world
        .query::<(Entity, &Name)>()
        .iter_mut()
        .map(|(e, _)| e)
        .collect();
    assert_eq!(ships.len(), 2);
    for ship in ships {
        assert!(world.get::<Relates<DockedAt>>(ship).is_none());
        assert_eq!(world.targets::<InSquad>(ship).count(), 0);
    }
}

struct Ordered;
impl Relation for Ordered {
    type Kind = ManyToMany;
    const ORDERED: bool = true;
}

#[test]
fn ordered_relations_keep_both_sides_in_order_through_random_edits() {
    let mut app = App::new().relation::<Ordered>().register::<Name>().build();
    let world = &mut app.world;
    let mut alive: Vec<Entity> = (0..24).map(|i| world.spawn_bundle((Name(i),))).collect();
    // Model: each source's targets and each target's sources, in order.
    let mut targets = std::collections::HashMap::<Entity, Vec<Entity>>::new();
    let mut sources = std::collections::HashMap::<Entity, Vec<Entity>>::new();
    let mut seed = 0x2545f4914f6cdd1d_u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let steps = if cfg!(miri) { 150 } else { 4000 };
    for step in 0..steps {
        let (a, b) = (alive[next(alive.len())], alive[next(alive.len())]);
        match next(12) {
            0..=3 => {
                world.relate(a, Ordered, b);
                if !targets.entry(a).or_default().contains(&b) {
                    targets.get_mut(&a).unwrap().push(b);
                    sources.entry(b).or_default().push(a);
                }
            }
            4..=6 => {
                let position = next(6);
                world.relate_at(a, Ordered, b, position);
                if !targets.entry(a).or_default().contains(&b) {
                    targets.get_mut(&a).unwrap().push(b);
                }
                let list = sources.entry(b).or_default();
                list.retain(|&s| s != a);
                list.insert(position.min(list.len()), a);
            }
            7..=9 => {
                let existed = targets.get(&a).is_some_and(|t| t.contains(&b));
                assert_eq!(world.unrelate::<Ordered>(a, b), existed, "step {step}");
                if existed {
                    targets.get_mut(&a).unwrap().retain(|&t| t != b);
                    sources.get_mut(&b).unwrap().retain(|&s| s != a);
                }
            }
            _ if alive.len() > 8 => {
                world.despawn(a);
                alive.retain(|&e| e != a);
                for (_, list) in Iterator::chain(targets.iter_mut(), sources.iter_mut()) {
                    list.retain(|&e| e != a);
                }
                targets.remove(&a);
                sources.remove(&a);
            }
            _ => {}
        }
        for &e in &alive {
            let empty = Vec::new();
            assert_eq!(
                world.targets::<Ordered>(e).collect::<Vec<_>>(),
                *targets.get(&e).unwrap_or(&empty),
                "targets of {e:?}, step {step}"
            );
            assert_eq!(
                world.sources::<Ordered>(e).collect::<Vec<_>>(),
                *sources.get(&e).unwrap_or(&empty),
                "sources of {e:?}, step {step}"
            );
        }
    }
}

struct Child;
impl Relation for Child {
    type Kind = OneToMany;
    const LINKED: bool = true;
    const ORDERED: bool = true;
    const ACYCLIC: bool = true;
}

#[test]
fn ordered_children_survive_removal_reparenting_and_reordering_in_joins() {
    use static_ecs::From;
    let mut app = App::new().relation::<Child>().register::<Name>().build();
    let world = &mut app.world;
    let (root, other) = (world.spawn_bundle((Name(100),)), world.spawn_bundle((Name(200),)));
    let kids: Vec<Entity> = (0..5).map(|i| world.spawn_bundle((Name(i), Child.to(root)))).collect();
    let names = |world: &mut World<_>, parent| {
        world.query::<From<Child, &Name>>().get_mut(parent).unwrap().map(|n| n.0).collect::<Vec<_>>()
    };
    assert_eq!(names(world, root), [0, 1, 2, 3, 4]);
    world.despawn(kids[1]);
    assert_eq!(names(world, root), [0, 2, 3, 4]);
    world.relate_at(kids[4], Child, root, 0);
    assert_eq!(names(world, root), [4, 0, 2, 3]);
    world.relate_at(kids[4], Child, root, 99);
    assert_eq!(names(world, root), [0, 2, 3, 4]);
    world.relate_at(kids[2], Child, other, 0);
    assert_eq!(names(world, root), [0, 3, 4]);
    assert_eq!(names(world, other), [2]);
    world.relate_at(kids[0], Child, root, 2);
    assert_eq!(names(world, root), [3, 4, 0]);
    world.despawn(root);
    assert!(kids.iter().all(|&k| k == kids[2] || !world.contains(k)));
    assert!(world.contains(kids[2]));
}
