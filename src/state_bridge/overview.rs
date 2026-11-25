use acdd_core::response::QueryOptions;
use acdd_core::subsystems::SubsystemId;
use acdd_index::{
    db::reader::{self, OverviewAspect},
    scanner::{self, Adapter},
};
use acdd_state::types::RunStatus;
use acdd_state::usecases::{ListReq, StateQuery, StateQueryOutput};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// Composes indexed, scanner-owned, and state-owned workspace evidence.
///
/// This is an adapter-level composition point: the database reader stays read-only
/// and never invokes scanners or mutating state commands.
pub(crate) fn overview(
    conn: &Connection,
    root: &Path,
    adapter: Option<&Adapter>,
    aspects: Option<&[String]>,
    path: Option<&str>,
    page: &QueryOptions,
) -> Result<Value> {
    if let Some(scoped) = path.map(str::trim).filter(|p| !p.is_empty()) {
        let mut result = reader::overview_for_path(conn, scoped, page)?;
        result["workspace_root"] = json!(root.display().to_string());
        return Ok(result);
    }
    let mut result =
        reader::overview_with_options(conn, &reader::OverviewOptions { aspects, page })?;
    if !includes_discovery(aspects) {
        return Ok(result);
    }

    let generation = result["generation"].as_str().unwrap_or_default().to_owned();
    let mut discovery = serde_json::Map::new();
    discovery.insert("workspace_root".into(), json!(root.display().to_string()));

    let manifests = match adapter {
        Some(adapter) => scanner::discovery::manifest_components(root, adapter, page, &generation)
            .unwrap_or_else(|error| unavailable("manifest_components", &error.to_string())),
        None => unavailable("manifest_components", "workspace adapter is unavailable"),
    };
    discovery.insert("manifest_components".into(), manifests);

    let hubs = if adapter.is_some() {
        reader::documentation_hubs(conn, page)?
    } else {
        unavailable("documentation_hubs", "workspace adapter is unavailable")
    };
    discovery.insert("documentation_hubs".into(), hubs);
    discovery.insert("active_runs".into(), active_runs(root, adapter, page));

    result["workspace_root"] = json!(root.display().to_string());
    result["discovery"] = Value::Object(discovery);
    Ok(result)
}

fn includes_discovery(aspects: Option<&[String]>) -> bool {
    match aspects {
        None | Some([]) => true,
        Some(aspects) => aspects.iter().any(|aspect| {
            aspect
                .parse::<OverviewAspect>()
                .is_ok_and(|a| a.is_discovery() || a == OverviewAspect::Components)
        }),
    }
}

fn unavailable(field: &str, cause: &str) -> Value {
    json!({"availability":"unavailable","cause":cause,"items":[],"field":field})
}

fn active_runs(root: &Path, adapter: Option<&Adapter>, page: &QueryOptions) -> Value {
    let owned_adapter;
    let cfg = match adapter {
        Some(a) => a,
        None => match scanner::load_adapter(root, None) {
            Ok(a) => {
                owned_adapter = a;
                &owned_adapter
            }
            Err(_) => {
                return unavailable(
                    "active_runs",
                    "workspace adapter unavailable for state query",
                );
            }
        },
    };
    if !cfg.modules.is_subsystem_enabled(SubsystemId::State) {
        return unavailable("active_runs", "state module is disabled in acdd.yaml");
    }
    let query = StateQuery::List(ListReq {
        workspace: None,
        status: Some(RunStatus::Active),
        include_tombstones: false,
        limit: Some(page.limit.clamp(1, 100)),
        offset: page.offset,
        generation: None,
    });
    let Ok(providers) = super::common::build_composite_providers(
        cfg,
        vec![
            std::sync::Arc::new(super::code_domain::CodeDomainProvider::new(
                root.to_path_buf(),
                cfg.clone(),
            )),
            std::sync::Arc::new(super::docs_domain::DocsDomainProvider::new(
                root.to_path_buf(),
                cfg.clone(),
            )),
        ],
    ) else {
        return unavailable("active_runs", "domain providers unavailable");
    };
    match super::common::execute_state_query(root, root, None, None, cfg, providers, query) {
        Ok(StateQueryOutput::List(resp)) => {
            let items: Vec<Value> = resp
                .page
                .runs
                .iter()
                .map(|run| {
                    json!({
                        "run_id": run.run_id.as_str(),
                        "run_name": run.run_name.as_str(),
                        "collection": run.collection,
                        "profile_id": run.profile_id,
                        "isolation": run.isolation.as_str(),
                        "spec_path": run.spec_path,
                        "status": run.status.as_str(),
                        "worktree": run.worktree,
                        "created_at": run.created_at,
                    })
                })
                .collect();
            let returned = items.len();
            let total = resp.page.total_runs;
            json!({
                "availability": "available",
                "total": total,
                "offset": resp.page.offset,
                "limit": resp.page.limit,
                "items": items,
                "omitted": total.saturating_sub(resp.page.offset.saturating_add(returned)),
                "continuation_available": resp.page.offset.saturating_add(returned) < total,
                "generation": Value::Null,
            })
        }
        Ok(_) => unavailable("active_runs", "unexpected state query response"),
        Err(err) => unavailable("active_runs", &err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::includes_discovery;

    #[test]
    fn overview_includes_discovery_aspect_matrix() {
        assert!(includes_discovery(None));
        assert!(includes_discovery(Some(&["COMPONENTS".into()])));
        assert!(includes_discovery(Some(&["milestones".into()])));
        assert!(!includes_discovery(Some(&[
            "counts".into(),
            "cycles".into()
        ])));
    }
}
