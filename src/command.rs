//! Deferred structural changes. Each system owns a queue of typed commands,
//! stored inline as bytes after their monomorphized apply function: no boxing,
//! no locking between parallel systems.
use std::marker::PhantomData;
use std::mem::{MaybeUninit, size_of};

use crate::lookup::ComponentLookup;
use crate::state::State;
use crate::storage::Reserve;
use crate::{Bundle, Entity, Storage, World};

/// Applies (or, without a world, drops) the command at `value`.
type Apply<S> = unsafe fn(*mut MaybeUninit<u8>, Option<&mut World<S>>);
/// Precedes each command: the cursor passes the whole record before it runs.
struct Meta<S: Storage> {
    apply: Apply<S>,
    size: usize,
}

#[doc(hidden)]
pub struct CommandQueue<S: Storage> {
    bytes: Vec<MaybeUninit<u8>>,
    world: PhantomData<fn(&mut World<S>)>,
}
// Every queued command is Send.
unsafe impl<S: Storage> Send for CommandQueue<S> {}
impl<S: Storage> crate::init::Init for CommandQueue<S> {
    fn init() -> Self {
        Self::default()
    }
}
impl<S: Storage> Default for CommandQueue<S> {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            world: PhantomData,
        }
    }
}
impl<S: Storage> CommandQueue<S> {
    fn push<C: FnOnce(&mut World<S>) + Send + 'static>(&mut self, command: C) {
        unsafe fn apply<S: Storage, C: FnOnce(&mut World<S>)>(
            value: *mut MaybeUninit<u8>,
            world: Option<&mut World<S>>,
        ) {
            let command = unsafe { value.cast::<C>().read_unaligned() };
            if let Some(world) = world {
                command(world);
            }
        }
        let meta = Meta::<S> {
            apply: apply::<S, C>,
            size: size_of::<C>(),
        };
        let start = self.bytes.len();
        let len = size_of::<Meta<S>>() + size_of::<C>();
        self.bytes.reserve(len);
        unsafe {
            let at = self.bytes.as_mut_ptr().add(start);
            at.cast::<Meta<S>>().write_unaligned(meta);
            at.add(size_of::<Meta<S>>())
                .cast::<C>()
                .write_unaligned(command);
            self.bytes.set_len(start + len);
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// Move every command of `other` to the end of this queue. Records are
    /// position-independent, so moving their bytes moves the commands.
    pub(crate) fn append(&mut self, other: &mut Self) {
        self.bytes.append(&mut other.bytes);
    }
    /// Apply every command in order. A panicking command drops the rest.
    pub(crate) fn apply(&mut self, world: &mut World<S>) {
        self.drain(Some(world));
    }
    fn drain(&mut self, mut world: Option<&mut World<S>>) {
        struct Rest<S: Storage> {
            base: *mut MaybeUninit<u8>,
            cursor: usize,
            end: usize,
            world: PhantomData<S>,
        }
        impl<S: Storage> Rest<S> {
            /// Take the next command; the cursor passes it before it runs.
            unsafe fn next(&mut self, world: Option<&mut World<S>>) {
                unsafe {
                    let at = self.base.add(self.cursor);
                    let meta = at.cast::<Meta<S>>().read_unaligned();
                    self.cursor += size_of::<Meta<S>>() + meta.size;
                    (meta.apply)(at.add(size_of::<Meta<S>>()), world);
                }
            }
        }
        impl<S: Storage> Drop for Rest<S> {
            fn drop(&mut self) {
                while self.cursor < self.end {
                    unsafe { self.next(None) };
                }
            }
        }
        let end = self.bytes.len();
        // Ownership of the commands moves to `rest` before any of them runs.
        unsafe { self.bytes.set_len(0) };
        let mut rest = Rest::<S> {
            base: self.bytes.as_mut_ptr(),
            cursor: 0,
            end,
            world: PhantomData,
        };
        while rest.cursor < rest.end {
            unsafe { rest.next(world.as_deref_mut()) };
        }
    }
}
impl<S: Storage> Drop for CommandQueue<S> {
    fn drop(&mut self) {
        self.drain(None);
    }
}

/// Structural changes recorded during a system and applied when the `emit`
/// that ran it finishes (or right after `System::run`), in system order. In a
/// hook, they apply before the structural operation that fired it returns.
/// Commands on entities despawned before they apply do nothing.
pub struct Commands<'a, S: Storage> {
    pub(crate) queue: &'a mut CommandQueue<S>,
    pub(crate) reserve: Reserve<'a>,
}
impl<S: Storage> Commands<'_, S> {
    /// Spawn a bundle. The entity is reserved now and live once applied.
    pub fn spawn<B: Bundle<S> + Send + 'static>(&mut self, bundle: B) -> Entity {
        let entity = self.reserve.entity();
        self.queue
            .push(move |world: &mut World<S>| world.spawn_reserved(entity, bundle));
        entity
    }
    pub fn insert<B: Bundle<S> + Send + 'static>(&mut self, entity: Entity, bundle: B) {
        self.queue.push(move |world: &mut World<S>| {
            if world.contains(entity) {
                world.insert_bundle(entity, bundle);
            }
        });
    }
    pub fn remove<B: Bundle<S> + 'static>(&mut self, entity: Entity) {
        self.queue.push(move |world: &mut World<S>| {
            world.remove_bundle::<B>(entity);
        });
    }
    pub fn despawn(&mut self, entity: Entity) {
        self.queue.push(move |world: &mut World<S>| {
            world.despawn(entity);
        });
    }
    pub fn insert_resource<T: Send + 'static>(&mut self, value: T)
    where
        S: ComponentLookup<T>,
    {
        self.queue.push(move |world: &mut World<S>| {
            world.insert_resource(value);
        });
    }
    pub fn remove_resource<T: 'static>(&mut self)
    where
        S: ComponentLookup<T>,
    {
        self.queue.push(move |world: &mut World<S>| {
            world.remove_resource::<T>();
        });
    }
    pub fn set_state<T: State>(&mut self, state: T) {
        self.queue
            .push(move |world: &mut World<S>| world.set_state(state));
    }
    pub fn clear_state<G: 'static>(&mut self) {
        self.queue
            .push(move |world: &mut World<S>| world.clear_state::<G>());
    }
    /// Any deferred change with exclusive world access.
    pub fn queue(&mut self, command: impl FnOnce(&mut World<S>) + Send + 'static) {
        self.queue.push(command);
    }
}
