use crate::models::{Node, Reference};
use crate::languages::profile::{LanguageMemberIndex, LanguageMemberResult};
use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

#[derive(Clone, Debug)]
pub(crate) enum Base<'a> {
    Local(&'a str),
    External(String, &'a str, usize),
    Unknown,
}

impl PartialEq for Base<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(left), Self::Local(right)) => left == right,
            (Self::External(left, ..), Self::External(right, ..)) => left == right,
            (Self::Unknown, Self::Unknown) => true,
            _ => false,
        }
    }
}
impl Eq for Base<'_> {}

#[derive(Clone, Copy)]
pub(crate) enum PythonReceiverMember<'a> {
    Local(&'a Node),
    External(
        &'a str,
        usize,
        Option<&'a str>,
        Option<(
            &'a str,
            &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        )>,
    ),
    Ambiguous,
    Unknown,
}

fn is_stub_or_overload(node: &Node) -> bool {
    node.details["is_stub"].as_bool().unwrap_or(false)
        || node.details["is_overload"].as_bool().unwrap_or(false)
}

/// Precomputes Python C3 order and member lookup before generic reference linking.
pub(crate) struct PythonReceivers<'a> {
    members: HashMap<&'a str, HashMap<&'a str, PythonReceiverMember<'a>>>,
    supers: HashMap<&'a str, HashMap<&'a str, PythonReceiverMember<'a>>>,
    implementors: HashMap<&'a str, Vec<&'a Node>>,
}

fn find_enclosing_python_class<'a>(
    class_names: &HashMap<(&str, &str), Vec<&'a Node>>,
    path: &str,
    qualname: &str,
    child: &Node,
) -> Option<&'a Node> {
    let candidates = class_names.get(&(path, qualname))?;
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    if let Some(enclosing) = candidates
        .iter()
        .find(|class| class.line <= child.line && child.end_line <= class.end_line)
    {
        return Some(*enclosing);
    }
    let child_is_stub = is_stub_or_overload(child);
    if let Some(matching) = candidates
        .iter()
        .find(|class| is_stub_or_overload(class) == child_is_stub)
    {
        return Some(*matching);
    }
    candidates.first().copied()
}

fn collect_python_mutation_owners<'a>(
    all: &'a dyn crate::FactsLookup,
    class_names: &HashMap<(&str, &str), Vec<&'a Node>>,
) -> (HashMap<&'a str, &'a Node>, HashSet<(&'a str, &'a str)>) {
    let mut mutating_members: HashMap<&str, Vec<&str>> = HashMap::new();
    for facts in all.values_facts() {
        for reference in facts.references.iter().filter(|r| r.kind == "mutates") {
            if let Some(("self" | "cls", member)) = reference.expression.split_once('.') {
                mutating_members
                    .entry(reference.source.as_str())
                    .or_default()
                    .push(member);
            }
        }
    }
    let mut mutation_owners = HashMap::new();
    let mut assigned_members = HashSet::new();
    if mutating_members.is_empty() {
        return (mutation_owners, assigned_members);
    }
    for facts in all.values_facts() {
        for node in &facts.nodes {
            let Some(members) = mutating_members.get(node.id.as_str()) else {
                continue;
            };
            let Some((mut scope, _)) = node.qualname.rsplit_once('.') else {
                continue;
            };
            while !scope.is_empty() {
                if let Some(class) = find_enclosing_python_class(class_names, &node.path, scope, node)
                {
                    mutation_owners.insert(node.id.as_str(), class);
                    assigned_members
                        .extend(members.iter().map(|member| (class.id.as_str(), *member)));
                    break;
                }
                scope = scope.rsplit_once('.').map_or("", |(parent, _)| parent);
            }
        }
    }
    (mutation_owners, assigned_members)
}

fn insert_python_class_node_member<'a>(
    members: &mut HashMap<&'a str, PythonReceiverMember<'a>>,
    node: &'a Node,
) {
    let callable = matches!(node.kind.as_str(), "function" | "method");
    let conditional = callable && node.details["is_conditional"].as_bool().unwrap_or(false);
    let node_is_stub = is_stub_or_overload(node);
    match members.get(node.name.as_str()) {
        None => {
            members.insert(
                node.name.as_str(),
                if conditional {
                    PythonReceiverMember::Unknown
                } else {
                    PythonReceiverMember::Local(node)
                },
            );
        }
        Some(PythonReceiverMember::Local(existing)) => {
            let existing_is_stub = is_stub_or_overload(existing);
            if existing_is_stub && !node_is_stub {
                members.insert(node.name.as_str(), PythonReceiverMember::Local(node));
            } else if !existing_is_stub && node_is_stub {
                // keep existing non-stub
            } else if existing_is_stub && node_is_stub {
                // both are stubs/overloads; keep existing stub so runtime implementation can replace it
            } else if conditional || !callable {
                members.insert(node.name.as_str(), PythonReceiverMember::Ambiguous);
            } else {
                members.insert(node.name.as_str(), PythonReceiverMember::Local(node));
            }
        }
        Some(PythonReceiverMember::Ambiguous | PythonReceiverMember::Unknown)
            if callable && !node_is_stub =>
        {
            members.insert(
                node.name.as_str(),
                if conditional {
                    PythonReceiverMember::Ambiguous
                } else {
                    PythonReceiverMember::Local(node)
                },
            );
        }
        Some(PythonReceiverMember::Ambiguous | PythonReceiverMember::Unknown)
        | Some(PythonReceiverMember::External(..)) => {}
    }
}

impl<'a> PythonReceivers<'a> {
    pub(crate) fn build(
        all: &'a dyn crate::FactsLookup,
        mut resolve: impl FnMut(&'a Node, &'a Reference) -> Base<'a>,
        mut framework_receiver: impl FnMut(
            &str,
            &str,
        ) -> Option<(
            &'a str,
            &'a str,
            &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        )>,
    ) -> Self {
        let classes: HashMap<&str, &Node> = all
            .values_facts()
            .flat_map(|facts| &facts.nodes)
            .filter(|node| node.language == "python" && node.kind == "class")
            .map(|node| (node.id.as_str(), node))
            .collect();
        if classes.is_empty() {
            return Self {
                members: HashMap::new(),
                supers: HashMap::new(),
                implementors: HashMap::new(),
            };
        }
        let mut bases: HashMap<&str, Vec<Base<'a>>> = HashMap::new();
        let mut own: HashMap<&str, HashMap<&str, PythonReceiverMember<'a>>> = HashMap::new();
        let mut class_names: HashMap<(&str, &str), Vec<&'a Node>> = HashMap::new();
        for class in classes.values() {
            class_names
                .entry((class.path.as_str(), class.qualname.as_str()))
                .or_default()
                .push(*class);
        }
        let (mutation_owners, assigned_members) =
            collect_python_mutation_owners(all, &class_names);
        for facts in all.values_facts() {
            let assigned_fields: HashSet<&str> = facts
                .edges
                .iter()
                .filter(|edge| edge.kind == "mutates")
                .map(|edge| edge.dst.as_str())
                .collect();
            for node in &facts.nodes {
                let owning_class = node
                    .qualname
                    .rsplit_once('.')
                    .and_then(|(parent, _)| {
                        find_enclosing_python_class(&class_names, &node.path, parent, node)
                    });
                // PythonReceiverMember reads create synthetic field nodes; they do not override methods.
                if node.kind == "field"
                    && !assigned_fields.contains(node.id.as_str())
                    && !owning_class.is_some_and(|class| {
                        assigned_members.contains(&(class.id.as_str(), node.name.as_str()))
                    })
                {
                    continue;
                }
                if let Some(class) = owning_class {
                    insert_python_class_node_member(own.entry(class.id.as_str()).or_default(), node);
                }
            }
            for reference in &facts.references {
                if reference.kind == "inherits" {
                    if let Some(class) = classes.get(reference.source.as_str()) {
                        bases
                            .entry(class.id.as_str())
                            .or_default()
                            .push(resolve(class, reference));
                    }
                }
                if reference.kind == "mutates" {
                    if let Some(("self" | "cls", member)) = reference.expression.split_once('.') {
                        if let Some(class) = mutation_owners.get(reference.source.as_str()) {
                            let members = own.entry(class.id.as_str()).or_default();
                            // A field's identity remains known after assignment even though its
                            // runtime value is not. Call resolution treats field members as
                            // unknown callables, while reference resolution can still link the
                            // exact field declaration.
                            if !matches!(members.get(member), Some(PythonReceiverMember::Local(node)) if node.kind == "field")
                            {
                                members.insert(member, PythonReceiverMember::Unknown);
                            }
                        }
                    }
                }
            }
        }
        for class in classes.values() {
            for binding in class.binding_names() {
                let member = own
                    .entry(class.id.as_str())
                    .or_default()
                    .entry(binding)
                    .or_insert(PythonReceiverMember::Unknown);
                if !matches!(member, PythonReceiverMember::Local(node) if node.kind == "field") {
                    *member = PythonReceiverMember::Unknown;
                }
            }
        }
        let mut class_ids: Vec<&'a str> = classes.keys().copied().collect();
        class_ids.sort_unstable();
        let orders = class_ids
            .par_iter()
            .map(|&id| {
                (
                    id,
                    linearize(id, &bases, &mut HashMap::new(), &mut HashSet::new()),
                )
            })
            .collect();
        Self {
            members: own,
            supers: HashMap::new(),
            implementors: HashMap::new(),
        }
        .finish(&classes, &class_ids, &orders, &mut framework_receiver)
    }

    fn inherit_members_for_order(
        order: &[Base<'a>],
        class_path: &str,
        super_call: bool,
        own: &HashMap<&'a str, HashMap<&'a str, PythonReceiverMember<'a>>>,
        framework_receiver: &mut impl FnMut(
            &str,
            &str,
        ) -> Option<(
            &'a str,
            &'a str,
            &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        )>,
    ) -> HashMap<&'a str, PythonReceiverMember<'a>> {
        let mut inherited = HashMap::new();
        for base in order.iter().skip(usize::from(super_call)) {
            match base {
                Base::Local(base_id) => {
                    if let Some(members) = own.get(base_id) {
                        for (name, member) in members {
                            inherited.entry(*name).or_insert(*member);
                        }
                    }
                }
                Base::External(key, origin, line) => {
                    if let Some((provider, pack, rule)) = framework_receiver(class_path, key) {
                        for name in &rule.members {
                            if name
                                .split_once('.')
                                .is_some_and(|(head, _)| inherited.contains_key(head))
                            {
                                continue;
                            }
                            inherited
                                .entry(name.as_str())
                                .or_insert(PythonReceiverMember::External(
                                    origin,
                                    *line,
                                    Some(provider),
                                    Some((pack, rule)),
                                ));
                        }
                    }
                    // An unindexed base can override any later base's members.
                    break;
                }
                Base::Unknown => break,
            }
        }
        inherited
    }

    fn finish(
        mut self,
        classes: &HashMap<&'a str, &'a Node>,
        class_ids: &[&'a str],
        orders: &HashMap<&'a str, Option<Vec<Base<'a>>>>,
        framework_receiver: &mut impl FnMut(
            &str,
            &str,
        ) -> Option<(
            &'a str,
            &'a str,
            &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        )>,
    ) -> Self {
        let own = std::mem::take(&mut self.members);
        for &id in class_ids {
            let Some(Some(order)) = orders.get(id) else {
                if let Some(members) = own.get(id) {
                    self.members.insert(id, members.clone());
                }
                continue;
            };
            let class_node = classes[id];
            if !is_stub_or_overload(class_node)
                && !has_protocol_or_abc_base(class_node)
            {
                for base in order.iter().skip(1) {
                    if let Base::Local(base_id) = base {
                        self.implementors.entry(base_id).or_default().push(class_node);
                    }
                }
            }
            for super_call in [false, true] {
                let inherited = Self::inherit_members_for_order(
                    order,
                    classes[id].path.as_str(),
                    super_call,
                    &own,
                    framework_receiver,
                );
                if super_call {
                    self.supers.insert(id, inherited);
                } else {
                    self.members.insert(id, inherited);
                }
            }
        }
        for list in self.implementors.values_mut() {
            list.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            list.dedup_by(|a, b| a.id == b.id);
        }
        self
    }

    pub(crate) fn implementors(&self, base_id: &str) -> &[&'a Node] {
        self.implementors.get(base_id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn lookup(&self, class: &Node, member: &str, super_call: bool) -> PythonReceiverMember<'a> {
        let map = if super_call {
            &self.supers
        } else {
            &self.members
        };
        if let Some((head, _)) = member.split_once('.') {
            if map
                .get(class.id.as_str())
                .is_some_and(|members| members.contains_key(head))
            {
                return PythonReceiverMember::Unknown;
            }
        }
        if let Some(result) = map
            .get(class.id.as_str())
            .and_then(|members| members.get(member))
        {
            return *result;
        }
        PythonReceiverMember::Unknown
    }

    pub(crate) fn return_policy(
        &self,
        class: &Node,
        member: &str,
    ) -> Option<(
        &'a str,
        &'a crate::languages::manifests::ClassHierarchyReceiverRule,
        &'a crate::languages::manifests::ReturnPolicy,
    )> {
        let PythonReceiverMember::External(_, _, _, Some((pack, rule))) = self.lookup(class, member, false)
        else {
            return None;
        };
        Some((pack, rule, rule.returns.get(member)?))
    }
}

impl LanguageMemberIndex for PythonReceivers<'_> {
    fn language_ids(&self) -> &'static [&'static str] {
        &["python"]
    }

    fn lookup_member<'a>(
        &'a self,
        receiver: &Node,
        member: &str,
        super_call: bool,
        _caller: &Node,
    ) -> LanguageMemberResult<'a> {
        match self.lookup(receiver, member, super_call) {
            PythonReceiverMember::Local(node) => LanguageMemberResult::Local(node),
            PythonReceiverMember::External(module, import_line, provider, hierarchy) => {
                LanguageMemberResult::External { module, import_line, provider, hierarchy }
            }
            PythonReceiverMember::Ambiguous => LanguageMemberResult::Ambiguous,
            PythonReceiverMember::Unknown => LanguageMemberResult::Unknown,
        }
    }

    fn implementors(&self, base: &Node) -> Option<&[&Node]> {
        Some(PythonReceivers::implementors(self, base.id.as_str()))
    }

    fn return_policy(
        &self,
        receiver: &Node,
        member: &str,
    ) -> Option<(&str, &crate::languages::manifests::ClassHierarchyReceiverRule, &crate::languages::manifests::ReturnPolicy)> {
        PythonReceivers::return_policy(self, receiver, member)
    }
}

pub(in crate::linker) fn has_protocol_or_abc_base(node: &Node) -> bool {
    node.details
        .get("bases")
        .and_then(|bases| bases.as_str())
        .is_some_and(|bases| bases.contains("Protocol") || bases.contains("ABC"))
}

fn linearize<'a>(
    id: &'a str,
    bases: &HashMap<&'a str, Vec<Base<'a>>>,
    cache: &mut HashMap<&'a str, Option<Vec<Base<'a>>>>,
    active: &mut HashSet<&'a str>,
) -> Option<Vec<Base<'a>>> {
    if let Some(order) = cache.get(id) {
        return order.clone();
    }
    if !active.insert(id) {
        return None;
    }
    let direct = bases.get(id).cloned().unwrap_or_default();
    let mut sequences = Vec::new();
    let mut valid = true;
    for base in &direct {
        match base {
            Base::Local(base_id) => match linearize(base_id, bases, cache, active) {
                Some(order) => sequences.push(order),
                None => valid = false,
            },
            Base::External(..) | Base::Unknown => sequences.push(vec![base.clone()]),
        }
    }
    sequences.push(direct);
    let mut order = vec![Base::Local(id)];
    while valid && sequences.iter().any(|sequence| !sequence.is_empty()) {
        let head = sequences
            .iter()
            .filter_map(|sequence| sequence.first())
            .find(|head| {
                !sequences
                    .iter()
                    .any(|sequence| sequence.iter().skip(1).any(|item| item == *head))
            })
            .cloned();
        let Some(head) = head else {
            valid = false;
            break;
        };
        order.push(head.clone());
        for sequence in &mut sequences {
            if sequence.first() == Some(&head) {
                sequence.remove(0);
            }
        }
    }
    active.remove(id);
    let result = valid.then_some(order);
    cache.insert(id, result.clone());
    result
}
