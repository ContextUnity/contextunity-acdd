use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::activation::{parse_language, ActivationCondition};
use super::projections::{Api, NamedBindingForm};
use crate::models::tags::LanguageId;

/// Bounded extraction operations; every API and keyword choice is input data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxRule {
    #[serde(skip)]
    pub pack: String,
    pub id: String,
    pub language: String,
    #[serde(default)]
    pub package_family: Option<String>,
    #[serde(default)]
    pub activation: Vec<ActivationCondition>,
    /// Compatibility labels only; admission and provenance use pack and rule identity.
    #[serde(default)]
    pub display: SyntaxDisplay,
    pub operation: SyntaxOperation,
}

/// Display labels for syntax rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxDisplay {
    pub builtin: Option<String>,
    pub autoimport: Option<String>,
    pub library: Option<String>,
    pub component: Option<String>,
}

/// Syntax operations supported by framework manifests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SyntaxOperation {
    FileRoute {
        directory: String,
        extension: String,
        index: String,
        method: String,
    },
    FactoryMembers {
        callee: Api,
        argument: usize,
        #[serde(default)]
        shape: FactoryShape,
        #[serde(default)]
        configured_module: Option<String>,
        #[serde(default)]
        state_member: String,
        #[serde(default)]
        actions_member: String,
    },
    NamedBinding {
        domain: String,
        binding: NamedBindingForm,
    },
    RenderContext {
        package: String,
        module: String,
        export: String,
        target_argument: usize,
        context_argument: usize,
        argument_count: usize,
    },
    NamedRouteCall {
        module: String,
        export: String,
        argument: usize,
        expression_prefix: String,
    },
    ReceiverRegistration {
        module: String,
        constructors: Vec<String>,
        member: String,
        path_argument: usize,
        handler_argument: usize,
        name_keyword: String,
    },
    DecoratedRegistration {
        modules: Vec<String>,
        constructor: String,
        member: String,
        name_keyword: String,
        provider: String,
        #[serde(default)]
        imported_decorator: bool,
        #[serde(default)]
        category: RegistrationCategory,
    },
    ClassMethodRegistration {
        module: String,
        base: String,
        member: String,
        provider: String,
    },
    CommandDispatch {
        module: String,
        field_attribute: String,
        field_marker: String,
        parser_derive: String,
        subcommand_derive: String,
        parse_method: String,
    },
    TestRunner {
        modules: Vec<String>,
        members: Vec<String>,
    },
    TemplateLoader {
        package: String,
        module: String,
        environment: String,
        loader: String,
        loader_keyword: String,
    },
    TemplateLibrary {
        directory: String,
        module: String,
        constructor: String,
        members: std::collections::BTreeMap<String, String>,
        name_keyword: String,
    },
    ObjectFactory {
        callees: Vec<String>,
        #[serde(default)]
        unshadowed_globals: Vec<String>,
    },
    ImplicitReceivers {
        names: Vec<String>,
    },
    ImplicitBuiltins {
        names: Vec<String>,
    },
    ReceiverType {
        receiver: ReceiverTypeRule,
    },
    ComponentEnvironment {
        package: String,
        config_file: String,
        config_callee: String,
        enabled_key: String,
        modules_key: String,
        directory: String,
        extension: String,
        virtual_module: String,
        autoimports: Vec<String>,
        builtins: Vec<String>,
        modules: std::collections::BTreeMap<String, ComponentModule>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callable_imports: Option<CallableImports>,
    },
    ComponentOptions {
        module: String,
        export: String,
        components_key: String,
    },
    ComponentMacros {
        props: String,
        emits: String,
        slots: String,
        markers: Vec<String>,
    },
    RouteObject {
        collection: String,
        path_key: String,
        handler_key: String,
        method: String,
    },
    HttpClientCall {
        callee: String,
        receivers: Vec<String>,
        methods: std::collections::BTreeMap<String, String>,
        default_method: String,
        url_key: String,
        method_key: String,
        relative_prefixes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_context: Option<HttpTargetContext>,
    },
    TemplateNamedRoute {
        tag: String,
        expression_prefix: String,
    },
    TemplateSymbols {
        requires_loader: bool,
        tags: Vec<String>,
        filters: Vec<String>,
        libraries: Vec<String>,
        library_routes: std::collections::BTreeMap<String, String>,
    },
    TemplateDialect {
        dialect: TemplateDialect,
    },
    HtmlRequestAttributes {
        request_node: String,
        context_node: String,
        namespace: String,
        context_label: String,
        prefixes: Vec<String>,
        methods: std::collections::BTreeMap<String, String>,
        contexts: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        value_dynamic_prefixes: Vec<String>,
    },
    HtmlScriptAttributes {
        event_prefixes: Vec<String>,
        object_attribute: String,
        event_scope: String,
        object_scope: String,
    },
}

/// Semantic category of a configured decorated registration.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationCategory {
    #[default]
    Tool,
    CliCommand,
    EventHandler,
}

/// HTTP target context configuration for framework operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpTargetContext {
    /// Request owner file paths.
    pub request_owners: Vec<String>,
    /// Route owner file paths.
    pub route_owners: Vec<String>,
}

impl HttpTargetContext {
    fn validate(&self) -> Result<()> {
        for owners in [&self.request_owners, &self.route_owners] {
            let mut seen = std::collections::BTreeSet::new();
            ensure!(
                !owners.is_empty()
                    && owners.len() <= 128
                    && owners
                        .iter()
                        .all(|path| super::projections::confined_path(path, false)
                            && seen.insert(path)),
                "invalid HTTP target context owners"
            );
        }
        Ok(())
    }
}

fn normal_relative_dir(dir: &str) -> bool {
    !dir.starts_with('/')
        && dir
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
}

fn uppercase_methods<'a>(methods: impl IntoIterator<Item = &'a str>) -> bool {
    methods
        .into_iter()
        .all(|m| !m.is_empty() && m.chars().all(|ch| ch.is_ascii_uppercase()))
}

fn key_identifier(value: &str) -> bool {
    super::projections::identifier(value, false)
}

fn is_ident_like(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn validate_component_export(export: &ComponentExport) -> bool {
    super::semantic::normal_relative_path(&export.directory)
        && export.directory.len() <= 256
        && export.extension == ".ts"
}

/// Component module configuration for UI frameworks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentModule {
    /// Package identifier.
    pub package: String,
    /// Recognized component names.
    pub components: Vec<String>,
    /// Export configurations.
    #[serde(default)]
    pub exports: Vec<ComponentExport>,
    /// Call result bindings.
    #[serde(default)]
    pub call_results: std::collections::BTreeMap<String, Vec<String>>,
}

impl ComponentModule {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.package.is_empty()
                && self
                    .components
                    .iter()
                    .all(|name| !name.is_empty() && !name.chars().any(char::is_whitespace)),
            "invalid component module"
        );
        ensure!(
            self.exports.len() <= 32 && self.call_results.len() <= 128,
            "component module result descriptors exceed limit"
        );
        ensure!(
            self.exports.iter().all(validate_component_export),
            "invalid component export path"
        );
        for (callee, members) in &self.call_results {
            let unique: std::collections::BTreeSet<_> = members.iter().collect();
            ensure!(
                key_identifier(callee)
                    && !members.is_empty()
                    && members.len() <= 128
                    && unique.len() == members.len()
                    && members.iter().all(|name| key_identifier(name)),
                "invalid component call result"
            );
        }
        Ok(())
    }
}

/// Component export directory and extension rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentExport {
    /// Target directory.
    pub directory: String,
    /// Target file extension.
    pub extension: String,
}

/// Callable imports configuration for components.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallableImports {
    /// Export selectors.
    pub exports: Vec<ComponentExport>,
    /// Import alias names.
    pub aliases: Vec<String>,
    /// Options key name.
    pub options_key: String,
    /// Enabled key name.
    pub enabled_key: String,
    /// Scan key name.
    pub scan_key: String,
    /// Unsupported key names.
    pub unsupported_keys: Vec<String>,
}

impl CallableImports {
    fn validate(&self) -> Result<()> {
        let unique_exports: std::collections::BTreeSet<_> = self
            .exports
            .iter()
            .map(|export| (&export.directory, &export.extension))
            .collect();
        ensure!(
            !self.exports.is_empty()
                && self.exports.len() <= 16
                && unique_exports.len() == self.exports.len(),
            "invalid callable export selectors"
        );
        ensure!(
            self.exports.iter().all(validate_component_export),
            "invalid callable export path"
        );
        let aliases: std::collections::BTreeSet<_> = self.aliases.iter().collect();
        ensure!(
            self.aliases.len() <= 8
                && aliases.len() == self.aliases.len()
                && self.aliases.iter().all(|alias| !alias.is_empty()
                    && alias.len() <= 128
                    && alias.ends_with('/')
                    && !alias.contains('\\')
                    && !alias.chars().any(char::is_control)
                    && normal_relative_dir(alias.trim_end_matches('/'))),
            "invalid callable import aliases"
        );
        ensure!(
            key_identifier(&self.options_key)
                && key_identifier(&self.enabled_key)
                && key_identifier(&self.scan_key)
                && self.enabled_key != self.scan_key,
            "invalid callable import keys"
        );
        let unsupported: std::collections::BTreeSet<_> = self.unsupported_keys.iter().collect();
        ensure!(
            self.unsupported_keys.len() <= 16
                && unsupported.len() == self.unsupported_keys.len()
                && self.unsupported_keys.iter().all(|path| path.len() <= 128
                    && path.split('.').count() <= 4
                    && path.split('.').all(key_identifier)),
            "invalid unsupported callable import paths"
        );
        Ok(())
    }
}

/// Factory shape pattern.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryShape {
    /// Options object shape.
    #[default]
    Options,
    /// Setup return object shape.
    SetupReturn,
}

/// Parsed return-type AST compiled once at manifest load time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReturnTypeNode {
    /// Preserves the proven receiver type (`Self`).
    SelfReceiver,
    /// Explicit unknown/opaque payload (`_`).
    Unknown,
    /// Named nominal or primitive type (`Playwright.Locator`, `rusqlite::Rows`, `str`).
    Named(String),
    /// Fixed-arity tuple (`(&str, &str)`).
    Tuple(Vec<ReturnTypeNode>),
    /// Rust reference (`&T` or `&mut T`).
    Reference {
        mutable: bool,
        inner: Box<ReturnTypeNode>,
    },
    /// Generic wrapper or applied type (`Option<T>`, `Result<T, E>`, `Promise<T>`, `Iterator<T>`, `QuerySet<T>`).
    Applied {
        base: String,
        args: Vec<ReturnTypeNode>,
    },
}

impl ReturnTypeNode {
    /// Visits every concrete nominal/applied type name inside this compiled return type.
    pub fn visit_named(&self, visitor: &mut impl FnMut(&str)) {
        match self {
            Self::Named(name) => visitor(name),
            Self::Reference { inner, .. } => inner.visit_named(visitor),
            Self::Tuple(items) => {
                for item in items {
                    item.visit_named(visitor);
                }
            }
            Self::Applied { base, args } => {
                visitor(base);
                for arg in args {
                    arg.visit_named(visitor);
                }
            }
            Self::SelfReceiver | Self::Unknown => {}
        }
    }
}

/// Validated, pre-parsed compound return specification serialized as a string in `.toml` manifests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnTypeSpec {
    raw: String,
    compiled: ReturnTypeNode,
}

impl ReturnTypeSpec {
    /// Parses and validates a return specification once at manifest compile/load time.
    pub fn parse(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        let trimmed = raw.trim();
        ensure!(
            !trimmed.is_empty() && trimmed.len() <= 4096 && !trimmed.chars().any(char::is_control),
            "invalid return type specification"
        );
        let compiled = parse_return_type_node(trimmed, 0)?;
        Ok(Self {
            raw: trimmed.to_owned(),
            compiled,
        })
    }

    /// Returns the pre-parsed return-type AST.
    pub fn compiled(&self) -> &ReturnTypeNode {
        &self.compiled
    }
}

impl Serialize for ReturnTypeSpec {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for ReturnTypeSpec {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

fn split_top_level_args(input: &str) -> Result<Vec<&str>> {
    let mut parts = Vec::new();
    let res = crate::languages::types::split_top_level_delimited(
        input,
        crate::languages::types::DelimitedSplitOptions {
            delimiter: ',',
            track_angles: true,
            track_brackets: true,
            track_quotes: false,
            strict: true,
        },
        |piece, is_tail| {
            if is_tail {
                if !piece.is_empty() {
                    parts.push(piece);
                } else if !parts.is_empty() {
                    return Err("trailing comma in return type");
                }
            } else {
                if piece.is_empty() {
                    return Err("empty type argument in return type");
                }
                parts.push(piece);
            }
            Ok(())
        },
    );
    match res {
        Ok(()) => {}
        Err(Ok(crate::languages::types::DelimitedSplitError::UnbalancedClose(ch))) => {
            anyhow::bail!("unbalanced '{ch}' in return type");
        }
        Err(Ok(crate::languages::types::DelimitedSplitError::UnbalancedOpen)) => {
            anyhow::bail!("unbalanced delimiters in return type");
        }
        Err(Err(msg)) => anyhow::bail!("{msg}"),
    }
    ensure!(parts.len() <= 16, "return type arity exceeds bound");
    Ok(parts)
}

fn find_matching_close(input: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in input.char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(idx);
            }
        }
    }
    None
}

fn parse_return_type_node(raw: &str, depth: usize) -> Result<ReturnTypeNode> {
    ensure!(
        depth <= crate::languages::types::MAX_VALUE_FLOW_TYPE_DEPTH,
        "return type nesting exceeds MAX_VALUE_FLOW_TYPE_DEPTH"
    );
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "empty return type");
    if trimmed == "Self" {
        return Ok(ReturnTypeNode::SelfReceiver);
    }
    if trimmed == "_" {
        return Ok(ReturnTypeNode::Unknown);
    }
    if let Some(rest) = trimmed.strip_prefix('&') {
        let rest = rest.trim_start();
        let (mutable, inner) = if let Some(after) = rest.strip_prefix("mut ") {
            (true, after.trim_start())
        } else {
            (false, rest)
        };
        return Ok(ReturnTypeNode::Reference {
            mutable,
            inner: Box::new(parse_return_type_node(inner, depth + 1)?),
        });
    }
    if trimmed.starts_with('(') {
        let close = find_matching_close(trimmed, '(', ')')
            .filter(|idx| *idx + 1 == trimmed.len())
            .ok_or_else(|| anyhow::anyhow!("malformed tuple return type '{trimmed}'"))?;
        let inner = trimmed[1..close].trim();
        let items = split_top_level_args(inner)?;
        ensure!(!items.is_empty(), "empty tuple return type");
        if items.len() == 1 && !inner.contains(',') {
            return parse_return_type_node(items[0], depth + 1);
        }
        let parsed = items
            .into_iter()
            .map(|item| parse_return_type_node(item, depth + 1))
            .collect::<Result<Vec<_>>>()?;
        return Ok(ReturnTypeNode::Tuple(parsed));
    }
    if let Some((open_idx, open_ch, close_ch)) = trimmed.char_indices().find_map(|(i, c)| match c {
        '<' => Some((i, '<', '>')),
        '[' => Some((i, '[', ']')),
        _ => None,
    }) {
        let rest = &trimmed[open_idx..];
        let close_rel = find_matching_close(rest, open_ch, close_ch)
            .filter(|idx| *idx + 1 == rest.len())
            .ok_or_else(|| anyhow::anyhow!("malformed generic return type '{trimmed}'"))?;
        let base = trimmed[..open_idx].trim();
        ensure!(
            !base.is_empty() && !base.chars().any(char::is_whitespace),
            "invalid generic base '{base}'"
        );
        let args = split_top_level_args(&rest[1..close_rel])?
            .into_iter()
            .map(|arg| parse_return_type_node(arg, depth + 1))
            .collect::<Result<Vec<_>>>()?;
        ensure!(!args.is_empty(), "generic return type requires arguments");
        return Ok(ReturnTypeNode::Applied {
            base: base.to_owned(),
            args,
        });
    }
    ensure!(
        !trimmed.chars().any(char::is_whitespace)
            && !trimmed.contains(['<', '>', '(', ')', '[', ']', ',']),
        "invalid return type name '{trimmed}'"
    );
    Ok(ReturnTypeNode::Named(trimmed.to_owned()))
}

/// Receiver type rule configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverTypeRule {
    /// Type name.
    pub name: String,
    /// Module name.
    pub module: Option<String>,
    /// Export name.
    pub export: Option<String>,
    /// Member names.
    pub members: Vec<String>,
    /// Property mappings.
    pub properties: std::collections::BTreeMap<String, String>,
    /// Callback callee names.
    pub callback_callees: Vec<String>,
    /// Whether type is generic.
    pub generic: bool,
    /// Argument factories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argument_factories: Vec<ReceiverArgumentFactory>,
    /// Declarative member return types (supporting compound generic wrappers).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub member_returns: std::collections::BTreeMap<String, ReturnTypeSpec>,
    /// Declarative call return types (supporting compound generic wrappers).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub call_returns: std::collections::BTreeMap<String, ReturnTypeSpec>,
}

/// Receiver argument factory configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverArgumentFactory {
    /// Callee API.
    pub callee: super::projections::Api,
    /// Positional argument index.
    pub argument: usize,
    /// Recognized literals.
    pub literals: Vec<String>,
}

impl ReceiverArgumentFactory {
    fn validate(&self, language: LanguageId) -> Result<()> {
        ensure!(
            matches!(language, LanguageId::Javascript | LanguageId::Typescript),
            "argument factories require JavaScript or TypeScript"
        );
        let super::projections::Api::GlobalMember {
            receiver: global,
            member,
        } = &self.callee
        else {
            anyhow::bail!("argument factory requires global_member");
        };
        ensure!(
            self.argument < 4 && !self.literals.is_empty() && self.literals.len() <= 32,
            "invalid argument factory bounds"
        );
        let mut unique = std::collections::HashSet::new();
        ensure!(
            self.literals.iter().all(|literal| !literal.is_empty()
                && literal.len() <= 256
                && !literal.chars().any(char::is_control)
                && unique.insert(literal)),
            "invalid or duplicate argument factory literal"
        );
        ensure!(
            [global, member].iter().all(|name| !name.is_empty()
                && name.len() <= 4096
                && !name.chars().any(char::is_whitespace)
                && !name.chars().any(char::is_control)),
            "invalid argument factory API"
        );
        Ok(())
    }
}

/// Template dialect configuration for HTML-family languages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateDialect {
    /// Template directory.
    pub directory: String,
    /// Assignment keyword.
    pub assignment: String,
    /// Function keyword.
    pub function: String,
    /// Import keyword.
    pub import: String,
    /// From keyword.
    pub from: String,
    /// Alias keyword.
    pub alias: String,
    /// Load keyword.
    pub load: String,
    /// Include keyword.
    pub include: String,
    /// Extends keyword.
    pub extends: String,
    /// Scope opening tags.
    pub scope_open: Vec<String>,
    /// Scope closing tags.
    pub scope_close: Vec<String>,
}

impl SyntaxRule {
    pub(crate) fn key(&self) -> String {
        serde_json::to_string(&(&self.pack, &self.id)).expect("rule identity strings serialize")
    }
    pub(crate) fn activation_family(&self) -> Option<crate::languages::types::LanguageFamily> {
        match self.package_family.as_deref() {
            Some(family) => crate::languages::types::Ecosystem::from_family(family)
                .and_then(|ecosystem| ecosystem.families().first().copied()),
            None => crate::languages::types::family_for_language(&self.language),
        }
    }
    pub(super) fn validate(&self) -> Result<()> {
        self.validate_display()?;
        ensure!(
            self.package_family.as_deref().is_none_or(|family| {
                crate::languages::types::Ecosystem::from_family(family).is_some()
            }),
            "invalid syntax package family"
        );
        ensure!(
            !self.id.trim().is_empty(),
            "syntax rule id must be nonempty"
        );
        let lang = parse_language(&self.language)?;
        ensure!(
            crate::languages::types::profile_available(lang.as_str()),
            "unsupported syntax language"
        );
        ActivationCondition::validate_slice(&self.activation)?;
        let strings = self.validate_operation(lang)?;
        ensure!(
            strings
                .into_iter()
                .all(|value| !value.is_empty() && !value.chars().any(char::is_whitespace)),
            "syntax identities must be nonempty identifier paths"
        );
        Ok(())
    }

    fn validate_display(&self) -> Result<()> {
        for label in [
            &self.display.builtin,
            &self.display.autoimport,
            &self.display.library,
            &self.display.component,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                !label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control),
                "invalid syntax display label"
            );
        }
        ensure!(
            self.display.builtin.is_none()
                || matches!(
                    self.operation,
                    SyntaxOperation::ImplicitBuiltins { .. }
                        | SyntaxOperation::ImplicitReceivers { .. }
                        | SyntaxOperation::TemplateSymbols { .. }
                        | SyntaxOperation::ComponentMacros { .. }
                ),
            "builtin display requires symbol operation"
        );
        ensure!(
            self.display.autoimport.is_none() && self.display.component.is_none()
                || matches!(self.operation, SyntaxOperation::ComponentEnvironment { .. }),
            "component display requires component operation"
        );
        ensure!(
            self.display.library.is_none()
                || matches!(self.operation, SyntaxOperation::TemplateSymbols { .. }),
            "library display requires template symbols"
        );
        Ok(())
    }

    fn validate_operation(&self, lang: LanguageId) -> Result<Vec<&str>> {
        if let Some(res) = self.validate_route_and_component(lang) {
            return res;
        }
        if let Some(res) = self.validate_python_and_rust(lang) {
            return res;
        }
        if let Some(res) = self.validate_receiver_and_implicit(lang) {
            return res;
        }
        if let Some(res) = self.validate_template_and_html(lang) {
            return res;
        }
        unreachable!("all SyntaxOperation variants handled")
    }

    fn validate_route_and_component(&self, lang: LanguageId) -> Option<Result<Vec<&str>>> {
        let res: Result<Vec<&str>> = match &self.operation {
            SyntaxOperation::FileRoute {
                directory,
                extension,
                index,
                method,
            } => (|| {
                ensure!(
                    lang == LanguageId::Vue
                        && directory
                            .strip_suffix('/')
                            .is_some_and(super::semantic::normal_relative_path)
                        && extension.starts_with('.')
                        && extension.len() > 1
                        && extension[1..].chars().all(|c| c.is_ascii_alphanumeric())
                        && key_identifier(index)
                        && method == "GET",
                    "invalid file route projection"
                );
                Ok(vec![])
            })(),
            SyntaxOperation::FactoryMembers {
                callee,
                argument,
                shape,
                configured_module,
                state_member,
                actions_member,
            } => (|| {
                ensure!(
                    matches!(lang, LanguageId::Javascript | LanguageId::Typescript)
                        && (matches!(callee, Api::Imported { .. })
                            || (matches!(callee, Api::Global { .. })
                                && configured_module.is_some()))
                        && *argument < 32
                        && match shape {
                            FactoryShape::Options => {
                                state_member != actions_member
                                    && super::projections::identifier(state_member, true)
                                    && super::projections::identifier(actions_member, true)
                            }
                            FactoryShape::SetupReturn => {
                                state_member.is_empty() && actions_member.is_empty()
                            }
                        }
                        && configured_module.as_ref().is_none_or(|name| {
                            !name.is_empty()
                                && name.len() <= 256
                                && !name.chars().any(char::is_whitespace)
                        }),
                    "invalid factory members projection"
                );
                callee.validate(lang)?;
                Ok(vec![])
            })(),
            SyntaxOperation::NamedBinding { domain, binding } => (|| {
                ensure!(key_identifier(domain), "invalid named binding domain");
                binding.validate(lang)?;
                Ok(vec![])
            })(),
            SyntaxOperation::ComponentEnvironment {
                package,
                config_file,
                config_callee,
                enabled_key,
                modules_key,
                directory,
                extension,
                virtual_module,
                autoimports,
                builtins,
                modules,
                callable_imports,
            } => (|| {
                ensure!(
                    matches!(lang, LanguageId::Typescript | LanguageId::Javascript),
                    "invalid component environment language"
                );
                ensure!(
                    !config_file.contains('/')
                        && normal_relative_dir(directory)
                        && extension.starts_with('.')
                        && !extension.contains('/'),
                    "invalid component environment paths"
                );
                for module in modules.values() {
                    module.validate()?;
                }
                if let Some(imports) = callable_imports {
                    imports.validate()?;
                }
                Ok([
                    package,
                    config_file,
                    config_callee,
                    enabled_key,
                    modules_key,
                    directory,
                    extension,
                    virtual_module,
                ]
                .into_iter()
                .map(String::as_str)
                .chain(
                    autoimports
                        .iter()
                        .chain(builtins)
                        .chain(modules.keys())
                        .map(String::as_str),
                )
                .collect())
            })(),
            SyntaxOperation::RouteObject {
                collection,
                path_key,
                handler_key,
                method,
            } => (|| {
                ensure!(
                    matches!(lang, LanguageId::Javascript | LanguageId::Typescript),
                    "invalid route object language"
                );
                Ok(vec![collection.as_str(), path_key, handler_key, method])
            })(),
            SyntaxOperation::HttpClientCall {
                callee,
                receivers,
                methods,
                default_method,
                url_key,
                method_key,
                relative_prefixes,
                target_context,
            } => (|| {
                if let Some(context) = target_context {
                    context.validate()?;
                }
                ensure!(
                    !callee.is_empty() || !receivers.is_empty(),
                    "HTTP client needs a callee or receivers"
                );
                ensure!(
                    uppercase_methods(
                        methods
                            .values()
                            .map(String::as_str)
                            .chain([default_method.as_str()])
                    ),
                    "invalid client method"
                );
                ensure!(
                    matches!(
                        lang,
                        LanguageId::Python | LanguageId::Typescript | LanguageId::Javascript
                    ) && !relative_prefixes.is_empty(),
                    "invalid HTTP client language or URL prefixes"
                );
                Ok(receivers
                    .iter()
                    .chain(methods.keys())
                    .chain([url_key, method_key])
                    .chain(relative_prefixes.iter())
                    .chain((!callee.is_empty()).then_some(callee))
                    .map(String::as_str)
                    .collect())
            })(),
            SyntaxOperation::ComponentMacros {
                props,
                emits,
                slots,
                markers,
            } => (|| {
                ensure!(lang == LanguageId::Vue, "invalid component macros language");
                let names: Vec<_> = [props, emits, slots].into_iter().chain(markers).collect();
                ensure!(
                    names.iter().all(|name| {
                        !name.is_empty()
                            && name
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$'))
                    }) && names
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == names.len(),
                    "invalid or ambiguous macro identities"
                );
                Ok(names.into_iter().map(String::as_str).collect())
            })(),
            SyntaxOperation::ComponentOptions {
                module,
                export,
                components_key,
            } => (|| {
                ensure!(
                    lang == LanguageId::Vue,
                    "invalid component options language"
                );
                Ok(vec![module.as_str(), export, components_key])
            })(),
            _ => return None,
        };
        Some(res)
    }

    fn validate_python_and_rust(&self, lang: LanguageId) -> Option<Result<Vec<&str>>> {
        let res: Result<Vec<&str>> = match &self.operation {
            SyntaxOperation::RenderContext {
                package,
                module,
                export,
                target_argument,
                context_argument,
                argument_count,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python
                        && *argument_count <= 32
                        && target_argument < argument_count
                        && context_argument < argument_count
                        && target_argument != context_argument,
                    "invalid render context operation"
                );
                Ok(vec![package.as_str(), module, export])
            })(),
            SyntaxOperation::NamedRouteCall {
                module,
                export,
                argument,
                expression_prefix,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python && *argument < 32,
                    "invalid named route operation"
                );
                Ok(vec![module.as_str(), export, expression_prefix])
            })(),
            SyntaxOperation::ReceiverRegistration {
                module,
                constructors,
                member,
                path_argument,
                handler_argument,
                name_keyword,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python
                        && !constructors.is_empty()
                        && *path_argument < 32
                        && *handler_argument < 32
                        && path_argument != handler_argument,
                    "invalid receiver registration operation"
                );
                Ok(std::iter::once(module.as_str())
                    .chain(constructors.iter().map(String::as_str))
                    .chain([member.as_str(), name_keyword.as_str()])
                    .collect())
            })(),
            SyntaxOperation::DecoratedRegistration {
                modules,
                constructor,
                member,
                name_keyword,
                provider,
                ..
            } => (|| {
                ensure!(
                    lang == LanguageId::Python && !modules.is_empty(),
                    "invalid decorated registration operation"
                );
                Ok(modules
                    .iter()
                    .map(String::as_str)
                    .chain([
                        constructor.as_str(),
                        member.as_str(),
                        name_keyword.as_str(),
                        provider.as_str(),
                    ])
                    .collect())
            })(),
            SyntaxOperation::ClassMethodRegistration {
                module,
                base,
                member,
                provider,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python && key_identifier(base) && key_identifier(member),
                    "invalid class method registration"
                );
                Ok(vec![module.as_str(), base, member, provider])
            })(),
            SyntaxOperation::TestRunner { modules, members } => (|| {
                ensure!(
                    matches!(lang, LanguageId::Javascript | LanguageId::Typescript)
                        && !modules.is_empty()
                        && modules.len() <= 128
                        && !members.is_empty()
                        && members.len() <= 128
                        && members.iter().all(|member| key_identifier(member)),
                    "invalid test runner operation"
                );
                Ok(modules.iter().chain(members).map(String::as_str).collect())
            })(),
            SyntaxOperation::CommandDispatch {
                module,
                field_attribute,
                field_marker,
                parser_derive,
                subcommand_derive,
                parse_method,
            } => (|| {
                ensure!(
                    lang == LanguageId::Rust
                        && [
                            module,
                            field_attribute,
                            field_marker,
                            parser_derive,
                            subcommand_derive,
                            parse_method
                        ]
                        .iter()
                        .all(|value| key_identifier(value)),
                    "invalid command dispatch operation"
                );
                Ok(vec![
                    module.as_str(),
                    field_attribute,
                    field_marker,
                    parser_derive,
                    subcommand_derive,
                    parse_method,
                ])
            })(),
            SyntaxOperation::TemplateLoader {
                package,
                module,
                environment,
                loader,
                loader_keyword,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python,
                    "invalid template loader language"
                );
                Ok(vec![
                    package.as_str(),
                    module,
                    environment,
                    loader,
                    loader_keyword,
                ])
            })(),
            SyntaxOperation::TemplateLibrary {
                directory,
                module,
                constructor,
                members,
                name_keyword,
            } => (|| {
                ensure!(
                    lang == LanguageId::Python
                        && !members.is_empty()
                        && members
                            .values()
                            .all(|kind| matches!(kind.as_str(), "tag" | "filter")),
                    "invalid template library operation"
                );
                ensure!(
                    normal_relative_dir(directory),
                    "invalid template library directory"
                );
                Ok(std::iter::once(directory.as_str())
                    .chain([module.as_str(), constructor.as_str(), name_keyword.as_str()])
                    .chain(members.keys().map(String::as_str))
                    .collect())
            })(),
            _ => return None,
        };
        Some(res)
    }

    fn validate_receiver_and_implicit(&self, lang: LanguageId) -> Option<Result<Vec<&str>>> {
        let res: Result<Vec<&str>> = match &self.operation {
            SyntaxOperation::ReceiverType { receiver } => (|| {
                ensure!(
                    receiver.argument_factories.len() <= 1,
                    "receiver permits one argument factory"
                );
                ensure!(
                    receiver.member_returns.len() <= 256 && receiver.call_returns.len() <= 256,
                    "receiver type permits at most 256 member_returns and 256 call_returns"
                );
                for factory in &receiver.argument_factories {
                    factory.validate(lang)?;
                }
                ensure!(
                    matches!(
                        lang,
                        LanguageId::Python
                            | LanguageId::Typescript
                            | LanguageId::Javascript
                            | LanguageId::Vue
                    ) && receiver.module.is_some() == receiver.export.is_some(),
                    "invalid receiver type operation"
                );
                Ok(std::iter::once(receiver.name.as_str())
                    .chain(
                        receiver
                            .module
                            .iter()
                            .chain(receiver.export.iter())
                            .chain(receiver.members.iter())
                            .chain(receiver.properties.keys())
                            .chain(receiver.properties.values())
                            .chain(receiver.member_returns.keys())
                            .chain(receiver.call_returns.keys())
                            .chain(receiver.callback_callees.iter())
                            .map(String::as_str),
                    )
                    .collect())
            })(),
            SyntaxOperation::ImplicitBuiltins { names } => (|| {
                ensure!(!names.is_empty(), "implicit builtin names must be nonempty");
                Ok(names.iter().map(String::as_str).collect())
            })(),
            SyntaxOperation::ImplicitReceivers { names } => (|| {
                ensure!(
                    matches!(lang, LanguageId::Javascript | LanguageId::Typescript)
                        && !names.is_empty()
                        && names.iter().all(|name| {
                            !name.is_empty()
                                && name.len() <= 256
                                && !name.chars().any(|ch| ch.is_whitespace() || ch.is_control())
                        })
                        && names
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == names.len(),
                    "invalid implicit receivers language"
                );
                Ok(names.iter().map(String::as_str).collect())
            })(),
            SyntaxOperation::ObjectFactory {
                callees,
                unshadowed_globals,
            } => (|| {
                ensure!(
                    lang == LanguageId::Javascript && !callees.is_empty(),
                    "invalid object factory operation"
                );
                Ok(callees
                    .iter()
                    .chain(unshadowed_globals)
                    .map(String::as_str)
                    .collect())
            })(),
            _ => return None,
        };
        Some(res)
    }

    fn validate_template_and_html(&self, lang: LanguageId) -> Option<Result<Vec<&str>>> {
        let res: Result<Vec<&str>> = match &self.operation {
            SyntaxOperation::TemplateNamedRoute {
                tag,
                expression_prefix,
            } => (|| {
                ensure!(lang == LanguageId::Html, "invalid template tag language");
                Ok(vec![tag.as_str(), expression_prefix])
            })(),
            SyntaxOperation::TemplateDialect { dialect } => (|| {
                ensure!(
                    lang == LanguageId::Html,
                    "invalid template dialect language"
                );
                ensure!(
                    normal_relative_dir(&dialect.directory),
                    "invalid template root directory"
                );
                Ok([
                    &dialect.directory,
                    &dialect.assignment,
                    &dialect.function,
                    &dialect.import,
                    &dialect.from,
                    &dialect.alias,
                    &dialect.load,
                    &dialect.include,
                    &dialect.extends,
                ]
                .into_iter()
                .chain(dialect.scope_open.iter())
                .chain(dialect.scope_close.iter())
                .map(String::as_str)
                .collect())
            })(),
            SyntaxOperation::TemplateSymbols {
                tags,
                filters,
                libraries,
                library_routes,
                ..
            } => (|| {
                ensure!(
                    lang == LanguageId::Html,
                    "invalid template symbols language"
                );
                let valid_names = tags
                    .iter()
                    .chain(filters)
                    .chain(libraries)
                    .all(|name| super::projections::identifier(name, false));
                ensure!(
                    valid_names
                        && tags.len() <= 512
                        && filters.len() <= 512
                        && libraries.len() <= 128
                        && library_routes.len() <= 128
                        && tags.iter().collect::<std::collections::BTreeSet<_>>().len()
                            == tags.len()
                        && filters
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == filters.len()
                        && libraries
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == libraries.len()
                        && library_routes.iter().all(|(tag, library)| {
                            super::projections::identifier(tag, false)
                                && super::projections::identifier(library, false)
                        }),
                    "invalid template symbol declarations"
                );
                Ok(tags
                    .iter()
                    .chain(filters)
                    .chain(libraries)
                    .chain(library_routes.keys())
                    .chain(library_routes.values())
                    .map(String::as_str)
                    .collect())
            })(),
            SyntaxOperation::HtmlScriptAttributes {
                event_prefixes,
                object_attribute,
                event_scope,
                object_scope,
            } => (|| {
                ensure!(
                    lang == LanguageId::Html && !event_prefixes.is_empty(),
                    "invalid HTML script attributes"
                );
                Ok(event_prefixes
                    .iter()
                    .map(String::as_str)
                    .chain([
                        object_attribute.as_str(),
                        event_scope.as_str(),
                        object_scope.as_str(),
                    ])
                    .collect())
            })(),
            SyntaxOperation::HtmlRequestAttributes {
                request_node,
                context_node,
                namespace,
                context_label,
                prefixes,
                methods,
                contexts,
                values,
                value_dynamic_prefixes,
            } => (|| {
                ensure!(
                    lang == LanguageId::Html && !prefixes.is_empty() && !methods.is_empty(),
                    "invalid HTML request operation"
                );
                ensure!(
                    values.is_some() || value_dynamic_prefixes.is_empty(),
                    "HTML request value prefixes require a values attribute"
                );
                ensure!(
                    request_node != context_node
                        && [request_node, context_node, namespace]
                            .into_iter()
                            .all(|value| is_ident_like(value)),
                    "invalid HTML node role mapping"
                );
                ensure!(
                    !context_label.trim().is_empty()
                        && context_label.len() <= 128
                        && !context_label.contains(['\n', '\r']),
                    "invalid HTML context label"
                );
                ensure!(
                    uppercase_methods(methods.values().map(String::as_str)),
                    "invalid request method"
                );
                Ok(prefixes
                    .iter()
                    .chain(methods.keys())
                    .chain(contexts)
                    .chain(values.iter())
                    .chain(value_dynamic_prefixes)
                    .map(String::as_str)
                    .collect())
            })(),
            _ => return None,
        };
        Some(res)
    }
}
