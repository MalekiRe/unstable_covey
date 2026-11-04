#![feature(
    specialization,
    gca_min_const_items,
    gca_const_items,
    associated_type_defaults,
    impl_trait_in_assoc_type
)]
#![deny(unsafe_op_in_unsafe_fn)]

mod access;
mod bridge;
mod change;
mod column;
mod command;
mod dispatch;
mod executor;
mod filter;
mod graph;
mod hook;
mod immutable;
mod init;
mod join;
mod lookup;
mod param;
mod plan;
mod plugin;
mod query;
mod relation;
mod remote;
mod storage;
mod task;
mod vocab;

pub use bridge::{AsyncWorld, Bridge, BridgeError, BridgeFn, BridgeState};
pub use change::{Mut, MutSlice};
pub use command::Commands;
pub use dispatch::{
    App, AppBuilder, Cons, Dispatch, Handle, Label, MaybeHandle, Nil, On, Placed, Plugins, Run,
    Schedule, Scheduled, schedule,
};
pub use executor::WorkerPool;
pub use filter::{Added, Changed, Or, Related, Relinked, With, Without};
pub use graph::{GraphConfig, MAX_NODES, Node, ReadyGraph, node, ready};
pub use hook::{Batch, Event, Hook, OnAdd, OnInsert, OnRemove, OnReplace, Trigger};
pub use immutable::Immutable;
pub use join::{From, JoinKind, Joined, To};
pub use lookup::ComponentLookup as Stores;
pub use param::Local;
pub use plan::{
    ExclusiveSystem, ExecutionMode, FunctionSystem, Input, IntoSystemConfigs, IntoSystems, NoInput,
    Ref, Shape, System, exclusive,
};
pub use plugin::{Plugin, Systems};
pub use query::{ContiguousError, Has, Query, QueryChunks, Single};
pub use relation::{ManyToMany, OneToMany, OneToOne, Relates, Relation, RelationHooks, Sources};
pub use remote::{
    Envelope, Everything, Remote, RemoteBridge, RemoteState, SyncPoint, SyncPointSystem, sync_point,
};
pub use task::{Task, TaskFn, task};

pub mod prelude {
    pub use crate::{
        Added, App, AsyncWorld, BridgeError, Changed, Commands, Components, ContiguousError,
        Entity, ExecutionMode, Has, Immutable, IntoSystemConfigs, Local, ManyToMany, Mut, MutSlice,
        NoInput, OnAdd, OnInsert, OnRemove, OnReplace, OneToMany, OneToOne, Or, Plugin, Query, Ref,
        Related, Relates, Relation, Relinked, Remote, Schedule, Single, Sources, Sparse, Storage,
        Stores, SyncPoint, Systems, Trigger, With, Without, WorkerPool, World, exclusive, node,
        ready, schedule, sync_point, task,
    };
}
pub use storage::{Bundle, Components, Entity, Point, Sparse, Storage, World};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
