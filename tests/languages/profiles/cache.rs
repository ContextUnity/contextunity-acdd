use acdd_index::ast;
use acdd_index::languages::{self, LanguageProfile, SyntaxGrammarSpec};
use acdd_index::models::Facts;

static FUNCTION_SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declarations: &[("function_definition", "function")],
    ..SyntaxGrammarSpec::DEFAULT
};
static CLASS_SPEC: SyntaxGrammarSpec = SyntaxGrammarSpec {
    declarations: &[("class_definition", "class")],
    ..SyntaxGrammarSpec::DEFAULT
};

struct Probe(&'static SyntaxGrammarSpec, &'static str);
impl LanguageProfile for Probe {
    fn id(&self) -> &'static str {
        self.1
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["py", "rs"]
    }
    fn grammar(&self, path: &str) -> tree_sitter::Language {
        if path.ends_with(".rs") {
            languages::require("rust").unwrap().grammar(path)
        } else {
            languages::require("python").unwrap().grammar(path)
        }
    }
    fn symbol_kind(&self, _kind: &str) -> Option<&'static str> {
        None
    }
    fn syntax_grammar_spec(&self) -> Option<&'static SyntaxGrammarSpec> {
        Some(self.0)
    }
    fn node_prefix(&self, _kind: &str) -> &'static str {
        "syntax-cache"
    }
    fn normalize_import(&self, _owner: &str, _module: &str) -> Option<languages::ImportPath> {
        None
    }
    fn extract_file(
        &self,
        path: &str,
        source: &str,
        module: &str,
        facts: &mut Facts,
    ) -> anyhow::Result<()> {
        ast::parse::parse_file(self, path, source, module, facts)
    }
}

#[test]
fn syntax_grammar_cache_isolated_by_spec_and_actual_grammar() {
    let source = "def run():\n    pass\n\nclass Thing:\n    pass\n";
    let mut function_facts = Facts::default();
    ast::parse::parse_file(
        &Probe(&FUNCTION_SPEC, "python"),
        "probe.py",
        source,
        "probe",
        &mut function_facts,
    )
    .unwrap();
    assert!(function_facts.nodes.iter().any(
        |node| node.name == "run" && node.kind == acdd_index::models::tags::NodeKind::Function
    ));

    let mut class_facts = Facts::default();
    ast::parse::parse_file(
        &Probe(&CLASS_SPEC, "python"),
        "probe.py",
        source,
        "probe",
        &mut class_facts,
    )
    .unwrap();
    assert!(
        class_facts
            .nodes
            .iter()
            .any(|node| node.name == "Thing"
                && node.kind == acdd_index::models::tags::NodeKind::Class)
    );

    let wrong_grammar = ast::parse::parse_file(
        &Probe(&FUNCTION_SPEC, "rust"),
        "probe.rs",
        "fn run() {}",
        "probe",
        &mut Facts::default(),
    );
    assert!(wrong_grammar.is_err());
    assert!(ast::parse::parse_file(
        &Probe(&FUNCTION_SPEC, "unknown-profile"),
        "probe.py",
        source,
        "probe",
        &mut Facts::default(),
    )
    .is_err());
}
