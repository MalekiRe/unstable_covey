//! The visible UI tree, flattened for Clay's passes: rebuilt only when the
//! structure changes, patched in place when nodes or text change, and not
//! laid out at all when nothing did.
use static_ecs::prelude::*;
use static_ui_layout::{self as layout, AttachTo, Config, Line, TextCache, Tree};

use crate::{
    AspectRatio, Attach, ChildOf, Clip, Closed, Floating, Measure, Node, Rect, Scroll, Template, Text, UiFrame,
    Viewport,
};

const NONE: u32 = u32::MAX;

/// The flattened tree and its last layout: a resource.
pub struct UiLayout {
    tree: Tree,
    cache: TextCache,
    /// Element `i + 1`'s entity (element 0 is the window root).
    entities: Vec<Entity>,
    /// Element by entity index.
    by_entity: Vec<u32>,
    /// The previous build's entities and boxes, to damage what left.
    previous: Vec<(Entity, layout::Rect)>,
    structure_dirty: bool,
    viewport: Viewport,
    runs: u64,
    rebuilds: u64,
    damage: Vec<layout::Rect>,
    /// Flattening work list: `Some(entity)` opens, `None` closes.
    stack: Vec<Option<Entity>>,
    roots: Vec<Entity>,
    attach: Vec<(u32, Entity)>,
}
impl Default for UiLayout {
    fn default() -> Self {
        Self {
            tree: Tree::new(),
            cache: TextCache::default(),
            entities: Vec::new(),
            by_entity: Vec::new(),
            previous: Vec::new(),
            structure_dirty: true,
            viewport: Viewport { width: -1.0, height: -1.0 },
            runs: 0,
            rebuilds: 0,
            damage: Vec::new(),
            stack: Vec::new(),
            roots: Vec::new(),
            attach: Vec::new(),
        }
    }
}
impl UiLayout {
    /// Layouts run so far: an unchanged UI runs none.
    pub fn runs(&self) -> u64 {
        self.runs
    }
    /// Times the visible tree was flattened again.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }
    /// The flattened tree, with its last layout.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }
    /// The element laid out for `entity`, if it is in the visible tree.
    pub fn element(&self, entity: Entity) -> Option<u32> {
        let element = *self.by_entity.get(entity.index() as usize)?;
        (element != NONE && self.entities[element as usize - 1] == entity).then_some(element)
    }
    /// The entity behind `element` (not the window root, element 0).
    pub fn entity(&self, element: u32) -> Entity {
        self.entities[element as usize - 1]
    }
    /// A text entity's wrapped lines.
    pub fn lines(&self, entity: Entity) -> &[Line] {
        self.element(entity).map_or(&[], |element| self.tree.lines(element))
    }
    /// Visible entities, in tree order (depth-first, roots by entity).
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }
    /// Force the next frame to flatten the tree again.
    pub fn mark_structure_dirty(&mut self) {
        self.structure_dirty = true;
    }
    /// Screen areas that changed since the last call: old and new boxes of
    /// moved elements, and boxes of elements that left the tree.
    pub fn take_damage(&mut self) -> std::vec::Drain<'_, layout::Rect> {
        self.damage.drain(..)
    }
    /// Report a damaged area, e.g. a restyled element.
    pub fn damage(&mut self, area: layout::Rect) {
        if !area.is_empty() {
            self.damage.push(area);
        }
    }
}

type NodeData = (
    Option<&'static Node>,
    Option<&'static Text>,
    Option<&'static Floating>,
    Option<&'static Clip>,
    Option<&'static Scroll>,
    Option<&'static AspectRatio>,
    Option<&'static Sources<ChildOf>>,
);
type Nodes<'w> = Query<'w, NodeData>;
type Roots<'w> = Query<'w, Entity, (Or<(With<Node>, With<Text>)>, Without<Relates<ChildOf>>)>;
type ChangedNodes<'w> = Query<'w, Entity, Or<(Changed<Node>, Changed<Scroll>, Changed<AspectRatio>)>>;

fn config(
    node: Option<&Node>,
    floating: Option<&Floating>,
    clip: Option<&Clip>,
    scroll: Option<&Scroll>,
    aspect: Option<&AspectRatio>,
) -> Config {
    let mut config = Config::from(node.copied().unwrap_or_default().0);
    if let Some(f) = floating {
        config.floating = layout::Floating {
            attach_to: match f.attach {
                Attach::Parent => AttachTo::Parent,
                Attach::Root => AttachTo::Root,
                Attach::To(_) => AttachTo::Element(NONE),
            },
            offset: f.offset,
            expand: f.expand,
            z: f.z,
            element: f.element,
            parent: f.parent,
            pointer: f.pointer,
            clip_to_parent: f.clip_to_parent,
        };
    }
    if let Some(clip) = clip {
        config.clip.horizontal = clip.horizontal;
        config.clip.vertical = clip.vertical;
    }
    if let Some(scroll) = scroll {
        config.clip.child_offset = layout::Vec2::new(-scroll.offset.x, -scroll.offset.y);
    }
    config.aspect_ratio = aspect.map_or(0.0, |a| a.0);
    config
}

impl UiLayout {
    /// Flatten the visible tree: roots by entity, children in order. Hidden
    /// entities (`Closed`, `Template`, `Disabled`) are not in `nodes`.
    fn rebuild(&mut self, nodes: &mut Nodes, roots: &mut Roots) {
        self.rebuilds += 1;
        self.previous.clear();
        for (i, &entity) in self.entities.iter().enumerate() {
            self.previous.push((entity, self.tree.rect(i as u32 + 1)));
        }
        for &entity in &self.entities {
            self.by_entity[entity.index() as usize] = NONE;
        }
        self.entities.clear();
        self.attach.clear();
        self.roots.clear();
        roots.each(|entity| self.roots.push(entity));
        self.roots.sort_unstable();
        self.tree.begin(self.viewport.width, self.viewport.height);
        for &root in self.roots.iter().rev() {
            self.stack.push(Some(root));
        }
        while let Some(next) = self.stack.pop() {
            let Some(entity) = next else {
                self.tree.close();
                continue;
            };
            let Some((node, text, floating, clip, scroll, aspect, children)) = nodes.get_mut(entity) else {
                continue; // hidden, or not a UI element
            };
            let element = if let Some(text) = text {
                self.tree.text(&text.text, text.config)
            } else if node.is_some() {
                let element = self.tree.open(config(node, floating, clip, scroll, aspect));
                self.stack.push(None);
                if let Some(children) = children {
                    self.stack.extend(children.sources().rev().map(Some));
                }
                if let Some(Floating { attach: Attach::To(target), .. }) = floating {
                    self.attach.push((element, *target));
                }
                element
            } else {
                continue;
            };
            self.entities.push(entity);
            let index = entity.index() as usize;
            if self.by_entity.len() <= index {
                self.by_entity.resize(index + 1, NONE);
            }
            self.by_entity[index] = element;
        }
        self.tree.end();
        for i in 0..self.attach.len() {
            let (element, target) = self.attach[i];
            let target = self.element(target);
            self.tree.set_attached(element, target);
        }
        // Whatever left the tree leaves its old box to repaint.
        for i in 0..self.previous.len() {
            let (entity, rect) = self.previous[i];
            if self.element(entity).is_none() {
                self.damage(rect);
            }
        }
    }
}

/// Lay the UI out if anything changed: flatten again if the structure did,
/// otherwise patch changed nodes and text; then write back moved `Rect`s.
#[allow(clippy::too_many_arguments)]
pub fn layout_system<M: Measure>(
    mut ui: ResMut<UiLayout>,
    mut measure: ResMut<M>,
    viewport: Res<Viewport>,
    mut nodes: Nodes,
    mut roots: Roots,
    mut changed_nodes: ChangedNodes,
    mut changed_text: Query<Entity, Changed<Text>>,
    mut relinked: Query<Entity, Relinked<ChildOf>>,
    mut rects: Query<&mut Rect>,
) {
    let ui = &mut *ui;
    let mut dirty = false;
    if *viewport != ui.viewport {
        ui.viewport = *viewport;
        ui.tree.set_root_size(viewport.width, viewport.height);
        ui.damage(layout::Rect::new(0.0, 0.0, viewport.width, viewport.height));
        dirty = true;
    }
    if ui.structure_dirty || relinked.iter_mut().next().is_some() {
        ui.structure_dirty = false;
        ui.rebuild(&mut nodes, &mut roots);
        dirty = true;
    } else {
        changed_nodes.each(|entity| {
            let Some(element) = ui.element(entity) else { return };
            if let Some((node, _, floating, clip, scroll, aspect, _)) = nodes.get_mut(entity) {
                *ui.tree.config_mut(element) = config(node, floating, clip, scroll, aspect);
                dirty = true;
            }
        });
        changed_text.each(|entity| {
            let Some(element) = ui.element(entity) else { return };
            if let Some((_, Some(text), ..)) = nodes.get_mut(entity) {
                ui.tree.set_text(element, &text.text, text.config);
                // A restyle without a move still repaints.
                let rect = ui.tree.rect(element);
                ui.damage(rect);
                dirty = true;
            }
        });
    }
    if !dirty {
        return;
    }
    ui.runs += 1;
    ui.tree.layout(&mut ui.cache, |text, config| measure.measure(text, config));
    for i in 0..ui.entities.len() {
        let entity = ui.entities[i];
        let new = ui.tree.rect(i as u32 + 1);
        if let Some(mut rect) = rects.get_mut(entity)
            && rect.0 != new
        {
            let old = rect.0;
            rect.0 = new;
            ui.damage(old);
            ui.damage(new);
        }
    }
}

/// Mark the structure dirty: a node appeared, left, or was hidden.
fn structure_changed(_: Trigger, ui: Option<ResMut<UiLayout>>) {
    if let Some(mut ui) = ui {
        ui.structure_dirty = true;
    }
}
type StructureHook = for<'a, 'b> fn(Trigger<'a>, Option<ResMut<'b, UiLayout>>);

/// The core's systems: structure hooks, resource setup, layout.
pub(crate) fn systems<S: Storage, M: Measure>() -> impl Systems<S> {
    let hook = structure_changed as StructureHook;
    (
        (
            schedule(OnAdd::<Node>::new(), hook),
            schedule(OnRemove::<Node>::new(), hook),
            schedule(OnAdd::<Text>::new(), hook),
            schedule(OnRemove::<Text>::new(), hook),
            schedule(OnAdd::<Closed>::new(), hook),
            schedule(OnRemove::<Closed>::new(), hook),
            schedule(OnAdd::<Template>::new(), hook),
            schedule(OnRemove::<Template>::new(), hook),
            schedule(OnAdd::<Disabled>::new(), hook),
            schedule(OnRemove::<Disabled>::new(), hook),
            schedule(OnInsert::<Floating>::new(), hook),
            schedule(OnRemove::<Floating>::new(), hook),
            schedule(OnInsert::<Clip>::new(), hook),
            schedule(OnRemove::<Clip>::new(), hook),
        ),
        schedule(
            UiFrame,
            exclusive(|world: &mut World<S>| {
                if world.resource::<UiLayout>().is_none() {
                    world.init_resource::<UiLayout>();
                }
                if world.resource::<M>().is_none() {
                    world.init_resource::<M>();
                }
                if world.resource::<Viewport>().is_none() {
                    world.init_resource::<Viewport>();
                }
                if world.resource::<crate::RenderList>().is_none() {
                    world.init_resource::<crate::RenderList>();
                }
                if world.resource::<crate::ClearColor>().is_none() {
                    world.init_resource::<crate::ClearColor>();
                }
                if world.resource::<crate::Redraw>().is_none() {
                    world.init_resource::<crate::Redraw>();
                }
            }),
        ),
        schedule(UiFrame, layout_system::<M>),
        schedule(UiFrame, crate::render_system),
    )
}
