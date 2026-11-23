#![cfg(feature = "lang-rust")]

use acdd_index::ast;
pub(crate) mod linker {
    pub(crate) fn link(
        all: &std::collections::BTreeMap<String, acdd_index::models::Facts>,
    ) -> acdd_index::models::Graph {
        crate::common::link_facts(all)
    }
}
use std::collections::BTreeMap;

mod builtin_receivers;
mod deref_pointers;
mod field_chains;
mod iterators;
mod module_paths;
mod trait_dispatch;
mod value_flow;
