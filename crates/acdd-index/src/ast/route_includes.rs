use crate::languages::{routes::prefixed_path, types::workspace_path};
use crate::models::{Edge, Node};
use crate::typed_facts::TypedFacts;
use serde_json::json;
use crate::models::details::NodeDetailKey;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn materialize_composed_route(
    all: &BTreeMap<String, TypedFacts>,
    owner_path: &str,
    include: &Node,
    child: &Node,
    route_path: &str,
    id: &str,
    ancestors: Option<&HashSet<String>>,
) -> Node {
    let method = child.detail_str(NodeDetailKey::METHOD).unwrap_or("ANY");
    let name = format!("{method} {route_path}");
    let mut route = child.clone();
    let source_path = child.detail_str(NodeDetailKey::SOURCE_PATH)
        .unwrap_or(child.path.as_str())
        .to_owned();
    let source_line = child
        .detail_u64(NodeDetailKey::SOURCE_LINE)
        .unwrap_or(child.line as u64);
    let source_id = child.detail_str(NodeDetailKey::SOURCE_ID)
        .unwrap_or(child.id.as_str())
        .to_owned();
    let origin = all
        .get(&source_path)
        .filter(|_| workspace_path(&source_path).0 == workspace_path(owner_path).0)
        .and_then(|source_facts| {
            source_facts.facts.nodes.iter().find(|node| {
                node.id == source_id && node.kind == "route" && node.path == source_path
            })
        });
    let source_qualname = origin.map(|node| node.qualname.clone());
    route.id = id.to_owned();
    route.name = name.clone();
    route.path = owner_path.to_owned();
    route.line = include.line;
    route.end_line = include.end_line;
    let parent_module = all
        .get(owner_path)
        .and_then(|typed| typed.facts.nodes.iter().find(|node| node.kind == "module"))
        .map_or(owner_path, |module| module.qualname.as_str());
    route.qualname = format!("{parent_module}.{name}");
    route.set_detail(NodeDetailKey::ROUTE_PATH, json!(route_path));
    route.set_detail(NodeDetailKey::NAME, json!(name));
    route.set_detail(NodeDetailKey::SOURCE_PATH, json!(source_path));
    route.set_detail(NodeDetailKey::SOURCE_LINE, json!(source_line));
    route.set_detail(NodeDetailKey::SOURCE_ID, json!(source_id));
    route.set_detail(
        NodeDetailKey::SOURCE_QUALNAME,
        source_qualname.map_or(serde_json::Value::Null, |qualname| json!(qualname)),
    );
    route.set_detail(NodeDetailKey::INCLUDED_FROM, json!(include.id));
    route.set_detail(
        NodeDetailKey::INCLUDE_DEPTH,
        json!(include.detail_u64(NodeDetailKey::INCLUDE_DEPTH).unwrap_or(0)
            .max(child.detail_u64(NodeDetailKey::INCLUDE_DEPTH).unwrap_or(0))
            + 1),
    );
    let mut namespaces = Vec::new();
    for candidate in [
        include.detail_str(NodeDetailKey::ROUTE_NAMESPACE),
        include.detail_str(NodeDetailKey::INCLUDE_NAMESPACE),
        child.detail_str(NodeDetailKey::ROUTE_NAMESPACE),
    ]
    .into_iter()
    .flatten()
    {
        namespaces.extend(candidate.split(':').filter(|part| !part.is_empty()));
    }
    route.set_detail(
        NodeDetailKey::ROUTE_NAMESPACE,
        if namespaces.is_empty() {
            serde_json::Value::Null
        } else {
            json!(namespaces.join(":"))
        },
    );
    let _ = ancestors;
    route
}

fn apply_composed_route_addition(
    all: &mut BTreeMap<String, TypedFacts>,
    changed_paths: &mut BTreeSet<String>,
    owner_path: String,
    route: Node,
    include: Node,
    child: Node,
) {
    let source_path = route.detail_str(NodeDetailKey::SOURCE_PATH)
        .unwrap_or(child.path.as_str())
        .to_owned();
    let source_id = route.detail_str(NodeDetailKey::SOURCE_ID)
        .unwrap_or(child.id.as_str())
        .to_owned();
    let child_edges = all
        .get(&child.path)
        .map(|source_facts| {
            source_facts
                .facts
                .edges
                .iter()
                .filter(|edge| edge.src == child.id && edge.kind != "contains")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source_references = all
        .get(&source_path)
        .filter(|_| workspace_path(&source_path).0 == workspace_path(&owner_path).0)
        .filter(|source_facts| {
            source_facts.facts.nodes.iter().any(|node| {
                node.id == source_id && node.kind == "route" && node.path == source_path
            })
        })
        .map(|source_facts| {
            source_facts
                .facts
                .references
                .iter()
                .filter(|reference| reference.source == source_id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(owner) = all.get_mut(&owner_path) else {
        return;
    };
    owner.facts.nodes.push(route.clone());
    owner.facts.edges.push(Edge {
        src: format!("module:{owner_path}"),
        dst: route.id.clone(),
        kind: "contains".into(),
        path: owner_path.clone(),
        line: include.line,
        evidence: "composed configured route include".into(),
        confidence: "exact".into(),
    });
    for mut edge in child_edges {
        edge.src = route.id.clone();
        edge.path = owner_path.clone();
        edge.line = include.line;
        owner.facts.edges.push(edge);
    }
    changed_paths.insert(owner_path);
    if let Some(origin) = all.get_mut(&source_path) {
        for mut reference in source_references {
            reference.source = route.id.clone();
            origin.facts.references.push(reference);
        }
        changed_paths.insert(source_path);
    }
}

/// Materializes configured include prefixes from extracted route facts without rereading source.
pub fn compose_route_includes(all: &mut BTreeMap<String, TypedFacts>) -> BTreeSet<String> {
    const MAX_INCLUDE_DEPTH: usize = 16;
    const MAX_COMPOSED_ROUTES: usize = 10_000;

    let mut removed: HashSet<String> = all
        .values()
        .flat_map(|typed| {
            typed
                .facts
                .nodes
                .iter()
                .filter(|node| node.detail_str(NodeDetailKey::INCLUDED_FROM).is_some())
                .map(|node| node.id.clone())
        })
        .collect();
    // Generated handles live in the original route's facts bucket, so a delta may
    // load those references without loading their old parent route owner. Capture
    // those foreign source IDs before removing generated nodes, then rebuild them
    // from the canonical source route below.
    removed.extend(
        all.values()
            .flat_map(|typed| typed.facts.references.iter())
            .filter(|reference| reference.source.contains(":included:"))
            .map(|reference| reference.source.clone()),
    );
    let mut changed_paths = BTreeSet::new();
    for (path, typed) in all.iter_mut() {
        let reference_count = typed.facts.references.len();
        let had_composed_routes = typed
            .facts
            .nodes
            .iter()
            .any(|node| node.detail_str(NodeDetailKey::INCLUDED_FROM).is_some());
        typed
            .facts
            .nodes
            .retain(|node| !removed.contains(node.id.as_str()));
        typed.facts.edges.retain(|edge| {
            !removed.contains(edge.src.as_str()) && !removed.contains(edge.dst.as_str())
        });
        typed
            .facts
            .references
            .retain(|reference| !removed.contains(reference.source.as_str()));
        if had_composed_routes || typed.facts.references.len() != reference_count {
            changed_paths.insert(path.clone());
        }
    }

    let module_paths: HashMap<(String, String), String> = all
        .iter()
        .flat_map(|(path, typed)| {
            let workspace = workspace_path(path).0.to_owned();
            typed
                .facts
                .nodes
                .iter()
                .filter(|node| node.kind == "module" && node.language == "python")
                .map(move |node| ((workspace.clone(), node.qualname.clone()), path.clone()))
        })
        .collect();

    let mut processed = HashSet::new();
    let mut route_ancestry: HashMap<String, HashSet<String>> = HashMap::new();
    let mut composed_count = 0;
    loop {
        let include_nodes: Vec<(String, Node)> = all
            .iter()
            .flat_map(|(path, typed)| {
                typed
                    .facts
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.kind == "route"
                            && node.detail_str(NodeDetailKey::INCLUDE_MODULE).is_some()
                            && node.detail_u64(NodeDetailKey::INCLUDE_DEPTH).unwrap_or(0)
                                < MAX_INCLUDE_DEPTH as u64
                    })
                    .map(move |node| (path.clone(), node.clone()))
            })
            .collect();
        let mut additions = Vec::new();
        for (owner_path, include) in include_nodes {
            let Some(module) = include.detail_str(NodeDetailKey::INCLUDE_MODULE) else {
                continue;
            };
            let workspace = workspace_path(&owner_path).0.to_owned();
            let Some(child_path) = module_paths.get(&(workspace, module.to_owned())) else {
                continue;
            };
            let Some(include_path) = include.detail_str(NodeDetailKey::ROUTE_PATH) else {
                continue;
            };
            let child_routes: Vec<Node> = all
                .get(child_path)
                .into_iter()
                .flat_map(|typed| typed.facts.nodes.iter())
                .filter(|node| {
                    node.kind == "route" && node.detail_str(NodeDetailKey::INCLUDE_MODULE).is_none()
                })
                .cloned()
                .collect();
            for child in child_routes {
                let ancestors = route_ancestry.get(&child.id);
                if ancestors.is_some_and(|ancestors| {
                    ancestors.len() >= MAX_INCLUDE_DEPTH || ancestors.contains(&include.id)
                }) {
                    continue;
                }
                if !processed.insert((include.id.clone(), child.id.clone())) {
                    continue;
                }
                if composed_count >= MAX_COMPOSED_ROUTES {
                    break;
                }
                let Some(child_path_value) = child.detail_str(NodeDetailKey::ROUTE_PATH) else {
                    continue;
                };
                let id = format!("route:{owner_path}:included:{}:{}", include.id, child.id);
                if all
                    .get(&owner_path)
                    .is_some_and(|typed| typed.facts.nodes.iter().any(|node| node.id == id))
                {
                    continue;
                }
                let route_path = prefixed_path(Some(include_path), child_path_value);
                let route = materialize_composed_route(
                    all,
                    &owner_path,
                    &include,
                    &child,
                    &route_path,
                    &id,
                    ancestors,
                );
                let mut ancestry = ancestors.cloned().unwrap_or_default();
                ancestry.insert(include.id.clone());
                route_ancestry.insert(id, ancestry);
                additions.push((owner_path.clone(), route, include.clone(), child));
                composed_count += 1;
            }
        }
        if additions.is_empty() {
            break;
        }
        for (owner_path, route, include, child) in additions {
            apply_composed_route_addition(all, &mut changed_paths, owner_path, route, include, child);
        }
    }
    changed_paths
}
