//! Strictly read-only [`StateQuery`] handlers operating directly over `dyn StateReader`.
//!
//! Never opens write handles and never creates missing `.sqlite` or `.bak.sqlite` files.

use crate::context::{build_claim_context, ClaimContextRequest};
use crate::machine::compute_schedule;
use crate::observability::render_prometheus_text;
use crate::profile::{GateDef, ResolvedGateProfile};
use crate::spec::{parse_spec, Receipt, SpecDocument, SpecStatus};
use crate::store::traits::StateReader;
use crate::types::{
    ArtifactKind, BlackboardListReq, GateNodeRecord, ListRunsReq, NodeKind, NodeStatus,
    RunGraphSnapshot, RunStatus, SubtaskRecord, ValidatedObservations,
};
use crate::usecases::dto::{
    BlackboardReadReq, BlackboardReadResp, ListReq, ListResp, NextReq, NextResp, ObservabilityReq,
    ObservabilityResp, ShowReq, ShowResp, StateQuery, StateQueryOutput, StatusReq, StatusResp,
};
use crate::usecases::lifecycle::{
    collect_external_satisfied_deps, compute_unintegrated_tasks, ensure_blackboard_enabled,
    ensure_state_enabled, now_utc_iso, parse_cursor_for_snapshot, parse_profile_from_run,
    primary_workspace_root, read_confined_utf8, resolve_node_worktree, resolve_run_optional,
};
use crate::usecases::StateEngineConfig;
use acdd_core::errors::{ErrorCode, StateError};
use acdd_core::ids::{NodeCursor, TreeOid, WorkerId};
use acdd_core::ports::CompositeDomainProvider;

/// Execute a read-only [`StateQuery`] over `reader` without opening any write handles.
pub fn execute_query(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    provider: &CompositeDomainProvider,
    query: StateQuery,
) -> Result<StateQueryOutput, StateError> {
    ensure_state_enabled(config)?;

    match query {
        StateQuery::Status(req) => Ok(StateQueryOutput::Status(Box::new(handle_status(
            config, reader, req,
        )?))),
        StateQuery::List(req) => Ok(StateQueryOutput::List(handle_list(reader, req)?)),
        StateQuery::Show(req) => Ok(StateQueryOutput::Show(Box::new(handle_show(
            config, reader, provider, req,
        )?))),
        StateQuery::Next(req) => Ok(StateQueryOutput::Next(handle_next(config, reader, req)?)),
        StateQuery::BlackboardRead(req) => {
            ensure_blackboard_enabled(config)?;
            Ok(StateQueryOutput::BlackboardRead(handle_blackboard_read(
                config, reader, req,
            )?))
        }
        StateQuery::Observability(req) => Ok(StateQueryOutput::Observability(
            handle_observability(reader, req)?,
        )),
    }
}

fn handle_status(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: StatusReq,
) -> Result<StatusResp, StateError> {
    let run_opt = match resolve_run_optional(config, reader, req.run.as_deref(), None, true) {
        Ok(found) => found,
        Err(err) if req.run.is_some() => return Err(err),
        Err(err) => return Err(err),
    };

    let Some(run) = run_opt else {
        return Ok(StatusResp {
            run: None,
            nodes: Vec::new(),
            ready_nodes: Vec::new(),
            spec_status: None,
        });
    };

    let Some(graph) = reader.load_run_graph(&run.run_id)? else {
        return Ok(StatusResp {
            run: None,
            nodes: Vec::new(),
            ready_nodes: Vec::new(),
            spec_status: None,
        });
    };

    let ext_deps = collect_external_satisfied_deps(config, &run.run_id, &graph.dependencies)?;
    let unintegrated_tasks = compute_unintegrated_tasks(config, &graph)?;
    let obs = ValidatedObservations {
        external_satisfied_deps: ext_deps,
        unintegrated_tasks,
        now_utc: now_utc_iso(),
        ..ValidatedObservations::default()
    };

    let ready_nodes = collect_ready_cursors(&graph, &obs)?;
    let spec_status = read_spec_status_readonly(config, &graph);

    Ok(StatusResp {
        run: Some(graph.run),
        nodes: graph.nodes,
        ready_nodes,
        spec_status,
    })
}

fn handle_list(reader: &dyn StateReader, req: ListReq) -> Result<ListResp, StateError> {
    let page = reader.list_runs(&ListRunsReq {
        workspace: req.workspace,
        status: req.status,
        include_tombstones: req.include_tombstones,
        limit: req.limit.unwrap_or(50).clamp(1, 100),
        offset: req.offset,
        generation: req.generation,
    })?;
    Ok(ListResp { page })
}

fn handle_show(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    provider: &CompositeDomainProvider,
    req: ShowReq,
) -> Result<ShowResp, StateError> {
    let run = resolve_run_optional(config, reader, req.run.as_deref(), None, false)?
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                "no run found matching show request",
            )
        })?;

    let graph = reader.load_run_graph(&run.run_id)?.ok_or_else(|| {
        StateError::new(
            ErrorCode::RunSelectorAmbiguous,
            format!("run '{}' no longer exists in active storage", run.run_id),
        )
    })?;

    let Some(ref raw_node) = req.node else {
        return Ok(ShowResp {
            run: graph.run,
            node: None,
            subtasks: graph.subtasks,
            context_preview: None,
        });
    };

    let cursor = parse_cursor_for_snapshot(&graph, raw_node)?;
    let node_id = cursor.to_node_id()?;
    let node = graph
        .nodes
        .iter()
        .find(|n| n.node_id == node_id)
        .cloned()
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("node '{raw_node}' does not exist in run '{}'", run.run_id),
            )
        })?;

    let subtask_owner_id = match node.kind {
        NodeKind::Task => node.node_id.clone(),
        NodeKind::TaskGate => node.parent_id.clone().unwrap_or_else(|| node.node_id.clone()),
        _ => String::new(),
    };
    let subtasks: Vec<SubtaskRecord> = if subtask_owner_id.is_empty() {
        Vec::new()
    } else {
        graph
            .subtasks
            .iter()
            .filter(|s| s.node_id == subtask_owner_id)
            .cloned()
            .collect()
    };

    let context_preview = if matches!(node.kind, NodeKind::RootGate | NodeKind::TaskGate) {
        build_show_context_preview(config, reader, provider, &graph, &node, &cursor)?
    } else {
        None
    };

    Ok(ShowResp {
        run: graph.run,
        node: Some(node),
        subtasks,
        context_preview,
    })
}

fn build_show_context_preview(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    provider: &CompositeDomainProvider,
    graph: &RunGraphSnapshot,
    node: &GateNodeRecord,
    cursor: &NodeCursor,
) -> Result<Option<serde_json::Value>, StateError> {
    let profile = parse_profile_from_run(&graph.run)?;
    let Some(gate_def) = find_profile_gate_def(&profile, cursor) else {
        return Ok(None);
    };
    let spec = read_spec_readonly(config, &graph.run);
    let worktree = resolve_node_worktree(config, &graph.run, node, None);
    let candidate_tree = node
        .claim_tree
        .clone()
        .or_else(|| node.accepted_tree.clone())
        .unwrap_or_else(|| TreeOid::new("0".repeat(40)).expect("40 zeros valid"));
    let worker_id = node
        .worker_id
        .clone()
        .unwrap_or_else(|| WorkerId::new("preview").expect("valid WorkerId"));

    let bb_page = if config.modules.is_enabled("blackboard") {
        reader.list_blackboard(
            &graph.run.run_id,
            &BlackboardListReq {
                limit: 15,
                offset: 0,
                ..BlackboardListReq::default()
            },
        )?
    } else {
        crate::types::BlackboardPage {
            items: Vec::new(),
            total: 0,
            generation: graph.run.generation,
            limit: 15,
            offset: 0,
        }
    };

    let dependency_receipts = collect_dependency_receipts(graph, node);
    let pre_task_slice = node
        .task_ref
        .as_ref()
        .and_then(|tr| extract_pre_task_slice(graph, tr.as_str()));

    let ctx_req = ClaimContextRequest {
        run: graph.run.clone(),
        node: node.clone(),
        gate: gate_def,
        profile,
        spec,
        claim_revision: node.claim_revision.max(1),
        worker_id,
        worktree,
        candidate_tree,
        unresolved_findings: None,
        failed_trace_log_tail: None,
        pre_task_slice,
        dependency_receipts,
        blackboard_items: bb_page.items,
        response_max_bytes: config.response_policy.max_output_bytes,
    };

    match build_claim_context(&ctx_req, provider) {
        Ok(resp) => Ok(Some(serde_json::to_value(resp).map_err(|err| {
            StateError::new(
                ErrorCode::ProfileSchemaInvalid,
                format!("failed to serialize context preview: {err}"),
            )
        })?)),
        Err(err) if err.message.contains("not implemented yet") => Ok(None),
        Err(err) => Err(err),
    }
}

fn handle_next(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: NextReq,
) -> Result<NextResp, StateError> {
    let run_opt = resolve_run_optional(config, reader, req.run.as_deref(), None, true)?;
    let Some(run) = run_opt else {
        return Ok(NextResp {
            run_id: None,
            isolation: None,
            ready_cursors: Vec::new(),
            waves: Vec::new(),
        });
    };

    let Some(graph) = reader.load_run_graph(&run.run_id)? else {
        return Ok(NextResp {
            run_id: None,
            isolation: None,
            ready_cursors: Vec::new(),
            waves: Vec::new(),
        });
    };

    let ext_deps = collect_external_satisfied_deps(config, &run.run_id, &graph.dependencies)?;
    let unintegrated_tasks = compute_unintegrated_tasks(config, &graph)?;
    let obs = ValidatedObservations {
        external_satisfied_deps: ext_deps,
        unintegrated_tasks,
        now_utc: now_utc_iso(),
        ..ValidatedObservations::default()
    };
    let schedule = compute_schedule(&graph, &obs)?;

    let mut ready_cursors = collect_ready_cursors(&graph, &obs)?;
    if let Some(limit) = req.limit {
        ready_cursors.truncate(limit);
    }
    let waves: Vec<Vec<String>> = schedule
        .waves
        .into_iter()
        .map(|w| w.into_iter().map(|t| t.as_str().to_owned()).collect())
        .collect();

    Ok(NextResp {
        run_id: Some(run.run_id),
        isolation: Some(run.isolation),
        ready_cursors,
        waves,
    })
}

fn handle_blackboard_read(
    config: &StateEngineConfig,
    reader: &dyn StateReader,
    req: BlackboardReadReq,
) -> Result<BlackboardReadResp, StateError> {
    let run = resolve_run_optional(config, reader, req.run.as_deref(), None, false)?
        .ok_or_else(|| {
            StateError::new(
                ErrorCode::RunSelectorAmbiguous,
                "no run found for blackboard read",
            )
        })?;

    let graph_opt = reader.load_run_graph(&run.run_id)?;
    let normalize_cursor = |raw: &str| -> Result<String, StateError> {
        if let Some(ref g) = graph_opt {
            parse_cursor_for_snapshot(g, raw)?.to_node_id().map_err(StateError::from)
        } else {
            raw.parse::<NodeCursor>()
                .map_err(StateError::from)?
                .to_node_id()
                .map_err(StateError::from)
        }
    };

    let node_id = req.node.as_deref().map(normalize_cursor).transpose()?;
    let recipient_node_id = req
        .recipient_node
        .as_deref()
        .map(normalize_cursor)
        .transpose()?;

    let page = reader.list_blackboard(
        &run.run_id,
        &BlackboardListReq {
            node_id,
            recipient_node_id,
            topic: req.topic,
            status: req.status,
            limit: req.limit.unwrap_or(50).clamp(1, 100),
            offset: req.offset,
            generation: req.generation,
        },
    )?;

    Ok(BlackboardReadResp {
        run_id: run.run_id,
        items: page.items,
        total: page.total,
        generation: page.generation,
        limit: page.limit,
        offset: page.offset,
    })
}

fn handle_observability(
    reader: &dyn StateReader,
    req: ObservabilityReq,
) -> Result<ObservabilityResp, StateError> {
    let snapshot = reader.observability_snapshot()?;
    let prometheus_text = if req.include_prometheus {
        Some(render_prometheus_text(&snapshot))
    } else {
        None
    };
    Ok(ObservabilityResp {
        snapshot,
        prometheus_text,
    })
}

fn collect_ready_cursors(
    graph: &RunGraphSnapshot,
    obs: &ValidatedObservations,
) -> Result<Vec<String>, StateError> {
    let schedule = compute_schedule(graph, obs)?;
    let mut out = Vec::new();
    for nid in &schedule.ready_root_nodes {
        out.push(NodeCursor::from_node_id(nid)?.to_string());
    }
    for tref in &schedule.ready_tasks {
        let parent_id = format!("task:{tref}");
        if let Some(gate) = graph
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::TaskGate
                    && n.parent_id.as_deref() == Some(parent_id.as_str())
                    && n.status == NodeStatus::Ready
            })
            .min_by_key(|n| n.seq_order)
        {
            out.push(NodeCursor::from_node_id(&gate.node_id)?.to_string());
        } else {
            out.push(tref.as_str().to_owned());
        }
    }
    Ok(out)
}

fn read_spec_status_readonly(
    config: &StateEngineConfig,
    graph: &RunGraphSnapshot,
) -> Option<SpecStatus> {
    if let Some(doc) = read_spec_readonly(config, &graph.run) {
        return Some(doc.status);
    }
    match graph.run.status {
        RunStatus::Active => Some(SpecStatus::Active),
        RunStatus::Completed => Some(SpecStatus::Completed),
        RunStatus::Cancelled => Some(SpecStatus::Cancelled),
    }
}

/// Read and parse the specification Markdown file for `run` read-only, if present on disk.
pub(crate) fn read_spec_readonly(
    config: &StateEngineConfig,
    run: &crate::types::RunRecord,
) -> Option<SpecDocument> {
    let primary = primary_workspace_root(config);
    let rel = if let Some(ref sp) = run.spec_path {
        sp.clone()
    } else if run.is_ephemeral {
        format!(".acdd/run/{}/spec.md", run.run_id)
    } else {
        return None;
    };
    let raw = read_confined_utf8(&primary, &rel).ok()?;
    parse_spec(&raw).ok()
}

/// Borrow the [`GateDef`] inside `profile` corresponding to `cursor`.
pub(crate) fn find_profile_gate_def_ref<'a>(
    profile: &'a ResolvedGateProfile,
    cursor: &NodeCursor,
) -> Option<&'a GateDef> {
    match cursor {
        NodeCursor::Root(gates) => {
            let first = gates.first()?;
            let mut current = profile
                .gates
                .iter()
                .find(|g| !g.is_tasks_block() && g.id == first.as_str())?;
            for next in &gates[1..] {
                current = current.gates.iter().find(|g| g.id == next.as_str())?;
            }
            Some(current)
        }
        NodeCursor::Task { gates, .. } => {
            let target_gate = gates.last()?;
            for item in &profile.gates {
                if let Some(ref tb) = item.tasks {
                    if let Some(found) = tb.gates.iter().find(|g| g.id == target_gate.as_str()) {
                        return Some(found);
                    }
                }
            }
            None
        }
    }
}

/// Find the [`GateDef`] inside `profile` corresponding to `cursor`.
pub(crate) fn find_profile_gate_def(
    profile: &ResolvedGateProfile,
    cursor: &NodeCursor,
) -> Option<GateDef> {
    find_profile_gate_def_ref(profile, cursor).cloned()
}

/// Collect `Receipt` artifacts from completed intra-run dependency tasks.
pub(crate) fn collect_dependency_receipts(
    graph: &RunGraphSnapshot,
    node: &GateNodeRecord,
) -> Vec<Receipt> {
    let Some(ref tref) = node.task_ref else {
        return Vec::new();
    };
    let dep_refs: Vec<&str> = graph
        .dependencies
        .iter()
        .filter(|d| &d.task_ref == tref && d.depends_on_run_id == graph.run.run_id)
        .map(|d| d.depends_on_ref.as_str())
        .collect();
    if dep_refs.is_empty() {
        return Vec::new();
    }

    let mut receipts = Vec::new();
    for dep_ref in dep_refs {
        let prefix = format!("task:{dep_ref}:");
        for art in &graph.artifacts {
            if art.kind == ArtifactKind::Receipt && art.node_id.starts_with(&prefix) {
                if let Ok(rec) = serde_json::from_str::<Receipt>(&art.payload_json) {
                    receipts.push(rec);
                }
            }
        }
    }
    receipts
}

/// Extract `tasks[<task_ref>]` slice from accepted pre-task root proof/receipt artifacts.
pub(crate) fn extract_pre_task_slice(
    graph: &RunGraphSnapshot,
    task_ref: &str,
) -> Option<serde_json::Value> {
    let min_tasks_seq = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Container && n.node_id.starts_with("tasks#"))
        .map(|n| n.seq_order)
        .min()?;

    let pre_node_ids: std::collections::BTreeSet<&str> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.seq_order < min_tasks_seq
                && matches!(n.kind, NodeKind::RootGate | NodeKind::Container)
                && !n.node_id.starts_with("tasks#")
        })
        .map(|n| n.node_id.as_str())
        .collect();

    for art in &graph.artifacts {
        if pre_node_ids.contains(art.node_id.as_str())
            && matches!(art.kind, ArtifactKind::Proof | ArtifactKind::Receipt)
        {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&art.payload_json) {
                if let Some(slice) = val.get("tasks").and_then(|t| t.get(task_ref)) {
                    return Some(slice.clone());
                }
            }
        }
    }
    None
}
