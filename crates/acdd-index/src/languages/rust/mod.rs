use crate::ast::relations;
use crate::ast::scope_analysis::ScopeAnalysis;
use crate::ast::syntax::{field, symbol_name, text, ScopeBindings, Syntax};
use crate::languages::context::{FileContext, LanguageContext};
use crate::languages::profile::LanguageProfile;
use crate::languages::types::{
    call, workspace_path, AstSearchCapabilities, AstSearchProbe, CallForm, DeclarationRole,
    ImportKind, ImportPath, LanguageFamily, LanguageFileState, RustFileState, SymbolMetadata,
    SyntaxContext, SyntaxGrammarSpec, Visibility,
};
use crate::models::{Facts, ReceiverHint, Reference};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub(crate) mod cli_registration;
mod macros;
pub(crate) mod patterns;
mod registrations;
pub(crate) mod value_flow;
pub(crate) mod visibility;
use patterns::collect_use;

fn language() -> tree_sitter::Language {
    tree_sitter_rust::language()
}

fn kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" => Some("function"),
        "function_signature_item" => Some("function"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "trait_item" => Some("trait"),
        "type_item" | "associated_type" => Some("type"),
        "const_item" | "static_item" | "enum_variant" => Some("constant"),
        "impl_item" => Some("impl"),
        "mod_item" => Some("module_declaration"),
        "macro_definition" => Some("macro"),
        _ => None,
    }
}

fn method_owner(node: Syntax<'_>) -> Option<Syntax<'_>> {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        match ancestor.kind() {
            "declaration_list" => parent = ancestor.parent(),
            "impl_item" | "trait_item" => return Some(ancestor),
            _ => return None,
        }
    }
    None
}

fn has_cfg_test(node: Syntax<'_>, source: &str) -> bool {
    let mut current = Some(node);
    while let Some(declaration) = current {
        let mut sibling = declaration.prev_named_sibling();
        while let Some(attribute) = sibling.filter(|sibling| sibling.kind() == "attribute_item") {
            if crate::models::details::is_cfg_test_attribute(text(attribute, source)) {
                return true;
            }
            sibling = attribute.prev_named_sibling();
        }
        current = declaration.parent();
    }
    false
}

fn has_main_attribute(node: Syntax<'_>, source: &str) -> bool {
    let mut previous = node.prev_named_sibling();
    while let Some(attribute) = previous.filter(|node| node.kind() == "attribute_item") {
        let raw: String = text(attribute, source)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if ["#[tokio::main", "#[async_std::main"].iter().any(|prefix| {
            raw.strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with(']') || suffix.starts_with('('))
        }) {
            return true;
        }
        previous = attribute.prev_named_sibling();
    }
    false
}

fn unrestricted_public(node: Syntax<'_>, source: &str) -> bool {
    if node.kind() == "macro_definition" {
        let mut previous = node.prev_named_sibling();
        while let Some(attribute) = previous.filter(|node| node.kind() == "attribute_item") {
            let raw: String = text(attribute, source)
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            if raw
                .strip_prefix("#[macro_export")
                .is_some_and(|tail| tail == "]" || tail.starts_with('('))
            {
                return true;
            }
            previous = attribute.prev_named_sibling();
        }
    }
    text(node, source)
        .trim_start()
        .strip_prefix("pub")
        .and_then(|rest| rest.chars().next())
        .is_some_and(char::is_whitespace)
}

fn static_module_path(node: Syntax<'_>, source: &str) -> Option<Option<String>> {
    let mut path = None;
    let mut previous = node.prev_named_sibling();
    while let Some(attribute) = previous.filter(|node| node.kind() == "attribute_item") {
        let raw = text(attribute, source)
            .trim()
            .strip_prefix("#[")?
            .strip_suffix(']')?;
        let (name, value) = raw.split_once('=')?;
        if name.trim() != "path" || path.is_some() {
            return None;
        }
        let value: String = serde_json::from_str(value.trim()).ok()?;
        let name = text(node.child_by_field_name("name")?, source);
        if value != format!("{name}.rs") {
            return None;
        }
        path = Some(value);
        previous = attribute.prev_named_sibling();
    }
    Some(path)
}

fn is_proven_bound(bound: &str) -> bool {
    bound.split('+').map(str::trim).all(|part| {
        part.is_empty()
            || part.starts_with('\'')
            || matches!(part, "Sized" | "Send" | "Sync" | "Unpin")
            || part.starts_with("Fn(")
            || part.starts_with("FnMut(")
            || part.starts_with("FnOnce(")
    })
}

fn is_conversion_bound(bound: &str) -> bool {
    is_proven_bound(bound)
        || bound.split('+').map(str::trim).all(|part| {
            let clean = part.split('<').next().unwrap_or(part).trim();
            is_proven_bound(part)
                || matches!(
                    clean.rsplit("::").next().unwrap_or(clean),
                    "Into" | "AsRef" | "Borrow" | "ToString" | "Display" | "Debug"
                )
        })
}

fn has_unproven_bounds_with(node: Syntax<'_>, source: &str, check: fn(&str) -> bool) -> bool {
    node.named_children(&mut node.walk()).any(|child| {
        child.kind() == "where_clause"
            && child.named_children(&mut child.walk()).any(|predicate| {
                let raw = text(predicate, source).trim_start();
                !raw.starts_with('\'') && !raw.split_once(':').is_some_and(|(_, rhs)| check(rhs))
            })
    }) || node
        .child_by_field_name("type_parameters")
        .is_some_and(|parameters| {
            parameters
                .named_children(&mut parameters.walk())
                .any(|parameter| {
                    let raw = text(parameter, source).trim_start();
                    !matches!(parameter.kind(), "lifetime" | "lifetime_parameter")
                        && !raw.starts_with('\'')
                        && (raw.contains('=')
                            || raw.split_once(':').is_some_and(|(_, rhs)| !check(rhs)))
                })
        })
}

fn has_unproven_bounds(node: Syntax<'_>, source: &str) -> bool {
    has_unproven_bounds_with(node, source, is_proven_bound)
}

fn path_reference_parameters(node: Syntax<'_>, source: &str, facts: &Facts) -> bool {
    if node
        .named_children(&mut node.walk())
        .any(|child| child.kind() == "where_clause")
    {
        return false;
    }
    let Some(parameters) = node.child_by_field_name("type_parameters") else {
        return false;
    };
    let mut bounded = false;
    for parameter in parameters.named_children(&mut parameters.walk()) {
        let raw: String = text(parameter, source)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if raw.contains('=') {
            return false;
        }
        if let Some((_, bound)) = raw.split_once(':') {
            if bound != "AsRef<Path>" {
                return false;
            }
            bounded = true;
        }
    }
    if !bounded
        || !facts.references.iter().any(|reference| {
            reference.kind == "imports"
                && reference.alias.as_deref() == Some("Path")
                && reference.module.as_deref() == Some("std::path::Path")
        })
    {
        return false;
    }
    if facts.references.iter().any(|reference| {
        reference.kind == "imports"
            && match reference.alias.as_deref() {
                Some("AsRef") => reference.module.as_deref() != Some("std::convert::AsRef"),
                Some("Path") => reference.module.as_deref() != Some("std::path::Path"),
                _ => false,
            }
    }) {
        return false;
    }
    let mut scope = node.parent();
    while let Some(parent) = scope {
        if parent.named_children(&mut parent.walk()).any(|child| {
            matches!(
                child.kind(),
                "struct_item" | "enum_item" | "type_item" | "trait_item" | "mod_item"
            ) && child
                .child_by_field_name("name")
                .is_some_and(|name| matches!(text(name, source), "Path" | "AsRef" | "std"))
        }) {
            return false;
        }
        scope = parent.parent();
    }
    true
}

fn uniform_generic_impl(node: Syntax<'_>, source: &str) -> bool {
    let Some(receiver) = node.child_by_field_name("type") else {
        return false;
    };
    if receiver.kind() != "generic_type" || has_unproven_bounds(node, source) {
        return false;
    }
    let Some(arguments) = receiver.child_by_field_name("type_arguments") else {
        return false;
    };
    let is_lifetime = |n: Syntax<'_>| {
        matches!(n.kind(), "lifetime" | "lifetime_parameter")
            || text(n, source).trim_start().starts_with('\'')
    };
    let mut names: Vec<_> = node
        .child_by_field_name("type_parameters")
        .map(|parameters| {
            parameters
                .named_children(&mut parameters.walk())
                .filter(|parameter| !is_lifetime(*parameter))
                .map(|parameter| text(parameter, source))
                .collect()
        })
        .unwrap_or_default();
    if names.iter().any(|name| {
        !name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }) {
        return false;
    }
    let mut saw_any_arg = false;
    for argument in arguments.named_children(&mut arguments.walk()) {
        saw_any_arg = true;
        if is_lifetime(argument) {
            continue;
        }
        if argument.kind() != "type_identifier" {
            return false;
        }
        let Some(index) = names
            .iter()
            .position(|name| *name == text(argument, source))
        else {
            return false;
        };
        names.swap_remove(index);
    }
    saw_any_arg && names.is_empty()
}

fn has_non_lifetime_type_params(node: Syntax<'_>, source: &str) -> bool {
    node.child_by_field_name("type_parameters")
        .is_some_and(|params| {
            let mut cursor = params.walk();
            let found = params.named_children(&mut cursor).any(|param| {
                !matches!(param.kind(), "lifetime" | "lifetime_parameter")
                    && !text(param, source).trim_start().starts_with('\'')
            });
            found
        })
}

fn is_primitive_type(name: &str) -> bool {
    matches!(
        name,
        "str"
            | "bool"
            | "char"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
    )
}

fn call_result_hint(node: Syntax<'_>, source: &str) -> Option<(ReceiverHint, bool)> {
    let function = node.child_by_field_name("function")?;
    let function = if function.kind() == "generic_function" {
        function.child_by_field_name("function")?
    } else {
        function
    };
    if function.kind() != "field_expression" {
        return None;
    }
    let receiver = function.child_by_field_name("value")?;
    let member = function.child_by_field_name("field")?;
    if member.kind() != "field_identifier" {
        return None;
    }
    let value = value_flow::value(receiver, source);
    let clear_dynamic = matches!(
        value,
        crate::semantic::ValueExpr::Annotated { .. }
            | crate::semantic::ValueExpr::Construct { .. }
            | crate::semantic::ValueExpr::Call { site: Some(_), .. }
    ) || matches!(&value, crate::semantic::ValueExpr::Alias { name } if name.contains("::"))
        || matches!(&value, crate::semantic::ValueExpr::Call { callee, site: None } if callee.contains("::"))
        || matches!(
            &value,
            crate::semantic::ValueExpr::RustProject {
                operation: crate::semantic::types::RustProjection::RangeSlice
                    | crate::semantic::types::RustProjection::ElementIndex
                    | crate::semantic::types::RustProjection::Index,
                ..
            }
        );
    let non_opaque = clear_dynamic
        || matches!(
            value,
            crate::semantic::ValueExpr::Project { .. }
                | crate::semantic::ValueExpr::RustField { .. }
                | crate::semantic::ValueExpr::RustProject { .. }
        );
    if non_opaque {
        if let Ok(serialized) = serde_json::to_string(&value) {
            return Some((
                ReceiverHint::CallResult {
                    callee: format!("value:{serialized}"),
                    member: text(member, source).to_owned(),
                },
                clear_dynamic,
            ));
        }
    }
    if receiver.kind() != "call_expression" {
        return None;
    }
    let callee = receiver
        .child_by_field_name("function")
        .and_then(|node| value_flow::callee_name(node, source))?;
    Some((
        ReceiverHint::CallResult {
            callee: callee.into_owned(),
            member: text(member, source).to_owned(),
        },
        false,
    ))
}

fn set_owner_flag(facts: &mut Facts, owner_id: &str, key: &str) {
    if let Some(owner) = facts.nodes.iter_mut().find(|node| node.id == owner_id) {
        *owner.detail_entry_mut(key) = serde_json::json!(true);
    }
}

fn is_non_prelude_stdlib_receiver(name: &str) -> bool {
    matches!(
        name,
        "Path" | "PathBuf" | "Rc" | "Arc" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
    )
}

pub(crate) struct Rust;
pub(crate) static RUST: Rust = Rust;

pub(crate) fn cargo_manifest_metadata(content: &str) -> (Option<String>, Vec<String>) {
    let mut package_name = None;
    let mut library_name = None;
    let mut invalid_name = false;
    let mut library_path = None;
    let mut autolib = None;
    let mut explicit_library = false;
    let mut invalid_layout = false;
    let mut dependencies = Vec::new();
    super::toml_manifest::visit_toml_pairs(content, &mut |path, value| {
        explicit_library |= path.starts_with("lib.");
        if path == "lib.path" {
            let declared = super::toml_manifest::toml_string(value, content);
            invalid_layout |= library_path.is_some()
                || !matches!(declared.as_deref(), Some("src/lib.rs" | "./src/lib.rs"));
            library_path = declared;
        } else if path == "package.autolib" {
            let declared = match &content[value.byte_range()] {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            invalid_layout |= autolib.is_some() || declared.is_none();
            autolib = declared;
            if declared == Some(false) {
                let mut document = value;
                while let Some(parent) = document.parent() {
                    document = parent;
                }
                let mut cursor = document.walk();
                explicit_library |= document.named_children(&mut cursor).any(|node| {
                    node.kind() == "table"
                        && node.named_child(0).is_some_and(|header| {
                            matches!(&content[header.byte_range()], "lib" | "'lib'" | "\"lib\"")
                        })
                });
            }
        }
        if matches!(path, "package.name" | "lib.name") {
            let name = super::toml_manifest::toml_string(value, content);
            let slot = if path == "package.name" {
                &mut package_name
            } else {
                &mut library_name
            };
            invalid_name |= slot.is_some()
                || name.as_deref().is_none_or(|name| {
                    name.is_empty()
                        || !name
                            .chars()
                            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))
                });
            *slot = name;
        }
        let parts: Vec<_> = path.split('.').collect();
        let is_dependency_section = |section: &str| {
            matches!(
                section,
                "dependencies" | "dev-dependencies" | "build-dependencies"
            )
        };
        let dependency_name = match parts.as_slice() {
            [section, name, ..] if is_dependency_section(section) => Some(*name),
            ["workspace", "dependencies", name, ..] => Some(*name),
            ["target", _, section, name, ..] if is_dependency_section(section) => Some(*name),
            _ => None,
        };
        if let Some(name) = dependency_name {
            let name = name.replace('-', "_");
            if !name.is_empty() {
                dependencies.push(name);
            }
        }
    });
    dependencies.sort();
    dependencies.dedup();
    let crate_name = if invalid_name
        || invalid_layout
        || (autolib == Some(false) && !explicit_library)
        || package_name.is_none()
    {
        None
    } else {
        library_name
            .or(package_name)
            .map(|name| name.replace('-', "_"))
    };
    (crate_name, dependencies)
}

impl Rust {
    pub(crate) fn normalize_root_crate_import(
        &self,
        crate_name: &str,
        module: &str,
    ) -> Option<ImportPath> {
        visibility::normalize_root_crate_import(crate_name, module)
    }
}

static RUST_STD_MEMBERS: std::sync::OnceLock<
    std::collections::HashMap<String, std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

static RUST_CARGO_TYPES: std::sync::OnceLock<std::collections::HashSet<String>> =
    std::sync::OnceLock::new();

#[derive(Debug, Clone, serde::Deserialize)]
struct DeclarativeMacroEntry {
    name: String,
    provider: String,
    pack: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeclarativeMacrosFile {
    macros: Vec<DeclarativeMacroEntry>,
}

static DECLARATIVE_MACRO_TABLE: std::sync::OnceLock<
    std::collections::HashMap<String, (&'static str, &'static str)>,
> = std::sync::OnceLock::new();

pub fn declarative_macro_classification(expression: &str) -> Option<(&'static str, &'static str)> {
    let table = DECLARATIVE_MACRO_TABLE.get_or_init(|| {
        let parsed: DeclarativeMacrosFile =
            ::toml::from_str(include_str!("declarative_macros.toml"))
                .expect("declarative_macros.toml must be valid TOML");
        let mut map = std::collections::HashMap::new();
        for entry in parsed.macros {
            let provider: &'static str = Box::leak(entry.provider.into_boxed_str());
            let pack: &'static str = Box::leak(entry.pack.into_boxed_str());
            map.insert(entry.name.clone(), (provider, pack));
            if provider != "Rust standard library" {
                map.insert(format!("{provider}::{}", entry.name), (provider, pack));
            }
            for alias in entry.aliases {
                map.insert(alias, (provider, pack));
            }
        }
        map
    });
    let name = expression.strip_prefix("::").unwrap_or(expression);
    table.get(name).copied()
}

pub fn is_registered_rust_type(name: &str) -> bool {
    let colon_name = name.replace('.', "::");
    let dot_name = name.replace("::", ".");
    let short = name.rsplit("::").next().unwrap_or(name);
    let short = short.rsplit('.').next().unwrap_or(short);
    rust_std_members().contains_key(name)
        || rust_std_members().contains_key(&colon_name)
        || rust_std_members().contains_key(&dot_name)
        || rust_std_members().contains_key(short)
        || rust_cargo_types().contains(name)
        || rust_cargo_types().contains(&colon_name)
        || rust_cargo_types().contains(&dot_name)
}

fn rust_cargo_types() -> &'static std::collections::HashSet<String> {
    RUST_CARGO_TYPES.get_or_init(|| {
        let cargo_manifest = super::manifests::FrameworkManifest::parse(
            "rust-cargo".to_owned(),
            "toml",
            include_str!("../manifests/rust/rust-cargo.toml"),
        )
        .expect("rust-cargo.toml must be valid");
        let mut types = std::collections::HashSet::new();
        for rule in cargo_manifest.class_hierarchy_receivers {
            for base in rule.bases {
                if rule.provider == "rayon" && matches!(base.as_str(), "Vec" | "Slice") {
                    continue;
                }
                if matches!(base.as_str(), "serde_json" | "serde_yaml") {
                    continue;
                }
                if base.contains("::") {
                    types.insert(base.replace("::", "."));
                    types.insert(base);
                }
            }
        }
        types
    })
}

fn rust_std_members(
) -> &'static std::collections::HashMap<String, std::collections::HashSet<String>> {
    RUST_STD_MEMBERS.get_or_init(|| {
        let manifest = super::manifests::FrameworkManifest::parse(
            "rust-std".to_owned(),
            "toml",
            include_str!("../manifests/rust/rust-std.toml"),
        )
        .expect("rust-std.toml must be valid");
        let mut members: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        for rule in manifest.class_hierarchy_receivers {
            for base in rule.bases {
                let short = base.rsplit("::").next().unwrap_or(&base);
                let include_short = matches!(
                    short,
                    "Option"
                        | "Result"
                        | "Vec"
                        | "String"
                        | "Box"
                        | "ToOwned"
                        | "ToString"
                        | "Into"
                        | "From"
                        | "AsRef"
                        | "AsMut"
                        | "Deref"
                        | "DerefMut"
                        | "Clone"
                        | "Default"
                        | "Display"
                        | "Debug"
                        | "Iterator"
                        | "Drop"
                        | "Slice"
                        | "Tuple"
                        | "Range"
                        | "str"
                        | "Path"
                        | "PathBuf"
                        | "Rc"
                        | "Arc"
                        | "Cow"
                        | "VecDeque"
                        | "HashMap"
                        | "HashSet"
                        | "BTreeMap"
                        | "BTreeSet"
                        | "OsStr"
                        | "OsString"
                );
                let dotted = base.replace("::", ".");
                let candidates: [&str; 3] = [
                    base.as_str(),
                    dotted.as_str(),
                    if include_short { short } else { base.as_str() },
                ];
                for name in candidates {
                    members
                        .entry(name.to_owned())
                        .or_default()
                        .extend(rule.members.iter().cloned());
                }
            }
        }
        members
    })
}

static RUST_SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declarations: &[("mod_item", "module_declaration")],
    declaration_roles: &[
        ("function_item", DeclarationRole::Function),
        ("function_signature_item", DeclarationRole::Function),
        ("struct_item", DeclarationRole::Struct),
        ("enum_item", DeclarationRole::Enum),
        ("trait_item", DeclarationRole::Trait),
        ("type_item", DeclarationRole::Type),
        ("associated_type", DeclarationRole::Type),
        ("const_item", DeclarationRole::Constant),
        ("static_item", DeclarationRole::Constant),
        ("enum_variant", DeclarationRole::Constant),
        ("impl_item", DeclarationRole::Impl),
        ("macro_definition", DeclarationRole::Macro),
    ],
    references: &[
        "call_expression",
        "macro_invocation",
        "struct_expression",
        "array_expression",
        "scoped_identifier",
        "string_literal",
        "raw_string_literal",
    ],
    call_forms: &[
        ("call_expression", CallForm::Direct),
        ("macro_invocation", CallForm::Macro),
    ],
    scopes: &[
        "source_file",
        "function_item",
        "function_signature_item",
        "closure_expression",
        "impl_item",
        "trait_item",
        "mod_item",
        "struct_item",
        "enum_item",
    ],
    imports: &["use_declaration"],
    import_forms: &[("use_declaration", ImportKind::Module)],
    comments: &["line_comment", "block_comment"],
    visibility_modifiers: &[("visibility_modifier", Visibility::Public)],
    mutations: &[
        "field_declaration",
        "compound_assignment_expr",
        "field_expression",
    ],
    ..SyntaxGrammarSpec::DEFAULT
};

impl LanguageProfile for Rust {
    fn id(&self) -> &'static str {
        "rust"
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(&RUST_SPEC)
    }
    fn route_extraction_enabled(&self) -> bool {
        false
    }
    fn ast_search_capabilities(&self) -> AstSearchCapabilities {
        AstSearchCapabilities {
            declaration_kinds: &[
                "function_item",
                "struct_item",
                "enum_item",
                "impl_item",
                "trait_item",
            ],
            declaration_body_kinds: &[
                "block",
                "field_declaration_list",
                "enum_variant_list",
                "declaration_list",
            ],
            declaration_completions: &[" {}", "() { __acdd_meta_body(); }"],
            fragment_probe: Some(AstSearchProbe {
                prefix: "fn __acdd_ast_pattern_probe() { ",
                suffix: " }",
            }),
            attribute_kinds: &["attribute_item"],
            valid_examples: &[
                "fn example",
                "fn example() {}",
                "client.fetch($VALUE)",
                "#[derive(Debug)] struct Item {}",
            ],
            diagnostic_hint: "Use a complete Rust item or expression, a bodyless declaration such as 'fn example', or a supported expression or macro fragment.",
        }
    }
    fn manifest_filenames(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }
    fn extract_manifest_dependencies(&self, filename: &str, content: &str) -> Vec<String> {
        if filename != "Cargo.toml" {
            return Vec::new();
        }
        cargo_manifest_metadata(content).1
    }
    fn is_stdlib(&self, module: &str) -> bool {
        visibility::is_rust_stdlib(module)
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }
    fn grammar(&self, _path: &str) -> tree_sitter::Language {
        language()
    }
    fn family(&self) -> LanguageFamily {
        LanguageFamily("rust")
    }
    fn symbol_kind(&self, k: &str) -> Option<&'static str> {
        kind(k)
    }
    fn symbol(&self, node: Syntax<'_>) -> Option<&'static str> {
        if matches!(node.kind(), "function_item" | "function_signature_item")
            && method_owner(node).is_some()
        {
            Some("method")
        } else {
            self.symbol_kind(node.kind())
        }
    }
    fn node_prefix(&self, kind: &str) -> &'static str {
        if kind == "type" {
            "type"
        } else if kind == "macro" {
            "macro"
        } else {
            "rs"
        }
    }
    fn module_name(&self, path: &str) -> String {
        visibility::rust_module_name(path)
    }
    fn symbol_name<'a>(&self, node: Syntax<'_>, source: &'a str) -> Option<&'a str> {
        if node.kind() == "impl_item" {
            if let Some(ty) = node.child_by_field_name("type") {
                let mut nominal = ty;
                while nominal.kind() == "generic_type" {
                    let Some(base) = nominal.child_by_field_name("type") else {
                        break;
                    };
                    nominal = base;
                }
                let raw = text(nominal, source).trim();
                let clean = raw.split_once('<').map_or(raw, |(base, _)| base.trim_end());
                if !clean.is_empty() {
                    return Some(clean);
                }
            }
        }
        symbol_name(node, source)
    }
    fn value_binding_applies(&self, reference: &Reference) -> bool {
        if reference.kind == "implements" {
            return false;
        }
        let head = reference
            .expression
            .split_once('.')
            .map_or(reference.expression.as_str(), |(before_dot, _)| before_dot);
        !head.contains("::")
    }
    fn prepare_with_context(
        &self,
        root: Syntax<'_>,
        source: &str,
        rules: &std::sync::Arc<LanguageContext>,
    ) -> FileContext {
        registrations::prepare(root, source, rules);
        macros::prepare(root, source);
        value_flow::clear_file_macro_facts();
        FileContext {
            rules: std::sync::Arc::clone(rules),
            state: LanguageFileState::Rust(RustFileState {
                declared_callables: collect_declared_callables(root, source),
            }),
            ..FileContext::default()
        }
    }
    fn finish(&self, facts: &mut Facts) {
        registrations::clear(facts);
        macros::clear();
        value_flow::clear_file_macro_facts();
    }
    fn module_exports_with_bindings(
        &self,
        root: Syntax<'_>,
        source: &str,
        _bindings: &ScopeBindings,
    ) -> Option<Vec<crate::semantic::ExportBinding>> {
        let container = if root.kind() == "mod_item" {
            if has_cfg_test(root, source) {
                return Some(Vec::new());
            }
            let Some(body) = root.child_by_field_name("body") else {
                return Some(Vec::new());
            };
            body
        } else {
            root
        };
        let mut exports = Vec::new();
        let mut cursor = container.walk();
        for declaration in container
            .named_children(&mut cursor)
            .filter(|node| matches!(node.kind(), "use_declaration" | "mod_item"))
        {
            if has_cfg_test(declaration, source) {
                continue;
            }
            if declaration.kind() == "mod_item" {
                let is_inline = declaration.child_by_field_name("body").is_some();
                if unrestricted_public(declaration, source)
                    && !declaration.has_error()
                    && (is_inline || static_module_path(declaration, source).is_some())
                {
                    if let Some(name) = declaration.child_by_field_name("name") {
                        let name = text(name, source).to_owned();
                        exports.push(crate::semantic::ExportBinding {
                            local: Some(name.clone()),
                            name,
                            module: None,
                            type_only: false,
                            star: false,
                        });
                    }
                }
                continue;
            }
            let mut cursor = declaration.walk();
            let is_public = declaration
                .named_children(&mut cursor)
                .find(|child| child.kind() == "visibility_modifier")
                .and_then(|visibility| source.get(visibility.byte_range()))
                .is_some_and(|visibility| visibility.trim() == "pub");
            if !is_public {
                continue;
            }
            let Some(argument) = declaration.child_by_field_name("argument") else {
                continue;
            };
            let mut imports = Vec::new();
            collect_use(argument, source, "", &mut imports);
            exports.extend(imports.into_iter().map(|(target, name)| {
                let star = name == "*";
                let local = (!star).then(|| name.clone());
                crate::semantic::ExportBinding {
                    name,
                    local,
                    module: Some(target),
                    type_only: false,
                    star,
                }
            }));
        }
        Some(exports)
    }
    fn value_flow(&self, node: Syntax<'_>, source: &str) -> crate::semantic::ValueFlowFacts {
        value_flow::extract(node, source)
    }
    fn scope_analysis<'source, 'tree>(
        &self,
        node: Syntax<'tree>,
        source: &'source str,
        _file: &FileContext,
    ) -> Option<ScopeAnalysis<'source, 'tree>> {
        Some(ScopeAnalysis::rust(node, source))
    }
    fn extract_imports(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        if ctx.node.kind() != "use_declaration" {
            return;
        }
        if has_cfg_test(ctx.node, ctx.source)
            && facts
                .nodes
                .iter()
                .find(|node| node.id == ctx.owner)
                .is_some_and(|owner| {
                    matches!(
                        owner.kind,
                        crate::models::tags::NodeKind::Module
                            | crate::models::tags::NodeKind::ModuleDeclaration
                    ) && !owner.is_test
                })
        {
            return;
        }
        if let Some(owner) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
            if let Some(scope) = visibility::restricted_scope(
                text(ctx.node, ctx.source),
                &owner.qualname,
                &owner.path,
            ) {
                if let Some(argument) = ctx.node.child_by_field_name("argument") {
                    let mut imports = Vec::new();
                    collect_use(argument, ctx.source, "", &mut imports);
                    let mut exports: Vec<visibility::RestrictedExport> = owner
                        .detail_get(crate::models::details::NodeDetailKey::RUST_RESTRICTED_EXPORTS)
                        .and_then(|value| serde_json::from_value(value.clone()).ok())
                        .unwrap_or_default();
                    exports.extend(imports.into_iter().map(|(target, name)| {
                        visibility::RestrictedExport {
                            module: (name == "*").then_some(target),
                            name,
                            scope: scope.clone(),
                        }
                    }));
                    *owner.detail_entry_mut(
                        crate::models::details::NodeDetailKey::RUST_RESTRICTED_EXPORTS,
                    ) = serde_json::to_value(exports).expect("Rust export visibility serializes");
                }
            }
        }
        if let Some(argument) = ctx.node.child_by_field_name("argument") {
            let mut imports = Vec::new();
            collect_use(argument, ctx.source, "", &mut imports);
            for (name, alias) in imports {
                ctx.import(facts, name.clone(), Some(alias), Some(name));
            }
        }
    }
    fn external_import(&self, module: &str) -> Option<&'static str> {
        match module.split("::").next()? {
            "std" | "core" | "alloc" => Some("Rust standard library"),
            "rusqlite" | "tree_sitter" | "tree-sitter" | "serde" | "serde_json" | "tokio"
            | "anyhow" | "rayon" | "clap" | "tracing" | "tempfile" | "regex" | "thiserror"
            | "parking_lot" | "crossbeam" | "log" | "hashbrown" | "indexmap" | "sha2" | "hex"
            | "ignore" | "walkdir" | "pulldown_cmark" | "chrono" | "dirs" | "libc" | "schemars"
            | "serde_yaml" | "toml" => Some("Cargo ecosystem pack"),
            _ => None,
        }
    }
    fn extract_calls(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        super::sql::extract_literal(ctx, facts);
        if ctx.node.kind() == "call_expression" {
            registrations::extract_route_builder_calls(ctx, facts);
            if let Some(callee) = ctx
                .node
                .child_by_field_name("function")
                .and_then(|function| value_flow::callee_name(function, ctx.source))
            {
                let callee = callee.into_owned();
                extract_combinator_callback_args(ctx, &callee, facts);
                let receiver_hint = if callee.contains("::") && callee.contains('.') {
                    call_result_hint(ctx.node, ctx.source).map(|(hint, _)| hint)
                } else {
                    None
                };
                facts.references.push(Reference {
                    source: ctx.owner.into(),
                    expression: callee,
                    kind: "calls".into(),
                    line: ctx.line(),
                    column: ctx.node.start_position().column,
                    alias: None,
                    module: None,
                    dynamic: false,
                    receiver_hint,
                });
            } else {
                let hint = call_result_hint(ctx.node, ctx.source);
                let previous = facts.references.len();
                call(ctx, facts, false);
                if let (Some((hint, clear_dynamic)), Some(reference)) =
                    (hint, facts.references.get_mut(previous))
                {
                    if clear_dynamic {
                        reference.dynamic = false;
                    }
                    reference.receiver_hint = Some(hint);
                }
                if let Some(reference) = facts.references.get(previous) {
                    let callee = reference.expression.clone();
                    extract_combinator_callback_args(ctx, &callee, facts);
                }
            }
        } else if ctx.kind() == "macro_invocation" {
            super::sql::extract_macro_type(ctx, facts);
            macros::project_macro_invocation(ctx, facts);
            if let Some(tree) = ctx
                .node
                .named_children(&mut ctx.node.walk())
                .find(|c| c.kind() == "token_tree")
            {
                extract_macro_invocation_paths(tree, ctx.source, ctx.owner, facts);
            }
            let previous = facts.references.len();
            call(ctx, facts, false);
            if let Some(reference) = facts.references.get_mut(previous) {
                reference.receiver_hint = Some(ReceiverHint::CallResult {
                    callee: "macro_invocation".to_string(),
                    member: String::new(),
                });
            }
        } else if ctx.kind() == "scoped_identifier" {
            extract_scoped_identifier_reference(ctx, facts);
        } else if ctx.kind() == "struct_expression" {
            if let Some(name) = ctx.node.child_by_field_name("name") {
                let text = relations::type_name(name, ctx.source);
                let clean = text.split('<').next().unwrap_or(text).trim();
                relations::reference(facts, ctx.owner, clean, "references", ctx.line());
            }
            if let Some(body) = ctx.node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    if child.kind() == "field_initializer" {
                        if let Some(val) = child.child_by_field_name("value") {
                            emit_callable_arg(val, ctx, facts, false);
                        }
                    }
                }
            }
        } else if ctx.kind() == "array_expression" {
            let mut cursor = ctx.node.walk();
            for child in ctx.node.named_children(&mut cursor) {
                emit_callable_arg(child, ctx, facts, false);
            }
        }
    }
    fn builtin(&self, symbol: &str) -> bool {
        let (receiver, member) = if let Some((r, m)) = symbol.split_once("::") {
            (r, m)
        } else if let Some((r, m)) = symbol.split_once('.') {
            (r, m)
        } else {
            (symbol, "")
        };
        if receiver == "Self" || is_non_prelude_stdlib_receiver(receiver) {
            return false;
        }
        if !member.is_empty() {
            return self.builtin_member(receiver, member);
        }
        if self.builtin_type(symbol) && !is_non_prelude_stdlib_receiver(symbol) {
            return true;
        }
        if is_primitive_type(symbol)
            || matches!(
                symbol,
                "Ok" | "Err"
                    | "Some"
                    | "None"
                    | "drop"
                    | "Drop"
                    | "Fn"
                    | "FnMut"
                    | "FnOnce"
                    | "Send"
                    | "Sync"
                    | "Sized"
                    | "Unpin"
                    | "Copy"
                    | "PartialEq"
                    | "Eq"
                    | "PartialOrd"
                    | "Ord"
                    | "Hash"
                    | "IntoIterator"
                    | "DoubleEndedIterator"
                    | "ExactSizeIterator"
                    | "FromIterator"
                    | "Extend"
                    | "TryFrom"
                    | "TryInto"
                    | "Borrow"
                    | "BorrowMut"
            )
        {
            return true;
        }
        false
    }
    fn builtin_type(&self, name: &str) -> bool {
        is_primitive_type(name) || rust_std_members().contains_key(name)
    }

    fn builtin_member(&self, receiver: &str, member: &str) -> bool {
        if receiver == "str"
            && matches!(
                member,
                "len"
                    | "is_empty"
                    | "as_bytes"
                    | "chars"
                    | "char_indices"
                    | "bytes"
                    | "trim"
                    | "trim_start"
                    | "trim_end"
                    | "split"
                    | "split_once"
                    | "rsplit_once"
                    | "split_whitespace"
                    | "contains"
                    | "starts_with"
                    | "ends_with"
                    | "is_ascii"
                    | "eq_ignore_ascii_case"
            )
        {
            return true;
        }
        if receiver == "Slice" && matches!(member, "iter" | "into_iter") {
            return true;
        }
        rust_std_members()
            .get(receiver)
            .is_some_and(|members| members.contains(member))
    }
    fn builtin_generic(&self, receiver: &str) -> bool {
        matches!(receiver, "Vec" | "Option" | "Result" | "Box")
            || is_non_prelude_stdlib_receiver(receiver)
    }

    fn normalize_import(&self, owner: &str, module: &str) -> Option<ImportPath> {
        self.normalize_import_with_root(None, owner, module)
    }

    fn normalize_import_with_root(
        &self,
        root: Option<&Path>,
        owner: &str,
        module: &str,
    ) -> Option<ImportPath> {
        let (workspace, local) = workspace_path(owner);
        let root = root.filter(|root| root.is_absolute() && workspace.is_empty());
        let (namespace, relative) = if module == "crate" || module.starts_with("crate::") {
            normalize_crate_relative_import(self, local, module)
        } else if module == "self" || module.starts_with("self::") {
            normalize_self_relative_import(self, local, module)
        } else if module == "super" || module.starts_with("super::") {
            normalize_super_relative_import(self, local, module)?
        } else {
            normalize_bare_or_crate_import(self, root, local, module)
        };
        Some(ImportPath {
            namespace,
            relative,
            symbol_path: true,
        })
    }
    fn extract_relations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        match ctx.node.kind() {
            "mod_item" => relate_mod_item(ctx, facts),
            "impl_item" => relate_impl_item(ctx, facts),
            "function_item" | "function_signature_item" => relate_function_item(ctx, facts),
            "macro_definition" => relate_macro_definition(ctx, facts),
            "enum_variant" => set_owner_flag(facts, ctx.owner, "rust_enum_variant"),
            "type_item" => relate_type_item(ctx, facts),
            "struct_item" | "enum_item" => relate_nominal_item(ctx, facts),
            _ => {}
        }
    }
    fn metadata(
        &self,
        node: Syntax<'_>,
        source: &str,
        _name: &str,
        _file: &FileContext,
    ) -> SymbolMetadata {
        let mut param_types = BTreeMap::new();
        let mut decorators = Vec::new();
        macros::for_each_preceding_attribute(node, |sibling| {
            decorators.push(text(sibling, source).chars().take(512).collect());
        });
        decorators.reverse();
        if matches!(node.kind(), "function_item" | "function_signature_item") {
            if let Some(parameters) = node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    let Some(pattern) = parameter.child_by_field_name("pattern") else {
                        continue;
                    };
                    let Some(ty) = parameter.child_by_field_name("type") else {
                        continue;
                    };
                    let pattern_text = text(pattern, source).trim();
                    let name = pattern_text
                        .strip_prefix("mut ")
                        .or_else(|| pattern_text.strip_prefix("ref "))
                        .unwrap_or(pattern_text)
                        .trim();
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|character| character.is_alphanumeric() || character == '_')
                        && !matches!(
                            value_flow::type_expr(ty, source),
                            crate::semantic::TypeExpr::Unknown
                        )
                    {
                        param_types.insert(name.to_owned(), text(ty, source).trim().to_owned());
                    }
                }
            }
        }
        if node.kind() == "enum_variant" {
            let enum_public = node
                .parent()
                .and_then(|list| list.parent())
                .filter(|parent| parent.kind() == "enum_item")
                .is_some_and(|enum_node| unrestricted_public(enum_node, source));
            return SymbolMetadata {
                param_types,
                decorators,
                is_static: Some(true),
                is_public: Some(enum_public),
                ..Default::default()
            };
        }
        let Some(owner) = method_owner(node) else {
            return SymbolMetadata {
                param_types,
                decorators,
                is_async: value_flow::is_async(node, source),
                is_public: Some(unrestricted_public(node, source)),
                ..Default::default()
            };
        };
        let is_callable = matches!(node.kind(), "function_item" | "function_signature_item");
        let receiver_type = if owner.kind() == "impl_item" {
            owner
                .child_by_field_name("type")
                .map(|receiver| text(receiver, source).trim().to_owned())
        } else {
            Some("Self".into())
        };
        let has_receiver = is_callable
            && node
                .child_by_field_name("parameters")
                .is_some_and(|parameters| {
                    let mut cursor = parameters.walk();
                    let has_receiver = parameters
                        .named_children(&mut cursor)
                        .any(|parameter| parameter.kind() == "self_parameter");
                    has_receiver
                });
        let is_public = if owner.kind() == "trait_item" {
            unrestricted_public(owner, source)
        } else {
            unrestricted_public(node, source)
        };
        SymbolMetadata {
            param_types,
            decorators,
            is_async: is_callable && value_flow::is_async(node, source),
            receiver_name: has_receiver.then(|| "self".into()),
            receiver_type,
            is_method: is_callable.then_some(true),
            is_static: Some(!has_receiver),
            is_public: Some(is_public),
            ..Default::default()
        }
    }
    fn extract_mutations(&self, ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
        let kind = ctx.kind();
        if !matches!(
            kind,
            "field_declaration"
                | "compound_assignment_expr"
                | "assignment_expr"
                | "field_expression"
        ) {
            return;
        }
        let span = relations::RelationSpan::from_ctx(ctx);
        relations::mutation(
            span,
            facts,
            &["field_declaration"],
            &["compound_assignment_expr", "assignment_expr"],
            &["field_expression"],
        );
        if kind == "field_declaration" {
            if let Some(name) = ctx.node.child_by_field_name("name") {
                if let Some(owner) = facts.nodes.iter().find(|owner| owner.id == ctx.owner) {
                    let qualified = format!("{}.{}", owner.qualname, text(name, ctx.source));
                    if let Some(field) = facts.nodes.iter_mut().find(|field| {
                        field.kind == crate::models::tags::NodeKind::Field
                            && field.qualname == qualified
                    }) {
                        field.details = serde_json::json!({
                            "signature": text(ctx.node, ctx.source),
                            "is_public": unrestricted_public(ctx.node, ctx.source),
                            "rust_declared_field": true,
                        });
                    }
                }
            }
        }
        relations::member_access(span, facts, &["field_expression"]);
    }
    fn test_attribute(&self, node: Syntax<'_>, source: &str) -> bool {
        let mut matched = false;
        macros::for_each_preceding_attribute(node, |sibling| {
            if matched {
                return;
            }
            let raw = text(sibling, source);
            if raw.contains("test") {
                let value = raw.replace(' ', "");
                if value == "#[test]"
                    || value.starts_with("#[tokio::test")
                    || value.starts_with("#[async_std::test")
                {
                    matched = true;
                }
            }
        });
        matched
    }
    fn entrypoint_role(
        &self,
        node: Syntax<'_>,
        source: &str,
        name: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        if node.kind() != "function_item" || method_owner(node).is_some() {
            return None;
        }
        let root = node
            .parent()
            .is_some_and(|parent| parent.kind() == "source_file");
        if (root && name == "main") || has_main_attribute(node, source) {
            Some("main")
        } else {
            None
        }
    }
    fn test_role(
        &self,
        node: Syntax<'_>,
        source: &str,
        _path: &str,
        _file: &FileContext,
    ) -> Option<&'static str> {
        if !matches!(node.kind(), "function_item" | "function_signature_item") {
            return None;
        }
        if self.test_attribute(node, source) {
            Some("test_case")
        } else if has_cfg_test(node, source) {
            Some("test_helper")
        } else {
            None
        }
    }
}

fn normalize_crate_relative_import(rust: &Rust, local: &str, module: &str) -> (String, bool) {
    let tail = module.strip_prefix("crate::").unwrap_or("");
    let crate_prefix = if let Some((before_src, _)) = local.split_once("/src/") {
        format!("{}.src", before_src.replace('/', "."))
    } else if local.starts_with("src/") {
        "src".to_string()
    } else if local.contains('/') {
        local
            .split_once('/')
            .map_or("src", |(first, _)| first)
            .replace('/', ".")
    } else {
        rust.module_name(local)
    };
    let ns = if tail.is_empty() || tail == "*" {
        crate_prefix
    } else {
        format!("{crate_prefix}.{}", tail.replace("::", "."))
    };
    (ns, true)
}

fn normalize_self_relative_import(rust: &Rust, local: &str, module: &str) -> (String, bool) {
    let tail = module.strip_prefix("self::").unwrap_or("");
    let ns = if tail.is_empty() || tail == "*" {
        rust.module_name(local)
    } else {
        format!("{}.{}", rust.module_name(local), tail.replace("::", "."))
    };
    (ns, true)
}

fn normalize_super_relative_import(
    rust: &Rust,
    local: &str,
    module: &str,
) -> Option<(String, bool)> {
    let mut base = rust
        .module_name(local)
        .split('.')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut tail = module;
    while let Some(rest) = tail.strip_prefix("super::") {
        base.pop()?;
        tail = rest;
    }
    if tail == "super" {
        base.pop()?;
        tail = "";
    }
    if !tail.is_empty() && tail != "*" {
        base.push(tail.replace("::", "."));
    }
    Some((base.join("."), true))
}

fn find_workspace_crate_prefix(root: Option<&Path>, first: &str) -> Option<String> {
    let root = root?;
    let hyphenated = first.contains('_').then(|| first.replace('_', "-"));
    for prefix in ["crates", "packages", "libs", "modules", ""] {
        for crate_dir in std::iter::once(first).chain(hyphenated.as_deref()) {
            let candidate = if prefix.is_empty() {
                std::path::PathBuf::from(crate_dir).join("src")
            } else {
                std::path::PathBuf::from(prefix).join(crate_dir).join("src")
            };
            if root.join(&candidate).exists() {
                return Some(
                    candidate
                        .to_string_lossy()
                        .replace('\\', "/")
                        .replace('/', "."),
                );
            }
        }
    }
    None
}

fn is_local_child_module(root: Option<&Path>, local: &str, first: &str) -> bool {
    let Some(root) = root else {
        return false;
    };
    let owner_path = std::path::Path::new(local);
    let parent = owner_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let directory = if matches!(
        owner_path.file_name().and_then(|name| name.to_str()),
        Some("mod.rs" | "lib.rs" | "main.rs")
    ) {
        parent.to_path_buf()
    } else {
        owner_path.with_extension("")
    };
    let directory = root.join(directory);
    directory.join(format!("{first}.rs")).is_file()
        || directory.join(first).join("mod.rs").is_file()
}

fn normalize_bare_or_crate_import(
    rust: &Rust,
    root: Option<&Path>,
    local: &str,
    module: &str,
) -> (String, bool) {
    let (first, tail) = module.split_once("::").unwrap_or((module, ""));
    if let Some(crate_mod) = find_workspace_crate_prefix(root, first) {
        let full = if tail.is_empty() || tail == "*" {
            crate_mod
        } else {
            format!("{crate_mod}.{}", tail.replace("::", "."))
        };
        (full, false)
    } else if is_local_child_module(root, local, first) {
        let owner_module = rust.module_name(local);
        let owner_module = owner_module
            .strip_suffix(".lib")
            .or_else(|| owner_module.strip_suffix(".main"))
            .unwrap_or(&owner_module);
        let module = module.strip_suffix("::*").unwrap_or(module);
        (
            format!("{owner_module}.{}", module.replace("::", ".")),
            true,
        )
    } else if rust.is_stdlib(first) || rust.external_import(first).is_some() {
        (module.replace("::", "."), false)
    } else if root.is_none() && !local.contains('/') {
        (
            format!("{}.{}", rust.module_name(local), module.replace("::", ".")),
            true,
        )
    } else {
        (module.replace("::", "."), false)
    }
}

fn relate_mod_item(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if ctx.node.child_by_field_name("body").is_some() {
        set_owner_flag(
            facts,
            ctx.owner,
            crate::models::details::NodeDetailKey::INLINE_MODULE,
        );
        return;
    }
    if !ctx
        .parent()
        .is_some_and(|parent| parent.kind() == "source_file")
        || ctx.node.has_error()
        || static_module_path(ctx.node, ctx.source).is_none()
    {
        return;
    }
    let Some(declaration) = facts
        .nodes
        .iter()
        .find(|node| node.id == ctx.owner)
        .cloned()
    else {
        return;
    };
    let Some(parent_id) = facts
        .edges
        .iter()
        .find(|edge| edge.kind == "contains" && edge.dst == declaration.id)
        .map(|edge| edge.src.clone())
    else {
        return;
    };
    if let Some(parent_node) = facts.nodes.iter_mut().find(|node| node.id == parent_id) {
        if let Some(scope) = visibility::restricted_scope(
            text(ctx.node, ctx.source),
            &parent_node.qualname,
            &parent_node.path,
        ) {
            let mut exports: Vec<visibility::RestrictedExport> = parent_node
                .detail_get(crate::models::details::NodeDetailKey::RUST_RESTRICTED_EXPORTS)
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
            exports.push(visibility::RestrictedExport {
                module: None,
                name: declaration.name.clone(),
                scope,
            });
            *parent_node
                .detail_entry_mut(crate::models::details::NodeDetailKey::RUST_RESTRICTED_EXPORTS) =
                serde_json::to_value(exports).expect("Rust export visibility serializes");
        }
    }
    let root = declaration.path.ends_with("/src/lib.rs")
        || declaration.path.ends_with("/src/main.rs")
        || matches!(declaration.path.as_str(), "src/lib.rs" | "src/main.rs");
    let module = format!(
        "{}::{}",
        if root { "crate" } else { "self" },
        declaration.name
    );
    facts.references.push(crate::models::Reference {
        source: parent_id,
        expression: module.clone(),
        kind: "imports".into(),
        line: ctx.line(),
        column: ctx.node.start_position().column,
        alias: Some(declaration.name),
        module: Some(module),
        dynamic: false,
        receiver_hint: None,
    });
}

fn relate_impl_item(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if let Some(owner) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
        *owner.detail_entry_mut(crate::models::details::NodeDetailKey::GENERIC_IMPL) =
            serde_json::json!(has_non_lifetime_type_params(ctx.node, ctx.source));
        if uniform_generic_impl(ctx.node, ctx.source) {
            owner.set_detail(
                crate::models::details::NodeDetailKey::UNIFORM_GENERIC_IMPL,
                serde_json::json!(true),
            );
        }
        if let Some(trait_name) = field(ctx.node, ctx.source, "trait") {
            owner.set_detail(
                crate::models::details::NodeDetailKey::IMPL_TRAIT,
                serde_json::json!(trait_name),
            );
            if matches!(
                trait_name.trim_start_matches("::"),
                "std::ops::Deref" | "core::ops::Deref"
            ) || !trait_name.contains("::")
            {
                let mut ancestor = Some(ctx.node);
                let mut attributed = false;
                while let Some(node) = ancestor {
                    attributed |= node
                        .prev_named_sibling()
                        .is_some_and(|previous| previous.kind() == "attribute_item");
                    ancestor = node.parent();
                }
                if attributed || ctx.node.has_error() || has_unproven_bounds(ctx.node, ctx.source) {
                    owner.set_detail(
                        crate::models::details::NodeDetailKey::DEREF_UNPROVEN,
                        serde_json::json!(true),
                    );
                }
            }
        }
        if let Some(receiver) = field(ctx.node, ctx.source, "type") {
            owner.set_detail(
                crate::models::details::NodeDetailKey::RECEIVER_TYPE,
                serde_json::json!(receiver),
            );
        }
    }
    if let Some(tr) = ctx.node.child_by_field_name("trait") {
        let tr_text = relations::type_name(tr, ctx.source);
        let clean = tr_text.split('<').next().unwrap_or(tr_text).trim();
        relations::reference(facts, ctx.owner, clean, "implements", ctx.line());
    }
}

fn relate_function_item(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if ctx.node.kind() == "function_item" {
        registrations::extract_attribute_registrations(ctx, facts);
        cli_registration::extract_dispatch_registrations(ctx, facts);
    }
    let unproven = has_unproven_bounds(ctx.node, ctx.source)
        && !path_reference_parameters(ctx.node, ctx.source, facts);
    let conversion_bounds_only =
        unproven && !has_unproven_bounds_with(ctx.node, ctx.source, is_conversion_bound);
    let trait_default = ctx.node.kind() == "function_item"
        && method_owner(ctx.node).is_some_and(|owner| owner.kind() == "trait_item");
    if unproven || trait_default {
        if let Some(method) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
            if unproven {
                method.set_detail(
                    crate::models::details::NodeDetailKey::UNPROVEN_BOUNDS,
                    serde_json::json!(true),
                );
            }
            if conversion_bounds_only {
                method.set_detail(
                    crate::models::details::NodeDetailKey::CONVERSION_BOUNDS_ONLY,
                    serde_json::json!(true),
                );
            }
            if trait_default {
                method.set_detail(
                    crate::models::details::NodeDetailKey::TRAIT_DEFAULT,
                    serde_json::json!(true),
                );
            }
        }
    }
    if let Some(parameters) = ctx.node.child_by_field_name("parameters") {
        let mut c = parameters.walk();
        for param in parameters.named_children(&mut c) {
            if let Some(ty) = param.child_by_field_name("type") {
                value_flow::type_references(facts, ctx.owner, ty, ctx.source, ctx.line());
            }
        }
    }
    if let Some(ret) = ctx.node.child_by_field_name("return_type") {
        value_flow::type_references(facts, ctx.owner, ret, ctx.source, ctx.line());
    }
}

fn relate_macro_definition(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    // Inspect transcription tokens only; matching patterns and metavariables are opaque.
    let mut impl_traits = Vec::new();
    let mut cursor = ctx.node.walk();
    for rule in ctx
        .node
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "macro_rule")
    {
        if let Some(body) = rule.child_by_field_name("right") {
            macro_transcription_references(body, ctx.source, ctx.owner, facts);
            macros::extract_macro_impl_traits(body, ctx.source, &mut impl_traits);
        }
    }
    if !impl_traits.is_empty() {
        let macro_line = ctx.line();
        if let Some(macro_node) = facts.nodes.iter_mut().find(|node| {
            node.kind == crate::models::tags::NodeKind::Macro && node.line == macro_line
        }) {
            macro_node.set_detail(
                crate::models::details::NodeDetailKey::RUST_MACRO_IMPL_TRAITS,
                serde_json::json!(impl_traits),
            );
            macro_node.set_detail(
                crate::models::details::NodeDetailKey::RUST_MACRO_PROJECTED,
                serde_json::json!(true),
            );
        }
    }
}

fn relate_type_item(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if has_non_lifetime_type_params(ctx.node, ctx.source)
        || ctx.node.has_error()
        || ctx
            .node
            .prev_named_sibling()
            .is_some_and(|previous| previous.kind() == "attribute_item")
    {
        set_owner_flag(facts, ctx.owner, "deref_unproven");
    }
    if let Some(ty) = ctx.node.child_by_field_name("type") {
        if let Some(owner) = facts.nodes.iter_mut().find(|node| node.id == ctx.owner) {
            owner.set_detail(
                crate::models::details::NodeDetailKey::ALIAS_TARGET,
                serde_json::json!(text(ty, ctx.source).trim()),
            );
        }
        value_flow::type_references(facts, ctx.owner, ty, ctx.source, ctx.line());
    }
}

fn unquote_rust_string<'a>(node: Syntax<'_>, source: &'a str) -> Option<std::borrow::Cow<'a, str>> {
    let raw = text(node, source);
    if node.kind() == "raw_string_literal" {
        let rest = raw.strip_prefix('r')?;
        let hashes = rest.bytes().take_while(|&b| b == b'#').count();
        let inner = rest[hashes..].strip_prefix('"')?;
        let tail = &inner[inner.len().checked_sub(1 + hashes)?..];
        return (tail.starts_with('"') && tail[1..].bytes().all(|b| b == b'#'))
            .then(|| std::borrow::Cow::Borrowed(&inner[..inner.len() - 1 - hashes]));
    }
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    if !inner.contains('\\') {
        return Some(std::borrow::Cow::Borrowed(inner));
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(std::borrow::Cow::Owned(out))
}

fn is_combinator_callback_name(tail: &str) -> bool {
    matches!(
        tail,
        "map"
            | "map_err"
            | "map_or"
            | "map_or_else"
            | "and_then"
            | "or_else"
            | "ok_or_else"
            | "unwrap_or_else"
            | "filter"
            | "filter_map"
            | "find"
            | "find_map"
            | "flat_map"
            | "any"
            | "all"
            | "for_each"
            | "try_for_each"
            | "inspect"
            | "inspect_err"
            | "map_rows"
            | "query_map"
            | "sort_by"
            | "sort_by_key"
            | "sort_by_cached_key"
            | "dedup_by"
            | "is_some_and"
            | "is_none_or"
            | "retain"
            | "retain_mut"
            | "partition"
            | "fold"
            | "try_fold"
            | "reduce"
            | "then"
            | "then_some"
    )
}

fn collect_declared_callables(root: Syntax<'_>, source: &str) -> HashSet<String> {
    let mut callables = HashSet::new();
    let mut stack = vec![root];
    let mut cursor = root.walk();
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if matches!(kind, "function_item" | "function_signature_item") {
            if let Some(name) = node.child_by_field_name("name") {
                let raw = text(name, source).trim();
                if !raw.is_empty() {
                    callables.insert(raw.to_owned());
                }
            }
        }
        if matches!(
            kind,
            "source_file"
                | "mod_item"
                | "impl_item"
                | "trait_item"
                | "declaration_list"
                | "function_item"
                | "block"
        ) {
            cursor.reset(node);
            stack.extend(node.named_children(&mut cursor));
        }
    }
    callables
}

fn is_file_declared_callable(ctx: &SyntaxContext<'_, '_>, facts: &Facts, name: &str) -> bool {
    ctx.file_state
        .rust()
        .is_some_and(|rust| rust.declared_callables.contains(name))
        || facts.nodes.iter().any(|n| {
            n.name == name
                && matches!(
                    n.kind,
                    crate::models::tags::NodeKind::Function | crate::models::tags::NodeKind::Method
                )
        })
}

fn is_lowercase_scoped_callable(raw: &str, allow_upper: bool) -> bool {
    if raw.is_empty()
        || raw.starts_with("std::")
        || raw.starts_with("core::")
        || raw.starts_with("alloc::")
    {
        return false;
    }
    allow_upper
        || raw
            .rsplit("::")
            .next()
            .and_then(|seg| seg.chars().next())
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch == '_')
}

fn emit_callable_arg(
    mut arg: Syntax<'_>,
    ctx: &SyntaxContext<'_, '_>,
    facts: &mut Facts,
    combinator: bool,
) {
    while arg.kind() == "reference_expression" {
        let Some(inner) = arg
            .child_by_field_name("value")
            .or_else(|| arg.named_child(0))
        else {
            return;
        };
        arg = inner;
    }
    let raw = text(arg, ctx.source).trim();
    let is_callable_arg = match arg.kind() {
        "identifier" => {
            !matches!(raw, "self" | "true" | "false" | "_")
                && (combinator || is_file_declared_callable(ctx, facts, raw))
        }
        "scoped_identifier" => is_lowercase_scoped_callable(raw, combinator),
        _ => false,
    };
    if is_callable_arg && !raw.is_empty() {
        facts.references.push(Reference {
            source: ctx.owner.into(),
            expression: raw.into(),
            kind: "calls".into(),
            line: arg.start_position().row + 1 + ctx.offset,
            column: arg.start_position().column,
            alias: None,
            module: None,
            dynamic: false,
            receiver_hint: (!combinator).then_some(ReceiverHint::CallableArg),
        });
    }
}

fn extract_combinator_callback_args(ctx: &SyntaxContext<'_, '_>, callee: &str, facts: &mut Facts) {
    let tail = callee
        .rsplit("::")
        .next()
        .unwrap_or(callee)
        .rsplit('.')
        .next()
        .unwrap_or(callee);
    let combinator = is_combinator_callback_name(tail);
    let Some(args) = ctx.node.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        emit_callable_arg(arg, ctx, facts, combinator);
    }
}

fn extract_serde_callable_references(
    attr: Syntax<'_>,
    source: &str,
    owner: &str,
    facts: &mut Facts,
) {
    let Some(attr_node) = attr.named_child(0).filter(|n| n.kind() == "attribute") else {
        return;
    };
    let mut cursor = attr_node.walk();
    let mut named = attr_node.named_children(&mut cursor);
    let Some(path_node) = named.next() else {
        return;
    };
    let attr_name = text(path_node, source).trim();
    let is_serde = attr_name == "serde" || attr_name.ends_with("::serde");
    let is_schemars = attr_name == "schemars" || attr_name.ends_with("::schemars");
    if !is_serde && !is_schemars {
        return;
    }
    let Some(token_tree) = attr_node
        .child_by_field_name("arguments")
        .or_else(|| named.find(|n| n.kind() == "token_tree"))
        .filter(|n| n.kind() == "token_tree")
    else {
        return;
    };
    let mut tree_cursor = token_tree.walk();
    let tokens: Vec<_> = token_tree
        .children(&mut tree_cursor)
        .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        .collect();
    let line = attr.start_position().row + 1;
    for window in tokens.windows(3) {
        let [key_node, eq_node, val_node] = window else {
            continue;
        };
        let key_ok = if is_serde {
            matches!(
                text(*key_node, source),
                "default" | "skip_serializing_if" | "serialize_with" | "deserialize_with" | "with"
            )
        } else {
            text(*key_node, source) == "schema_with"
        };
        if !matches!(key_node.kind(), "identifier" | "default")
            || text(*eq_node, source) != "="
            || !matches!(val_node.kind(), "string_literal" | "raw_string_literal")
            || !key_ok
        {
            continue;
        }
        if let Some(path) = unquote_rust_string(*val_node, source) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                relations::reference(facts, owner, trimmed, "calls", line);
            }
        }
    }
}

fn relate_nominal_item(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    cli_registration::extract_nominal_metadata(ctx, facts);
    if ctx.node.kind() == "struct_item" && has_non_lifetime_type_params(ctx.node, ctx.source) {
        set_owner_flag(facts, ctx.owner, "deref_unproven");
    }
    macros::for_each_preceding_attribute(ctx.node, |attr| {
        extract_serde_callable_references(attr, ctx.source, ctx.owner, facts);
    });
    if let Some(body) = ctx.node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "attribute_item" {
                extract_serde_callable_references(child, ctx.source, ctx.owner, facts);
            } else if child.kind() == "field_declaration" {
                if let Some(ty) = child.child_by_field_name("type") {
                    value_flow::type_references(
                        facts,
                        ctx.owner,
                        ty,
                        ctx.source,
                        child.start_position().row + 1,
                    );
                }
            } else if child.kind() == "enum_variant" {
                if let Some(vbody) = child.child_by_field_name("body") {
                    let mut vcursor = vbody.walk();
                    for vchild in vbody.named_children(&mut vcursor) {
                        if vchild.kind() == "attribute_item" {
                            extract_serde_callable_references(vchild, ctx.source, ctx.owner, facts);
                            continue;
                        }
                        if matches!(vchild.kind(), "line_comment" | "block_comment") {
                            continue;
                        }
                        let mut inner_cursor = vchild.walk();
                        for attr in vchild
                            .named_children(&mut inner_cursor)
                            .filter(|n| n.kind() == "attribute_item")
                        {
                            extract_serde_callable_references(attr, ctx.source, ctx.owner, facts);
                        }
                        if let Some(ty) = vchild
                            .child_by_field_name("type")
                            .or_else(|| (vchild.kind() != "field_declaration").then_some(vchild))
                        {
                            value_flow::type_references(
                                facts,
                                ctx.owner,
                                ty,
                                ctx.source,
                                vchild.start_position().row + 1,
                            );
                        }
                    }
                }
            }
        }
    }
    let derives = macros::extract_derives(ctx.node, ctx.source);
    if derives.is_empty() {
        return;
    }
    if let Some(owner) = facts
        .nodes
        .iter()
        .find(|node| node.id == ctx.owner)
        .cloned()
    {
        let synth_owner = macros::SyntheticOwner {
            id: &owner.id,
            qualname: &owner.qualname,
            name: &owner.name,
            path: &owner.path,
            line: ctx.line(),
            end_line: ctx.line(),
            is_public: *owner.detail_entry(crate::models::details::NodeDetailKey::IS_PUBLIC)
                == true,
            is_test: owner.is_test,
        };
        macros::synthesize_derive_methods(&synth_owner, &derives, facts);
    }
}

fn extract_scoped_identifier_reference(ctx: &SyntaxContext<'_, '_>, facts: &mut Facts) {
    if let Some(parent) = ctx.parent() {
        if matches!(
            parent.kind(),
            "scoped_identifier" | "scoped_type_identifier"
        ) || (parent.kind() == "call_expression"
            && parent
                .child_by_field_name("function")
                .is_some_and(|f| f.id() == ctx.node.id()))
            || (parent.kind() == "generic_function"
                && parent
                    .child_by_field_name("function")
                    .is_some_and(|f| f.id() == ctx.node.id()))
            || (parent.kind() == "struct_expression"
                && parent
                    .child_by_field_name("name")
                    .is_some_and(|n| n.id() == ctx.node.id()))
            || (parent.kind() == "macro_invocation"
                && parent
                    .child_by_field_name("macro")
                    .is_some_and(|m| m.id() == ctx.node.id()))
        {
            return;
        }
    }
    let mut anc = ctx.parent();
    while let Some(node) = anc {
        match node.kind() {
            "use_declaration"
            | "attribute_item"
            | "meta_item"
            | "type_arguments"
            | "visibility_modifier" => return,
            "function_item" | "source_file" | "impl_item" | "trait_item" | "mod_item" => break,
            _ => anc = node.parent(),
        }
    }
    let raw = text(ctx.node, ctx.source).trim();
    if raw.is_empty()
        || raw.starts_with("Self::")
        || raw.starts_with("std::")
        || raw.starts_with("core::")
        || raw.starts_with("alloc::")
    {
        return;
    }
    relations::reference(facts, ctx.owner, raw, "references", ctx.line());
}

fn read_scoped_token_path(tokens: &[Syntax<'_>], start: usize, source: &str) -> (String, usize) {
    let (mut end, mut path) = (start, text(tokens[start], source).to_owned());
    while end + 2 < tokens.len()
        && text(tokens[end + 1], source) == "::"
        && matches!(
            tokens[end + 2].kind(),
            "identifier" | "type_identifier" | "super" | "self"
        )
    {
        path.push_str("::");
        path.push_str(text(tokens[end + 2], source));
        end += 2;
    }
    (path, end)
}

fn emit_macro_combinator_callbacks(
    args_tree: Syntax<'_>,
    source: &str,
    owner: &str,
    facts: &mut Facts,
) {
    let tokens: Vec<_> = args_tree.children(&mut args_tree.walk()).collect();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if !matches!(
            tok.kind(),
            "identifier" | "type_identifier" | "crate" | "super" | "self"
        ) {
            i += 1;
            continue;
        }
        let first = text(tok, source);
        let (path, end) = read_scoped_token_path(&tokens, i, source);
        let prev_ok = i == 0 || matches!(text(tokens[i - 1], source), "(" | ",");
        let next_ok = end + 1 >= tokens.len() || matches!(text(tokens[end + 1], source), ")" | ",");
        if prev_ok
            && next_ok
            && ((end > i && is_lowercase_scoped_callable(&path, true))
                || (end == i && !matches!(first, "self" | "true" | "false" | "_")))
        {
            relations::reference(facts, owner, &path, "calls", tok.start_position().row + 1);
        }
        i = end + 1;
    }
}

fn extract_macro_invocation_paths(tree: Syntax<'_>, source: &str, owner: &str, facts: &mut Facts) {
    let mut pending = vec![tree];
    while let Some(node) = pending.pop() {
        let children: Vec<_> = node.children(&mut node.walk()).collect();
        let mut i = 0;
        while i < children.len() {
            if children[i].kind() == "token_tree" {
                pending.push(children[i]);
                i += 1;
                continue;
            }
            if !matches!(
                children[i].kind(),
                "identifier" | "crate" | "super" | "self"
            ) {
                i += 1;
                continue;
            }
            let first = text(children[i], source);
            let (path, end) = read_scoped_token_path(&children, i, source);
            let line = children[i].start_position().row + 1;
            let prev_tok = i.checked_sub(1).map(|prev| text(children[prev], source));
            let next_tok = children.get(end + 1).copied();
            let next_call_tree = next_tok.filter(|next| {
                next.kind() == "token_tree" && text(*next, source).trim_start().starts_with('(')
            });
            if end > i && !matches!(first, "std" | "core" | "alloc") {
                let kind = if next_call_tree.is_some() {
                    "calls"
                } else {
                    "references"
                };
                relations::reference(facts, owner, &path, kind, line);
            } else if end == i && prev_tok == Some(".") && next_call_tree.is_some() {
                if is_combinator_callback_name(first) {
                    if let Some(args_tree) = next_call_tree {
                        emit_macro_combinator_callbacks(args_tree, source, owner, facts);
                    }
                }
                let mut start = i;
                while start >= 2
                    && text(children[start - 1], source) == "."
                    && matches!(children[start - 2].kind(), "identifier" | "self")
                {
                    start -= 2;
                }
                if start < i {
                    let mut dotted = text(children[start], source).to_owned();
                    for k in ((start + 2)..=i).step_by(2) {
                        dotted.push('.');
                        dotted.push_str(text(children[k], source));
                    }
                    relations::reference(facts, owner, &dotted, "calls", line);
                }
            } else if end == i
                && first.starts_with(|ch: char| ch.is_ascii_lowercase())
                && ((prev_tok == Some("=>")
                    && next_tok.is_none_or(|n| matches!(text(n, source), "," | ")")))
                    || (prev_tok == Some(",")
                        && next_tok.is_some_and(|n| {
                            n.kind() == "token_tree"
                                && text(n, source).trim_start().starts_with('{')
                        })))
            {
                relations::reference(facts, owner, first, "calls", line);
            } else if end == i
                && matches!(
                    first,
                    "default"
                        | "skip_serializing_if"
                        | "serialize_with"
                        | "deserialize_with"
                        | "with"
                )
                && i + 2 < children.len()
                && text(children[i + 1], source) == "="
                && matches!(
                    children[i + 2].kind(),
                    "string_literal" | "raw_string_literal"
                )
            {
                if let Some(trimmed) = unquote_rust_string(children[i + 2], source)
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty())
                {
                    relations::reference(facts, owner, &trimmed, "calls", line);
                }
            } else if end == i
                && !matches!(prev_tok, Some("::" | "."))
                && next_call_tree.is_some()
                && !matches!(
                    first,
                    "Some"
                        | "Ok"
                        | "Err"
                        | "None"
                        | "Box"
                        | "Vec"
                        | "if"
                        | "while"
                        | "for"
                        | "match"
                        | "return"
                        | "let"
                        | "mut"
                        | "ref"
                        | "pub"
                        | "fn"
                        | "crate"
                        | "super"
                        | "self"
                        | "Self"
                        | "true"
                        | "false"
                )
            {
                relations::reference(facts, owner, first, "calls", line);
            }
            i = end + 1;
        }
    }
}

fn macro_transcription_references(node: Syntax<'_>, source: &str, owner: &str, facts: &mut Facts) {
    let mut pending = vec![node];
    while let Some(tree) = pending.pop() {
        let mut cursor = tree.walk();
        let children: Vec<_> = tree.children(&mut cursor).collect();
        for (index, token) in children.iter().enumerate() {
            if token.kind() == "token_tree" {
                pending.push(*token);
                continue;
            }
            let prev_dollar = index
                .checked_sub(1)
                .is_some_and(|previous| text(children[previous], source) == "$");
            if token.kind() != "identifier" || prev_dollar {
                continue;
            }
            let Some(next) = children.get(index + 1) else {
                continue;
            };
            let next = text(*next, source).trim_start();
            if !(next.starts_with('(') || next == "::" || next.starts_with('{')) {
                continue;
            }
            let sym = text(*token, source);
            if sym != "Self" {
                relations::reference(
                    facts,
                    owner,
                    sym,
                    "references",
                    token.start_position().row + 1,
                );
            }
        }
    }
}

/// Language profiles provided by this module.
pub(crate) static PROFILES: &[&dyn LanguageProfile] = &[&RUST];
