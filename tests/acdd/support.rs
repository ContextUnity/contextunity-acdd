use acdd_core::config::default_specs;
use acdd_core::response::ResponsePolicy;
use acdd_core::subsystems::SubsystemRegistry;
use acdd_core::ports::{
    ActionCapability, CheckCapability, CompositeDomainProvider, DomainActionReq,
    DomainActionResult, DomainCapabilities, DomainCheckOutcome, DomainCheckReq, DomainContextReq,
    DomainProvider, DomainToolDef, DomainToolOutput, DomainToolReq,
};
use acdd_core::schema::{AdditionalPropertiesDef, SchemaDef};
use acdd_state::usecases::StateEngineConfig;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

pub(crate) fn base_engine_config(workspace_root: &Path) -> StateEngineConfig {
    StateEngineConfig {
        workspace_root: workspace_root.to_path_buf(),
        worktree: workspace_root.to_path_buf(),
        state_db_override: None,
        code_db_path: None,
        spec_collections: default_specs(),
        profiles_dirs: vec![".acdd/profiles".to_owned()],
        linked_workspaces: BTreeMap::new(),
        modules: SubsystemRegistry::default(),
        response_policy: ResponsePolicy::default(),
        retain_traces: 20,
        max_snapshot_file_bytes: None,
        allowed_binary_extensions: None,
    }
}

pub(crate) fn git_cmd(repo: &Path, args: &[&str]) -> String {
    let out = acdd_core::git::git_command(repo, args)
        .unwrap_or_else(|e| panic!("git {:?} failed in {}: {e}", args, repo.display()));
    assert!(
        out.status.success(),
        "git {:?} failed in {}: {}",
        args,
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

pub(crate) fn init_git_repo_on_branch(repo: &Path, branch: &str) {
    let _ = git_cmd(repo, &["init", "-b", branch]);
    let _ = git_cmd(repo, &["config", "user.name", "ACDD Test"]);
    let _ = git_cmd(repo, &["config", "user.email", "test@acdd.local"]);
    std::fs::write(repo.join("README.md"), "# ACDD Fixture Repo\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn base() -> i32 { 1 }\n").unwrap();
    let _ = git_cmd(repo, &["add", "."]);
    let _ = git_cmd(repo, &["commit", "-m", "initial commit"]);
}

pub(crate) struct TestProvider {
    id: String,
    caps: DomainCapabilities,
}

impl TestProvider {
    pub(crate) fn full_composite() -> Arc<CompositeDomainProvider> {
        let code = Self {
            id: "code".to_owned(),
            caps: DomainCapabilities {
                actions: vec![],
                checks: vec![
                    CheckCapability {
                        name: "seam".to_owned(),
                        input_schema: SchemaDef::object(),
                    },
                    CheckCapability {
                        name: "signature_scope".to_owned(),
                        input_schema: SchemaDef::object(),
                    },
                    CheckCapability {
                        name: "lint".to_owned(),
                        input_schema: acdd_core::lint::lint_check_input_schema(),
                    },
                ],
                context_lenses: BTreeSet::from([
                    "symbols".to_owned(),
                    "tests".to_owned(),
                    "external_callers".to_owned(),
                ]),
                tools: vec![],
            },
        };
        let docs = Self {
            id: "docs".to_owned(),
            caps: DomainCapabilities {
                actions: vec![],
                checks: vec![
                    CheckCapability {
                        name: "links".to_owned(),
                        input_schema: SchemaDef::object(),
                    },
                    CheckCapability {
                        name: "specs".to_owned(),
                        input_schema: SchemaDef::object(),
                    },
                ],
                context_lenses: BTreeSet::from([
                    "adrs".to_owned(),
                    "invariants".to_owned(),
                    "instructions".to_owned(),
                    "workspace_config".to_owned(),
                ]),
                tools: vec![],
            },
        };
        let mut url_schema = SchemaDef::object();
        url_schema
            .properties
            .insert("url".to_owned(), SchemaDef::string());
        url_schema.required = vec!["url".to_owned()];
        url_schema.additional_properties = Some(AdditionalPropertiesDef::Boolean(false));

        let http = Self {
            id: "http".to_owned(),
            caps: DomainCapabilities {
                actions: vec![ActionCapability {
                    name: "probe".to_owned(),
                    input_schema: url_schema.clone(),
                    mutating: false,
                    supports_result_lookup: false,
                }],
                checks: vec![CheckCapability {
                    name: "health".to_owned(),
                    input_schema: SchemaDef::object(),
                }],
                context_lenses: BTreeSet::from(["endpoints".to_owned()]),
                tools: vec![DomainToolDef {
                    name: "ping".to_owned(),
                    description: "Ping endpoint".to_owned(),
                    input_schema: url_schema,
                    read_only: true,
                }],
            },
        };
        Arc::new(
            CompositeDomainProvider::new(vec![Arc::new(code), Arc::new(docs), Arc::new(http)])
                .expect("composite provider valid"),
        )
    }
}

impl DomainProvider for TestProvider {
    fn domain_id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> DomainCapabilities {
        self.caps.clone()
    }

    fn execute_action(
        &self,
        _action: &str,
        _params: &Value,
        _idempotency_key: &str,
        _req: &DomainActionReq,
    ) -> anyhow::Result<DomainActionResult> {
        Ok(DomainActionResult {
            summary: "ok".to_owned(),
            output: json!({"ok": true}),
        })
    }

    fn run_check(
        &self,
        _check: &str,
        _params: &Value,
        _req: &DomainCheckReq,
    ) -> anyhow::Result<DomainCheckOutcome> {
        Ok(DomainCheckOutcome::Passed {
            summary: "check passed".to_owned(),
            execution_digest_salt: Some("salt-v1".to_owned()),
        })
    }

    fn provide_context(&self, lens: &str, _req: &DomainContextReq) -> anyhow::Result<Value> {
        if lens == "workspace_config" {
            Ok(json!({
                "lens": lens,
                "docs_roots": ["docs"],
                "available_profiles": ["spec_planning"],
            }))
        } else {
            Ok(json!({"lens": lens, "items": []}))
        }
    }

    fn call_tool(
        &self,
        _tool: &str,
        args: &Value,
        _req: &DomainToolReq,
    ) -> anyhow::Result<DomainToolOutput> {
        Ok(DomainToolOutput::Json(json!({"pong": args})))
    }
}
