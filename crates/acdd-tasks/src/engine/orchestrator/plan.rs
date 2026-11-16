use crate::{
    engine::{gate_graph, git_snapshot},
    tasks::{
        profile::{GateDriver, GateProfile, PersistenceAction, ProofKind},
        SubtaskSpec,
    },
};
use acdd_core::models::DocSection;
use anyhow::{ensure, Context, Result};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// A source-owned plan step projected from production documentation sections.
#[derive(Debug, Clone, Serialize)]
pub struct PlanStep {
    pub title: String,
    pub target: String,
    pub scope: Vec<String>,
    pub depends_on: Vec<String>,
    pub status: String,
    pub evidence: Option<String>,
    pub subtasks: Vec<SubtaskSpec>,
    #[serde(skip)]
    heading: String,
}

/// Validated plan source and stable step order, without a second task registry.
#[derive(Debug, Serialize)]
pub struct PlanDocument {
    pub path: PathBuf,
    pub steps: Vec<PlanStep>,
    #[serde(skip)]
    source: String,
}

fn field_lines(content: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut lines = Vec::new();
    let code_blocks = Parser::new(content)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::CodeBlock(_))).then_some(range)
        })
        .collect::<Vec<_>>();
    let mut offset = 0;
    for raw in content.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let start = offset;
        offset += raw.len();
        if code_blocks.iter().any(|range| range.contains(&start)) {
            continue;
        }
        lines.push((start..start + line.len(), line));
    }
    lines
}

fn fields(content: &str) -> Result<BTreeMap<String, String>> {
    let mut fields = BTreeMap::new();
    for (_, line) in field_lines(content) {
        if let Some((key, value)) = line
            .trim()
            .strip_prefix("- **")
            .and_then(|line| line.split_once("**:"))
        {
            ensure!(
                fields
                    .insert(key.to_owned(), value.trim().to_owned())
                    .is_none(),
                "PLAN_FIELD_DUPLICATE: {key}"
            );
        }
    }
    Ok(fields)
}

fn string_list(value: &str) -> Result<Vec<String>> {
    if value.starts_with('[') {
        Ok(serde_yaml::from_str(value)?)
    } else {
        Ok(vec![value.to_owned()])
    }
}

impl PlanDocument {
    pub(crate) fn project_completion(&self, task_ref: &str, evidence: &Value) -> Result<String> {
        let step = self
            .steps
            .iter()
            .find(|step| format!("step-{:x}", Sha256::digest(step.title.as_bytes())) == task_ref)
            .context("TASK_AUTHORITY_MISSING: plan step not found")?;
        self.complete_source(step, evidence)
    }
    /// Project the current subtask state into its owning plan step.
    pub(crate) fn project_subtasks(
        &self,
        task_ref: &str,
        subtasks: &[SubtaskSpec],
    ) -> Result<String> {
        let step = self
            .steps
            .iter()
            .find(|step| format!("step-{:x}", Sha256::digest(step.title.as_bytes())) == task_ref)
            .context("TASK_AUTHORITY_MISSING: plan step not found")?;
        let mut heading = None;
        let mut title = String::new();
        let mut start = None;
        let mut end = self.source.len();
        for (event, range) in Parser::new(&self.source).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H2,
                    ..
                }) => {
                    heading = Some(range.start);
                    title.clear();
                }
                Event::Text(text) | Event::Code(text) if heading.is_some() => title.push_str(&text),
                Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                    if start.is_some() {
                        end = heading.context("PLAN_HEADING_INVALID")?;
                        break;
                    }
                    if title == step.heading {
                        start = heading;
                    }
                    heading = None;
                }
                _ => {}
            }
        }
        let start = start.context("TASK_AUTHORITY_MISSING: plan step heading not found")?;
        let section = &self.source[start..end];
        let matching = field_lines(section)
            .into_iter()
            .filter(|(_, line)| line.trim_start().starts_with("- **Subtasks**:"))
            .collect::<Vec<_>>();
        ensure!(
            matching.len() <= 1,
            "PLAN_SOURCE_DRIFT: subtask field is ambiguous"
        );
        let replacement = format!("- **Subtasks**: {}", serde_json::to_string(subtasks)?);
        let mut updated = section.to_owned();
        if let Some((span, _)) = matching.first() {
            updated.replace_range(span.clone(), &replacement);
        } else {
            let insert = section
                .find('\n')
                .map_or(section.len(), |offset| offset + 1);
            updated.insert_str(insert, &format!("{replacement}\n"));
        }
        Ok(format!(
            "{}{}{}",
            &self.source[..start],
            updated,
            &self.source[end..]
        ))
    }
    /// Parse the same step fields directly when task operations reopen plan authority.
    pub fn from_source(path: &Path, source: String) -> Result<Self> {
        let content_start = source
            .strip_prefix("---\n")
            .and_then(|body| body.find("\n---\n").map(|end| end + 9))
            .unwrap_or(0);
        let mut headings = Vec::new();
        let mut current = None;
        for (event, range) in Parser::new(&source).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H2,
                    ..
                }) => current = Some((range.start, range.end, String::new())),
                Event::Text(text) | Event::Code(text) if current.is_some() => {
                    current.as_mut().unwrap().2.push_str(&text)
                }
                Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                    if let Some((start, _, title)) = current.take() {
                        if start >= content_start {
                            headings.push((start, range.end, title));
                        }
                    }
                }
                _ => {}
            }
        }
        let section = |title: String, content: String| DocSection {
            doc_id: String::new(),
            path: path.to_string_lossy().into_owned(),
            section_title: title,
            doc_type: "plan".into(),
            content,
            invariants: vec![],
            referenced_symbols: vec![],
            mtime: 0.0,
            size: source.len() as u64,
            is_invariant: false,
        };
        let mut sections = vec![section(String::new(), String::new())];
        for (index, (_, end, title)) in headings.iter().enumerate() {
            let next = headings
                .get(index + 1)
                .map(|heading| heading.0)
                .unwrap_or(source.len());
            sections.push(section(title.clone(), source[*end..next].to_owned()));
        }
        Self::from_sections(path, source, &sections)
    }
    /// Project plan authority into the shared task contract model.
    pub fn task_contract(&self, repository: &str, project: &str) -> crate::tasks::Milestone {
        let reference = |title: &str| format!("step-{:x}", Sha256::digest(title.as_bytes()));
        crate::tasks::Milestone {
            id: format!(
                "plan-{:x}",
                Sha256::digest(self.path.to_string_lossy().as_bytes())
            ),
            repository: repository.into(),
            project: project.into(),
            spec: None,
            acceptance_criteria: vec![],
            acdd_profile: None,
            invariants: vec![],
            owners: vec![],
            depends_on: vec![],
            required_skills: vec![],
            repositories: vec![],
            linked_to: vec![],
            guidance: BTreeMap::new(),
            concurrency: None,
            status: None,
            deferred_defects: vec![],
            tasks: self
                .steps
                .iter()
                .map(|step| crate::tasks::TaskSpec {
                    task_ref: reference(&step.title),
                    target: step.target.clone(),
                    spec: None,
                    acceptance_criteria: vec![],
                    agent_type: None,
                    acdd_profile: None,
                    proof_policy: "seam-test-first".into(),
                    scope: step.scope.clone(),
                    extended_scope: vec![],
                    symbols: vec![],
                    extended_symbols: vec![],
                    scope_roots: vec![],
                    contract_revision: 1,
                    depends_on: step.depends_on.iter().map(|name| reference(name)).collect(),
                    concurrency: None,
                    invariants: vec![],
                    status: (step.status == "cancelled").then(|| "cancelled".into()),
                    receipt: None,
                    subtasks: step.subtasks.clone(),
                })
                .collect(),
        }
    }
    /// Project canonical numbered step sections from the existing docs extractor.
    pub fn from_sections(path: &Path, source: String, sections: &[DocSection]) -> Result<Self> {
        ensure!(
            !sections.is_empty() && sections.iter().all(|section| section.doc_type == "plan"),
            "PLAN_TYPE_INVALID: expected doc_type: plan"
        );
        let mut step_headings = BTreeSet::new();
        let mut heading = None;
        for event in Parser::new(&source) {
            match event {
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H2,
                    ..
                }) => heading = Some(String::new()),
                Event::Text(text) | Event::Code(text) if heading.is_some() => {
                    heading.as_mut().unwrap().push_str(&text)
                }
                Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                    if let Some(title) = heading.take() {
                        step_headings.insert(title);
                    }
                }
                _ => {}
            }
        }
        let mut steps = Vec::new();
        for section in sections.iter().skip(1) {
            if !step_headings.contains(&section.section_title) {
                continue;
            }
            let (number, title) = section
                .section_title
                .split_once(". ")
                .context("PLAN_HEADING_INVALID: numbered step heading required")?;
            ensure!(
                number.parse::<usize>().is_ok() && !title.trim().is_empty(),
                "PLAN_HEADING_INVALID: {}",
                section.section_title
            );
            let fields = fields(&section.content)?;
            let target = fields
                .get("Target")
                .filter(|value| !value.is_empty())
                .context("PLAN_TARGET_MISSING")?
                .clone();
            let scope = string_list(fields.get("Scope").context("PLAN_SCOPE_MISSING")?)?;
            ensure!(
                !scope.is_empty()
                    && scope
                        .iter()
                        .all(|path| crate::fs::is_safe_project_path(path)),
                "PLAN_SCOPE_INVALID: {title}"
            );
            let depends_on = fields
                .get("Depends On")
                .map(|value| string_list(value))
                .transpose()?
                .unwrap_or_default();
            let status = fields.get("Status").context("PLAN_STATUS_MISSING")?.clone();
            ensure!(
                matches!(
                    status.as_str(),
                    "ready" | "pending" | "in_progress" | "completed" | "cancelled" | "blocked"
                ),
                "PLAN_STATUS_INVALID: {status}"
            );
            let evidence = fields.get("Evidence").cloned();
            let subtasks = fields
                .get("Subtasks")
                .map(|value| serde_yaml::from_str::<Vec<SubtaskSpec>>(value))
                .transpose()?
                .unwrap_or_default();
            steps.push(PlanStep {
                title: title.to_owned(),
                target,
                scope,
                depends_on,
                status,
                evidence,
                subtasks,
                heading: section.section_title.clone(),
            });
        }
        ensure!(!steps.is_empty(), "PLAN_STEPS_EMPTY");
        let plan = Self {
            path: path.to_owned(),
            steps,
            source,
        };
        plan.order()?;
        Ok(plan)
    }

    fn order(&self) -> Result<Vec<usize>> {
        let names = self
            .steps
            .iter()
            .map(|step| step.title.as_str())
            .collect::<BTreeSet<_>>();
        ensure!(names.len() == self.steps.len(), "PLAN_STEP_DUPLICATE");
        for step in &self.steps {
            for dependency in &step.depends_on {
                ensure!(
                    names.contains(dependency.as_str()),
                    "PLAN_DEPENDENCY_UNKNOWN: {dependency}"
                );
            }
        }
        let mut scheduled = BTreeSet::new();
        let mut order = Vec::new();
        while order.len() != self.steps.len() {
            let before = order.len();
            for (index, step) in self.steps.iter().enumerate() {
                if !scheduled.contains(step.title.as_str())
                    && step
                        .depends_on
                        .iter()
                        .all(|dependency| scheduled.contains(dependency.as_str()))
                {
                    scheduled.insert(step.title.as_str());
                    order.push(index);
                }
            }
            ensure!(order.len() > before, "PLAN_DEPENDENCY_CYCLE");
        }
        Ok(order)
    }

    fn complete_source(&self, step: &PlanStep, evidence: &Value) -> Result<String> {
        let mut heading = None;
        let mut step_level = None;
        let mut title = String::new();
        let mut start = None;
        let mut end = self.source.len();
        let mut nested_start = None;
        for (event, range) in Parser::new(&self.source).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    heading = Some((range.start, level));
                    title.clear();
                }
                Event::Text(text) | Event::Code(text) if heading.is_some() => title.push_str(&text),
                Event::End(TagEnd::Heading(level)) => {
                    if step_level.is_some_and(|parent| level <= parent) {
                        end = heading.context("PLAN_HEADING_INVALID")?.0;
                        break;
                    }
                    if start.is_some() && nested_start.is_none() {
                        nested_start = heading.map(|(offset, _)| offset);
                    }
                    if start.is_none() && title == step.heading {
                        start = heading.map(|(offset, _)| offset);
                        step_level = Some(level);
                    }
                    heading = None;
                }
                _ => {}
            }
        }
        let start = start.context("PLAN_SOURCE_DRIFT: step heading is missing")?;
        let section = &self.source[start..end];
        let status = format!("- **Status**: {}", step.status);
        let status_fields = field_lines(section)
            .into_iter()
            .filter(|(_, line)| line.trim() == status)
            .collect::<Vec<_>>();
        ensure!(
            status_fields.len() == 1,
            "PLAN_SOURCE_DRIFT: status is missing or ambiguous"
        );
        let status_pos = status_fields[0].0.start;
        let mut updated = section.to_owned();
        updated.replace_range(status_fields[0].0.clone(), "- **Status**: completed");
        if let Some(nested) = nested_start.map(|offset| offset - start) {
            if status_pos > nested {
                updated.replace_range(
                    status_pos..status_pos + "- **Status**: completed\n".len(),
                    "",
                );
                updated.insert_str(nested, "- **Status**: completed\n\n");
            }
        }
        if let Some(previous) = &step.evidence {
            let field = format!("- **Evidence**: {previous}");
            let matching = field_lines(&updated)
                .into_iter()
                .filter(|(_, line)| line.trim() == field)
                .collect::<Vec<_>>();
            ensure!(
                matching.len() == 1,
                "PLAN_SOURCE_DRIFT: evidence is missing or ambiguous"
            );
            let span = matching[0].0.clone();
            updated.replace_range(span, &format!("- **Evidence**: {evidence}"));
        } else if nested_start.is_some() {
            let offset = Parser::new(&updated)
                .into_offset_iter()
                .filter_map(|(event, range)| match event {
                    Event::Start(Tag::Heading { .. }) => Some(range.start),
                    _ => None,
                })
                .nth(1)
                .context("PLAN_HEADING_INVALID")?;
            updated.insert_str(offset, &format!("- **Evidence**: {evidence}\n\n"));
        } else {
            updated = format!("{}\n- **Evidence**: {evidence}\n\n", updated.trim_end());
        }
        Ok(format!(
            "{}{}{}",
            &self.source[..start],
            updated,
            &self.source[end..]
        ))
    }
}

fn plan_profile(profile: &GateProfile, explicit: bool) -> Result<GateProfile> {
    if explicit {
        ensure!(
            profile.persistence == Some(PersistenceAction::Snapshot),
            "PLAN_PROFILE_COMMIT_FORBIDDEN: explicit plans require save: snapshot"
        );
        ensure!(
            profile.gates.iter().all(|gate| gate.tasks.is_some()),
            "PLAN_PROFILE_ROOT_GATES: explicit plan profiles contain only a tasks container"
        );
    }
    let mut profile = profile.clone();
    profile.gates.retain(|gate| gate.tasks.is_some());
    ensure!(!profile.gates.is_empty(), "PLAN_PROFILE_TASKS_MISSING");
    profile.persistence = Some(PersistenceAction::Snapshot);
    if !explicit {
        fn snapshots(gates: &mut [crate::tasks::profile::GateDef]) {
            for gate in gates {
                if gate.receipt || gate.proof.kind() == Some(ProofKind::Delivery) {
                    gate.persistence.post = Some(PersistenceAction::Snapshot);
                    gate.auto_commit = false;
                }
                snapshots(&mut gate.gates);
                if let Some(tasks) = &mut gate.tasks {
                    snapshots(&mut tasks.gates);
                }
            }
        }
        snapshots(&mut profile.gates);
    }
    profile.validate()?;
    for cursor in gate_graph::collect_cursors(&profile)? {
        ensure!(
            [cursor.gate.persistence.pre, cursor.gate.persistence.post]
                .into_iter()
                .all(|save| save.is_none_or(|save| matches!(
                    save,
                    PersistenceAction::Snapshot | PersistenceAction::None
                ))),
            "PLAN_PROFILE_COMMIT_FORBIDDEN"
        );
        ensure!(
            cursor.gate.receipt
                || matches!(
                    cursor.gate.driver(),
                    Some(GateDriver::Command | GateDriver::Agent)
                ),
            "PLAN_RUNNER_REQUIRED: {}",
            cursor.canonical_stage_id
        );
    }
    Ok(profile)
}

/// Run source-owned steps in-place, persisting completed observations as Git snapshots.
pub fn run_plan_workflow(
    root: &Path,
    plan: PlanDocument,
    profile: &GateProfile,
    explicit: bool,
) -> Result<Value> {
    let profile = plan_profile(profile, explicit)?;
    let order = plan.order()?;
    for step in &plan.steps {
        ensure!(
            step.status != "blocked",
            "PLAN_STEP_BLOCKED: {}",
            step.title
        );
        for scope in &step.scope {
            crate::tasks::confined_path(root, scope)?;
        }
        for dependency in &step.depends_on {
            ensure!(
                !plan
                    .steps
                    .iter()
                    .any(|candidate| &candidate.title == dependency
                        && candidate.status == "cancelled"),
                "PLAN_DEPENDENCY_INCOMPLETE: {dependency}"
            );
        }
    }
    let (repository, project) = crate::engine::project_identity(root)?;
    let contract = plan.task_contract(&repository, &project);
    let mut store = crate::engine::store(root)?;
    store.set_profile(std::sync::Arc::new(profile));
    let reference = plan.path.to_string_lossy().into_owned();
    if plan
        .steps
        .iter()
        .all(|step| matches!(step.status.as_str(), "completed" | "cancelled"))
        && contract.tasks.iter().zip(&plan.steps).all(|(task, step)| {
            step.status == "cancelled"
                || (step
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| !evidence.trim().is_empty())
                    && store.inspect(&contract.task_id(task)).is_ok_and(|stored| {
                        stored.status == "completed" && stored.receipt.is_some()
                    }))
        })
    {
        return Ok(
            json!({"status":"completed", "path":plan.path, "steps":plan.steps, "snapshots":[]}),
        );
    }
    let tasks = store.sync_plan(&contract, &reference, root)?;
    drop(store);
    let options = super::task_runner::TaskRunOptions {
        worktree: root.to_owned(),
        worker_id: "plan-coordinator".into(),
        session_fingerprint: "plan-coordinator-session".into(),
        runner_timeout: std::time::Duration::from_secs(3600),
    };
    let mut snapshots = Vec::new();
    let mut expected_source = plan.source.clone();
    for index in order {
        if plan.steps[index].status == "cancelled" {
            continue;
        }
        let id = contract.task_id(&contract.tasks[index]);
        let task = tasks
            .iter()
            .find(|task| task.task_id == id)
            .context("PLAN_TASK_MISSING")?;
        super::task_runner::run_task_lifecycle(root, &id, &options, &mut |_| {})?;
        if task.status != "completed" {
            let stored = crate::engine::store(root)?.inspect(&id)?;
            let expected = PlanDocument::from_source(&plan.path, expected_source)?;
            let expected_step = expected
                .steps
                .iter()
                .find(|step| step.title == plan.steps[index].title)
                .context("PLAN_SOURCE_DRIFT: step missing")?;
            expected_source = if serde_json::to_value(&expected_step.subtasks)?
                != serde_json::to_value(&stored.spec.subtasks)?
            {
                expected.project_subtasks(&stored.spec.task_ref, &stored.spec.subtasks)?
            } else {
                expected.source.clone()
            };
            let expected = PlanDocument::from_source(&plan.path, expected_source)?;
            let receipt = stored
                .receipt
                .as_ref()
                .context("PLAN_SOURCE_DRIFT: completed step missing receipt")?;
            expected_source = expected
                .project_completion(&stored.spec.task_ref, &serde_json::to_value(receipt)?)?;
            let actual = std::fs::read_to_string(crate::tasks::confined_path(root, &reference)?)?;
            ensure!(
                actual == expected_source,
                "PLAN_SOURCE_DRIFT: plan source changed outside task projection"
            );
            let name = format!(
                "plan-{}-{:x}",
                &format!("{:x}", Sha256::digest(reference.as_bytes()))[..16],
                Sha256::digest(id.as_bytes())
            );
            snapshots.push(git_snapshot::create_named_snapshot(
                root,
                &crate::engine::database_path(root)?,
                &name,
            )?);
        }
    }
    let source = std::fs::read_to_string(crate::tasks::confined_path(root, &reference)?)?;
    let projected = PlanDocument::from_source(&plan.path, source)?;
    Ok(
        json!({"status":"completed", "path":plan.path, "steps":projected.steps, "snapshots":snapshots}),
    )
}

pub(crate) fn gate_snapshot(
    root: &Path,
    path: &Path,
    step: &str,
    gate: &str,
    phase: &str,
) -> Result<()> {
    let plan_rel = path.strip_prefix(root).unwrap_or(path);
    let plan_tag = format!(
        "{:x}",
        Sha256::digest(plan_rel.to_string_lossy().as_bytes())
    );
    let name = format!(
        "plan-gate-{}-{:x}",
        &plan_tag[..16],
        Sha256::digest(format!(
            "{}\n{step}\n{gate}\n{phase}\n{}",
            path.display(),
            chrono::Utc::now()
                .timestamp_nanos_opt()
                .context("PLAN_CLOCK_INVALID")?
        ))
    );
    git_snapshot::create_named_snapshot(root, &crate::engine::database_path(root)?, &name)?;
    Ok(())
}
