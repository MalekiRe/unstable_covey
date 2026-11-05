//! Helpers for widget behaviors (see `static_ecs::Behavior`): racing
//! futures, scopes whose entities die with them, animations driven by
//! frames, timers, and opening and closing panels.
use std::future::poll_fn;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ecs::{Cx, Inbox};

use crate::{ChildOf, Closed, Redraw, SpawnTree};

/// The app's update: input is in, behaviors and app logic run; layout and
/// rendering follow under `UiFrame`. Emit it before `UiFrame`.
pub struct UiUpdate;
impl Schedule for UiUpdate {}

/// One of two outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}
/// Whichever of `a` and `b` finishes first; the other is dropped (for
/// `cx.on`, it stops listening; events it would have taken stay queued).
pub async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let (mut a, mut b) = (pin!(a), pin!(b));
    poll_fn(|cx| {
        if let Poll::Ready(x) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(x));
        }
        b.as_mut().poll(cx).map(Either::Right)
    })
    .await
}
/// The first of `futures` to finish, with its index.
pub async fn select_all<F: Future>(futures: impl IntoIterator<Item = F>) -> (usize, F::Output) {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    poll_fn(|cx| {
        for (i, future) in futures.iter_mut().enumerate() {
            if let Poll::Ready(x) = future.as_mut().poll(cx) {
                return Poll::Ready((i, x));
            }
        }
        Poll::Pending
    })
    .await
}

/// Entities spawned in a [`Scope`] relate to its scope entity with this
/// linked relation: despawning the scope entity despawns them.
#[derive(Clone, Copy, Debug, Default)]
pub struct InScope;
impl Relation for InScope {
    type Kind = OneToMany;
    const LINKED: bool = true;
}
/// The scope entity's marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScopeRoot;

/// A region of a behavior whose entities live only as long as it: spawn
/// with [`Scope::spawn`]; when the scope is dropped (returned from, left by
/// `?`, or dropped with the behavior), its entities are despawned.
pub struct Scope<S: Storage> {
    cx: Cx<S>,
    root: Entity,
}
impl<S: Storage> Scope<S> {
    /// Open a scope.
    pub fn new(cx: Cx<S>) -> Self {
        let root = cx.with(|world: &mut World<S>| world.spawn_bundle((ScopeRoot,))).expect("exclusive access");
        Self { cx, root }
    }
    /// Spawn a UI tree (under `parent`, or as a root) that dies with the
    /// scope.
    pub fn spawn(&self, parent: Option<Entity>, tree: impl SpawnTree<S>) -> Entity {
        let root = self.root;
        self.cx
            .with(move |world: &mut World<S>| {
                let entity = tree.spawn_tree(world, parent);
                world.relate(entity, InScope, root);
                entity
            })
            .expect("exclusive access")
    }
    /// The scope entity (its sources are the scope's entities).
    pub fn entity(&self) -> Entity {
        self.root
    }
}
impl<S: Storage> Drop for Scope<S> {
    fn drop(&mut self) {
        // Outside a poll step the app is being dropped with everything.
        if self.cx.is_polling() {
            let root = self.root;
            let _ = self.cx.with(move |world: &mut World<S>| world.despawn(root));
        }
    }
}
/// Run `body` in a fresh [`Scope`], closed when it returns.
pub async fn scoped<S: Storage, R>(cx: Cx<S>, body: impl AsyncFnOnce(&Scope<S>) -> R) -> R {
    let scope = Scope::new(cx);
    body(&scope).await
}

/// Sent to animating behaviors every frame, with the frame's time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTick {
    pub now: Instant,
}
/// An easing curve: `t` in 0–1 to progress in 0–1.
pub type Ease = fn(f32) -> f32;
pub fn linear(t: f32) -> f32 {
    t
}
pub fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 }
}
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
/// Call `step` once per frame for `duration` with eased progress; the
/// progress never decreases and the last call sees exactly `1.0`. Frames
/// keep coming while it runs ([`Redraw::animating`]), and stop after.
/// `me` is the entity the behavior runs for.
pub async fn animate<S: Storage>(cx: Cx<S>, me: Entity, duration: Duration, ease: Ease, mut step: impl FnMut(f32)) {
    let start = cx.on::<FrameTick>(me).await.now;
    loop {
        let elapsed = cx.on::<FrameTick>(me).await.now.saturating_duration_since(start);
        let t = if duration.is_zero() { 1.0 } else { (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0) };
        step(if t >= 1.0 { 1.0 } else { ease(t) });
        if t >= 1.0 {
            return;
        }
    }
}

/// Pending timers: a resource. The frame loop wakes at the earliest.
#[derive(Default)]
pub struct Timers {
    entries: Vec<(Instant, u64, Waker)>,
    next_id: u64,
}
impl Timers {
    /// The earliest deadline.
    pub fn next(&self) -> Option<Instant> {
        self.entries.iter().map(|e| e.0).min()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
/// Resolves once `duration` has passed (checked each `UiUpdate`; the window
/// loop sleeps until the earliest timer).
pub fn sleep<S: Storage>(cx: Cx<S>, duration: Duration) -> Sleep<S> {
    Sleep { cx, deadline: Instant::now() + duration, id: None }
}
/// The future [`sleep`] returns.
pub struct Sleep<S: Storage> {
    cx: Cx<S>,
    deadline: Instant,
    id: Option<u64>,
}
impl<S: Storage> Future for Sleep<S> {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            if let Some(id) = self.id.take() {
                let _ = self.cx.with(move |mut timers: ResMut<Timers>| timers.entries.retain(|e| e.1 != id));
            }
            return Poll::Ready(());
        }
        let (deadline, waker, known) = (self.deadline, context.waker().clone(), self.id);
        let id = self
            .cx
            .with(move |mut timers: ResMut<Timers>| match known {
                Some(id) => id,
                None => {
                    timers.next_id += 1;
                    let id = timers.next_id;
                    timers.entries.push((deadline, id, waker));
                    id
                }
            })
            .expect("`Timers` is a UiPlugin resource");
        self.id = Some(id);
        Poll::Pending
    }
}
impl<S: Storage> Drop for Sleep<S> {
    fn drop(&mut self) {
        if let Some(id) = self.id
            && self.cx.is_polling()
        {
            let _ = self.cx.with(move |mut timers: ResMut<Timers>| timers.entries.retain(|e| e.1 != id));
        }
    }
}

/// `UiUpdate`, first: fire due timers and tick animations.
pub fn frame_start(mut timers: ResMut<Timers>, mut ticks: ResMut<Inbox<FrameTick>>) {
    let now = Instant::now();
    timers.entries.retain(|(deadline, _, waker)| {
        let due = *deadline <= now;
        if due {
            waker.wake_by_ref();
        }
        !due
    });
    ticks.deliver_to_waiting(FrameTick { now });
}
/// `UiFrame`, last: keep frames coming while something animates, and wake
/// for the earliest timer.
pub fn frame_end(timers: Res<Timers>, ticks: Res<Inbox<FrameTick>>, mut redraw: ResMut<Redraw>) {
    let next = Redraw { animating: ticks.any_waiting(), deadline: timers.next() };
    if *redraw != next {
        *redraw = next;
    }
}

/// Close `root`'s subtree from a behavior.
pub fn close<S: Storage>(cx: Cx<S>, root: Entity) {
    let _ = cx.with(move |world: &mut World<S>| world.disable_subtree::<Closed, ChildOf>(root));
}
/// Reopen `root`'s subtree from a behavior.
pub fn open<S: Storage>(cx: Cx<S>, root: Entity) {
    let _ = cx.with(move |world: &mut World<S>| world.enable_subtree::<Closed, ChildOf>(root));
}
