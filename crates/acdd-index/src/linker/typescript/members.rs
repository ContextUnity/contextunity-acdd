use crate::languages::profile::{LanguageMemberIndex, LanguageMemberResult};
use crate::linker::value_flow::types::{SemanticResolver, Symbol, SymbolRole, TypeTarget};
use crate::models::Node;
use crate::semantic::SourcePosition;
use hashbrown::{HashMap, HashSet};
use std::hash::BuildHasher;

type MemberMap<'a> = HashMap<(&'a str, bool), Vec<&'a Node>>;

pub(crate) struct TypeScriptMembers<'a> {
    members: HashMap<&'a str, MemberMap<'a>>,
    fields: HashMap<&'a str, MemberMap<'a>>,
    implementors: HashMap<&'a str, Vec<&'a Node>>,
}

impl<'a> TypeScriptMembers<'a> {
    pub(crate) fn build(
        all: &'a dyn crate::models::FactsLookup,
        resolver: &impl SemanticResolver<'a>,
    ) -> Self {
        let classes = collect_type_script_classes(all);
        let mut own = HashMap::<&str, MemberMap<'a>>::new();
        let mut own_fields = HashMap::<&str, MemberMap<'a>>::new();
        let mut parents = HashMap::<&str, Option<Vec<&Node>>>::new();
        let mut implements_targets = HashMap::<&str, Vec<&Node>>::new();
        for class in &classes {
            let Some(facts) = all.get_facts(&class.path) else {
                continue;
            };
            collect_own_class_members(class, facts, resolver, &mut own, &mut own_fields);
            let (bases, invalid) =
                collect_class_bases(class, facts, resolver, &mut implements_targets);
            parents.insert(class.id.as_str(), (!invalid).then_some(bases));
        }
        let mut members = HashMap::new();
        let mut fields = HashMap::new();
        let mut states = HashMap::new();
        let mut invalid = HashSet::new();
        for class in &classes {
            validate_chain(class.id.as_str(), &parents, &mut states, &mut invalid, 0);
        }
        for class in &invalid {
            parents.insert(class, None);
        }
        let mut implementors = HashMap::<&'a str, Vec<&'a Node>>::new();
        for class in &classes {
            inherited(class, &own, &parents, &mut members, &mut HashSet::new(), 0);
            if !own_fields.is_empty() {
                inherited(
                    class,
                    &own_fields,
                    &parents,
                    &mut fields,
                    &mut HashSet::new(),
                    0,
                );
            }
            record_concrete_implementors(class, &implements_targets, &parents, &mut implementors);
        }
        for list in implementors.values_mut() {
            list.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            list.dedup_by(|a, b| a.id == b.id);
        }
        Self {
            members,
            fields,
            implementors,
        }
    }

    pub(crate) fn implementors(&self, base_id: &str) -> &[&'a Node] {
        self.implementors.get(base_id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn lookup(&self, receiver: &Node, member: &str, associated: bool) -> &[&'a Node] {
        let Some(members) = self.members.get(receiver.id.as_str()) else {
            return &[];
        };
        let hash = members.hasher().hash_one((member, associated));
        members
            .raw_entry()
            .from_hash(hash, |(name, is_static)| {
                *name == member && *is_static == associated
            })
            .map_or(&[], |(_, nodes)| nodes.as_slice())
    }

    pub(crate) fn lookup_field(&self, receiver: &Node, member: &str) -> &[&'a Node] {
        let Some(fields) = self.fields.get(receiver.id.as_str()) else {
            return &[];
        };
        let hash = fields.hasher().hash_one((member, false));
        fields
            .raw_entry()
            .from_hash(hash, |(name, is_static)| *name == member && !*is_static)
            .map_or(&[], |(_, nodes)| nodes.as_slice())
    }
}

impl LanguageMemberIndex for TypeScriptMembers<'_> {
    fn language_ids(&self) -> &'static [&'static str] {
        &["typescript", "javascript", "vue"]
    }

    fn lookup_member<'a>(
        &'a self,
        receiver: &Node,
        member: &str,
        associated: bool,
        _caller: &Node,
    ) -> LanguageMemberResult<'a> {
        let candidates = self.lookup(receiver, member, associated);
        if candidates.is_empty() {
            LanguageMemberResult::Unknown
        } else if let [node] = candidates {
            LanguageMemberResult::Local(node)
        } else {
            LanguageMemberResult::Candidates(candidates.to_vec())
        }
    }

    fn lookup_property<'a>(
        &'a self,
        receiver: &Node,
        member: &str,
    ) -> LanguageMemberResult<'a> {
        let candidates = self.lookup_field(receiver, member);
        if candidates.is_empty() {
            LanguageMemberResult::Unknown
        } else if let [node] = candidates {
            LanguageMemberResult::Local(node)
        } else {
            LanguageMemberResult::Candidates(candidates.to_vec())
        }
    }

    fn implementors(&self, base: &Node) -> Option<&[&Node]> {
        Some(TypeScriptMembers::implementors(self, base.id.as_str()))
    }
}

fn collect_type_script_classes(all: &dyn crate::models::FactsLookup) -> Vec<&Node> {
    all.values_facts()
        .flat_map(|facts| &facts.nodes)
        .filter(|node| {
            matches!(node.language.as_str(), "typescript" | "javascript" | "vue")
                && (matches!(node.kind.as_str(), "class" | "interface" | "enum")
                    || (node.language == "javascript"
                        && node.kind == "function"
                        && node.details["prototype_constructor"] == true))
        })
        .collect()
}

fn is_valid_prototype_method<'a>(
    class: &'a Node,
    method: &'a Node,
    resolver: &impl SemanticResolver<'a>,
) -> bool {
    if method.details["prototype"] != true {
        return true;
    }
    if method.details["prototype_valid"] != true {
        return false;
    }
    let assignment_line = method.details["prototype_assignment_line"]
        .as_u64()
        .unwrap_or(method.line as u64) as usize;
    let assignment_column = method.details["prototype_assignment_column"]
        .as_u64()
        .or_else(|| method.details["column"].as_u64())
        .unwrap_or(0) as usize;
    let lexical_class = resolver.resolve_symbol(
        method,
        &class.name,
        SourcePosition {
            line: assignment_line,
            column: assignment_column,
        },
        SymbolRole::Constructor,
    );
    let exact_owner =
        matches!(lexical_class, Symbol::Type(TypeTarget::Local(node)) if node.id == class.id);
    let class_visible = class.kind == "function"
        || class.line < assignment_line
        || (class.line == assignment_line
            && class.details["column"].as_u64().unwrap_or(0) < assignment_column as u64);
    exact_owner && class_visible
}

fn collect_own_class_members<'a>(
    class: &'a Node,
    facts: &'a crate::models::Facts,
    resolver: &impl SemanticResolver<'a>,
    own: &mut HashMap<&'a str, MemberMap<'a>>,
    own_fields: &mut HashMap<&'a str, MemberMap<'a>>,
) {
    let prefix = format!("{}.", class.qualname);
    let methods = own.entry(class.id.as_str()).or_default();
    for method in &facts.nodes {
        let is_method = matches!(method.kind.as_str(), "method" | "function")
            || (matches!(class.kind.as_str(), "enum" | "interface") && method.kind == "field");
        let is_field = method.kind == "field";
        if !is_method && !is_field {
            continue;
        }
        let Some(name) = method.details["prototype_member"].as_str().or_else(|| {
            method
                .qualname
                .strip_prefix(&prefix)
                .filter(|name| !name.contains('.'))
        }) else {
            continue;
        };
        if !is_valid_prototype_method(class, method, resolver) {
            continue;
        }
        if is_field {
            own_fields
                .entry(class.id.as_str())
                .or_default()
                .entry((name, method.details["is_static"] == true))
                .or_default()
                .push(method);
        }
        if is_method {
            methods
                .entry((name, method.details["is_static"] == true))
                .or_default()
                .push(method);
        }
    }
    for candidates in methods.values_mut() {
        if candidates.iter().any(|node| node.details["is_stub"] != true) {
            candidates.retain(|node| node.details["is_stub"] != true);
        }
        candidates.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    }
    if let Some(fields) = own_fields.get_mut(class.id.as_str()) {
        for candidates in fields.values_mut() {
            candidates.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        }
    }
}

fn collect_class_bases<'a>(
    class: &'a Node,
    facts: &'a crate::models::Facts,
    resolver: &impl SemanticResolver<'a>,
    implements_targets: &mut HashMap<&'a str, Vec<&'a Node>>,
) -> (Vec<&'a Node>, bool) {
    let mut bases = Vec::new();
    let mut invalid = false;
    for reference in facts.references.iter().filter(|reference| {
        reference.source == class.id && matches!(reference.kind.as_str(), "inherits" | "implements")
    }) {
        match resolver.resolve_symbol(
            class,
            &reference.expression,
            reference.position(),
            SymbolRole::Type,
        ) {
            Symbol::Type(TypeTarget::Local(base))
                if matches!(base.kind.as_str(), "class" | "interface") =>
            {
                if reference.kind == "inherits" {
                    if !bases.iter().any(|node: &&Node| node.id == base.id) {
                        bases.push(base);
                    }
                } else if !implements_targets
                    .get(class.id.as_str())
                    .is_some_and(|list| list.iter().any(|node| node.id == base.id))
                {
                    implements_targets
                        .entry(class.id.as_str())
                        .or_default()
                        .push(base);
                }
            }
            _ if reference.kind == "inherits" => invalid = true,
            _ => {}
        }
    }
    if class.kind == "class" && bases.len() > 1 {
        invalid = true;
    }
    (bases, invalid)
}

fn record_concrete_implementors<'a>(
    class: &'a Node,
    implements_targets: &HashMap<&'a str, Vec<&'a Node>>,
    parents: &HashMap<&'a str, Option<Vec<&'a Node>>>,
    implementors: &mut HashMap<&'a str, Vec<&'a Node>>,
) {
    if class.kind != "class" || class.details["is_abstract"] == true {
        return;
    }
    for base in implements_targets
        .get(class.id.as_str())
        .into_iter()
        .flatten()
        .copied()
        .chain(
            parents
                .get(class.id.as_str())
                .and_then(|o| o.as_ref())
                .into_iter()
                .flatten()
                .copied(),
        )
    {
        implementors.entry(base.id.as_str()).or_default().push(class);
    }
}

fn validate_chain<'a>(
    class: &'a str,
    parents: &HashMap<&'a str, Option<Vec<&'a Node>>>,
    states: &mut HashMap<&'a str, u8>,
    invalid: &mut HashSet<&'a str>,
    depth: usize,
) -> bool {
    if states.get(class) == Some(&2) {
        return !invalid.contains(class);
    }
    if depth >= 128 || states.get(class) == Some(&1) {
        invalid.insert(class);
        return false;
    }
    states.insert(class, 1);
    let valid = match parents.get(class) {
        Some(Some(bases)) => bases
            .iter()
            .all(|base| validate_chain(base.id.as_str(), parents, states, invalid, depth + 1)),
        Some(None) | None => false,
    };
    if !valid {
        invalid.insert(class);
    }
    states.insert(class, 2);
    valid
}

fn inherited<'a>(
    class: &'a Node,
    own: &HashMap<&'a str, MemberMap<'a>>,
    parents: &HashMap<&'a str, Option<Vec<&'a Node>>>,
    completed: &mut HashMap<&'a str, MemberMap<'a>>,
    visiting: &mut HashSet<&'a str>,
    depth: usize,
) -> MemberMap<'a> {
    if let Some(members) = completed.get(class.id.as_str()) {
        return members.clone();
    }
    let direct = own.get(class.id.as_str()).cloned().unwrap_or_default();
    if depth >= 128 || !visiting.insert(class.id.as_str()) {
        return direct;
    }
    let mut result = direct.clone();
    if let Some(Some(bases)) = parents.get(class.id.as_str()) {
        for base in bases {
            if visiting.contains(base.id.as_str()) {
                continue;
            }
            for (key, methods) in inherited(base, own, parents, completed, visiting, depth + 1) {
                if direct.contains_key(&key) {
                    continue;
                }
                let candidates = result.entry(key).or_default();
                for method in methods {
                    if !candidates.iter().any(|node| node.id == method.id) {
                        candidates.push(method);
                    }
                }
                candidates.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            }
        }
    }
    visiting.remove(class.id.as_str());
    completed.insert(class.id.as_str(), result.clone());
    result
}
