//! Turning raw input into UI events: hit testing against the last layout,
//! hover and press state written in place on [`Interaction`], events
//! delivered to the hit entity and bubbled up `ChildOf` until something
//! awaits them, scroll containers, drag, and keyboard focus.
use static_ecs::prelude::*;
use static_ecs::{Components, Inbox};
use static_ui_layout::{PointerCapture, Rect as Box2, Vec2};

use crate::input::{Input, Key, Modifiers, MouseButton};
use crate::{ChildOf, Clip, Inert, Scroll, UiLayout};

/// Hover, press and focus, updated in place (it never moves the entity).
/// Entities holding it get its state; others only receive events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
}
/// Can take keyboard focus (Tab cycles through these in tree order).
#[derive(Clone, Copy, Debug, Default)]
pub struct Focusable;

/// The pointer entered the entity's box (not bubbled).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEnter;
/// The pointer left it (not bubbled).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerLeave;
/// A button went down over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub button: MouseButton,
    pub position: Vec2,
}
/// A button went up over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Release {
    pub button: MouseButton,
    pub position: Vec2,
}
/// Pressed and released over it without dragging.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub button: MouseButton,
    pub position: Vec2,
}
/// The pointer moved far enough with a button down: a drag began where the
/// press was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragStart {
    pub position: Vec2,
}
/// The drag moved; delivered to whatever took `DragStart`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub delta: Vec2,
    pub position: Vec2,
}
/// The drag ended (button up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragEnd {
    pub position: Vec2,
}
/// Wheel or touchpad scrolling over it, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wheel {
    pub delta: Vec2,
}
/// A key went down while it (or a descendant) had focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyPress {
    pub key: Key,
    pub modifiers: Modifiers,
}
/// Text typed while it (or a descendant) had focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInput(pub char);
/// It gained keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusIn;
/// It lost keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusOut;

/// Pixels the pointer moves with a button down before a press becomes a
/// drag.
pub const DRAG_THRESHOLD: f32 = 3.0;

/// The pointer, as the UI sees it: a resource.
#[derive(Clone, Debug, Default)]
pub struct Pointer {
    pub position: Option<Vec2>,
    /// The deepest element under the pointer.
    pub hovered: Option<Entity>,
    /// The entity whose `Interaction` shows hover.
    hovered_interactive: Option<Entity>,
    /// Where the button went down, on what, and the interactive entity
    /// shown pressed.
    press: Option<(Vec2, Entity, MouseButton, Option<Entity>)>,
    /// What receives `Drag`: a behavior that took `DragStart`, or a scroll
    /// container moved by the built-in drag-to-scroll.
    drag: Option<DragTarget>,
    /// Hit tests run, for measuring.
    pub hit_tests: u64,
}
#[derive(Clone, Copy, Debug)]
enum DragTarget {
    Behavior(Entity),
    Scroll(Entity),
}
/// The entity with keyboard focus: a resource.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Focus(pub Option<Entity>);

/// Input handling: hit testing, interaction state, events, focus.
pub struct InputPlugin;
impl Plugin for InputPlugin {
    type Components = (
        Interaction,
        Focusable,
        Pointer,
        Focus,
        Inert,
        Scroll,
        Clip,
        Inbox<PointerEnter>,
        Inbox<PointerLeave>,
        Inbox<Press>,
        Inbox<Release>,
        Inbox<Click>,
        Inbox<DragStart>,
        Inbox<Drag>,
        Inbox<DragEnd>,
        Inbox<Wheel>,
        Inbox<KeyPress>,
        Inbox<TextInput>,
        Inbox<FocusIn>,
        Inbox<FocusOut>,
    );
    type Relations = (ChildOf,);
    type Storage = (Components<Interaction, true>,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        // Any `Input` value names the label.
        schedule(Input::PointerLeft, exclusive(|input: &Input, world: &mut World<S>| handle(world, input)))
    }
}

/// The deepest visible, non-`Inert` element containing `point`, topmost
/// first: floating roots by descending z, later siblings over earlier ones,
/// clipping respected; a passthrough floating element is hit only through
/// its children.
pub fn hit_test<S: Storage>(world: &World<S>, point: Vec2) -> Option<Entity> {
    let ui = world.resource::<UiLayout>()?;
    let tree = ui.tree();
    if tree.is_empty() {
        return None;
    }
    for root in tree.roots().rev() {
        let window = tree.rect(0);
        if let Some(hit) = hit_in(world, ui, root, point, window) {
            let passthrough = root != 0 && tree.config(root).floating.pointer == PointerCapture::Passthrough;
            if passthrough && hit == ui.entity(root) {
                continue;
            }
            return Some(hit);
        }
    }
    None
}
fn hit_in<S: Storage>(world: &World<S>, ui: &UiLayout, element: u32, point: Vec2, clip: Box2) -> Option<Entity> {
    // Nothing in this subtree reaches the point.
    if !ui.subtree_bounds(element).contains(point) {
        return None;
    }
    let tree = ui.tree();
    let rect = tree.rect(element);
    let inside = rect.contains(point) && clip.contains(point);
    let entity = (element != 0).then(|| ui.entity(element));
    if entity.is_some_and(|e| world.get::<Inert>(e).is_some()) {
        return None;
    }
    let config = tree.config(element);
    let child_clip = if !tree.is_text(element) && (config.clip.horizontal || config.clip.vertical) { clip.intersect(rect) } else { clip };
    // Children can overflow their parent unless it clips.
    for &child in tree.children(element).iter().rev() {
        if let Some(hit) = hit_in(world, ui, child, point, child_clip) {
            return Some(hit);
        }
    }
    if inside { entity } else { None }
}

fn parent<S: Storage>(world: &World<S>, entity: Entity) -> Option<Entity> {
    world.get::<Relates<ChildOf>>(entity).map(|r| r.target())
}
/// `entity` or its nearest ancestor holding `T`.
fn nearest<S: Storage, T: 'static>(world: &World<S>, mut entity: Entity) -> Option<Entity> {
    loop {
        if world.get::<T>(entity).is_some() {
            return Some(entity);
        }
        entity = parent(world, entity)?;
    }
}
/// Deliver `event` to `from`, then up its ancestors, until something awaits
/// it; returns who did.
pub fn bubble<S: Storage, E: Clone + Send + Sync + 'static>(world: &mut World<S>, from: Entity, event: E) -> Option<Entity> {
    let mut entity = Some(from);
    while let Some(e) = entity {
        if world.deliver(e, event.clone()) {
            return Some(e);
        }
        entity = parent(world, e);
    }
    None
}
fn set_interaction<S: Storage>(world: &mut World<S>, entity: Option<Entity>, change: impl Fn(&mut Interaction)) {
    if let Some(entity) = entity
        && let Some(mut interaction) = world.get_mut::<Interaction>(entity)
    {
        let mut next = *interaction;
        change(&mut next);
        if next != *interaction {
            *interaction = next;
        }
    }
}
fn set_focus<S: Storage>(world: &mut World<S>, next: Option<Entity>) {
    let previous = world.resource::<Focus>().and_then(|f| f.0);
    if previous == next {
        return;
    }
    if let Some(previous) = previous {
        set_interaction(world, Some(previous), |i| i.focused = false);
        world.deliver(previous, FocusOut);
    }
    if let Some(mut focus) = world.resource_mut::<Focus>() {
        focus.0 = next;
    }
    if let Some(next) = next {
        set_interaction(world, Some(next), |i| i.focused = true);
        world.deliver(next, FocusIn);
    }
}
/// Scroll `container` by `delta`, clamped to its content.
fn scroll_by<S: Storage>(world: &mut World<S>, container: Entity, delta: Vec2) -> bool {
    let Some(element) = world.resource::<UiLayout>().and_then(|ui| ui.element(container)) else { return false };
    let ui = world.resource::<UiLayout>().unwrap();
    let (rect, content) = (ui.tree().rect(element), ui.tree().content_size(element));
    let clip = world.get::<Clip>(container).copied().unwrap_or_default();
    let Some(mut scroll) = world.get_mut::<Scroll>(container) else { return false };
    let max_x = (content.width - rect.width).max(0.0);
    let max_y = (content.height - rect.height).max(0.0);
    let mut offset = scroll.offset;
    if clip.horizontal {
        offset.x = (offset.x + delta.x).clamp(0.0, max_x);
    }
    if clip.vertical {
        offset.y = (offset.y + delta.y).clamp(0.0, max_y);
    }
    if offset != scroll.offset {
        scroll.offset = offset;
    }
    true
}
fn scroll_container<S: Storage>(world: &World<S>, from: Entity) -> Option<Entity> {
    let mut entity = Some(from);
    while let Some(e) = entity {
        if world.get::<Scroll>(e).is_some() && world.get::<Clip>(e).is_some_and(|c| c.horizontal || c.vertical) {
            return Some(e);
        }
        entity = parent(world, e);
    }
    None
}

/// Handle one input event.
pub fn handle<S: Storage>(world: &mut World<S>, input: &Input) {
    if world.resource::<Pointer>().is_none() {
        world.init_resource::<Pointer>();
    }
    if world.resource::<Focus>().is_none() {
        world.init_resource::<Focus>();
    }
    let mut pointer = world.resource::<Pointer>().unwrap().clone();
    match *input {
        Input::PointerMoved { x, y } => {
            let position = Vec2::new(x, y);
            let previous = pointer.position.replace(position);
            pointer.hit_tests += 1;
            let hit = hit_test(world, position);
            if hit != pointer.hovered {
                let interactive = hit.and_then(|h| nearest::<S, Interaction>(world, h));
                // Enter and leave reach both the element under the pointer
                // and the interactive one around it (a button, not its
                // label), each once.
                let distinct = |a: Option<Entity>, b: Option<Entity>| [a, b.filter(|&b| Some(b) != a)];
                let old = distinct(pointer.hovered, pointer.hovered_interactive);
                let new = distinct(hit, interactive);
                for entity in old.into_iter().flatten().filter(|&e| !new.contains(&Some(e))) {
                    world.deliver(entity, PointerLeave);
                }
                for entity in new.into_iter().flatten().filter(|&e| !old.contains(&Some(e))) {
                    world.deliver(entity, PointerEnter);
                }
                pointer.hovered = hit;
                if interactive != pointer.hovered_interactive {
                    set_interaction(world, pointer.hovered_interactive, |i| i.hovered = false);
                    set_interaction(world, interactive, |i| i.hovered = true);
                    pointer.hovered_interactive = interactive;
                }
            }
            if let Some((start, target, _, _)) = pointer.press {
                if pointer.drag.is_none() && ((position.x - start.x).powi(2) + (position.y - start.y).powi(2)).sqrt() >= DRAG_THRESHOLD {
                    pointer.drag = Some(match bubble(world, target, DragStart { position: start }) {
                        Some(taker) => DragTarget::Behavior(taker),
                        None => match scroll_container(world, target) {
                            Some(container) => DragTarget::Scroll(container),
                            None => DragTarget::Behavior(target),
                        },
                    });
                }
                let last = previous.unwrap_or(start);
                let delta = Vec2::new(position.x - last.x, position.y - last.y);
                match pointer.drag {
                    Some(DragTarget::Behavior(taker)) => {
                        world.deliver(taker, Drag { delta, position });
                    }
                    Some(DragTarget::Scroll(container)) => {
                        scroll_by(world, container, Vec2::new(-delta.x, -delta.y));
                    }
                    None => {}
                }
            }
        }
        Input::PointerLeft => {
            let (hovered, interactive) = (pointer.hovered.take(), pointer.hovered_interactive.take());
            if let Some(old) = hovered {
                world.deliver(old, PointerLeave);
            }
            if let Some(old) = interactive.filter(|&i| Some(i) != hovered) {
                world.deliver(old, PointerLeave);
            }
            set_interaction(world, interactive, |i| i.hovered = false);
            pointer.position = None;
        }
        Input::Button { button, pressed: true } => {
            let Some(position) = pointer.position else { return };
            let Some(hit) = pointer.hovered else {
                set_focus(world, None);
                return;
            };
            bubble(world, hit, Press { button, position });
            let interactive = nearest::<S, Interaction>(world, hit);
            set_interaction(world, interactive, |i| i.pressed = true);
            pointer.press = Some((position, hit, button, interactive));
            pointer.drag = None;
            let focusable = nearest::<S, Focusable>(world, hit);
            set_focus(world, focusable);
        }
        Input::Button { button, pressed: false } => {
            // Only the button that pressed releases the press.
            let Some((_, target, pressed_button, interactive)) = pointer.press else { return };
            if pressed_button != button {
                return;
            }
            pointer.press = None;
            set_interaction(world, interactive, |i| i.pressed = false);
            let position = pointer.position.unwrap_or_default();
            match pointer.drag.take() {
                Some(DragTarget::Behavior(taker)) => {
                    world.deliver(taker, DragEnd { position });
                }
                Some(DragTarget::Scroll(_)) => {}
                None => {
                    if let Some(hit) = pointer.hovered {
                        bubble(world, hit, Release { button, position });
                        // A click lands where both press and release did.
                        let mut on_path = Some(hit);
                        while let Some(e) = on_path {
                            if e == target {
                                bubble(world, target, Click { button, position });
                                break;
                            }
                            on_path = parent(world, e);
                        }
                    }
                }
            }
        }
        Input::Scroll { dx, dy } => {
            let Some(hit) = pointer.hovered else { return };
            let delta = Vec2::new(dx, dy);
            let mut entity = Some(hit);
            while let Some(e) = entity {
                if world.get::<Scroll>(e).is_some() && scroll_by(world, e, delta) {
                    break;
                }
                if world.deliver(e, Wheel { delta }) {
                    break;
                }
                entity = parent(world, e);
            }
        }
        Input::Key { key, pressed: true, modifiers } => {
            if key == Key::Tab {
                let order: Vec<Entity> = world
                    .resource::<UiLayout>()
                    .map(|ui| ui.entities().iter().copied().filter(|&e| world.get::<Focusable>(e).is_some()).collect())
                    .unwrap_or_default();
                if !order.is_empty() {
                    let current = world.resource::<Focus>().unwrap().0.and_then(|f| order.iter().position(|&e| e == f));
                    let next = match (current, modifiers.shift) {
                        (None, false) => 0,
                        (None, true) => order.len() - 1,
                        (Some(i), false) => (i + 1) % order.len(),
                        (Some(i), true) => (i + order.len() - 1) % order.len(),
                    };
                    set_focus(world, Some(order[next]));
                }
            } else if let Some(focused) = world.resource::<Focus>().unwrap().0 {
                bubble(world, focused, KeyPress { key, modifiers });
            }
        }
        Input::Key { pressed: false, .. } => {}
        Input::Text(ch) => {
            if let Some(focused) = world.resource::<Focus>().unwrap().0 {
                bubble(world, focused, TextInput(ch));
            }
        }
        Input::FocusLost => {
            if let Some((_, _, _, interactive)) = pointer.press.take() {
                set_interaction(world, interactive, |i| i.pressed = false);
            }
            pointer.drag = None;
        }
    }
    *world.resource_mut::<Pointer>().unwrap() = pointer;
}
