mod collect;
mod framework_packs;
/// Trait-isolated ecosystem manifest parser interface and registry.
pub(crate) mod parser;
/// Built-in ecosystem manifest parser implementations.
pub(crate) mod parsers;
pub(crate) mod registry;
#[cfg(test)]
mod tests;

pub use collect::is_dependency_manifest_filename;
pub use framework_packs::named_binding_scope;
pub(crate) use framework_packs::{
    active_component_module_additions, component_environment_config_path,
    component_export_matches_provider, named_binding_project,
};
pub use registry::DependencyRegistry;
#[cfg(test)]
pub(crate) use registry::{ManifestSource, WorkspaceManifestSources};
