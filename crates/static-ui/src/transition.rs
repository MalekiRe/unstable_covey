//! Transitions: a `Transition<P>` component animates one property of its
//! entity from a value to another; one system per property advances them,
//! and costs nothing while none exist. `open_animated` / `close_animated`
//! play them on enter and exit: closing waits for the exit to finish, then
//! closes the subtree.
use std::marker::PhantomData;
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ui_layout::{Sizing, Vec2};

use crate::behave::{Ease, UiClock};
use crate::{Background, ChildOf, Closed, Color, Floating, Node};

/// A property a transition can drive.
pub trait Property: Send + Sync + 'static {
    /// The component holding it.
    type Target: Send + Sync + 'static;
    type Value: Copy + PartialEq + Send + Sync + 'static;
    fn read(target: &Self::Target) -> Self::Value;
    fn write(target: &mut Self::Target, value: Self::Value);
    fn lerp(from: Self::Value, to: Self::Value, t: f32) -> Self::Value;
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
/// `Background` color.
pub struct BackgroundColor;
impl Property for BackgroundColor {
    type Target = Background;
    type Value = Color;
    fn read(target: &Background) -> Color {
        target.0
    }
    fn write(target: &mut Background, value: Color) {
        target.0 = value;
    }
    fn lerp(from: Color, to: Color, t: f32) -> Color {
        let channel = |a: u8, b: u8| lerp(a as f32, b as f32, t).round() as u8;
        Color::rgba(channel(from.r, to.r), channel(from.g, to.g), channel(from.b, to.b), channel(from.a, to.a))
    }
}
/// A node's width, as a fixed size.
pub struct Width;
impl Property for Width {
    type Target = Node;
    type Value = f32;
    fn read(target: &Node) -> f32 {
        target.0.width.min
    }
    fn write(target: &mut Node, value: f32) {
        target.0.width = Sizing::fixed(value);
    }
    fn lerp(from: f32, to: f32, t: f32) -> f32 {
        lerp(from, to, t)
    }
}
/// A node's height, as a fixed size.
pub struct Height;
impl Property for Height {
    type Target = Node;
    type Value = f32;
    fn read(target: &Node) -> f32 {
        target.0.height.min
    }
    fn write(target: &mut Node, value: f32) {
        target.0.height = Sizing::fixed(value);
    }
    fn lerp(from: f32, to: f32, t: f32) -> f32 {
        lerp(from, to, t)
    }
}
/// A floating element's offset.
pub struct Offset;
impl Property for Offset {
    type Target = Floating;
    type Value = Vec2;
    fn read(target: &Floating) -> Vec2 {
        target.offset
    }
    fn write(target: &mut Floating, value: Vec2) {
        target.offset = value;
    }
    fn lerp(from: Vec2, to: Vec2, t: f32) -> Vec2 {
        Vec2::new(lerp(from.x, to.x, t), lerp(from.y, to.y, t))
    }
}

/// What happens when a transition ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Then {
    #[default]
    Stay,
    /// Close the entity's subtree (an exit transition).
    Close,
}
/// Animate `P` of this entity from `from` to `to`.
pub struct Transition<P: Property> {
    pub from: P::Value,
    pub to: P::Value,
    pub duration: Duration,
    pub ease: Ease,
    pub then: Then,
    /// Set on the first frame it runs.
    start: Option<Instant>,
    property: PhantomData<fn() -> P>,
}
impl<P: Property> Transition<P> {
    pub fn new(from: P::Value, to: P::Value, duration: Duration, ease: Ease) -> Self {
        Self { from, to, duration, ease, then: Then::Stay, start: None, property: PhantomData }
    }
    pub fn then(self, then: Then) -> Self {
        Self { then, ..self }
    }
}

/// Transitions advanced this frame: frames continue while any run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveTransitions(pub u32);

/// Advance every `Transition<P>`: write the eased value; at the end write
/// the target exactly, remove the transition, and close if asked.
#[allow(clippy::type_complexity)]
pub fn transition_system<S: Storage, P: Property>(
    clock: Option<Res<UiClock>>,
    mut active: ResMut<ActiveTransitions>,
    mut transitions: Query<(Entity, &mut Transition<P>, &mut P::Target)>,
    mut commands: Commands<S>,
) {
    let now = clock.map_or(Instant::now(), |c| c.now());
    transitions.each(|(entity, mut transition, mut target)| {
        let start = *transition.start.get_or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        let t = if transition.duration.is_zero() { 1.0 } else { (elapsed.as_secs_f32() / transition.duration.as_secs_f32()).min(1.0) };
        let value = if t >= 1.0 { transition.to } else { P::lerp(transition.from, transition.to, (transition.ease)(t)) };
        if P::read(&target) != value {
            P::write(&mut target, value);
        }
        if t >= 1.0 {
            commands.remove::<(Transition<P>,)>(entity);
            if transition.then == Then::Close {
                commands.disable_subtree::<Closed, ChildOf>(entity);
            }
        } else {
            active.0 += 1;
        }
    });
}
/// Reset the count before transitions run.
pub fn reset_active(mut active: ResMut<ActiveTransitions>) {
    if active.0 != 0 {
        active.0 = 0;
    }
}

/// Open `root`'s subtree and play `transition` on it.
pub fn open_animated<S: Storage, P: Property>(commands: &mut Commands<S>, root: Entity, transition: Transition<P>) {
    commands.enable_subtree::<Closed, ChildOf>(root);
    commands.insert(root, (transition,));
}
/// Play `transition` on `root`, then close its subtree.
pub fn close_animated<S: Storage, P: Property>(commands: &mut Commands<S>, root: Entity, transition: Transition<P>) {
    commands.insert(root, (transition.then(Then::Close),));
}

/// Transitions of `P`: register one per animated property.
pub struct TransitionPlugin<P>(PhantomData<fn() -> P>);
impl<P> Default for TransitionPlugin<P> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<P: Property> Plugin for TransitionPlugin<P> {
    type Components = (Transition<P>, P::Target, ActiveTransitions);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        schedule(crate::UiUpdate, transition_system::<S, P>)
    }
}
/// Transitions of background color, width, height and floating offset.
pub fn transitions() -> (TransitionPlugin<BackgroundColor>, TransitionPlugin<Width>, TransitionPlugin<Height>, TransitionPlugin<Offset>) {
    Default::default()
}
