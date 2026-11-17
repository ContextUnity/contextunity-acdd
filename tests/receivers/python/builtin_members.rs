use super::*;

#[test]
fn builtin_member_provenance_matrix() {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        BuiltinTypeMembersAreFiniteAndTypeSpecific,
        PythonBuiltinTypeMembersRequireUnshadowedTypeNames,
        PythonNamespaceBuiltinsRequireUnshadowedNames,
    }
    struct SourceCase {
        kind: Case,
        path: &'static str,
        source: &'static str,
    }
    for row in [
        // builtin_type_members_are_finite_and_type_specific
        SourceCase { kind: Case::BuiltinTypeMembersAreFiniteAndTypeSpecific, path: "builtins.py", source: "def use(text: str, items: list, mapping: dict):\n    text.strip()\n    items.append(1)\n    mapping.get('key')\n    text.append(1)\n    converted = str(unknown())\n    converted.strip()\n" },
        // python_builtin_type_members_require_unshadowed_type_names
        SourceCase { kind: Case::PythonBuiltinTypeMembersRequireUnshadowedTypeNames, path: "builtin_type_members.py", source: "def standard():\n    dict.fromkeys(('a',))\n    str.maketrans('a', 'b')\ndef shadow(dict, str):\n    dict.fromkeys(('a',))\n    str.maketrans('a', 'b')\n" },
        // python_namespace_builtins_require_unshadowed_names
        SourceCase { kind: Case::PythonNamespaceBuiltinsRequireUnshadowedNames, path: "namespace_builtins.py", source: "def standard():\n    globals()\n    locals()\n    input()\ndef shadow(globals, locals, input):\n    globals()\n    locals()\n    input()\n" },
    ] {
        let source = row.source;
        let facts = ast::extract(row.path, "python", source).unwrap();
        if matches!(row.kind, Case::BuiltinTypeMembersAreFiniteAndTypeSpecific) {
let profile = acdd_index::languages::by_id("python").unwrap();
    for (receiver, member) in [
        ("str", "strip"),
        ("list", "append"),
        ("dict", "get"),
        ("set", "add"),
        ("tuple", "count"),
        ("int", "bit_length"),
        ("float", "hex"),
    ] {
        assert!(profile.builtin_member(receiver, member));
    }
    for (receiver, member) in [
        ("str", "append"),
        ("dict", "add"),
        ("Unknown", "strip"),
        ("list", "invented_method"),
    ] {
        assert!(!profile.builtin_member(receiver, member));
    }
        }
        let graph = linker::link(&BTreeMap::from([(row.path.to_owned(), facts)]));
        match row.kind {
            Case::BuiltinTypeMembersAreFiniteAndTypeSpecific => {
for expression in [
        "text.strip",
        "items.append",
        "mapping.get",
        "converted.strip",
    ] {
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == expression)
            .unwrap();
        assert_eq!(coverage.status, "external", "{expression}: {coverage:#?}");
        assert!(
            coverage.evidence.contains("Python standard library"),
            "{expression}: {coverage:#?}"
        );
    }
    assert_eq!(
        graph
            .coverage
            .iter()
            .find(|coverage| coverage.expression == "text.append")
            .unwrap()
            .status,
        "unresolved"
    );
            }
            Case::PythonBuiltinTypeMembersRequireUnshadowedTypeNames => {
for (line, expression, expected) in [
        (2, "dict.fromkeys", "external"),
        (3, "str.maketrans", "external"),
        (5, "dict.fromkeys", "unresolved"),
        (6, "str.maketrans", "unresolved"),
    ] {
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.line == line && coverage.expression == expression)
            .unwrap();
        assert_eq!(coverage.status, expected, "{expression} at {line}");
        if expected == "external" {
            assert!(
                coverage
                    .evidence
                    .contains("Python standard library built-in"),
                "{}",
                coverage.evidence
            );
        }
    }
            }
            Case::PythonNamespaceBuiltinsRequireUnshadowedNames => {
for (line, expression, expected) in [
        (2, "globals", "external"),
        (3, "locals", "external"),
        (4, "input", "external"),
        (6, "globals", "unresolved"),
        (7, "locals", "unresolved"),
        (8, "input", "unresolved"),
    ] {
        let coverage = graph
            .coverage
            .iter()
            .find(|coverage| coverage.line == line && coverage.expression == expression)
            .unwrap();
        assert_eq!(coverage.status, expected, "{expression} at {line}");
        if expected == "external" {
            assert!(
                coverage
                    .evidence
                    .contains("Python standard library built-in"),
                "{}",
                coverage.evidence
            );
        }
    }
            }
        }
    }
}
