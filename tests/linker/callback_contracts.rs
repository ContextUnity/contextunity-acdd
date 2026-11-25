use acdd_index::models::{Facts, Node};
use acdd_index::semantic::{
    CallableParameter, CallableSignature, SourcePosition, TypeExpr, ValueExpr, ValueFlowFacts,
};
use acdd_index::typed_facts::{decode_facts, encode_facts, TypedFacts};
use std::collections::BTreeSet;

#[test]
fn callback_param_callable_signature_and_compact_graph_contracts() {
    let sig = CallableSignature {
        has_receiver: true,
        params: vec![CallableParameter {
            name: Some("cb".into()),
            type_expr: TypeExpr::Named {
                name: "FnMut".into(),
            },
            callback_params: vec![TypeExpr::Named {
                name: "Item".into(),
            }],
        }],
        return_type: Some(TypeExpr::Named {
            name: "bool".into(),
        }),
    };
    assert!(!sig.is_empty());
    assert!(sig.valid());

    let callback_expr = ValueExpr::CallbackParam {
        callee: Box::new(ValueExpr::Call {
            callee: "items.iter().map".into(),
            site: None,
        }),
        arg_index: 0,
        param_index: 0,
    };
    assert!(callback_expr.valid_call_sites(0));

    let mut flow = ValueFlowFacts::default();
    flow.bindings.push(acdd_index::semantic::BindingFact {
        name: "item".into(),
        position: SourcePosition { line: 4, column: 8 },
        value: callback_expr,
        conditional: false,
    });
    flow.callable_signature = Some(sig.clone());

    let mut typed_keys = BTreeSet::new();
    flow.reference_keys(|k| {
        typed_keys.insert(k.to_owned());
    });
    let serialized = serde_json::to_value(&flow).unwrap();
    let mut raw_keys = BTreeSet::new();
    ValueFlowFacts::reference_keys_from_value(&serialized, |k| {
        raw_keys.insert(k.to_owned());
    });
    assert_eq!(typed_keys, raw_keys);
    assert!(typed_keys.contains("Item"));
    assert!(typed_keys.contains("items.iter().map"));

    flow.offset_lines(10);
    assert_eq!(flow.bindings[0].position.line, 14);

    let node = Node {
        id: "fn:src/lib.rs:1:apply".into(),
        kind: acdd_index::models::tags::NodeKind::Function,
        name: "apply".into(),
        qualname: "lib.apply".into(),
        path: "src/lib.rs".into(),
        line: 1,
        end_line: 3,
        is_test: false,
        language: acdd_index::models::tags::LanguageId::Rust,
        generated: false,
        details: serde_json::Value::Null,
        native_semantic: None,
    };
    let mut typed = TypedFacts::from_public(Facts {
        nodes: vec![node],
        ..Default::default()
    });
    typed.flows.insert("fn:src/lib.rs:1:apply".into(), flow);

    let blob = encode_facts(&typed).unwrap();
    let reopened = decode_facts(&blob).unwrap();
    let reopened_flow = reopened.flows.get(&reopened.nodes[0]).unwrap();
    assert_eq!(reopened_flow.callable_signature.as_ref(), Some(&sig));
}

#[test]
fn rust_callable_signature_and_callback_param_extraction_matrix() {
    let source = r#"
pub struct Item;
impl Item {
    pub fn mark(&mut self) -> bool { true }
}

pub fn apply_fn_ptr(item: &Item, f: fn(&Item) -> bool) -> bool {
    f(item)
}

pub fn apply_impl_fn_mut(item: &mut Item, mut cb: impl FnMut(&mut Item, usize) -> bool) -> bool {
    cb(item, 1)
}

pub fn apply_boxed_dyn(item: Item, cb: Box<dyn FnOnce(Item) -> bool>) -> bool {
    cb(item)
}

pub struct Runner;
impl Runner {
    pub fn run_where<F>(&self, item: &Item, mut f: F) -> bool
    where
        F: FnMut(&Item) -> bool,
    {
        f(item)
    }

    pub fn run_inline_bound<G: FnOnce(Item) -> bool>(&self, item: Item, g: G) -> bool {
        g(item)
    }
}

pub fn drive(r: &Runner, mut item: Item) {
    let _ = apply_fn_ptr(&item, |x| x.mark());
    let _ = apply_impl_fn_mut(&mut item, |mut_item, _idx| mut_item.mark());
    let _ = apply_boxed_dyn(Item, Box::new(|owned| owned.mark()));
    let _ = r.run_where(&item, |elem| elem.mark());
    let _ = r.run_inline_bound(item, |owned| owned.mark());
}
"#;
    let typed = acdd_index::ast::extract_typed("src/lib.rs", "rust", source)
        .expect("extract_typed should parse rust callback matrix");

    for (fn_name, expected_has_receiver, expected_param_names, expected_cb_len) in [
        ("apply_fn_ptr", false, vec!["Item"], 1),
        ("apply_impl_fn_mut", false, vec!["Item", "usize"], 2),
        ("apply_boxed_dyn", false, vec!["Item"], 1),
        ("run_where", true, vec!["Item"], 1),
        ("run_inline_bound", true, vec!["Item"], 1),
    ] {
        let node = typed
            .facts
            .nodes
            .iter()
            .find(|n| matches!(n.kind.as_str(), "function" | "method") && n.name == fn_name)
            .unwrap_or_else(|| panic!("missing function/method node {fn_name}"));
        let flow = typed
            .flows
            .get(node)
            .unwrap_or_else(|| panic!("missing value_flow for {fn_name}"));
        let sig = flow
            .callable_signature
            .as_ref()
            .unwrap_or_else(|| panic!("missing callable_signature for {fn_name}"));
        assert_eq!(sig.has_receiver, expected_has_receiver, "fn={fn_name}");
        let cb_param = sig
            .params
            .last()
            .unwrap_or_else(|| panic!("missing callback param for {fn_name}"));
        assert_eq!(
            cb_param.callback_params.len(),
            expected_cb_len,
            "fn={fn_name} params={cb_param:?}"
        );
        for expected_ty in expected_param_names {
            assert!(
                format!("{:?}", cb_param.callback_params).contains(expected_ty),
                "fn={fn_name} missing callback param type {expected_ty}: {:?}",
                cb_param.callback_params
            );
        }
    }

    let drive_node = typed
        .facts
        .nodes
        .iter()
        .find(|n| n.kind == acdd_index::models::tags::NodeKind::Function && n.name == "drive")
        .expect("missing drive node");
    let drive_flow = typed
        .flows
        .get(drive_node)
        .expect("missing drive value_flow");
    fn unwrap_scoped(v: &ValueExpr) -> &ValueExpr {
        match v {
            ValueExpr::Scoped { value, .. } => value.as_ref(),
            other => other,
        }
    }
    for (binding_name, expected_callee, expected_arg_idx, expected_param_idx) in [
        ("x", "apply_fn_ptr", 1, 0),
        ("mut_item", "apply_impl_fn_mut", 1, 0),
        ("_idx", "apply_impl_fn_mut", 1, 1),
        ("elem", "run_where", 1, 0),
        ("owned", "run_inline_bound", 1, 0),
    ] {
        assert!(
            drive_flow.bindings.iter().any(|b| {
                b.name == binding_name
                    && matches!(
                        unwrap_scoped(&b.value),
                        ValueExpr::CallbackParam { callee, arg_index, param_index }
                        if *arg_index == expected_arg_idx
                            && *param_index == expected_param_idx
                            && matches!(callee.as_ref(), ValueExpr::Call { callee: name, .. } if name == expected_callee)
                    )
            }),
            "missing CallbackParam binding {binding_name}({expected_arg_idx},{expected_param_idx}) on {expected_callee} in {:?}",
            drive_flow.bindings
        );
    }
}

#[test]
fn rust_dynamic_callback_and_zero_false_shadowing_matrix() {
    let workspace = crate::common::Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"cb_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    workspace.write(
        "src/lib.rs",
        r#"
pub struct Item;
impl Item {
    pub fn check(&self) -> bool { true }
}

pub fn invoke_callback(item: &Item, cb: impl Fn(&Item) -> bool) -> bool {
    cb(item)
}

pub fn destructured_no_shadowing(
    left: Option<Item>,
    right: Option<Item>,
    nested: Option<(Item, Item)>,
) -> bool {
    let (Some(a), Some(b)) = (left, right) else {
        return false;
    };
    let Some((x, y)) = nested else {
        return false;
    };
    a.check() && b.check() && x.check() && y.check()
}
"#,
    );
    workspace.build();
    let conn = workspace.open();

    let analysis = acdd_index::db::reader::analyze_paged(
        &conn,
        "src/lib.rs",
        None,
        &acdd_core::response::QueryOptions {
            limit: 50,
            offset: 0,
            detail: acdd_core::response::Detail::Full,
            generation: None,
        },
    )
    .expect("analyze_paged should succeed");

    let unresolved_samples = analysis["resolution"]["items"]
        .as_array()
        .expect("resolution.items should be an array");
    assert!(
        unresolved_samples
            .iter()
            .any(|row| row["expression"] == "cb" && row["reason"] == "dynamic_callback"),
        "expected dynamic_callback diagnostic for cb(item), got: {unresolved_samples:?}"
    );
    assert!(
        !unresolved_samples.iter().any(|row| {
            matches!(
                row["expression"].as_str(),
                Some("a.check" | "b.check" | "x.check" | "y.check")
            )
        }),
        "destructured let-else locals should never produce false-shadowing diagnostics: {unresolved_samples:?}"
    );

    for expr in ["a.check", "b.check", "x.check", "y.check"] {
        let status: String = conn
            .query_row(
                "SELECT r.status FROM resolution_coverage r
                 JOIN path_dictionary p ON p.path_id = r.path_id
                 JOIN coverage_expressions x ON x.expression_id = r.expression_id
                 WHERE p.path = 'src/lib.rs' AND x.expression = ?1
                 LIMIT 1",
                [expr],
                |row| row.get(0),
            )
            .unwrap_or_else(|e| panic!("missing coverage row for {expr}: {e}"));
        assert_eq!(status, "resolved", "expr={expr}");
    }
}

#[cfg(all(
    feature = "lang-rust",
    feature = "lang-typescript",
    feature = "lang-python"
))]
#[test]
fn typed_callback_param_resolution_across_languages_matrix() {
    let workspace = crate::common::Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"cb_resolution\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    workspace.write(
        "src/lib.rs",
        r#"
pub struct Item;
impl Item {
    pub fn mark(&self) -> bool { true }
}

pub struct Runner;
impl Runner {
    pub fn run_where<F: FnMut(&Item) -> bool>(&self, item: &Item, mut f: F) -> bool {
        f(item)
    }
}

pub fn apply_fn_ptr(item: &Item, f: fn(&Item) -> bool) -> bool {
    f(item)
}

pub fn drive(r: &Runner, item: Item, items: Vec<Item>, opt: Option<Item>) {
    let _ = apply_fn_ptr(&item, |x| x.mark());
    let _ = r.run_where(&item, |elem| elem.mark());
    items.iter().for_each(|iter_item| {
        let _ = iter_item.mark();
    });
    let _ = opt.map(|opt_item| opt_item.mark());
}
"#,
    );
    workspace.write(
        "callbacks.ts",
        r#"
class Service {
    work(): void {}
}

function apply(cb: (item: Service) => void): void {
    cb(new Service());
}

function driveTs(items: Service[]): void {
    apply((ts_item) => ts_item.work());
    items.forEach((arr_item) => arr_item.work());
}
"#,
    );
    workspace.write(
        "callbacks.py",
        r#"
from typing import Callable

class PyService:
    def work(self):
        pass

def apply_py(cb: Callable[[PyService], PyService]) -> PyService:
    return cb(PyService())

def drive_py():
    apply_py(lambda py_item: py_item.work())
"#,
    );
    workspace.build();
    let conn = workspace.open();

    for (path, expr, expected_status) in [
        ("src/lib.rs", "x.mark", "resolved"),
        ("src/lib.rs", "elem.mark", "resolved"),
        ("src/lib.rs", "iter_item.mark", "resolved"),
        ("src/lib.rs", "opt_item.mark", "resolved"),
        ("callbacks.ts", "ts_item.work", "resolved"),
        ("callbacks.ts", "arr_item.work", "resolved"),
        ("callbacks.py", "cb", "resolved"),
        ("callbacks.py", "py_item.work", "resolved"),
    ] {
        let (status, evidence): (String, String) = conn
            .query_row(
                "SELECT r.status, e.evidence FROM resolution_coverage r
                 JOIN path_dictionary p ON p.path_id = r.path_id
                 JOIN coverage_expressions x ON x.expression_id = r.expression_id
                 JOIN coverage_evidence e ON e.evidence_id = r.evidence_id
                 WHERE p.path = ?1 AND x.expression = ?2
                 LIMIT 1",
                [path, expr],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("missing coverage for {path} `{expr}`: {e}"));
        assert_eq!(status, expected_status, "{path} `{expr}` ({evidence})");
    }
}
