#![feature(specialization, gca_min_const_items, gca_const_items)]
#![deny(unsafe_op_in_unsafe_fn)]

mod access;
mod change;
mod column;
mod dispatch;
mod executor;
mod filter;
mod graph;
mod lookup;
mod param;
mod plan;
mod query;
mod storage;
mod vocab;

pub use change::{Mut, MutSlice};
pub use dispatch::{
    App, AppBuilder, Callback, Cons, Dispatch, Handle, Install, MaybeHandle, Nil, On, Run,
    Scheduled, system,
};
pub use executor::WorkerPool;
pub use filter::{Added, Changed, Or, With, Without};
pub use graph::{GraphConfig, MAX_NODES, Node, ReadyGraph, node, ready};
pub use lookup::ComponentLookup as Has;
pub use param::Local;
pub use plan::{
    ExclusiveSystem, ExecutionMode, IntoSystemConfigs, IntoSystems, Shape, System, exclusive,
};
pub use query::{ContiguousError, Query, QueryChunks, Single};

pub mod prelude {
    pub use crate::{
        Added, App, Changed, Components, ContiguousError, Entity, ExecutionMode, Has,
        IntoSystemConfigs, Local, Mut, MutSlice, Or, Query, Single, Sparse, Storage, With, Without,
        WorkerPool, World, exclusive, node, ready, system,
    };
}
pub use storage::{Bundle, Components, Entity, Sparse, Storage, World};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
