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

pub use change::{Mut, MutSlice};
pub use dispatch::{
    App, Bus, Cons, Dispatch, Handle, Install, Listener, MaybeHandle, Nil, On, Run, Scheduled,
    Subscribe, subscribe, system,
};
pub use executor::WorkerPool;
pub use filter::{Added, Changed, Or, With, Without};
pub use graph::{GraphConfig, Node, ReadyGraph, node, ready};
pub use param::Local;
pub use plan::{ExecutionMode, IntoSystemConfigs, IntoSystems, System, exclusive};
pub use query::{ContiguousError, Query, QueryChunks, Single};

pub mod prelude {
    pub use crate::{
        Added, App, BundleSlot, Bus, Changed, Components, ContiguousError, Entity, ExecutionMode,
        IntoSystemConfigs, Local, Mut, MutSlice, Or, Query, Single, Sparse, Storage, With, Without,
        WorkerPool, World, exclusive, node, ready, subscribe, system,
    };
}
pub use storage::{Bundle, BundleSlot, Components, Entity, Sparse, Storage, World};
