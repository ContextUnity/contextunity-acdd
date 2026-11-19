//! Pure Markdown presentation for task lifecycle responses.

use super::inline_code;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Whether a public task/state operation uses the lifecycle presentation.
pub(super) fn handles_title(title: &str) -> bool {
    matches!(
        title,
        "acdd_state"
            | "acdd_claim"
            | "acdd_verify"
            | "acdd_submit"
            | "task_list"
            | "task_claim"
            | "task_submit"
            | "task_manage"
    )
}

/// Renders one task/state response without changing its JSON domain value.
pub(super) fn render(value: &Value, title: &str) -> String {
    match title {
        "acdd_claim" | "task_claim" => render_context(value, "Gate claim"),
        "acdd_verify" => render_record(value, "acdd_verify", "Gate verification"),
        "acdd_submit" | "task_submit" => render_record(value, title, "Gate submission"),
        "task_list" => render_task_list(value),
        "task_manage" => render_manage(value),
        _ => render_record(value, title, title),
    }
}

/// Renders one blocker using the embedded task-context presentation contract.
///
/// Coordination uses this same pure helper for standalone blackboard messages.
pub(crate) fn render_blocker_callout(message: &Value) -> String {
    let id = message
        .get("id")
        .map(scalar_text)
        .unwrap_or_else(|| "unknown".into());
    let author = message
        .get("author")
        .map(scalar_text)
        .unwrap_or_else(|| "unknown".into());
    let mut output = format!("> [!CAUTION] BLOCKER (#{id}) by {author}\n");
    quote_field(&mut output, "Message id", message.get("id"));
    let scope = message
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if message.get("subtask_ref").and_then(Value::as_str).is_some() {
                "subtask"
            } else if message.get("node_id").and_then(Value::as_str).is_some()
                || message.get("task_id").and_then(Value::as_str).is_some()
            {
                "node"
            } else if message.get("run_id").and_then(Value::as_str).is_some()
                || message
                    .get("milestone_ref")
                    .and_then(Value::as_str)
                    .is_some()
            {
                "run"
            } else {
                "unknown"
            }
        });
    output.push_str("> Scope: ");
    output.push_str(&inline_code(scope));
    output.push('\n');
    quote_field(&mut output, "Run id", message.get("run_id"));
    quote_field(&mut output, "Node id", message.get("node_id"));
    quote_field(&mut output, "Recipient node", message.get("recipient_node_id"));
    quote_field(&mut output, "Milestone ref", message.get("milestone_ref"));
    quote_field(&mut output, "Task id", message.get("task_id"));
    quote_field(&mut output, "Subtask ref", message.get("subtask_ref"));
    quote_field(&mut output, "Gate", message.get("gate"));
    quote_field(&mut output, "Timestamp", message.get("created_at"));

    if let Some(payload) = message.get("payload").and_then(Value::as_str) {
        append_quoted_body(&mut output, payload);
        if summary_is_truncated(payload) {
            append_recovery_hint(&mut output, message);
        }
    } else {
        output.push_str("> Body not included in this message summary.\n");
        append_recovery_hint(&mut output, message);
    }
    output.push('\n');
    output
}

fn render_manage(value: &Value) -> String {
    if value.get("spec").is_some() {
        return render_record(value, "task_manage", "Task management result");
    }
    if value.get("context_bundle").is_some_and(Value::is_object)
        || (value.get("workflow_guidance").is_some_and(Value::is_object)
            && value.get("task_id").is_some()
            && value.get("allowed_write_scope").is_some())
    {
        return render_context(value, "Task context");
    }
    if value.get("tasks").and_then(Value::as_array).is_some() {
        return render_task_list(value);
    }
    render_record(value, "task_manage", "Task management result")
}

fn render_task_list(value: &Value) -> String {
    let mut output = String::from("# Task list\n");
    let mut state = RenderState::default();
    let Some(object) = value.as_object() else {
        render_field(&mut output, "result", value, 0, &mut state);
        return output;
    };
    if let Some(resolution) = object.get("resolution") {
        output.push_str(&super::coordination::render_resolution_notice(resolution));
    }

    if let Some(tasks) = object.get("tasks").and_then(Value::as_array) {
        output.push_str("\n## Tasks\n");
        if tasks.is_empty() {
            output.push_str("\nNo tasks were returned for this selection.\n");
        } else {
            for task in tasks {
                output.push('\n');
                render_task_row(&mut output, task, &mut state);
            }
        }
    }
    if let Some(workspaces) = object.get("workspaces").and_then(Value::as_object) {
        output.push_str("\n## Workspaces\n");
        for (name, workspace) in ordered_fields(workspaces) {
            output.push_str("\n### ");
            output.push_str(&inline_code(name));
            output.push('\n');
            render_field(&mut output, "workspace", workspace, 0, &mut state);
        }
    }

    let remaining = fields_without(object, &["tasks", "workspaces", "resolution"]);
    if !remaining.is_empty() {
        output.push_str("\n## Listing details\n");
        render_fields(&mut output, &remaining, 0, &mut state);
    }
    output
}

fn render_task_row(output: &mut String, task: &Value, state: &mut RenderState) {
    let Some(object) = task.as_object() else {
        render_field(output, "task", task, 0, state);
        return;
    };
    let id = object
        .get("task_id")
        .map(scalar_text)
        .unwrap_or_else(|| "Task".into());
    output.push_str("### ");
    output.push_str(&inline_code(&id));
    output.push('\n');

    const ROW_FIELDS: &[&str] = &[
        "task_id",
        "target",
        "status",
        "stage",
        "owner",
        "agent_type",
        "rev",
        "depends_on",
        "subtasks",
    ];
    for key in ROW_FIELDS {
        if let Some(value) = object.get(*key) {
            match *key {
                "status" => render_status_checkbox(output, "Task status", value, 0),
                "subtasks" => render_subtask_rows(output, value, state),
                _ => render_field(output, key, value, 0, state),
            }
        }
    }
    let remaining = fields_without(object, ROW_FIELDS);
    if !remaining.is_empty() {
        output.push_str("- **Additional task fields**\n");
        render_fields(output, &remaining, 1, state);
    }
}

fn render_status_checkbox(output: &mut String, label: &str, value: &Value, depth: usize) {
    let status = scalar_text(value);
    bullet_prefix(output, depth);
    output.push('[');
    output.push_str(if status == "completed" { "x" } else { " " });
    output.push_str("] **");
    output.push_str(label);
    output.push_str("**: ");
    output.push_str(&inline_code(&status));
    output.push('\n');
}

fn render_subtask_rows(output: &mut String, value: &Value, state: &mut RenderState) {
    let Some(subtasks) = value.as_array() else {
        render_field(output, "subtasks", value, 0, state);
        return;
    };
    bullet_prefix(output, 0);
    output.push_str("**Subtasks**\n");
    for subtask in subtasks {
        let Some(fields) = subtask.as_object() else {
            render_field(output, "subtask", subtask, 1, state);
            continue;
        };
        let reference = fields
            .get("subtask_ref")
            .map(scalar_text)
            .unwrap_or_else(|| "subtask".into());
        let status = fields.get("status").cloned().unwrap_or(Value::Null);
        bullet_prefix(output, 1);
        output.push('[');
        output.push_str(if status.as_str() == Some("completed") {
            "x"
        } else {
            " "
        });
        output.push_str("] **Subtask**: ");
        output.push_str(&inline_code(&reference));
        if !status.is_null() {
            output.push_str("; **Status**: ");
            output.push_str(&inline_code(&scalar_text(&status)));
        }
        output.push('\n');
        let details = fields_without(fields, &["subtask_ref", "status"]);
        render_fields(output, &details, 2, state);
    }
}

fn render_context(value: &Value, heading: &str) -> String {
    let mut output = format!("# {heading}\n");
    let Some(object) = value.as_object() else {
        let mut state = RenderState::default();
        render_field(&mut output, "result", value, 0, &mut state);
        return output;
    };
    let mut state = RenderState::default();

    render_lifecycle_state(&mut output, object);
    let bundle = object.get("context_bundle").and_then(Value::as_object);
    let contract = bundle
        .and_then(|bundle| bundle.get("contract"))
        .and_then(Value::as_object);
    if let Some(contract) = contract {
        output.push_str("\n## Contract\n");
        let contract_identity_fields = [
            "task_id",
            "stage",
            "status",
            "claim_revision",
            "contract_revision",
            "worker_id",
            "worktree",
        ]
        .into_iter()
        .filter(|field| object.get(*field) == contract.get(*field))
        .collect::<Vec<_>>();
        let contract_fields = fields_without(contract, &contract_identity_fields);
        render_fields(&mut output, &contract_fields, 0, &mut state);
    } else {
        let contract_fields = root_contract_fields(object);
        if !contract_fields.is_empty() {
            output.push_str("\n## Contract\n");
            render_fields(&mut output, &contract_fields, 0, &mut state);
        }
    }

    let guidance_sources = [
        object.get("workflow_guidance"),
        bundle.and_then(|bundle| bundle.get("guidance")),
    ];
    let mut rendered_guidance = Vec::<&Value>::new();
    for source in guidance_sources.into_iter().flatten() {
        let Some(guidance) = source.as_object() else {
            continue;
        };
        if rendered_guidance.contains(&source) {
            continue;
        }
        let heading = if rendered_guidance.is_empty() {
            "Stage guidance"
        } else {
            "Additional stage guidance"
        };
        rendered_guidance.push(source);
        output.push_str(&format!("\n## {heading}\n"));
        let mut guidance_skip = vec!["blackboard_messages"];
        if guidance.get("actionable_steps") == guidance.get("steps") {
            guidance_skip.push("actionable_steps");
        }
        let guidance_fields = fields_without(guidance, &guidance_skip);
        render_fields(&mut output, &guidance_fields, 0, &mut state);
    }

    if let Some(bundle) = bundle {
        let extra = fields_without(
            bundle,
            &["contract", "guidance", "blackboard", "blackboard_info"],
        );
        if !extra.is_empty() {
            output.push_str("\n## Scope and proof context\n");
            render_fields(&mut output, &extra, 0, &mut state);
        }
    }

    let blackboard_sources = [
        object.get("blackboard"),
        bundle.and_then(|bundle| bundle.get("blackboard")),
        object
            .get("workflow_guidance")
            .and_then(|guidance| guidance.get("blackboard_messages")),
        bundle
            .and_then(|bundle| bundle.get("guidance"))
            .and_then(|guidance| guidance.get("blackboard_messages")),
    ];
    let mut rendered_blackboards = Vec::<&Value>::new();
    for source in blackboard_sources.into_iter().flatten() {
        let Some(messages) = source.as_array() else {
            continue;
        };
        if rendered_blackboards.contains(&source) {
            continue;
        }
        rendered_blackboards.push(source);
        render_blackboard(&mut output, messages, &mut state);
    }

    let blackboard_infos = [
        object.get("blackboard_info"),
        bundle.and_then(|bundle| bundle.get("blackboard_info")),
    ];
    let mut rendered_infos = Vec::<&Value>::new();
    for info in blackboard_infos.into_iter().flatten() {
        if rendered_infos.contains(&info) {
            continue;
        }
        rendered_infos.push(info);
        output.push_str("\n## Blackboard summary\n");
        render_field(&mut output, "blackboard_info", info, 0, &mut state);
    }

    let mut skip = vec![
        "task_id",
        "stage",
        "status",
        "claim_revision",
        "contract_revision",
        "worker_id",
        "worktree",
        "digest",
        "milestone_ref",
        "context_bundle",
        "workflow_guidance",
        "blackboard",
        "blackboard_info",
    ];
    if object.get("initial_scope") == object.get("allowed_write_scope") {
        skip.push("initial_scope");
    }
    if let Some(guidance) = object.get("workflow_guidance").and_then(Value::as_object) {
        if guidance.get("breadcrumb") == object.get("breadcrumb") {
            skip.push("breadcrumb");
        }
        if let Some(cursor) = guidance.get("gate_cursor").and_then(Value::as_object) {
            for (root_field, cursor_field) in [
                ("active_gate_path", "gate_path"),
                ("active_stage_id", "stage_id"),
                ("subagent_role", "subagent_role"),
            ] {
                if cursor.get(cursor_field).is_some()
                    && (cursor.get(cursor_field) == object.get(root_field)
                        || cursor.get(root_field) == object.get(root_field))
                {
                    skip.push(root_field);
                }
            }
        }
    }
    if let Some(contract) = contract {
        for (root_key, contract_key) in [
            ("goal", "target"),
            ("proof_policy", "proof_policy"),
            ("allowed_write_scope", "allowed_scope"),
            ("subtasks", "subtasks"),
            ("depends_on", "depends_on"),
            ("applicable_invariants", "invariants"),
        ] {
            if object.get(root_key) == contract.get(contract_key) {
                skip.push(root_key);
            }
        }
    } else {
        skip.extend([
            "goal",
            "proof_policy",
            "allowed_write_scope",
            "subtasks",
            "depends_on",
            "applicable_invariants",
        ]);
    }
    let remaining = fields_without(object, &skip);
    if !remaining.is_empty() {
        output.push_str("\n## Additional task data\n");
        render_fields(&mut output, &remaining, 0, &mut state);
    }
    output
}

fn render_lifecycle_state(output: &mut String, object: &Map<String, Value>) {
    const STATE_FIELDS: &[&str] = &[
        "task_id",
        "milestone_ref",
        "stage",
        "status",
        "claim_revision",
        "contract_revision",
        "worker_id",
        "worktree",
    ];
    let fields = object
        .iter()
        .filter(|(key, _)| STATE_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Map<_, _>>();
    if fields.is_empty() {
        return;
    }
    output.push_str("\n## Lifecycle state\n");
    let mut state = RenderState::default();
    render_fields(output, &fields, 0, &mut state);
}

fn root_contract_fields(object: &Map<String, Value>) -> Map<String, Value> {
    let mut fields = Map::new();
    for (target, source) in [
        ("target", "goal"),
        ("proof_policy", "proof_policy"),
        ("allowed_scope", "allowed_write_scope"),
        ("subtasks", "subtasks"),
        ("depends_on", "depends_on"),
        ("invariants", "applicable_invariants"),
    ] {
        if let Some(value) = object.get(source) {
            fields.insert(target.into(), value.clone());
        }
    }
    fields
}

fn render_record(value: &Value, title: &str, heading: &str) -> String {
    let mut output = format!("# {heading}\n");
    let Some(object) = value.as_object() else {
        let mut state = RenderState::default();
        render_field(&mut output, "result", value, 0, &mut state);
        return output;
    };
    let mut state = RenderState::default();
    render_lifecycle_state(&mut output, object);
    if title == "task_manage" && object.get("spec").is_some() {
        output.push_str("\n## Task record\n");
    } else {
        output.push_str("\n## Result details\n");
    }
    let skip = [
        "task_id",
        "milestone_ref",
        "stage",
        "status",
        "claim_revision",
        "contract_revision",
        "worker_id",
        "worktree",
        "digest",
        "_presentation_scope_uris",
    ];
    let task_inspection = title == "task_manage" && object.get("spec").is_some();
    let fields = if task_inspection {
        task_inspection_fields(object, &skip)
    } else {
        fields_without(object, &skip)
    };
    let scope_uris = task_inspection
        .then(|| {
            object
                .get("_presentation_scope_uris")
                .and_then(Value::as_object)
        })
        .flatten();
    if let Some(scope_uris) = scope_uris {
        render_task_fields(&mut output, &fields, scope_uris, 0, &mut state);
    } else {
        render_fields(&mut output, &fields, 0, &mut state);
    }
    output
}

fn task_inspection_fields(object: &Map<String, Value>, skip: &[&str]) -> Map<String, Value> {
    let mut fields = fields_without(object, skip);
    let Some(mut spec) = object.get("spec").and_then(Value::as_object).cloned() else {
        return fields;
    };
    let mut authority = object
        .get("authority_snapshot")
        .and_then(Value::as_object)
        .cloned();

    for (spec_key, authority_key, response_key) in [
        ("target", "task_target", "goal"),
        ("proof_policy", "task_proof_policy", "proof_policy"),
    ] {
        if let Some(value) = spec.get(spec_key) {
            if authority
                .as_ref()
                .and_then(|snapshot| snapshot.get(authority_key))
                == Some(value)
            {
                if let Some(snapshot) = authority.as_mut() {
                    snapshot.remove(authority_key);
                }
            }
            if fields.get(response_key) == Some(value) {
                fields.remove(response_key);
            }
        }
    }

    if let (Some(current), Some(initial), Some(snapshot), Some(allowed)) = (
        spec.get("scope").and_then(Value::as_array),
        fields.get("initial_scope").and_then(Value::as_array),
        authority.as_ref().and_then(|value| value.get("task_scope")),
        fields.get("allowed_write_scope").and_then(Value::as_array),
    ) {
        let current = unique_scope_values(current);
        let initial = unique_scope_values(initial);
        if snapshot
            .as_array()
            .is_some_and(|scope| scope_values_equal(&initial, &unique_scope_values(scope)))
            && scope_values_equal(&unique_scope_values(allowed), &current)
        {
            let presentation_scope = if scope_values_equal(&initial, &current) {
                Some(Value::Array(current.clone()))
            } else if let Some(added) = extended_scope_values(&initial, &current) {
                let mut scope = Map::new();
                scope.insert("initial_scope".into(), Value::Array(initial));
                scope.insert("extended_scope".into(), Value::Array(added));
                Some(Value::Object(scope))
            } else {
                None
            };
            if let Some(scope) = presentation_scope {
                spec.insert("scope".into(), scope);
                fields.remove("initial_scope");
                fields.remove("allowed_write_scope");
                if let Some(snapshot) = authority.as_mut() {
                    snapshot.remove("task_scope");
                }
            }
        }
    }

    if let Some(task_invariants) = spec.get("invariants").and_then(Value::as_array) {
        if authority
            .as_ref()
            .and_then(|snapshot| snapshot.get("task_invariants"))
            == spec.get("invariants")
        {
            let milestone_invariants = authority
                .as_ref()
                .and_then(|snapshot| snapshot.get("milestone_invariants"))
                .and_then(Value::as_array)
                .cloned();
            if let Some(snapshot) = authority.as_mut() {
                snapshot.remove("task_invariants");
            }
            if let Some(applicable) = fields
                .get("applicable_invariants")
                .and_then(Value::as_array)
                .cloned()
            {
                let remaining = applicable
                    .iter()
                    .filter(|invariant| {
                        !task_invariants.contains(invariant)
                            && !milestone_invariants
                                .as_ref()
                                .is_some_and(|items| items.contains(invariant))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if remaining.len() != applicable.len() {
                    if remaining.is_empty() {
                        fields.remove("applicable_invariants");
                    } else {
                        fields.insert("applicable_invariants".into(), Value::Array(remaining));
                    }
                }
            }
        }
    }

    fields.insert("spec".into(), Value::Object(spec));
    if let Some(authority) = authority {
        fields.insert("authority_snapshot".into(), Value::Object(authority));
    }
    fields
}

fn unique_scope_values(values: &[Value]) -> Vec<Value> {
    let mut unique = Vec::with_capacity(values.len());
    for value in values {
        if !unique.contains(value) {
            unique.push(value.clone());
        }
    }
    unique
}

fn scope_values_equal(left: &[Value], right: &[Value]) -> bool {
    left.len() == right.len() && left.iter().all(|value| right.contains(value))
}

fn extended_scope_values(initial: &[Value], current: &[Value]) -> Option<Vec<Value>> {
    if initial.len() >= current.len() || !initial.iter().all(|value| current.contains(value)) {
        return None;
    }
    Some(
        current
            .iter()
            .filter(|value| !initial.contains(value))
            .cloned()
            .collect(),
    )
}

fn render_blackboard(output: &mut String, messages: &[Value], state: &mut RenderState) {
    output.push_str("\n## Blackboard messages\n");
    if messages.is_empty() {
        output.push_str("\nNo messages were included in this context.\n");
        return;
    }
    for message in messages {
        let key = message.get("id").map(scalar_text);
        if let Some(key) = key.as_ref() {
            if state.seen_messages.get(key) == Some(message) {
                continue;
            }
            state.seen_messages.insert(key.clone(), message.clone());
        }
        if message.get("topic").and_then(Value::as_str) == Some("blockers") {
            output.push('\n');
            output.push_str(&render_blocker_callout(message));
        } else {
            render_nonblocker_message(output, message, state);
        }
    }
}

fn render_nonblocker_message(output: &mut String, message: &Value, state: &mut RenderState) {
    let topic = message
        .get("topic")
        .map(scalar_text)
        .unwrap_or_else(|| "message".into());
    let id = message
        .get("id")
        .map(scalar_text)
        .unwrap_or_else(|| "unknown".into());
    output.push_str("\n### Blackboard ");
    output.push_str(&topic);
    output.push_str(" message (#");
    output.push_str(&id);
    output.push_str(")\n");
    let fields = message
        .as_object()
        .map(|object| fields_without(object, &["payload"]));
    if let Some(fields) = fields {
        render_fields(output, &fields, 0, state);
    }
    let payload = message
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_default();
    output.push_str("- **Body**\n");
    append_quoted_body(output, payload);
    if summary_is_truncated(payload) {
        append_recovery_hint(output, message);
    }
}

fn append_recovery_hint(output: &mut String, message: &Value) {
    let id = message
        .get("id")
        .map(scalar_text)
        .unwrap_or_else(|| "unknown".into());
    output.push_str("> Full body recovery: ");
    output.push_str(&inline_code(&format!(
        "task_blackboard(action: \"inspect\", message_id: {id})"
    )));
    output.push_str(".\n");
}

fn append_quoted_body(output: &mut String, payload: &str) {
    if payload.is_empty() {
        output.push_str("> Body: \"\"\n");
        return;
    }
    output.push_str("> Body:\n");
    for line in payload.split_inclusive('\n') {
        output.push_str("> ");
        output.push_str(line);
        if !line.ends_with('\n') {
            output.push('\n');
        }
    }
}

fn quote_field(output: &mut String, label: &str, value: Option<&Value>) {
    let value = value.map(scalar_text).unwrap_or_else(|| "null".into());
    output.push_str("> ");
    output.push_str(label);
    output.push_str(": ");
    output.push_str(&inline_code(&value));
    output.push('\n');
}

fn summary_is_truncated(payload: &str) -> bool {
    payload.ends_with("... [truncated]")
}

fn render_task_fields(
    output: &mut String,
    fields: &Map<String, Value>,
    scope_uris: &Map<String, Value>,
    depth: usize,
    state: &mut RenderState,
) {
    for (key, value) in ordered_fields(fields) {
        if key == "spec" {
            if let Some(spec) = value.as_object() {
                bullet_prefix(output, depth);
                output.push_str("**");
                output.push_str(&escape_label(display_label(key)));
                output.push_str("**\n");
                for (spec_key, spec_value) in ordered_fields(spec) {
                    if spec_key == "scope" {
                        render_task_scope(output, spec_value, scope_uris, depth + 1, state);
                    } else {
                        render_field(output, spec_key, spec_value, depth + 1, state);
                    }
                }
                continue;
            }
        }
        render_field(output, key, value, depth, state);
    }
}

fn render_task_scope(
    output: &mut String,
    scope: &Value,
    scope_uris: &Map<String, Value>,
    depth: usize,
    state: &mut RenderState,
) {
    match scope {
        Value::Array(paths) => render_scope_group(output, "scope", paths, scope_uris, depth, state),
        Value::Object(groups) => {
            bullet_prefix(output, depth);
            output.push_str("**Scope**\n");
            for (key, value) in ordered_fields(groups) {
                if let Some(paths) = value.as_array() {
                    render_scope_group(output, key, paths, scope_uris, depth + 1, state);
                } else {
                    render_field(output, key, value, depth + 1, state);
                }
            }
        }
        _ => render_field(output, "scope", scope, depth, state),
    }
}

fn render_scope_group(
    output: &mut String,
    key: &str,
    paths: &[Value],
    scope_uris: &Map<String, Value>,
    depth: usize,
    state: &mut RenderState,
) {
    if paths.is_empty() {
        render_field(output, key, &Value::Array(Vec::new()), depth, state);
        return;
    }
    bullet_prefix(output, depth);
    output.push_str("**");
    output.push_str(&escape_label(display_label(key)));
    output.push_str("**\n");
    for (index, value) in paths.iter().enumerate() {
        let item_key = format!("[{index}]");
        let Some(path) = value.as_str() else {
            render_field(output, &item_key, value, depth + 1, state);
            continue;
        };
        if let Some(uri) = scope_uris.get(path).and_then(Value::as_str) {
            bullet_prefix(output, depth + 1);
            output.push_str(&format!("[{}]({uri})\n", escape_label(path)));
        } else {
            render_field(output, &item_key, value, depth + 1, state);
        }
    }
}

#[derive(Default)]
struct RenderState {
    seen_messages: BTreeMap<String, Value>,
}

fn render_fields(
    output: &mut String,
    fields: &Map<String, Value>,
    depth: usize,
    state: &mut RenderState,
) {
    for (key, value) in ordered_fields(fields) {
        render_field(output, key, value, depth, state);
    }
}

fn render_field(
    output: &mut String,
    key: &str,
    value: &Value,
    depth: usize,
    state: &mut RenderState,
) {
    match value {
        Value::Object(object) if object.is_empty() => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**: ");
            output.push_str(&inline_code("{}"));
            output.push('\n');
        }
        Value::Object(object) => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**\n");
            render_fields(output, object, depth + 1, state);
        }
        Value::Array(items) if items.is_empty() => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**: ");
            output.push_str(&inline_code("[]"));
            output.push('\n');
        }
        Value::Array(items) => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**\n");
            for (index, item) in items.iter().enumerate() {
                render_field(output, &format!("[{index}]"), item, depth + 1, state);
            }
        }
        Value::String(text) if text.contains('\n') => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**\n");
            append_fenced_text(output, text, depth + 1);
        }
        scalar => {
            bullet_prefix(output, depth);
            output.push_str("**");
            output.push_str(&escape_label(display_label(key)));
            output.push_str("**: ");
            let text = match scalar {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            output.push_str(&inline_code(&text));
            output.push('\n');
        }
    }
}

fn append_fenced_text(output: &mut String, text: &str, depth: usize) {
    let longest = longest_backtick_run(text);
    let backtick = char::from(96).to_string();
    let fence = backtick.repeat(longest.max(2) + 1);
    indent(output, depth);
    output.push_str(&fence);
    output.push_str("text\n");
    indent(output, depth);
    output.push_str(text);
    if !text.ends_with('\n') {
        output.push('\n');
    }
    indent(output, depth);
    output.push_str(&fence);
    output.push('\n');
}

fn fields_without(object: &Map<String, Value>, excluded: &[&str]) -> Map<String, Value> {
    object
        .iter()
        .filter(|(key, _)| !excluded.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn ordered_fields(object: &Map<String, Value>) -> Vec<(&str, &Value)> {
    let mut fields = object
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect::<Vec<_>>();
    fields.sort_unstable_by_key(|(key, _)| *key);
    fields
}

fn display_label(key: &str) -> &str {
    match key {
        "task_id" => "Task ID",
        "milestone_ref" => "Milestone",
        "stage" => "Stage",
        "status" => "Status",
        "claim_revision" => "Claim revision",
        "contract_revision" | "rev" => "Contract revision",
        "worker_id" | "owner" => "Worker",
        "worktree" => "Worktree",
        "goal" | "target" => "Target",
        "allowed_scope" | "allowed_write_scope" => "Write scope",
        "initial_scope" => "Initial scope",
        "extended_scope" => "Extended scope",
        "proof_policy" => "Proof policy",
        "depends_on" => "Dependencies",
        "applicable_invariants" | "invariants" => "Invariants",
        "subtask_dod" => "Subtask definition of done",
        "actionable_steps" => "Actionable steps",
        "expected_evidence_template" => "Expected evidence template",
        "scoped_commands" => "Scoped commands",
        "receipt" => "Durable receipt",
        "findings" => "Findings",
        "candidate_snapshot" => "Candidate snapshot",
        "latest_snapshot" => "Latest snapshot",
        _ => key,
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn longest_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in value.chars() {
        if character == char::from(96) {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn escape_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('*', "\\*")
}

fn bullet_prefix(output: &mut String, depth: usize) {
    indent(output, depth);
    output.push_str("- ");
}

fn indent(output: &mut String, depth: usize) {
    for _ in 0..depth {
        output.push_str("  ");
    }
}

fn format_path_or_link(base_dir: Option<&std::path::Path>, rel_or_abs: &str) -> String {
    let candidate = match base_dir {
        Some(base) if !std::path::Path::new(rel_or_abs).is_absolute() => base.join(rel_or_abs),
        _ => std::path::PathBuf::from(rel_or_abs),
    };
    if candidate.is_absolute() && candidate.exists() {
        if let Ok(canon) = candidate.canonicalize() {
            if let Some(uri) = super::discovery::file_uri_for_path(&canon, None) {
                return format!("[`{rel_or_abs}`]({uri})");
            }
        }
    }
    inline_code(rel_or_abs)
}

/// Render a [`acdd_state::context::ClaimResp`] as readable Markdown (`ADR-0006`).
pub fn render_claim_markdown(claim: &acdd_state::context::ClaimResp) -> String {
    let mut out = format!("# Gate Claim: {}\n\n", inline_code(&claim.node_cursor.to_string()));
    out.push_str("## Lifecycle state\n");
    out.push_str(&format!("- **Run**: {}\n", inline_code(claim.run_id.as_str())));
    out.push_str(&format!("- **Node cursor**: {}\n", inline_code(&claim.node_cursor.to_string())));
    out.push_str(&format!("- **Claim revision**: {}\n", inline_code(&claim.claim_revision.to_string())));
    out.push_str(&format!("- **Contract revision**: {}\n", inline_code(&claim.contract_revision.to_string())));
    out.push_str(&format!("- **Worker**: {}\n", inline_code(claim.worker_id.as_str())));
    out.push_str(&format!("- **Worktree**: {}\n", format_path_or_link(None, &claim.worktree)));
    out.push_str(&format!("- **Candidate tree**: {}\n", inline_code(claim.candidate_tree.as_str())));
    out.push_str(&format!("- **Inspect**: {}\n", inline_code(&claim.inspect_cmd)));
    if claim.context_reset_required {
        out.push_str(
            "\n> [!WARNING]\n\
             > **CONTEXT RESET REQUIRED**: A prior `architectural_drift` rejection triggered `rearchitect`. \
             Discard cached builder assumptions, read the latest `rearchitect` decision on `acdd_blackboard`, and implement cleanly from scratch.\n",
        );
    }

    let wt_path = std::path::Path::new(&claim.worktree);
    let mut state = RenderState::default();
    if let Some(contract) = claim.contract.as_object() {
        out.push_str("\n## Contract\n");
        let mut remaining = contract.clone();
        for scope_key in ["scope", "extended_scope"] {
            if let Some(arr) = remaining.remove(scope_key).and_then(|v| v.as_array().cloned()) {
                if !arr.is_empty() {
                    out.push_str(&format!("- **{}**\n", display_label(scope_key)));
                    for entry in arr {
                        if let Some(s) = entry.as_str() {
                            out.push_str(&format!("  - {}\n", format_path_or_link(Some(wt_path), s)));
                        } else if let Some(p) = entry.get("path").and_then(Value::as_str) {
                            let reason = entry.get("reason").and_then(Value::as_str).unwrap_or("");
                            out.push_str(&format!(
                                "  - {} ({})\n",
                                format_path_or_link(Some(wt_path), p),
                                reason
                            ));
                        }
                    }
                }
            }
        }
        render_fields(&mut out, &remaining, 0, &mut state);
    }

    if !claim.guidance.is_empty() {
        out.push_str("\n## Guidance\n");
        for item in &claim.guidance {
            out.push_str("- ");
            out.push_str(&scalar_text(item));
            out.push('\n');
        }
    }
    if let Some(ref findings) = claim.unresolved_findings {
        out.push_str("\n## Unresolved findings\n");
        render_field(&mut out, "findings", findings, 0, &mut state);
    }
    if let Some(ref slice) = claim.pre_task_slice {
        out.push_str("\n## Pre-task architectural slice\n");
        render_field(&mut out, "pre_task_slice", slice, 0, &mut state);
    }
    if !claim.dependency_receipts.is_empty() {
        out.push_str("\n## Dependency receipts\n");
        for receipt in &claim.dependency_receipts {
            render_field(&mut out, "receipt", receipt, 0, &mut state);
        }
    }
    if !claim.blackboard.is_empty() {
        render_blackboard(&mut out, &claim.blackboard, &mut state);
    }
    if !claim.context_lenses.is_empty() {
        out.push_str("\n## Context lenses\n");
        for (lens, value) in &claim.context_lenses {
            render_field(&mut out, lens, value, 0, &mut state);
        }
    }
    if !claim.omitted_sections.is_empty() {
        out.push_str("\n## Omitted sections\n");
        for section in &claim.omitted_sections {
            out.push_str(&format!("- {}\n", inline_code(section)));
        }
    }
    out
}

/// Render a [`acdd_state::usecases::VerifyResp`] as readable Markdown (`ADR-0006`).
pub fn render_verify_markdown(verify: &acdd_state::usecases::VerifyResp) -> String {
    let mut out = format!(
        "# Gate Verification: {}\n\n",
        inline_code(&verify.node_cursor.to_string())
    );
    out.push_str(&format!(
        "- [{}] **Passed**: {}\n",
        if verify.passed { "x" } else { " " },
        inline_code(&verify.passed.to_string())
    ));
    out.push_str(&format!("- **Run**: {}\n", inline_code(verify.run_id.as_str())));
    out.push_str(&format!(
        "- **Claim revision**: {}\n",
        inline_code(&verify.claim_revision.to_string())
    ));
    out.push_str(&format!(
        "- **Trace status**: {}\n",
        inline_code(verify.trace_status.as_str())
    ));
    out.push_str(&format!(
        "- **Execution digest**: {}\n",
        inline_code(&verify.execution_digest)
    ));
    if let Some(code) = verify.exit_code {
        out.push_str(&format!("- **Exit code**: {}\n", inline_code(&code.to_string())));
    }
    out.push_str(&format!(
        "- **Tests**: passed={}, failed={}\n",
        inline_code(&verify.tests_passed.to_string()),
        inline_code(&verify.tests_failed.to_string())
    ));
    out.push_str(&format!(
        "- **Log**: {}\n",
        inline_code(&verify.log_relpath)
    ));
    if !verify.check_results.is_empty() {
        out.push_str("\n## Domain checks\n");
        for chk in &verify.check_results {
            out.push_str(&format!(
                "- [{}] `{}.{}`: {}\n",
                if chk.passed { "x" } else { " " },
                chk.domain,
                chk.check,
                chk.summary
            ));
            for diag in &chk.diagnostics {
                out.push_str(&format!("  - {diag}\n"));
            }
        }
    }
    out
}

/// Render a [`acdd_state::usecases::SubmitResp`] as readable Markdown (`ADR-0006`).
pub fn render_submit_markdown(submit: &acdd_state::usecases::SubmitResp) -> String {
    let mut out = format!(
        "# Gate Submission: {}\n\n",
        inline_code(&submit.node_cursor.to_string())
    );
    out.push_str(&format!("- **Run**: {}\n", inline_code(submit.run_id.as_str())));
    out.push_str(&format!("- **Action**: {}\n", inline_code(submit.action.as_str())));
    out.push_str(&format!(
        "- **New node status**: {}\n",
        inline_code(submit.new_node_status.as_str())
    ));
    out.push_str(&format!(
        "- **Run status**: {}\n",
        inline_code(submit.run_status.as_str())
    ));
    if let Some(ref oid) = submit.committed_oid {
        out.push_str(&format!("- **Committed OID**: {}\n", inline_code(oid.as_str())));
    }
    if let Some(ref sref) = submit.snapshot_ref {
        out.push_str(&format!("- **Snapshot ref**: {}\n", inline_code(sref)));
    }
    if let Some(ref archive) = submit.archive_path {
        out.push_str(&format!("- **Archive**: {}\n", format_path_or_link(None, archive)));
    }
    if !submit.unblocked_nodes.is_empty() {
        out.push_str("\n## Unblocked nodes\n");
        for node in &submit.unblocked_nodes {
            out.push_str(&format!("- {}\n", inline_code(node)));
        }
    }
    if let Some(ref receipt) = submit.receipt {
        let val = serde_json::to_value(receipt).unwrap_or(Value::Null);
        let mut state = RenderState::default();
        out.push_str("\n## Receipt\n");
        render_field(&mut out, "receipt", &val, 0, &mut state);
    }
    out
}

/// Render a [`acdd_state::usecases::StateCommandOutput`] as readable Markdown (`ADR-0006`).
pub fn render_state_command_markdown(
    output: &acdd_state::usecases::StateCommandOutput,
    title: &str,
) -> String {
    use acdd_state::usecases::StateCommandOutput;
    let mut state = RenderState::default();
    match output {
        StateCommandOutput::Claim(claim) => render_claim_markdown(claim),
        StateCommandOutput::Verify(verify) => render_verify_markdown(verify),
        StateCommandOutput::Submit(submit) => render_submit_markdown(submit),
        StateCommandOutput::BlackboardPost(resp) => {
            let val = serde_json::json!({ "message": resp.entry });
            super::coordination::render_blackboard_markdown(&val, title)
        }
        StateCommandOutput::CallPluginTool(resp) => {
            super::coordination::render_plugin_tool_markdown(&resp.tool_name, &resp.output)
        }
        StateCommandOutput::Start(resp) => {
            let mut out = format!("# Run Started: {}\n\n", inline_code(resp.run.run_id.as_str()));
            let ws = std::path::Path::new(&resp.run.workspace);
            out.push_str(&format!("- **Run name**: {}\n", inline_code(resp.run.run_name.as_str())));
            out.push_str(&format!("- **Profile**: {}\n", inline_code(&resp.run.profile_id)));
            out.push_str(&format!("- **Isolation**: {}\n", inline_code(resp.run.isolation.as_str())));
            if let Some(ref spec) = resp.materialized_spec_path {
                out.push_str(&format!("- **Spec**: {}\n", format_path_or_link(Some(ws), spec)));
            }
            if let Some(ref wt) = resp.run.worktree {
                out.push_str(&format!("- **Worktree**: {}\n", format_path_or_link(None, wt)));
            }
            if !resp.ready_nodes.is_empty() {
                out.push_str("\n## Ready nodes\n");
                for node in &resp.ready_nodes {
                    out.push_str(&format!("- {}\n", inline_code(node)));
                }
            }
            out
        }
        StateCommandOutput::Draft(resp) => {
            let mut out = format!("# Drafted Spec: {}\n\n", inline_code(&resp.spec_id));
            out.push_str(&format!("- **Collection**: {}\n", inline_code(&resp.collection)));
            out.push_str(&format!("- **Path**: {}\n", format_path_or_link(None, &resp.spec_path)));
            out
        }
        other => {
            let val = serde_json::to_value(other).unwrap_or(Value::Null);
            let inner = val
                .as_object()
                .and_then(|map| map.values().next())
                .unwrap_or(&val);
            let mut out = format!("# {}\n\n", escape_label(title));
            if let Some(map) = inner.as_object() {
                render_fields(&mut out, map, 0, &mut state);
            } else {
                render_field(&mut out, "result", inner, 0, &mut state);
            }
            out
        }
    }
}

/// Render a [`acdd_state::usecases::StateQueryOutput`] as readable Markdown (`ADR-0006`).
pub fn render_state_query_markdown(
    output: &acdd_state::usecases::StateQueryOutput,
    title: &str,
) -> String {
    use acdd_state::usecases::StateQueryOutput;
    let mut state = RenderState::default();
    match output {
        StateQueryOutput::Status(resp) => {
            let mut out = String::from("# Run Status\n\n");
            let Some(ref run) = resp.run else {
                out.push_str("No active run found.\n");
                return out;
            };
            let ws = std::path::Path::new(&run.workspace);
            out.push_str(&format!("- **Run ID**: {}\n", inline_code(run.run_id.as_str())));
            out.push_str(&format!("- **Run name**: {}\n", inline_code(run.run_name.as_str())));
            out.push_str(&format!("- **Status**: {}\n", inline_code(run.status.as_str())));
            out.push_str(&format!("- **Profile**: {}\n", inline_code(&run.profile_id)));
            out.push_str(&format!("- **Isolation**: {}\n", inline_code(run.isolation.as_str())));
            if let Some(ref spec) = run.spec_path {
                out.push_str(&format!("- **Spec**: {}\n", format_path_or_link(Some(ws), spec)));
            }
            if !resp.ready_nodes.is_empty() {
                out.push_str("\n## Ready nodes\n");
                for r in &resp.ready_nodes {
                    out.push_str(&format!("- {}\n", inline_code(r)));
                }
            }
            if !resp.nodes.is_empty() {
                out.push_str("\n## Gate nodes\n");
                for node in &resp.nodes {
                    let checked = node.status == acdd_state::types::NodeStatus::Completed;
                    out.push_str(&format!(
                        "- [{}] {} (`{}`)\n",
                        if checked { "x" } else { " " },
                        inline_code(&node.node_id),
                        node.status.as_str()
                    ));
                }
            }
            out
        }
        StateQueryOutput::List(resp) => {
            let mut out = String::from("# Runs\n\n");
            out.push_str(&format!(
                "- **Total runs**: {}, **Total tombstones**: {}, **Offset**: {}, **Limit**: {}\n\n",
                resp.page.total_runs, resp.page.total_tombstones, resp.page.offset, resp.page.limit
            ));
            if resp.page.runs.is_empty() && resp.page.tombstones.is_empty() {
                out.push_str("No runs found.\n");
                return out;
            }
            for run in &resp.page.runs {
                let ws = std::path::Path::new(&run.workspace);
                let spec = run
                    .spec_path
                    .as_deref()
                    .map(|p| format_path_or_link(Some(ws), p))
                    .unwrap_or_else(|| inline_code("<ephemeral>"));
                out.push_str(&format!(
                    "- **{}** (`{}`): status={}, profile={}, spec={}\n",
                    run.run_name,
                    run.run_id,
                    inline_code(run.status.as_str()),
                    inline_code(&run.profile_id),
                    spec
                ));
            }
            if !resp.page.tombstones.is_empty() {
                out.push_str("\n## Closed runs (tombstones)\n");
                for tomb in &resp.page.tombstones {
                    out.push_str(&format!(
                        "- **{}** (`{}`): terminal_status={}, closed_at={}\n",
                        tomb.run_name,
                        tomb.run_id,
                        inline_code(tomb.terminal_status.as_str()),
                        inline_code(&tomb.closed_at)
                    ));
                }
            }
            out
        }
        StateQueryOutput::Show(resp) => {
            let mut out = format!("# Run Details: {}\n\n", inline_code(resp.run.run_id.as_str()));
            let ws = std::path::Path::new(&resp.run.workspace);
            out.push_str(&format!("- **Run name**: {}\n", inline_code(resp.run.run_name.as_str())));
            out.push_str(&format!("- **Status**: {}\n", inline_code(resp.run.status.as_str())));
            if let Some(ref spec) = resp.run.spec_path {
                out.push_str(&format!("- **Spec**: {}\n", format_path_or_link(Some(ws), spec)));
            }
            if let Some(ref node) = resp.node {
                out.push_str(&format!("\n## Node: {}\n", inline_code(&node.node_id)));
                out.push_str(&format!("- **Status**: {}\n", inline_code(node.status.as_str())));
                out.push_str(&format!("- **Claim revision**: {}\n", node.claim_revision));
                out.push_str(&format!("- **Contract revision**: {}\n", node.contract_revision));
                if !node.scope.is_empty() {
                    out.push_str("- **Scope**\n");
                    for s in &node.scope {
                        out.push_str(&format!("  - {}\n", format_path_or_link(Some(ws), s)));
                    }
                }
            }
            if !resp.subtasks.is_empty() {
                out.push_str("\n## Subtasks\n");
                for st in &resp.subtasks {
                    let checked = st.status == acdd_state::types::SubtaskStatus::Completed;
                    out.push_str(&format!(
                        "- [{}] {} (`{}`): {}\n",
                        if checked { "x" } else { " " },
                        inline_code(st.subtask_ref.as_str()),
                        st.status.as_str(),
                        st.title
                    ));
                }
            }
            if let Some(ref preview) = resp.context_preview {
                out.push_str("\n## Context preview\n");
                render_field(&mut out, "context_preview", preview, 0, &mut state);
            }
            out
        }
        StateQueryOutput::Next(resp) => {
            let mut out = String::from("# Next Ready Nodes\n\n");
            if let Some(ref rid) = resp.run_id {
                out.push_str(&format!("- **Run ID**: {}\n", inline_code(rid.as_str())));
            }
            if let Some(iso) = resp.isolation {
                out.push_str(&format!("- **Isolation**: {}\n", inline_code(iso.as_str())));
            }
            if resp.ready_cursors.is_empty() {
                out.push_str("\nNo ready nodes.\n");
            } else {
                out.push_str("\n## Ready cursors\n");
                for cursor in &resp.ready_cursors {
                    out.push_str(&format!("- {}\n", inline_code(cursor)));
                }
            }
            if !resp.waves.is_empty() {
                out.push_str("\n## Waves\n");
                for (idx, wave) in resp.waves.iter().enumerate() {
                    out.push_str(&format!("- Wave {}: {}\n", idx + 1, wave.join(", ")));
                }
            }
            out
        }
        StateQueryOutput::BlackboardRead(resp) => {
            let val = serde_json::json!({
                "run_id": resp.run_id,
                "messages": resp.items,
                "pagination": {
                    "total": resp.total,
                    "generation": resp.generation,
                    "limit": resp.limit,
                    "offset": resp.offset,
                }
            });
            super::coordination::render_blackboard_markdown(&val, title)
        }
        StateQueryOutput::Observability(resp) => {
            if let Some(ref prom) = resp.prometheus_text {
                format!("# Observability\n\n```text\n{prom}\n```\n")
            } else {
                let val = serde_json::to_value(&resp.snapshot).unwrap_or(Value::Null);
                let mut out = String::from("# Observability\n\n");
                render_field(&mut out, "snapshot", &val, 0, &mut state);
                out
            }
        }
        StateQueryOutput::Validate(resp) => {
            let mut out = format!(
                "# Validation: {}\n\n- **valid:** `{}`\n- **scope:** `{}`\n- **passed / failed / warnings / docs:** {} / {} / {} / {}\n",
                if resp.valid { "PASS" } else { "FAIL" },
                resp.valid,
                resp.scope,
                resp.summary.passed,
                resp.summary.failed,
                resp.summary.warnings,
                resp.summary.documents_validated
            );
            for d in &resp.diagnostics {
                out.push_str(&format!(
                    "- `{}` [{}] {}:{}:{} — {}\n",
                    d.code, d.status, d.path, d.line, d.column, d.message
                ));
            }
            out
        }
    }
}
