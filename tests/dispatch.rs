use static_ecs::{App, Bus, Components, Single, World, subscribe};
use std::cell::RefCell;

mod first {
    pub struct Event(pub u32);
}
mod second {
    pub struct Event(pub u32);
}
struct Unhandled;

#[test]
fn specialization_filters_types_preserving_order_and_captures() {
    let log = RefCell::new(Vec::new());
    let mut count = 0;
    let mut bus = Bus::new().install((
        subscribe(|e: &first::Event| {
            count += 1;
            log.borrow_mut().push(("first", e.0 * count));
        }),
        (
            subscribe(|e: &second::Event| log.borrow_mut().push(("other", e.0))),
            subscribe(|e: &first::Event| log.borrow_mut().push(("last", e.0))),
        ),
    ));
    bus.emit(&first::Event(3));
    bus.emit(&Unhandled);
    bus.emit(&second::Event(7));
    bus.emit(&first::Event(5));
    assert_eq!(
        *log.borrow(),
        vec![
            ("first", 3),
            ("last", 3),
            ("other", 7),
            ("first", 10),
            ("last", 5)
        ]
    );
}

#[test]
fn schedule_labels_and_event_payloads_select_only_matching_systems() {
    struct Update;
    struct Set(u32);
    let mut world = World::<Components<u32>>::default();
    let entity = world.spawn();
    world.insert(entity, 0_u32);
    let mut app = App::new(world)
        .add_systems(Update, |mut n: Single<&mut u32>| **n += 1)
        .on(move |event: &Set, world: &mut World<Components<u32>>| {
            world.insert(entity, event.0);
        })
        .add_systems(Update, |mut n: Single<&mut u32>| **n *= 2);
    app.emit(&Set(10));
    assert_eq!(app.world.get::<u32>(entity), Some(&10));
    app.emit(&Update);
    assert_eq!(app.world.get::<u32>(entity), Some(&22));
    app.emit(&Unhandled);
    assert_eq!(app.world.get::<u32>(entity), Some(&22));
    app.emit(&Update);
    assert_eq!(app.world.get::<u32>(entity), Some(&46));
}
