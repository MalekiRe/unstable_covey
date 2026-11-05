use static_ecs::prelude::*;
use std::rc::Rc;

// A world holding an `Rc` could move to another thread while a clone of the
// `Rc` stays behind: worlds only store `Send + Sync` components.
fn main() {
    let mut app = App::new()
        .add_systems((), |q: Query<&Rc<u8>>| drop(q))
        .build();
    app.emit(&());
}
