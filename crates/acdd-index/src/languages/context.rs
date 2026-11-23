//! Per-file language context, compiled rule dispatch, and scope projection helpers.

use super::manifests;
use super::semantic;
use super::types::LanguageFileState;
use crate::ast::syntax::{text, ScopeBindings};
use crate::models::Facts;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, RwLock},
};

#[derive(Default)]
/// Per-file extraction state shared by hooks of one language profile.
pub struct FileContext {
    /// Effective validated extraction context for this file.
    pub rules: Arc<LanguageContext>,
    /// Bindings prepared for this exact syntax root; consumed by the shared visitor.
    pub root_bindings: Option<(usize, ScopeBindings)>,
    /// Typed per-language state owned by the active profile.
    pub state: LanguageFileState,
}

#[derive(Debug, Default)]
struct CompiledRuleIR {
    call_kinds: [Option<u16>; 2],
    route_decorators: Vec<CompiledRouteRule<manifests::RouteDecoratorRule>>,
    route_calls: Vec<CompiledRouteRule<manifests::RouteCallRule>>,
    di_calls: Vec<CompiledRouteRule<manifests::DiCallRule>>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledRouteRule<T> {
    rule: T,
    pub(crate) origin: Option<crate::semantic::ExtractionRuleOrigin>,
}

impl<T> std::ops::Deref for CompiledRouteRule<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.rule
    }
}

impl CompiledRuleIR {
    fn compile(grammar: &tree_sitter::Language, context: &LanguageContext) -> Self {
        let call_kinds = [
            manifests::named_kind(grammar, "call"),
            manifests::named_kind(grammar, "call_expression"),
        ];
        let mut compiled = Self {
            call_kinds,
            ..Self::default()
        };
        if call_kinds.iter().any(Option::is_some) {
            compiled.route_calls = context
                .route_calls
                .iter()
                .enumerate()
                .map(|(index, rule)| CompiledRouteRule {
                    rule: rule.clone(),
                    origin: context.route_rule_origins.get(&(false, index)).cloned(),
                })
                .collect();
            compiled.di_calls = context
                .di_calls
                .iter()
                .enumerate()
                .map(|(index, rule)| CompiledRouteRule {
                    rule: rule.clone(),
                    origin: context.di_rule_origins.get(&index).cloned(),
                })
                .collect();
            if manifests::named_kind(grammar, "decorator").is_some() {
                compiled.route_decorators = context
                    .route_decorators
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| CompiledRouteRule {
                        rule: rule.clone(),
                        origin: context.route_rule_origins.get(&(true, index)).cloned(),
                    })
                    .collect();
            }
        }
        compiled
    }

    fn contains_call_kind(&self, kind: u16) -> bool {
        self.call_kinds.contains(&Some(kind))
    }
}

/// Immutable effective-context dispatch plus an optional grammar-bound view.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct RuleIRHandle {
    prepared: Arc<RwLock<HashMap<tree_sitter::Language, Arc<CompiledRuleIR>>>>,
    bound: Option<Arc<CompiledRuleIR>>,
}

impl RuleIRHandle {
    pub(crate) fn prepare(_context: &LanguageContext) -> Self {
        Self::default()
    }

    pub(crate) fn bind(
        &self,
        grammar: &tree_sitter::Language,
        context: &LanguageContext,
    ) -> Self {
        if let Some(cached) = self
            .prepared
            .read()
            .expect("rule IR cache lock")
            .get(grammar)
            .cloned()
        {
            return Self {
                prepared: Arc::clone(&self.prepared),
                bound: Some(cached),
            };
        }
        let compiled = Arc::new(CompiledRuleIR::compile(grammar, context));
        let bound = self
            .prepared
            .write()
            .expect("rule IR cache lock")
            .entry(grammar.clone())
            .or_insert_with(|| Arc::clone(&compiled))
            .clone();
        Self {
            prepared: Arc::clone(&self.prepared),
            bound: Some(bound),
        }
    }

    pub(crate) fn route_decorators_for_kind(
        &self,
        kind: u16,
    ) -> &[CompiledRouteRule<manifests::RouteDecoratorRule>] {
        self.bound
            .as_ref()
            .filter(|compiled| compiled.contains_call_kind(kind))
            .map_or(&[], |compiled| compiled.route_decorators.as_slice())
    }

    pub(crate) fn route_calls_for_kind(
        &self,
        kind: u16,
    ) -> &[CompiledRouteRule<manifests::RouteCallRule>] {
        self.bound
            .as_ref()
            .filter(|compiled| compiled.contains_call_kind(kind))
            .map_or(&[], |compiled| compiled.route_calls.as_slice())
    }

    pub(crate) fn di_calls_for_kind(&self, kind: u16) -> &[CompiledRouteRule<manifests::DiCallRule>] {
        self.bound
            .as_ref()
            .filter(|compiled| compiled.contains_call_kind(kind))
            .map_or(&[], |compiled| compiled.di_calls.as_slice())
    }
}

/// Manifest-backed extraction rules prepared once for one source file.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct LanguageContext {
    /// External class hierarchy receiver rules.
    pub class_hierarchy_receivers: Vec<manifests::ClassHierarchyReceiverRule>,
    /// Decorator route rules.
    pub route_decorators: Vec<manifests::RouteDecoratorRule>,
    /// Registration call route rules.
    pub route_calls: Vec<manifests::RouteCallRule>,
    pub(crate) route_rule_origins:
        HashMap<(bool, usize), crate::semantic::ExtractionRuleOrigin>,
    /// Dependency-injection call rules.
    pub di_calls: Vec<manifests::DiCallRule>,
    pub(crate) di_rule_origins: HashMap<usize, crate::semantic::ExtractionRuleOrigin>,
    /// Semantic extraction rules.
    pub semantic_rules: Vec<semantic::EffectiveSemanticRule>,
    /// Syntax operation rules.
    pub syntax_rules: Vec<manifests::syntax::SyntaxRule>,
    /// Necessary operation families derived during effective rule preparation.
    pub(crate) has_factory_members: bool,
    pub(crate) has_object_call_definitions: bool,
    named_member_rules: HashMap<String, Vec<usize>>,
    projection_rules: Option<ProjectionRuleIndex>,
    prepared_projections: HashMap<String, PreparedRules>,
    file_provider_rules: Option<HashMap<String, Vec<usize>>>,
    syntax_activation_keys: manifests::semantic::SyntaxActivationKeys,
    has_syntax_gated_rules: bool,
    pub(crate) workspace_activated_rules: BTreeMap<String, BTreeSet<String>>,
    pub(crate) rule_ir: RuleIRHandle,
}

#[derive(Clone, Debug, Default)]
struct ProjectionRuleIndex {
    globals: HashMap<String, Vec<usize>>,
    members: HashMap<String, Vec<usize>>,
    imported: Vec<usize>,
}

/// Merge necessary candidates without allocating or changing effective row order.
pub(crate) struct ProjectionRuleCandidates<'a> {
    slices: [&'a [usize]; 4],
    positions: [usize; 4],
    legacy: std::ops::Range<usize>,
}

impl Iterator for ProjectionRuleCandidates<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(index) = self.legacy.next() {
            return Some(index);
        }
        let next = self
            .slices
            .iter()
            .zip(self.positions)
            .filter_map(|(slice, position)| slice.get(position))
            .min()
            .copied()?;
        for (slice, position) in self.slices.iter().zip(&mut self.positions) {
            if slice.get(*position) == Some(&next) {
                *position += 1;
            }
        }
        Some(next)
    }
}

impl LanguageContext {
    fn is_workspace_activated(&self, rule: &manifests::syntax::SyntaxRule) -> bool {
        self.workspace_activated_rules
            .get(&rule.pack)
            .is_some_and(|ids| ids.contains(&rule.id))
    }

    pub(crate) fn prepare_named_member_rules(&mut self) {
        use manifests::{projections::NamedBindingForm, syntax::SyntaxOperation};
        self.named_member_rules.clear();
        self.has_factory_members = false;
        self.has_object_call_definitions = false;
        self.syntax_activation_keys = manifests::semantic::SyntaxActivationKeys::default();
        self.has_syntax_gated_rules = false;
        let mut projections = ProjectionRuleIndex::default();
        for (index, rule) in self.syntax_rules.iter().enumerate() {
            if !rule.activation.is_empty()
                && !self.is_workspace_activated(rule)
                && rule
                    .activation
                    .iter()
                    .any(manifests::semantic::ActivationCondition::is_file_scoped)
            {
                self.has_syntax_gated_rules = true;
                for condition in &rule.activation {
                    condition.collect_syntax_keys(&mut self.syntax_activation_keys);
                }
            }
            self.has_factory_members |=
                matches!(&rule.operation, SyntaxOperation::FactoryMembers { .. });
            self.has_object_call_definitions |= matches!(
                &rule.operation,
                SyntaxOperation::NamedBinding {
                    binding: NamedBindingForm::CallDefinition {
                        target_kind: manifests::projections::DefinitionKind::Object,
                        ..
                    },
                    ..
                }
            );
            if let SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::MemberReference { root },
                ..
            } = &rule.operation
            {
                self.named_member_rules
                    .entry(root.clone())
                    .or_default()
                    .push(index);
            }
            let api = manifests::semantic::syntax_rule_api(rule).and_then(Result::ok);
            match api {
                Some(manifests::projections::Api::Global { name }) => {
                    projections
                        .globals
                        .entry(name.clone())
                        .or_default()
                        .push(index);
                }
                Some(manifests::projections::Api::GlobalMember { member, .. }) => {
                    projections
                        .members
                        .entry(member.clone())
                        .or_default()
                        .push(index);
                }
                Some(manifests::projections::Api::Imported { .. }) => {
                    projections.imported.push(index)
                }
                _ => {}
            }
        }
        self.projection_rules = Some(projections);
        let mut providers: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, effective) in self
            .semantic_rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.active)
        {
            if let manifests::SemanticOperation::CallToFile {
                target: manifests::SemanticTarget::ProjectPathSuffix { language, .. },
                ..
            }
            | manifests::SemanticOperation::ClassAttributeToFile {
                target: manifests::SemanticTarget::ProjectPathSuffix { language, .. },
                ..
            } = &effective.rule.operation
            {
                providers.entry(language.clone()).or_default().push(index);
            }
        }
        self.file_provider_rules = Some(providers);
        self.prepared_projections = prepare_projection_rules(self);
    }

    pub(crate) fn prepared_projections(&self, language: &str) -> Option<&PreparedRules> {
        self.prepared_projections.get(language)
    }

    /// Returns manifest-declared syntax attribute names and prefixes collected for deferred rules.
    pub fn syntax_activation_keys(&self) -> &manifests::semantic::SyntaxActivationKeys {
        &self.syntax_activation_keys
    }

    /// Reports whether any retained syntax rule requires per-file extension or syntax signal checks.
    pub fn has_syntax_gated_rules(&self) -> bool {
        self.has_syntax_gated_rules
    }

    /// Evaluates whether `rule` is active for `path` and its observed `signals` in O(1) fast-path.
    pub fn is_syntax_rule_active(
        &self,
        rule: &manifests::syntax::SyntaxRule,
        path: &str,
        signals: &manifests::semantic::FileActivationSignals,
    ) -> bool {
        if !self.has_syntax_gated_rules
            || rule.activation.is_empty()
            || self.is_workspace_activated(rule)
            || !rule
                .activation
                .iter()
                .any(manifests::semantic::ActivationCondition::is_file_scoped)
        {
            return true;
        }
        manifests::semantic::ActivationCondition::matches_file_slice(&rule.activation, path, signals)
    }

    /// Iterates over syntax rules active for `path` and `signals`.
    pub fn syntax_rules_for_signals<'a>(
        &'a self,
        path: &'a str,
        signals: &'a manifests::semantic::FileActivationSignals,
    ) -> impl Iterator<Item = &'a manifests::syntax::SyntaxRule> + 'a {
        self.syntax_rules
            .iter()
            .filter(move |rule| self.is_syntax_rule_active(rule, path, signals))
    }

    pub(crate) fn named_member_rules(&self, root: &str) -> &[usize] {
        self.named_member_rules.get(root).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn has_projection_rules_for_kind(&self, kind: &str) -> bool {
        match kind {
            "call_expression" => self.projection_rules.as_ref().is_none_or(|rules| {
                !rules.globals.is_empty()
                    || !rules.members.is_empty()
                    || !rules.imported.is_empty()
                    || !self.named_member_rules.is_empty()
            }),
            "member_expression" => {
                self.projection_rules.is_none() || !self.named_member_rules.is_empty()
            }
            _ => false,
        }
    }

    pub(crate) fn projection_rule_candidates(
        &self,
        kind: &str,
        callee: &str,
    ) -> ProjectionRuleCandidates<'_> {
        let mut candidates = ProjectionRuleCandidates {
            slices: [&[]; 4],
            positions: [0; 4],
            legacy: 0..0,
        };
        let Some(index) = &self.projection_rules else {
            candidates.legacy = 0..self.syntax_rules.len();
            return candidates;
        };
        if kind == "call_expression" {
            candidates.slices[0] = index.globals.get(callee).map_or(&[], Vec::as_slice);
            if let Some((_, member)) = callee.rsplit_once('.') {
                candidates.slices[1] = index.members.get(member).map_or(&[], Vec::as_slice);
            }
            if callee.split('.').count() <= 2 {
                candidates.slices[2] = &index.imported;
            }
        }
        if matches!(kind, "call_expression" | "member_expression") {
            if let Some((root, _)) = callee.split_once('.') {
                candidates.slices[3] = self.named_member_rules(root);
            }
        }
        candidates
    }

    pub(crate) fn file_provider_rules_for_language(&self, language: &str) -> Option<&[usize]> {
        self.file_provider_rules
            .as_ref()
            .map(|rules| rules.get(language).map_or(&[][..], Vec::as_slice))
    }

    pub(crate) fn component_macro(
        &self,
        name: &str,
    ) -> Option<(
        &manifests::syntax::SyntaxRule,
        crate::semantic::ComponentMacroRole,
    )> {
        use crate::semantic::ComponentMacroRole;
        let mut matches = self.syntax_rules.iter().filter_map(|rule| {
            let manifests::syntax::SyntaxOperation::ComponentMacros {
                props,
                emits,
                slots,
                markers,
            } = &rule.operation
            else {
                return None;
            };
            let role = if name == props {
                ComponentMacroRole::Props
            } else if name == emits {
                ComponentMacroRole::Emits
            } else if name == slots {
                ComponentMacroRole::Slots
            } else if markers.iter().any(|marker| marker == name) {
                ComponentMacroRole::Marker
            } else {
                return None;
            };
            Some((rule, role))
        });
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    pub(crate) fn bind_rule_ir(&self, grammar: &tree_sitter::Language) -> RuleIRHandle {
        self.rule_ir.bind(grammar, self)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedRules {
    pub(crate) render_rules: Vec<usize>,
    pub(crate) loader_rules: Vec<usize>,
    pub(crate) named_route_rules: Vec<usize>,
    pub(crate) named_route_members: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) decorated_registration_rules: Vec<usize>,
    pub(crate) class_attributes: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) class_defaults: Vec<usize>,
    pub(crate) macro_tables: hashbrown::HashMap<String, Vec<usize>>,
    pub(crate) function_semantic: Vec<usize>,
    pub(crate) call_semantic: Vec<usize>,
    pub(crate) call_to_file: Vec<usize>,
    pub(crate) named_references: Vec<usize>,
    pub(crate) registry_definitions: Vec<usize>,
    pub(crate) semantic_constructors: Vec<usize>,
    pub(crate) syntax_constructors: Vec<usize>,
}

pub(crate) fn prepare_projection_rules(
    rules: &LanguageContext,
) -> HashMap<String, PreparedRules> {
    use manifests::{
        projections::{Api, AttachmentSource, NamedBindingForm},
        syntax::SyntaxOperation,
        SemanticOperation,
    };
    let mut compiled = HashMap::<String, PreparedRules>::new();
    for (index, effective) in rules
        .semantic_rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule.active)
    {
        match &effective.rule.operation {
            SemanticOperation::ClassAttributeToFile {
                language,
                attribute,
                ..
            }
            | SemanticOperation::ClassAttributeToSymbol {
                language,
                attribute,
                ..
            }
            | SemanticOperation::ClassAttributeToTable {
                language,
                attribute,
                ..
            } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .class_attributes
                    .entry(attribute.clone())
                    .or_default()
                    .push(index);
            }
            SemanticOperation::CallToFile { language, .. }
            | SemanticOperation::CallToSymbol { language, .. } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .call_to_file
                    .push(index);
            }
            SemanticOperation::ClassDefaultTable { language, .. } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .class_defaults
                    .push(index);
            }
            SemanticOperation::MacroTypeToTable {
                language,
                macro_path,
            } => {
                compiled
                    .entry(language.clone())
                    .or_default()
                    .macro_tables
                    .entry(macro_path.clone())
                    .or_default()
                    .push(index);
            }
            SemanticOperation::SymbolAttachment {
                language,
                callee,
                source,
                ..
            } => {
                let prepared = compiled.entry(language.clone()).or_default();
                match source {
                    AttachmentSource::DecoratedCallable => prepared.function_semantic.push(index),
                    AttachmentSource::Argument { .. } => prepared.call_semantic.push(index),
                }
                if matches!(callee, Api::ConstructedMember { .. }) {
                    prepared.semantic_constructors.push(index);
                }
            }
            SemanticOperation::EndpointProjection {
                language,
                callee,
                attachment,
                ..
            } => {
                let prepared = compiled.entry(language.clone()).or_default();
                prepared.call_semantic.push(index);
                if [callee, &attachment.callee]
                    .iter()
                    .any(|api| matches!(api, Api::ConstructedMember { .. }))
                {
                    prepared.semantic_constructors.push(index);
                }
            }
        }
    }
    for (index, rule) in rules.syntax_rules.iter().enumerate() {
        let prepared = compiled.entry(rule.language.clone()).or_default();
        match &rule.operation {
            SyntaxOperation::RenderContext { .. } => prepared.render_rules.push(index),
            SyntaxOperation::TemplateLoader { .. } => prepared.loader_rules.push(index),
            SyntaxOperation::NamedRouteCall { export, .. } => {
                prepared.named_route_rules.push(index);
                prepared
                    .named_route_members
                    .entry(export.clone())
                    .or_default()
                    .push(index);
            }
            SyntaxOperation::DecoratedRegistration { .. } => {
                prepared.decorated_registration_rules.push(index)
            }
            SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::CallReference { callee, .. },
                ..
            } => {
                prepared.named_references.push(index);
                if matches!(callee, Api::ConstructedMember { .. }) {
                    prepared.syntax_constructors.push(index);
                }
            }
            SyntaxOperation::NamedBinding {
                binding: NamedBindingForm::AssignmentDefinition { .. },
                ..
            } => {
                prepared.registry_definitions.push(index);
            }
            _ => {}
        }
    }
    compiled
}

pub(crate) struct CallRulesInput<'a, 'source, 'tree> {
    pub(crate) ctx: &'a super::types::SyntaxContext<'source, 'tree>,
    pub(crate) rules: &'a LanguageContext,
    pub(crate) indices: &'a [usize],
    pub(crate) language: &'a str,
    pub(crate) alias: &'a str,
}

pub(crate) struct CallRulesResolvers<'a, 'source> {
    pub(crate) candidate: &'a dyn Fn(&str) -> bool,
    pub(crate) argument_value: &'a dyn Fn(usize) -> Option<&'source str>,
    pub(crate) imported: &'a dyn Fn(&str, &str) -> bool,
}

fn import_scope_visible_from_call(
    source: &str,
    module_owner: &str,
    call_owner: &str,
    call_line: usize,
    visible_local_import_owners: &mut Option<Vec<String>>,
    facts: &Facts,
) -> bool {
    if source == module_owner || source == call_owner {
        return true;
    }
    if source.starts_with("module:") {
        return false;
    }
    visible_local_import_owners
        .get_or_insert_with(|| visible_local_import_owners_for_call(call_owner, call_line, facts))
        .iter()
        .any(|owner| owner.as_str() == source)
}

fn visible_local_import_owners_for_call(
    call_owner: &str,
    call_line: usize,
    facts: &Facts,
) -> Vec<String> {
    let Some(call_scope) = facts.nodes.iter().find(|node| node.id == call_owner) else {
        return Vec::new();
    };
    let call_qualname = call_scope.qualname.as_str();
    facts
        .nodes
        .iter()
        .filter(|scope| {
            scope.id != call_owner
                && matches!(scope.kind.as_str(), "function" | "method")
                && scope.line <= call_line
                && call_line <= scope.end_line
                && scope.has_binding_names()
                && call_qualname
                    .strip_prefix(&scope.qualname)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
        .map(|scope| scope.id.clone())
        .collect()
}

pub(crate) fn execute_call_rules<'source>(
    input: CallRulesInput<'_, 'source, '_>,
    resolvers: &CallRulesResolvers<'_, 'source>,
    facts: &mut Facts,
) {
    let ctx = input.ctx;
    let Some(module_owner) = facts
        .nodes
        .iter()
        .find(|node| node.kind == "module")
        .map(|node| node.id.clone())
    else {
        return;
    };
    let position = ctx.node.start_position();
    let call_line = position.row + ctx.offset + 1;
    let mut visible_local_import_owners = None;
    for &index in input.indices {
        let effective = &input.rules.semantic_rules[index];
        let (manifests::SemanticOperation::CallToFile {
            language: expected,
            callee,
            argument,
            target,
            relation,
        }
        | manifests::SemanticOperation::CallToSymbol {
            language: expected,
            callee,
            argument,
            target,
            relation,
        }) = &effective.rule.operation
        else {
            continue;
        };
        let manifests::SemanticCallee::Imported { module, export } = callee;
        if expected != input.language || !(resolvers.candidate)(module) {
            continue;
        }
        let Some(literal) = (resolvers.argument_value)(*argument)
            .filter(|literal| manifests::semantic::normal_relative_path(literal))
        else {
            continue;
        };
        let Some(import_owner) = facts
            .references
            .iter()
            .find(|reference| {
                reference.kind == "imports"
                    && reference.alias.as_deref().unwrap_or(&reference.expression) == input.alias
                    && reference.module.as_deref() == Some(module)
                    && reference.expression == *export
                    && import_scope_visible_from_call(
                        &reference.source,
                        &module_owner,
                        ctx.owner,
                        call_line,
                        &mut visible_local_import_owners,
                        facts,
                    )
            })
            .map(|reference| reference.source.clone())
        else {
            continue;
        };
        if !(resolvers.imported)(module, export) {
            continue;
        }
        semantic::push(
            facts,
            effective.fact(
                ctx.owner,
                position.row + ctx.offset + 1,
                position.column,
                literal.to_owned(),
                text(ctx.node, ctx.source).to_owned(),
                target,
                semantic::SemanticFactRole::CallToFile {
                    import_owner,
                    alias: input.alias.to_owned(),
                    module: module.clone(),
                    export: export.clone(),
                    relation: *relation,
                },
            ),
        );
    }
}

/// Project effective rules onto actual module identities, including empty files.
pub(crate) fn file_providers(facts: &mut Facts, context: &LanguageContext) {
    let Some(module) = facts.nodes.iter().find(|node| node.kind == "module") else {
        return;
    };
    let (id, path, language) = (
        module.id.clone(),
        module.path.clone(),
        module.language.clone(),
    );
    let prepared = context.file_provider_rules_for_language(&language);
    let indices = prepared.into_iter().flatten().copied().chain(
        0..if prepared.is_none() {
            context.semantic_rules.len()
        } else {
            0
        },
    );
    for index in indices {
        let effective = &context.semantic_rules[index];
        if !effective.active {
            continue;
        }
        let target = match &effective.rule.operation {
            manifests::SemanticOperation::CallToFile { target, .. }
            | manifests::SemanticOperation::ClassAttributeToFile { target, .. } => target,
            manifests::SemanticOperation::CallToSymbol { .. }
            | manifests::SemanticOperation::ClassAttributeToSymbol { .. }
            | manifests::SemanticOperation::ClassAttributeToTable { .. }
            | manifests::SemanticOperation::ClassDefaultTable { .. }
            | manifests::SemanticOperation::MacroTypeToTable { .. }
            | manifests::SemanticOperation::SymbolAttachment { .. }
            | manifests::SemanticOperation::EndpointProjection { .. } => continue,
        };
        for literal in target.paths(&path, &language) {
            semantic::push(
                facts,
                effective.fact(
                    &id,
                    1,
                    0,
                    literal,
                    path.clone(),
                    target,
                    semantic::SemanticFactRole::FileProvider { target: id.clone() },
                ),
            );
        }
    }
}

pub(crate) fn publish_scope_projections(
    facts: &mut Facts,
    mut projections: HashMap<String, crate::semantic::ValueFlowFacts>,
) {
    if projections.is_empty() {
        return;
    }
    for node in &mut facts.nodes {
        let Some(flow) = projections.remove(&node.id) else {
            continue;
        };
        let output = &mut node.details["value_flow"];
        if !output.is_object() {
            *output = serde_json::json!({});
        }
        for (key, mut value, present) in [
            (
                "component_configurations",
                serde_json::to_value(flow.component_configurations().collect::<Vec<_>>())
                    .expect("typed configurations"),
                flow.component_configurations().next().is_some(),
            ),
            (
                "symbol_attachments",
                serde_json::to_value(flow.symbol_attachments().collect::<Vec<_>>())
                    .expect("typed attachments"),
                flow.symbol_attachments().next().is_some(),
            ),
            (
                "named_bindings",
                serde_json::to_value(flow.named_bindings().collect::<Vec<_>>())
                    .expect("typed named bindings"),
                flow.named_bindings().next().is_some(),
            ),
            (
                "endpoint_requests",
                serde_json::to_value(flow.endpoint_requests().collect::<Vec<_>>())
                    .expect("typed endpoints"),
                flow.endpoint_requests().next().is_some(),
            ),
        ] {
            if present {
                if output.get(key).is_none() {
                    output[key] = serde_json::json!([]);
                }
                let values = output[key]
                    .as_array_mut()
                    .expect("parser-owned projection vector");
                let incoming = value.as_array_mut().expect("typed projection vector");
                if key == "component_configurations" {
                    values.append(incoming);
                } else {
                    let overflow = values.len().saturating_add(incoming.len()) > 128;
                    values.extend(
                        incoming
                            .drain(..)
                            .take(129usize.saturating_sub(values.len())),
                    );
                    if overflow {
                        output["fact_limit_exceeded"] = true.into();
                    }
                }
            }
        }
        if flow.fact_limit_exceeded {
            output["fact_limit_exceeded"] = true.into();
        }
    }
}
