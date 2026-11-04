#![feature(
    specialization,
    gca_min_const_items,
    gca_const_items,
    associated_type_defaults
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
mod lookup;
mod param;
mod plan;
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
    App, AppBuilder, Cons, Dispatch, Handle, Label, Listener, MaybeHandle, Nil, On, Placed,
    Plugins, Run, Schedule, Scheduled, schedule,
};
pub use executor::WorkerPool;
pub use filter::{Added, Changed, Or, Related, With, Without};
pub use graph::{GraphConfig, MAX_NODES, Node, ReadyGraph, node, ready};
pub use hook::{Batch, Event, Hook, OnAdd, OnInsert, OnRemove, OnReplace, Trigger};
pub use lookup::ComponentLookup as Has;
pub use param::Local;
pub use plan::{
    ExclusiveSystem, ExecutionMode, FunctionSystem, Input, IntoSystemConfigs, IntoSystems, NoInput,
    Ref, Shape, System, exclusive,
};
pub use query::{ContiguousError, Query, QueryChunks, Single};
pub use relation::{
    ManyToMany, OneToMany, OneToOne, Relates, Relation, RelationHooks, Sources, relation,
};
pub use remote::{Envelope, Remote, RemoteBridge, RemoteState, SyncPoint, sync_point};
pub use task::{Task, TaskFn, task};

pub mod prelude {
    pub use crate::{
        Added, App, AsyncWorld, BridgeError, Changed, Commands, Components, ContiguousError,
        Entity, ExecutionMode, Has, IntoSystemConfigs, Local, ManyToMany, Mut, MutSlice, NoInput,
        OnAdd, OnInsert, OnRemove, OnReplace, OneToMany, OneToOne, Or, Query, Ref, Related,
        Relates, Relation, Schedule, Single, Sources, Sparse, Storage, Trigger, With, Without,
        WorkerPool, World, exclusive, node, ready, schedule, sync_point, task,
    };
}
pub use storage::{Bundle, Components, Entity, Sparse, Storage, World};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
