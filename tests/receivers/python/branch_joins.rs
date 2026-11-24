use super::*;

pub(super) const PERSISTED_FILES: &[(&str, &str)] = &[("branch_join.py", "class Alpha:\n    def execute(self): pass\nclass Beta:\n    def execute(self): pass\ndef run(condition):\n    if condition:\n        receiver = Alpha()\n    else:\n        receiver = Beta()\n    receiver.execute()\n")];

#[test]
fn branch_join_transfer_matrix() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        GuardedPythonBindingsResolveOnlyInsideTheirBranch,
        PythonBranchJoinTransferReachesLaterAliasesAsPossibleCalls,
        PythonBranchJoinIncludesIncomingValuesAndRetainsUnknownPaths,
        PythonBranchJoinEnforcesTheEightCandidateBound,
    }
    for case in [
        Case::GuardedPythonBindingsResolveOnlyInsideTheirBranch,
        Case::PythonBranchJoinTransferReachesLaterAliasesAsPossibleCalls,
        Case::PythonBranchJoinIncludesIncomingValuesAndRetainsUnknownPaths,
        Case::PythonBranchJoinEnforcesTheEightCandidateBound,
    ] {
        match case {
            // guarded_python_bindings_resolve_only_inside_their_branch
            Case::GuardedPythonBindingsResolveOnlyInsideTheirBranch => {
                let source = "class Client:\n    def execute(self): pass\ndef run(value, condition):\n    if condition:\n        value = Client()\n        value.execute()\n    else:\n        value = unknown_factory()\n        value.execute()\n    value.execute()\n";
                let facts = ast::extract("guarded.py", "python", source).unwrap();
                let graph = linker::link(&BTreeMap::from([("guarded.py".to_owned(), facts)]));
                let calls: Vec<_> = graph
                    .coverage
                    .iter()
                    .filter(|coverage| coverage.expression == "value.execute")
                    .collect();
                assert_eq!(calls.len(), 3);
                assert_eq!(
                    calls
                        .iter()
                        .map(|call| (call.line, call.status.as_str()))
                        .collect::<Vec<_>>(),
                    vec![(6, "resolved"), (9, "unresolved"), (10, "unresolved")]
                );
            }
            // python_branch_join_transfer_reaches_later_aliases_as_possible_calls
            Case::PythonBranchJoinTransferReachesLaterAliasesAsPossibleCalls => {
                let source = "class Alpha:\n    def execute(self): pass\nclass Beta:\n    def execute(self): pass\ndef run(condition):\n    if condition:\n        receiver = Alpha()\n    else:\n        receiver = Beta()\n    alias = receiver\n    alias.execute()\n";
                let facts = ast::extract("branch_join_alias.py", "python", source).unwrap();
                let expected_targets: std::collections::BTreeSet<_> = facts
                    .nodes
                    .iter()
                    .filter(|node| node.kind == acdd_index::models::tags::NodeKind::Method && node.name == "execute")
                    .map(|node| node.id.clone())
                    .collect();
                let graph = linker::link(&BTreeMap::from([(
                    "branch_join_alias.py".to_owned(),
                    facts,
                )]));
                let possible_targets: std::collections::BTreeSet<_> = graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "possible_calls" && edge.evidence == "alias.execute"
                    })
                    .map(|edge| edge.dst.clone())
                    .collect();

                assert_eq!(possible_targets, expected_targets);
                assert!(!graph
                    .edges
                    .iter()
                    .any(|edge| edge.kind == "calls" && edge.evidence == "alias.execute"));
                assert_eq!(
                    graph
                        .coverage
                        .iter()
                        .find(|coverage| coverage.expression == "alias.execute")
                        .unwrap()
                        .status,
                    "ambiguous"
                );
            }
            // python_branch_join_includes_incoming_values_and_retains_unknown_paths
            Case::PythonBranchJoinIncludesIncomingValuesAndRetainsUnknownPaths => {
                let source = "class Alpha:\n    def execute(self): pass\nclass Beta:\n    def execute(self): pass\ndef run(condition):\n    receiver = Alpha()\n    if condition:\n        receiver = Beta()\n    receiver.execute()\n    uncertain = Alpha()\n    if condition:\n        uncertain = unknown_factory()\n    uncertain.execute()\n";
                let facts = ast::extract("branch_join_paths.py", "python", source).unwrap();
                let graph = linker::link(&BTreeMap::from([(
                    "branch_join_paths.py".to_owned(),
                    facts,
                )]));

                let mut incoming_targets: Vec<_> = graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "possible_calls" && edge.evidence == "receiver.execute"
                    })
                    .map(|edge| edge.dst.as_str())
                    .collect();
                incoming_targets.sort_unstable();
                assert_eq!(incoming_targets.len(), 2, "{incoming_targets:?}");
                assert!(!graph
                    .edges
                    .iter()
                    .any(|edge| { edge.kind == "calls" && edge.evidence == "receiver.execute" }));

                let unknown_target_count = graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.kind == "possible_calls" && edge.evidence == "uncertain.execute"
                    })
                    .count();
                assert_eq!(unknown_target_count, 1);
                assert!(!graph
                    .edges
                    .iter()
                    .any(|edge| { edge.kind == "calls" && edge.evidence == "uncertain.execute" }));
                let coverage = graph
                    .coverage
                    .iter()
                    .find(|coverage| coverage.expression == "uncertain.execute")
                    .unwrap();
                assert_eq!(coverage.status, "unresolved");
                assert!(
                    coverage.evidence.contains("incomplete"),
                    "{}",
                    coverage.evidence
                );
            }
            // python_branch_join_enforces_the_eight_candidate_bound
            Case::PythonBranchJoinEnforcesTheEightCandidateBound => {
                use std::fmt::Write as _;

                for count in [8, 9] {
                    let mut source = String::new();
                    for index in 0..count {
                        writeln!(source, "class Choice{index}:\n    def execute(self): pass")
                            .unwrap();
                    }
                    source.push_str("def run(choice):\n");
                    for index in 0..count - 1 {
                        if index == 0 {
                            writeln!(source, "    if choice == {index}:").unwrap();
                        } else {
                            writeln!(source, "    elif choice == {index}:").unwrap();
                        }
                        writeln!(source, "        receiver = Choice{index}()").unwrap();
                    }
                    source.push_str("    else:\n");
                    writeln!(source, "        receiver = Choice{}()", count - 1).unwrap();
                    source.push_str("    receiver.execute()\n");

                    let path = format!("branch_bound_{count}.py");
                    let facts = ast::extract(&path, "python", &source).unwrap();
                    let mut expected_targets: Vec<_> = facts
                        .nodes
                        .iter()
                        .filter(|node| node.kind == acdd_index::models::tags::NodeKind::Method && node.name == "execute")
                        .map(|node| node.id.clone())
                        .collect();
                    expected_targets.sort();
                    assert_eq!(expected_targets.len(), count);
                    let graph = linker::link(&BTreeMap::from([(path, facts)]));
                    let possible: Vec<_> = graph
                        .edges
                        .iter()
                        .filter(|edge| {
                            edge.kind == "possible_calls" && edge.evidence == "receiver.execute"
                        })
                        .collect();
                    let calls: Vec<_> = graph
                        .edges
                        .iter()
                        .filter(|edge| edge.kind == "calls" && edge.evidence == "receiver.execute")
                        .collect();
                    assert!(calls.is_empty(), "K={count}: {calls:?}");
                    let coverage = graph
                        .coverage
                        .iter()
                        .find(|coverage| coverage.expression == "receiver.execute")
                        .unwrap();
                    if count == 8 {
                        assert_eq!(possible.len(), 8, "{possible:?}");
                        assert_eq!(
                            possible
                                .iter()
                                .map(|edge| edge.dst.clone())
                                .collect::<Vec<_>>(),
                            expected_targets,
                            "possible target ordering must be stable"
                        );
                        assert_eq!(coverage.status, "ambiguous");
                    } else {
                        assert!(
                            possible.is_empty(),
                            "overflow must not emit a prefix: {possible:?}"
                        );
                        assert_eq!(coverage.status, "unresolved");
                        assert!(
                            coverage.evidence.contains("Unknown/Incomplete")
                                && coverage
                                    .evidence
                                    .contains("no truncated target set emitted"),
                            "{}",
                            coverage.evidence
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn complete_python_branch_join_emits_possible_targets_without_exact_calls() {
    let source = PERSISTED_FILES[0].1;
    let facts = ast::extract("branch_join.py", "python", source).unwrap();
    let mut expected_targets: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == acdd_index::models::tags::NodeKind::Method && node.name == "execute")
        .map(|node| node.id.clone())
        .collect();
    expected_targets.sort();
    assert_eq!(expected_targets.len(), 2);

    let graph = linker::link(&BTreeMap::from([("branch_join.py".to_owned(), facts)]));
    let mut possible_targets: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == "possible_calls" && edge.evidence == "receiver.execute")
        .map(|edge| edge.dst.clone())
        .collect();
    possible_targets.sort();
    assert_eq!(possible_targets, expected_targets);
    assert!(!graph
        .edges
        .iter()
        .any(|edge| { edge.kind == "calls" && edge.evidence == "receiver.execute" }));

    let db = receiver_snapshot();
    commitments::verify(&db).unwrap();
    assert_eq!(
        persisted_edge_provenance(&db, "branch_join.py", "receiver.execute"),
        [
            (
                "possible_calls".to_owned(),
                "receiver.execute".to_owned(),
                "branch_join.Alpha.execute".to_owned(),
                "possible".to_owned(),
            ),
            (
                "possible_calls".to_owned(),
                "receiver.execute".to_owned(),
                "branch_join.Beta.execute".to_owned(),
                "possible".to_owned(),
            ),
        ]
    );
    assert!(!persisted_edge(
        &db,
        "branch_join.py",
        "calls",
        "receiver.execute",
        "branch_join.py",
        "branch_join.Alpha.execute"
    ));
    assert!(!persisted_edge(
        &db,
        "branch_join.py",
        "calls",
        "receiver.execute",
        "branch_join.py",
        "branch_join.Beta.execute"
    ));
}

#[test]
fn union_parameter_types_emit_possible_calls_and_widen_beyond_bound() {
    let too_many_classes: String = (1..=9)
        .map(|i| format!("class C{i}:\n    def execute(self): pass\n"))
        .collect();
    let too_many_union = (1..=9).map(|i| format!("C{i}")).collect::<Vec<_>>().join(" | ");
    let source = format!(
        "from typing import Union\n\
         class Alpha:\n    def execute(self): pass\n\
         class Beta:\n    def execute(self): pass\n\
         {too_many_classes}\
         def use_pipe(x: Alpha | Beta):\n    x.execute()\n\
         def use_union(y: Union[Alpha, Beta]):\n    y.execute()\n\
         def use_wide(z: {too_many_union}):\n    z.execute()\n"
    );
    let facts = ast::extract("unions.py", "python", &source).unwrap();
    let graph = linker::link(&BTreeMap::from([("unions.py".to_owned(), facts)]));
    for (expr, expected_status, expected_possible) in [
        ("x.execute", "ambiguous", vec!["unions.Alpha.execute", "unions.Beta.execute"]),
        ("y.execute", "ambiguous", vec!["unions.Alpha.execute", "unions.Beta.execute"]),
        ("z.execute", "unresolved", vec![]),
    ] {
        let cov = graph
            .coverage
            .iter()
            .find(|c| c.expression == expr)
            .unwrap_or_else(|| panic!("missing coverage for {expr}"));
        assert_eq!(cov.status, expected_status, "{expr}: {cov:#?}");
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.kind == "calls" && e.evidence == expr),
            "union receiver must not emit exact calls: {expr}"
        );
        let mut actual_possible: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.kind == "possible_calls" && e.evidence == expr)
            .map(|e| e.dst.as_str())
            .collect();
        actual_possible.sort();
        let mut expected_ids: Vec<String> = expected_possible
            .iter()
            .map(|q| {
                facts_nodes_id(&source, q)
            })
            .collect();
        expected_ids.sort();
        assert_eq!(actual_possible, expected_ids, "{expr}");
    }
}

fn facts_nodes_id(source: &str, qualname: &str) -> String {
    ast::extract("unions.py", "python", source)
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.qualname == qualname)
        .unwrap_or_else(|| panic!("missing node {qualname}"))
        .id
}
