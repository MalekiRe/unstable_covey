//! A small fleet, told through relations: every cardinality, both storages,
//! edge data, joins in every direction and depth, the three ways to iterate
//! when joins write, `cascade`, `Relinked`, `LINKED` and `ACYCLIC`.
//!
//! `cargo run --release --example relations`
use static_ecs::prelude::*;
use static_ecs::{From, To};

// ---- Relations -------------------------------------------------------------

/// Ship → station, with the berth as edge data. Many ships per station.
struct DockedAt {
    berth: u8,
}
impl Relation for DockedAt {
    type Kind = OneToMany;
}

/// Pilot ↔ ship: one each way. Relating again displaces the old partner.
struct Piloting;
impl Relation for Piloting {
    type Kind = OneToOne;
}

/// Ship → flagship. Fragmenting: a fleet's ships share tables, so "every ship
/// of this fleet" is whole tables, and a ship's flagship is read once per table.
struct InFleet;
impl Relation for InFleet {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
}

/// Ship → ship, both ways allowed, any number.
struct Allied;
impl Relation for Allied {
    type Kind = ManyToMany;
}

/// Hunter → marks. Fragmenting many-to-many: tables keyed by target sets.
struct Hunting;
impl Relation for Hunting {
    type Kind = ManyToMany;
    const FRAGMENTING: bool = true;
}

/// Follower → leader. Acyclic, so a convoy always has a head and `cascade`
/// can move leaders before followers.
struct Follows;
impl Relation for Follows {
    type Kind = OneToMany;
    const ACYCLIC: bool = true;
}

/// Drone → carrier. Linked: the drones go down with their carrier.
struct Aboard;
impl Relation for Aboard {
    type Kind = OneToMany;
    const LINKED: bool = true;
}

// ---- Components ------------------------------------------------------------

struct Name(&'static str);
struct Cargo(u32);
struct Fuel(u32);
struct Hull(u32);
struct Skill(u32);
struct Pos(i32);
struct Gap(i32);
struct Speed(i32);
struct Carrier;

struct Update;
impl Schedule for Update {}

// ---- Systems ---------------------------------------------------------------

/// Docked ships unload into their station. Many ships share one station, so
/// the station's `&mut Cargo` is reachable from several rows: `for` would not
/// compile; `each` keeps one row alive at a time.
fn unload(mut ships: Query<(&mut Cargo, To<DockedAt, &mut Cargo>)>) {
    ships.each(|(mut hold, station)| {
        if let Some(mut station) = station {
            station.0 += std::mem::take(&mut hold.0);
        }
    });
}

/// Pilots patch their ship. One-to-one: each row reaches a different ship, so
/// a plain `for` loop works.
fn repair(mut pilots: Query<(&Skill, To<Piloting, &mut Hull>)>) {
    for (skill, ship) in &mut pilots {
        if let Some(mut hull) = ship {
            hull.0 = (hull.0 + skill.0).min(100);
        }
    }
}

/// The flagship hands 10 fuel to each ship of its fleet. A row writing its
/// own `Fuel` and its sources' `Fuel` is sound (a ship never sources
/// itself), but rows are not independent, so: the lending form.
fn refuel(mut flagships: Query<(&mut Fuel, From<InFleet, &mut Fuel>)>) {
    let mut rows = flagships.iter_lending();
    while let Some((mut tank, fleet)) = rows.fetch_next() {
        for mut ship in fleet {
            let given = tank.0.min(10);
            tank.0 -= given;
            ship.0 += given;
        }
    }
}

/// The convoy head moves; everyone else keeps their gap to whoever they
/// follow. `cascade` visits leaders first, so followers read updated positions.
fn advance(mut heads: Query<(&Speed, &mut Pos)>) {
    for (speed, mut pos) in &mut heads {
        pos.0 += speed.0;
    }
}
type Convoy = (To<Follows, &'static Pos>, (&'static Gap, &'static mut Pos));
fn keep_formation(mut convoy: Query<Convoy>) {
    convoy.cascade(|(leader, (gap, mut pos))| {
        if let Some(leader) = leader {
            pos.0 = leader.0 + gap.0;
        }
    });
}

type Docking = (
    &'static Name,
    &'static Relates<DockedAt>,
    To<DockedAt, &'static Name>,
);
/// Only ships whose docking changed since this system last ran.
fn harbor_log(mut moved: Query<Docking, Relinked<DockedAt>>) {
    for (ship, dock, station) in &mut moved {
        let station = station.map_or("?", |s| s.0);
        println!("  harbor: {} at {station}, berth {}", ship.0, dock.berth);
    }
}

/// Station rosters, two joins deep: docked ships, then each ship's pilot.
type Roster = (
    &'static Name,
    &'static Cargo,
    From<DockedAt, (&'static Name, From<Piloting, &'static Name>)>,
);
fn rosters(mut stations: Query<Roster>) {
    for (station, cargo, ships) in &mut stations {
        let ships: Vec<String> = ships
            .map(|(ship, pilot)| match pilot {
                Some(pilot) => format!("{} ({})", ship.0, pilot.0),
                None => format!("{} (no pilot)", ship.0),
            })
            .collect();
        if !ships.is_empty() || cargo.0 > 0 {
            println!(
                "  {} holds {} cargo; docked: {}",
                station.0,
                cargo.0,
                list(ships)
            );
        }
    }
}

/// Read-only joins both ways, for both many-to-many storages.
type Politics = (
    &'static Name,
    (To<Allied, &'static Name>, From<Allied, &'static Name>),
    From<Hunting, &'static Name>,
);
fn politics(mut ships: Query<Politics, With<Hull>>) {
    for (ship, (allies, allied_by), hunters) in &mut ships {
        let (allies, allied_by, hunters) = (names(allies), names(allied_by), names(hunters));
        println!(
            "  {}: allies {}, allied by {}, hunted by {}",
            ship.0,
            list(allies),
            list(allied_by),
            list(hunters)
        );
    }
}

/// Every ship's status, reading its flagship through the fragmenting join.
type Status = (
    &'static Name,
    (&'static Fuel, &'static Hull, &'static Pos),
    To<InFleet, &'static Name>,
);
fn status(mut ships: Query<Status>) {
    let mut lines: Vec<String> = Vec::new();
    for (ship, (fuel, hull, pos), flagship) in &mut ships {
        let fleet = flagship.map_or("-".to_string(), |f| f.0.to_string());
        lines.push(format!(
            "  {:<9} fuel {:>3}  hull {:>3}  pos {:>4}  fleet {fleet}",
            ship.0, fuel.0, hull.0, pos.0
        ));
    }
    lines.sort();
    lines.iter().for_each(|line| println!("{line}"));
}

fn names<'a>(joined: impl Iterator<Item = &'a Name>) -> Vec<String> {
    joined.map(|n| n.0.to_string()).collect()
}
fn list(items: Vec<String>) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

fn main() {
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<Piloting>()
        .relation::<InFleet>()
        .relation::<Allied>()
        .relation::<Hunting>()
        .relation::<Follows>()
        .relation::<Aboard>()
        .register::<Carrier>() // only filtered on: nothing else stores it
        // Carriers launch two drones each, once. Bundles relate on spawn.
        // (A closure: `Commands<_>` infers the world type.)
        .add_systems(
            Update,
            |mut carriers: Query<(Entity, &Name), With<Carrier>>,
             mut launched: Local<bool>,
             mut commands: Commands<_>| {
                if std::mem::replace(&mut *launched, true) {
                    return;
                }
                for (carrier, name) in &mut carriers {
                    println!("  {} launches 2 drones", name.0);
                    for drone in ["drone-a", "drone-b"] {
                        commands.spawn((Name(drone), Aboard.to(carrier)));
                    }
                }
            },
        )
        // Cargo and Hull are disjoint, joins included: proven at compile time.
        .add_systems(Update, (unload, repair).parallel())
        .add_systems(Update, refuel)
        .add_systems(Update, advance)
        .add_systems(Update, keep_formation)
        .add_systems(Update, harbor_log)
        .add_systems(Update, rosters)
        .add_systems(Update, politics)
        .add_systems(Update, status)
        .build();

    let world = &mut app.world;
    // Followers spawn before their leaders: iteration order would be wrong,
    // cascade order is not.
    let heron = world.spawn_bundle((
        Name("Heron"),
        Cargo(4),
        Fuel(20),
        Hull(40),
        Pos(0),
        Gap(-10),
    ));
    let osprey = world.spawn_bundle((
        Name("Osprey"),
        Cargo(7),
        Fuel(30),
        Hull(90),
        Pos(0),
        Gap(-10),
    ));
    let kestrel = world.spawn_bundle((
        Name("Kestrel"),
        Cargo(12),
        Fuel(50),
        Hull(70),
        Pos(0),
        Speed(5),
    ));
    let dauntless = world.spawn_bundle((
        Name("Dauntless"),
        Cargo(0),
        Fuel(200),
        Hull(100),
        Pos(-50),
        Carrier,
    ));
    let [ceres, vesta] = ["Ceres", "Vesta"].map(|n| world.spawn_bundle((Name(n), Cargo(0))));
    let vex = world.spawn_bundle((Name("Vex"),));
    let [ada, grace] =
        [("Ada", 3), ("Grace", 5)].map(|(n, s)| world.spawn_bundle((Name(n), Skill(s))));

    // Relate by value: bundles, inserts, or `relate`.
    world.insert(kestrel, DockedAt { berth: 1 }.to(ceres));
    world.insert(osprey, DockedAt { berth: 2 }.to(ceres));
    world.insert(heron, DockedAt { berth: 1 }.to(vesta));
    world.relate(ada, Piloting, kestrel);
    world.relate(grace, Piloting, osprey);
    for member in [heron, osprey, kestrel] {
        world.relate(member, InFleet, dauntless);
    }
    world.relate(osprey, Follows, kestrel);
    world.relate(heron, Follows, osprey);
    world.relate(kestrel, Allied, osprey);
    world.relate(osprey, Allied, kestrel);
    world.relate(heron, Allied, kestrel);
    world.relate(vex, Hunting, kestrel);
    world.relate(vex, Hunting, osprey);

    println!("== frame 1");
    app.emit(&Update);

    println!("== frame 2: Osprey moves to Vesta, Grace takes the Heron, Vex drops the Kestrel");
    let world = &mut app.world;
    world.insert(osprey, Cargo(9));
    world.insert(osprey, DockedAt { berth: 3 }.to(vesta)); // retarget: one insert
    world.relate(grace, Piloting, heron); // one-to-one: the Osprey loses its pilot
    world.unrelate::<Hunting>(vex, kestrel);
    // The convoy cannot close on itself: relating refuses (panics), and the
    // world is untouched. Quiet the default hook for this one expected panic.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let looped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.world.relate(kestrel, Follows, heron);
    }));
    std::panic::set_hook(hook);
    assert!(looped.is_err());
    println!("  (Kestrel following Heron was refused: Follows is acyclic)");
    app.emit(&Update);

    println!("== frame 3: Dauntless is lost; Ceres closes");
    let world = &mut app.world;
    let drones = world.descendants::<Aboard>(dauntless).count();
    world.despawn(dauntless); // drones go with it (LINKED); ships leave the fleet
    world.despawn(ceres); // Kestrel undocks (not LINKED)
    let drones_left = world
        .query_filtered::<Entity, Related<Aboard>>()
        .iter_mut()
        .count();
    println!("  {drones} drones were aboard, {drones_left} remain");
    println!(
        "  Kestrel docked: {}, Vesta's ships: {}",
        world.targets::<DockedAt>(kestrel).count() > 0,
        world.sources::<DockedAt>(vesta).count()
    );
    app.emit(&Update);
}
