//! Tasks: futures an app owns and polls during `emit`. A task sits in its
//! app's registration order like a system, under every label at once: each
//! `emit` reaches it at that position and polls it if it is waiting on that
//! label, was woken, or has not started. Its bridges to the label being
//! emitted run right there, with exclusive world access, between the systems
//! registered before and after it.
use std::any::TypeId;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use crate::bridge::{AsyncWorld, Parked, Scope};
use crate::change::SystemWorld;
use crate::dispatch::{Cons, Fits, MaybeHandle, Plugins};
use crate::executor::{self, Continue, Flow, Project, Tail};
use crate::{Storage, World};

/// A task body: `|world: AsyncWorld<_>| async move { ... }` or an async closure.
pub trait TaskFn<S: Storage>: Sized {
    type Future: Future<Output = ()>;
    fn start(self, world: AsyncWorld<S>) -> Self::Future;
}
impl<S: Storage, F: FnOnce(AsyncWorld<S>) -> Fut, Fut: Future<Output = ()>> TaskFn<S> for F {
    type Future = Fut;
    fn start(self, world: AsyncWorld<S>) -> Fut {
        self(world)
    }
}

/// Register an async task, as a plugin: `.add_plugins(task(|world:
/// AsyncWorld<_>| async move { ... }))`. It starts at the app's first `emit`
/// and needs no `Send` unless the app runs its frames on worker threads.
pub fn task<S: Storage, F: TaskFn<S>>(body: F) -> Task<S, F> {
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    Task {
        start: Some(body),
        future: None,
        parked: Parked::No,
        finished: false,
        waker: Waker::from(woken.clone()),
        woken,
    }
}

struct Woken(AtomicBool);
impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

/// A registered task: its body until the first `emit`, then its future.
pub struct Task<S: Storage, F: TaskFn<S>> {
    start: Option<F>,
    future: Option<Pin<Box<F::Future>>>,
    parked: Parked,
    finished: bool,
    woken: Arc<Woken>,
    waker: Waker,
}
impl<S: Storage, F: TaskFn<S>> Task<S, F> {
    /// Poll if due at `label`. Returns whether the task ran.
    /// # Safety
    /// `world` is live and exclusively held by this call.
    unsafe fn visit(&mut self, label: TypeId, world: *mut World<S>, clock: &mut u64) -> bool {
        if self.finished {
            return false;
        }
        let due = self.start.is_some()
            || match self.parked {
                Parked::No => false,
                Parked::At(parked) => parked == label,
                Parked::Anywhere => true,
            }
            || self.woken.0.load(Ordering::Acquire);
        if !due {
            return false;
        }
        self.woken.0.store(false, Ordering::Relaxed);
        let scope = Scope::new(label, world, *clock);
        if let Some(body) = self.start.take() {
            self.future = Some(Box::pin(body.start(AsyncWorld::new(scope.id))));
        }
        // Stays set if the poll unwinds: a panicked task never runs again.
        self.finished = true;
        let future = self.future.as_mut().expect("a started task has a future");
        let polled = scope.enter(|| future.as_mut().poll(&mut Context::from_waker(&self.waker)));
        *clock = scope.clock.get();
        match polled {
            Poll::Ready(()) => self.future = None,
            Poll::Pending => {
                self.finished = false;
                self.parked = scope.parked.get();
            }
        }
        true
    }
}

impl<S: Storage, F: TaskFn<S>, C: Storage, L, K> Plugins<C, L, K> for Task<S, F> {
    type Components = C;
    type Listeners = Cons<Self, L>;
    type Hooks = K;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, K) {
        (Cons(self, listeners), hooks)
    }
}
impl<S: Storage, F: TaskFn<S>> Fits<World<S>> for Task<S, F> {
    const VALID: bool = true;
}
// Every label reaches a task; `visit` decides whether it polls.
impl<E: 'static, S: Storage, F: TaskFn<S>> MaybeHandle<E, World<S>> for Task<S, F> {
    const HAS_PARALLEL: bool = false;
    const WEIGHT: usize = 0;
    #[inline]
    unsafe fn maybe_handle(&mut self, _: *const (), world: &mut World<S>) {
        let world: *mut World<S> = world;
        unsafe {
            let mut clock = World::clock(world);
            self.visit(TypeId::of::<E>(), world, &mut clock);
            World::set_clock(world, clock);
        }
    }
    #[inline]
    fn maybe_defer(&mut self, _: &mut <World<S> as SystemWorld>::Queue) {}
    #[inline]
    unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<World<S>, R>>(
        context: &executor::Context<'_, World<S>, R>,
        flow: Flow,
    ) -> Tail<World<S>, R>
    where
        World<S>: Send + Sync,
    {
        let mut clock = flow.clock();
        // SAFETY: registrations run in order, so this task holds the world.
        unsafe {
            let ran = (*P::get(context.nodes)).visit(TypeId::of::<E>(), context.world, &mut clock);
            K::resume(context, Flow::new(clock, flow.ran() || ran))
        }
    }
}
