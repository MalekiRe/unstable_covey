use static_ecs::From;
use static_ecs::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct Label(&'static str);
#[derive(Clone, Copy, Debug, PartialEq)]
struct Size(u32);
/// Not `Clone`: prototypes keep it, copies do not.
#[derive(Default)]
struct Template;
impl Disabling for Template {}
/// Not `Clone` either.
struct Handle(#[allow(dead_code)] Box<u32>);

#[derive(Clone)]
struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
    const LINKED: bool = true;
    const ORDERED: bool = true;
}
struct Likes;
impl Relation for Likes {
    type Kind = ManyToMany;
}

fn world() -> World<impl Storage> {
    App::new()
        .relation::<ChildOf>()
        .relation::<Likes>()
        .register::<Label>()
        .register::<Size>()
        .register::<Template>()
        .register::<Handle>()
        .build()
        .world
}

#[test]
fn cloning_an_entity_copies_only_clone_components() {
    let mut world = world();
    let other = world.spawn_bundle((Label("other"),));
    let original = world.spawn_bundle((Label("a"), Size(3), Handle(Box::new(1))));
    world.relate(original, Likes, other);
    let copy = world.clone_entity(original).unwrap();
    assert_eq!(world.get::<Label>(copy), Some(&Label("a")));
    assert_eq!(world.get::<Size>(copy), Some(&Size(3)));
    assert!(world.get::<Handle>(copy).is_none());
    assert_eq!(world.targets::<Likes>(copy).count(), 0, "links are not copied");
    assert_eq!(world.targets::<Likes>(original).collect::<Vec<_>>(), [other]);
    world.despawn(original);
    assert!(world.clone_entity(original).is_none());
}

#[test]
fn cloning_a_template_subtree_yields_a_visible_copy_in_order() {
    let mut world = world();
    let row = world.spawn_bundle((Label("row"), Template));
    for name in ["icon", "title", "badge"] {
        let child = world.spawn_bundle((Label(name), Size(1), Template, ChildOf.to(row)));
        world.spawn_bundle((Label("inner"), Template, ChildOf.to(child)));
    }
    assert_eq!(world.query::<&Label>().iter_mut().count(), 0, "templates are hidden");
    let copy = world.clone_subtree::<ChildOf>(row).unwrap();
    let labels: Vec<_> = world.query::<&Label>().iter_mut().map(|l| l.0).collect();
    assert_eq!(labels.len(), 7, "the copy is visible: {labels:?}");
    let children = world.query::<From<ChildOf, (Entity, &Label)>>().get_mut(copy).unwrap().map(|(e, l)| (e, l.0)).collect::<Vec<_>>();
    assert_eq!(children.iter().map(|c| c.1).collect::<Vec<_>>(), ["icon", "title", "badge"]);
    for (child, _) in children {
        assert_eq!(world.sources::<ChildOf>(child).count(), 1);
    }
    assert_eq!(world.sources::<ChildOf>(row).count(), 3, "the prototype keeps its children");
    // Despawning the copy leaves the prototype intact.
    world.despawn(copy);
    assert_eq!(world.query::<&Label>().iter_mut().count(), 0);
    assert_eq!(world.query_filtered::<&Label, With<Template>>().iter_mut().count(), 7);
}
