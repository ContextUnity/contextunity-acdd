pub mod c_cpp;
pub mod cargo;
pub mod csharp;
pub mod go;
pub mod jvm;
pub mod npm;
pub mod php;
pub mod python;
pub mod ruby;

use super::parser::ManifestParser;

/// Registered ecosystem manifest parsers evaluated during workspace discovery.
pub(crate) static MANIFEST_PARSERS: &[&'static dyn ManifestParser] = &[
    &cargo::CARGO_PARSER,
    &npm::NPM_PARSER,
    &python::PYTHON_PARSER,
    &go::GO_PARSER,
    &jvm::JVM_PARSER,
    &php::PHP_PARSER,
    &csharp::CSHARP_PARSER,
    &ruby::RUBY_PARSER,
    &c_cpp::C_CPP_PARSER,
];
