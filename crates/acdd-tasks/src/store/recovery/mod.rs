//! SQLite-owned active task checkpoints. Git records committed state, never a gate decision.

use super::{now, Task, TasksStore};
use crate::git::git_command;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Write,
    ops::Deref,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const PENDING: &str = "active_checkpoint_pending";
const ROOT: &str = "active_checkpoint_root";

fn existing_commits(workspace: &Path, candidates: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    crate::git::git_commits_exist(workspace, candidates)
}

#[derive(Debug)]
/// SQL committed, but publication has not completed. Retrying may publish only this checkpoint.
pub struct CheckpointPending;
impl std::fmt::Display for CheckpointPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TASK_CHECKPOINT_PENDING: a managed task operation or committed checkpoint publication is incomplete. Do: retry checkpoint publication with milestone db-recover after the operation releases its fence; do not reset or roll back committed SQLite state.")
    }
}
impl std::error::Error for CheckpointPending {}

/// Recognizes committed publication failures without conflating ordinary transaction failures.
pub fn is_pending(error: &anyhow::Error) -> bool {
    error.downcast_ref::<CheckpointPending>().is_some()
}

struct LockedFile(File);

impl Drop for LockedFile {
    fn drop(&mut self) {
        // close() alone leaves flock held by descriptors inherited during fork.
        // The operation, rather than an unrelated child's eventual exec, owns it.
        let _ = self.0.unlock();
    }
}

#[derive(Clone)]
/// An operation-owned physical fence. Clones share its final explicit unlock.
pub struct DatabaseFence(std::sync::Arc<LockedFile>);

impl Deref for DatabaseFence {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0 .0
    }
}

/// The existing physical task database fence. Acquire before any SQLite writer transaction.
pub fn lock_database(database: &Path) -> Result<DatabaseFence> {
    if database == Path::new(":memory:") {
        bail!("checkpoint recovery requires a file-backed task store");
    }
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database.with_extension("receipt.lock"))?;
    file.lock()?;
    Ok(DatabaseFence(std::sync::Arc::new(LockedFile(file))))
}

/// Holds a nonblocking shared observation fence across file and SQLite composition.
/// Readers never wait behind a writer or take a SQLite writer lock.
pub fn read_database(database: &Path) -> Result<Option<DatabaseFence>> {
    if database == Path::new(":memory:") {
        bail!("checkpoint recovery requires a file-backed task store");
    }
    if !database.parent().is_some_and(Path::exists) {
        return Ok(None);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database.with_extension("receipt.lock"))?;
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(DatabaseFence(std::sync::Arc::new(LockedFile(file))))),
        Err(std::fs::TryLockError::WouldBlock) => Err(CheckpointPending.into()),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Detect a crashed operation's precommit intent before composing file-backed
/// authority with SQL. The caller holds the physical shared/exclusive fence.
pub fn ensure_no_intents(root: &Path) -> Result<()> {
    ensure_no_intents_except(root, &BTreeSet::new())
}

pub(crate) fn ensure_no_intents_except(root: &Path, obsolete: &BTreeSet<String>) -> Result<()> {
    let refs = git_command(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/acdd/state/"],
    )?;
    if !refs.status.success() {
        return Ok(());
    } // Non-Git workspaces have no keepers.
    let active_refs: Vec<String> = String::from_utf8(refs.stdout)?
        .lines()
        .filter(|reference| {
            !obsolete
                .iter()
                .any(|owner| reference.starts_with(&format!("refs/acdd/state/{owner}/")))
        })
        .map(str::to_owned)
        .collect();
    if active_refs.is_empty() {
        return Ok(());
    }
    let specs: Vec<String> = active_refs
        .iter()
        .map(|reference| format!("{reference}:checkpoint.json"))
        .collect();
    let blobs = crate::git::cat_file_batch(root, &specs)?;
    let expected_common = crate::git::git_common_dir(root)
        .map_err(|error| anyhow::anyhow!("TASK_RECOVERY_CHECKPOINT_INVALID: {error}"))?;
    let mut common_cache = BTreeMap::<PathBuf, PathBuf>::new();
    for (reference, payload) in active_refs.into_iter().zip(blobs) {
        let Some(bytes) = payload else {
            // Older candidate-only refs contain no checkpoint or task intent.
            continue;
        };
        let checkpoint: ActiveCheckpoint = serde_json::from_slice(&bytes)?;
        if !checkpoint.workspace.exists() {
            let _ = git_command(root, &["update-ref", "-d", &reference]);
            continue;
        }
        let validated = validate_parsed_checkpoint(
            root,
            &expected_common,
            &mut common_cache,
            &reference,
            checkpoint,
        )?;
        if validated.intent.is_some() {
            return Err(CheckpointPending.into());
        }
    }
    Ok(())
}

/// Pins the metadata check and subsequent row reads to one deferred SQLite snapshot.
pub(crate) struct ReadSnapshot<'a>(Option<Transaction<'a>>);
impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        self.0.take();
    }
}
pub(crate) fn read_snapshot(connection: &Connection) -> Result<ReadSnapshot<'_>> {
    let snapshot = if connection.is_autocommit() {
        Some(connection.unchecked_transaction()?)
    } else {
        None
    };
    ensure_observable(connection)?;
    Ok(ReadSnapshot(snapshot))
}
pub(crate) fn ensure_observable(connection: &Connection) -> Result<()> {
    if connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_store_metadata WHERE key=?1)",
        [PENDING],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(CheckpointPending.into());
    }
    Ok(())
}

/// Stored gate tuple (gate, revision, state, evidence).
pub type StoredGateRecord = (String, u64, String, Option<String>);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A real task's active SQL provenance, with revision reservations independent of claim state.
pub struct ActiveCheckpoint {
    version: u32,
    /// Canonical repository owner.
    pub repository: String,
    /// Canonical project owner, with aliases already applied once.
    pub project: String,
    /// Admitted milestone identity.
    pub milestone_id: String,
    /// Genuine task owner, never a synthetic coordinator.
    pub task_id: String,
    /// Worktree containing the checkpoint's actual Git object home.
    pub workspace: PathBuf,
    /// Committed active descriptor, absent for a deleted task's allocation fence.
    pub task: Option<Task>,
    /// Maximum allocated revision, including revoked reservations.
    pub max_revision: u64,
    pub(crate) claims: Vec<(u64, String, String, i64, bool)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    claim_session_fingerprints: BTreeMap<u64, String>,
    pub(crate) gates: Vec<StoredGateRecord>,
    findings: Vec<(u64, String)>,
    scopes: Vec<(String, String, bool)>,
    dependencies: Vec<(String, bool)>,
    pub(crate) submissions: Vec<(u64, String)>,
    prepared_proofs: BTreeSet<String>,
    intent: Option<String>,
    cohort: Vec<(PathBuf, String)>,
    cohort_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    active: Vec<ActiveCheckpoint>,
    retired: Vec<ActiveCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest_replacement: Option<ManifestReplacement>,
}

/// A manifest replacement committed as part of the SQLite publication intent.
///
/// The payload stores the exact relative path, the expected current-content hash,
/// and the target bytes/hash so recovery can finish the write without re-running
/// the task mutation.

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestReplacement {
    root: PathBuf,
    relative_path: String,
    before_hash: String,
    target_hash: String,
    target: Vec<u8>,
}

impl ManifestReplacement {
    pub(crate) fn new(
        root: &Path,
        relative_path: &str,
        before: &[u8],
        after: &[u8],
    ) -> Result<Self> {
        let root = crate::core::fs::canonical_root(root)?;
        let relative = crate::tasks::relative_path(relative_path)?;
        if relative.to_str() != Some(relative_path)
            || relative
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("md")
        {
            bail!("TASK_MANIFEST_PUBLICATION_INVALID: expected an exact relative Markdown path");
        }
        let path = crate::tasks::confined_path(&root, relative_path)?;
        if path != root.join(&relative) || !path.is_file() {
            bail!("TASK_MANIFEST_PUBLICATION_INVALID: manifest path is not a file");
        }
        let current = std::fs::read(&path)?;
        let before_hash = crate::core::commitments::hash(before);
        if current.as_slice() != before || crate::core::commitments::hash(&current) != before_hash {
            bail!("TASK_MANIFEST_PUBLICATION_CONFLICT: manifest changed before staging");
        }
        let target_hash = crate::core::commitments::hash(after);
        Ok(Self {
            root,
            relative_path: relative_path.to_owned(),
            before_hash,
            target_hash,
            target: after.to_vec(),
        })
    }

    fn validate(&self) -> Result<PathBuf> {
        let root = crate::core::fs::canonical_root(&self.root)?;
        if root != self.root
            || crate::tasks::relative_path(&self.relative_path)?.to_str()
                != Some(self.relative_path.as_str())
            || Path::new(&self.relative_path)
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("md")
            || crate::core::commitments::hash(&self.target) != self.target_hash
        {
            bail!("TASK_MANIFEST_PUBLICATION_INVALID");
        }
        let path = crate::tasks::confined_path(&root, &self.relative_path)?;
        if path != root.join(&self.relative_path) || !path.is_file() {
            bail!("TASK_MANIFEST_PUBLICATION_INVALID: manifest path is not a file");
        }
        Ok(path)
    }

    fn validate_preimage(&self) -> Result<()> {
        let path = self.validate()?;
        let current = std::fs::read(path)?;
        let current_hash = crate::core::commitments::hash(&current);
        if current_hash != self.before_hash && current_hash != self.target_hash {
            bail!("TASK_MANIFEST_PUBLICATION_CONFLICT: manifest no longer matches its preimage or target");
        }
        Ok(())
    }

    fn publish(&self) -> Result<()> {
        let path = self.validate()?;
        let current = std::fs::read(&path)?;
        let current_hash = crate::core::commitments::hash(&current);
        if current_hash == self.target_hash {
            return Ok(());
        }
        if current_hash != self.before_hash {
            bail!("TASK_MANIFEST_PUBLICATION_CONFLICT: manifest no longer matches its preimage or target");
        }
        crate::core::fs::atomic_replace(&path, &self.target)?;
        let published = std::fs::read(&path)?;
        if crate::core::commitments::hash(&published) != self.target_hash {
            bail!("TASK_MANIFEST_PUBLICATION_CONFLICT: published manifest does not match target");
        }
        Ok(())
    }
}

impl Publication {
    fn publish(&self) -> Result<()> {
        self.publish_with(|replacement| replacement.publish())
    }

    fn publish_with(
        &self,
        publish_manifest: impl FnMut(&ManifestReplacement) -> Result<()>,
    ) -> Result<()> {
        self.publish_with_known(&BTreeMap::new(), publish_manifest)
    }

    fn publish_with_known(
        &self,
        known: &BTreeMap<(PathBuf, String), BTreeSet<String>>,
        mut publish_manifest: impl FnMut(&ManifestReplacement) -> Result<()>,
    ) -> Result<()> {
        publish_cohort(&self.active, known)?;
        let mut retired_by_repo = BTreeMap::<PathBuf, Vec<(&Path, String)>>::new();
        for state in &self.retired {
            if state.task.is_some() || state.intent.is_some() {
                bail!("TASK_CHECKPOINT_RETIREMENT_INVALID");
            }
            let common = crate::git::git_common_dir(&state.workspace)?;
            retired_by_repo
                .entry(common)
                .or_default()
                .push((&state.workspace, state.reference()));
        }
        for (_repo, items) in retired_by_repo {
            let repo_root = items[0].0;
            let refs_out = git_command(
                repo_root,
                &["for-each-ref", "--format=%(refname)\t%(objectname)", "refs/acdd/state/"],
            )?;
            let mut tip_by_ref = BTreeMap::<String, String>::new();
            if refs_out.status.success() {
                for line in String::from_utf8(refs_out.stdout)?.lines() {
                    if let Some((name, oid)) = line.split_once('\t') {
                        tip_by_ref.insert(name.to_owned(), oid.to_owned());
                    }
                }
            }
            let mut updates = String::new();
            for (_ws, reference) in items {
                if let Some(previous) = tip_by_ref.get(&reference) {
                    updates.push_str(&format!("delete {reference} {previous}\n"));
                }
            }
            if !updates.is_empty() {
                let mut child = Command::new("git")
                    .args(["-c", "core.fsync=committed", "update-ref", "--stdin"])
                    .current_dir(repo_root)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                child.stdin.take().expect("piped stdin").write_all(updates.as_bytes())?;
                let deleted = child.wait_with_output()?;
                if !deleted.status.success() {
                    bail!(
                        "TASK_CHECKPOINT_RETIREMENT_FAILED: {}",
                        String::from_utf8_lossy(&deleted.stderr)
                    );
                }
            }
        }
        if let Some(replacement) = &self.manifest_replacement {
            publish_manifest(replacement)?;
        }
        Ok(())
    }
}

type CandidateObjects = (BTreeSet<String>, Vec<(Option<PathBuf>, String)>);

impl ActiveCheckpoint {
    fn allocation_fence(mut self) -> Self {
        self.task = None;
        self.claims.clear();
        self.gates.clear();
        self.findings.clear();
        self.scopes.clear();
        self.dependencies.clear();
        self.submissions.clear();
        self.prepared_proofs.clear();
        self.intent = None;
        self.cohort.clear();
        self
    }
    /// The canonical per-task ref; no store-wide owner is created.
    pub fn reference(&self) -> String {
        let slug = self
            .task_id
            .rsplit_once(':')
            .map(|(_, slug)| slug)
            .unwrap_or(&self.task_id);
        format!(
            "refs/acdd/state/{}/{}/{}/{slug}",
            self.repository, self.project, self.milestone_id
        )
    }
    fn collect_candidate_objects(&self) -> Result<CandidateObjects> {
        let mut candidates = self.prepared_proofs.clone();
        let mut proof_candidates: Vec<(Option<PathBuf>, String)> = self
            .prepared_proofs
            .iter()
            .map(|s| (None, s.clone()))
            .collect();
        for (_, _, _, evidence) in &self.gates {
            if let Some(evidence) = evidence {
                let evidence: crate::tasks::gates::Evidence = serde_json::from_str(evidence)
                    .context(
                        "TASK_GATE_EVIDENCE_CORRUPTED: cannot checkpoint malformed gate evidence",
                    )?;
                let wt = PathBuf::from(&evidence.worktree);
                for sha in evidence
                    .commit
                    .into_iter()
                    .chain(evidence.candidate_baseline_head)
                {
                    if matches!(sha.len(), 40 | 64)
                        && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        candidates.insert(sha.clone());
                        proof_candidates.push((Some(wt.clone()), sha));
                    }
                }
            }
        }
        fn objects(value: &serde_json::Value, into: &mut BTreeSet<String>) {
            match value {
                serde_json::Value::String(s)
                    if matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    into.insert(s.clone());
                }
                serde_json::Value::Object(map) => {
                    for value in map.values() {
                        objects(value, into);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        objects(value, into);
                    }
                }
                _ => (),
            }
        }
        if let Some(task) = &self.task {
            objects(&serde_json::to_value(task)?, &mut candidates);
        }
        for (_, _, _, evidence) in &self.gates {
            if let Some(evidence) = evidence {
                objects(
                    &serde_json::from_str(evidence).context(
                        "TASK_GATE_EVIDENCE_CORRUPTED: cannot checkpoint malformed gate evidence",
                    )?,
                    &mut candidates,
                );
            }
        }
        for (_, submission) in &self.submissions {
            objects(&serde_json::from_str(submission)?, &mut candidates);
        }
        Ok((candidates, proof_candidates))
    }
    fn publish(&self) -> Result<()> {
        self.publish_with_known(&BTreeSet::new()).map(|_| ())
    }
    fn publish_with_known(&self, known: &BTreeSet<String>) -> Result<BTreeSet<String>> {
        let mut known_map = BTreeMap::new();
        if !known.is_empty() {
            known_map.insert((self.workspace.clone(), self.task_id.clone()), known.clone());
        }
        let mut parents = publish_checkpoints_batch(std::slice::from_ref(self), &known_map)?;
        Ok(parents.pop().unwrap_or_default().into_iter().collect())
    }
    /// Refuse ambiguous checkpoints and incomplete multi-task publication cohorts.
    pub fn validate_cohort(&self) -> Result<()> {
        validate_cohort_states(std::slice::from_ref(self))
    }
}

fn resolve_batch_parents(
    states: &[ActiveCheckpoint],
    known: &BTreeMap<(PathBuf, String), BTreeSet<String>>,
) -> Result<Vec<Vec<String>>> {
    let mut collected = Vec::with_capacity(states.len());
    let mut unchecked_by_workspace = BTreeMap::<&Path, BTreeSet<String>>::new();
    for state in states {
        let (candidates, proof_candidates) = state.collect_candidate_objects()?;
        let known_for_state = known.get(&(state.workspace.clone(), state.task_id.clone()));
        for sha in &candidates {
            if !known_for_state.is_some_and(|set| set.contains(sha)) {
                unchecked_by_workspace
                    .entry(state.workspace.as_path())
                    .or_default()
                    .insert(sha.clone());
            }
        }
        collected.push((candidates, proof_candidates, known_for_state));
    }
    let mut existing_by_workspace = BTreeMap::<&Path, BTreeSet<String>>::new();
    for (workspace, unchecked) in unchecked_by_workspace {
        existing_by_workspace.insert(workspace, existing_commits(workspace, &unchecked)?);
    }
    let mut extra_by_worktree = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    for (index, (_, proof_candidates, known_for_state)) in collected.iter().enumerate() {
        let ws_existing = existing_by_workspace.get(states[index].workspace.as_path());
        for (worktree, sha) in proof_candidates {
            let in_known = known_for_state.is_some_and(|set| set.contains(sha));
            if !in_known && !ws_existing.is_some_and(|set| set.contains(sha)) {
                if let Some(wt) = worktree {
                    extra_by_worktree
                        .entry(wt.clone())
                        .or_default()
                        .insert(sha.clone());
                }
            }
        }
    }
    let worktree_existing: BTreeMap<PathBuf, BTreeSet<String>> = extra_by_worktree
        .into_iter()
        .map(|(wt, shas)| {
            let found = existing_commits(&wt, &shas).unwrap_or_default();
            (wt, found)
        })
        .collect();

    let mut result = Vec::with_capacity(states.len());
    for (index, (candidates, proof_candidates, known_for_state)) in
        collected.into_iter().enumerate()
    {
        let ws_existing = existing_by_workspace.get(states[index].workspace.as_path());
        let mut existing = BTreeSet::new();
        for sha in candidates {
            if known_for_state.is_some_and(|set| set.contains(&sha))
                || ws_existing.is_some_and(|set| set.contains(&sha))
            {
                existing.insert(sha);
            }
        }
        for (worktree, sha) in &proof_candidates {
            let in_worktree = !existing.contains(sha)
                && worktree
                    .as_ref()
                    .and_then(|w| worktree_existing.get(w))
                    .is_some_and(|shas| shas.contains(sha));
            if !existing.contains(sha) && !in_worktree {
                bail!("TASK_CHECKPOINT_PROOF_UNAVAILABLE: {sha} has no object home");
            }
        }
        result.push(existing.into_iter().collect());
    }
    Ok(result)
}

fn publish_checkpoints_batch(
    states: &[ActiveCheckpoint],
    known: &BTreeMap<(PathBuf, String), BTreeSet<String>>,
) -> Result<Vec<Vec<String>>> {
    publish_checkpoints_batch_on_repo(states, known, |_, _| Ok(()))
}

fn publish_checkpoints_batch_on_repo(
    states: &[ActiveCheckpoint],
    known: &BTreeMap<(PathBuf, String), BTreeSet<String>>,
    mut before_repo_publish: impl FnMut(&[usize], &[Vec<String>]) -> Result<()>,
) -> Result<Vec<Vec<String>>> {
    if states.is_empty() {
        return Ok(Vec::new());
    }
    let all_parents = resolve_batch_parents(states, known)?;
    let mut by_repo = BTreeMap::<PathBuf, Vec<(usize, String, Vec<u8>)>>::new();
    for (index, state) in states.iter().enumerate() {
        if !state.workspace.is_absolute() {
            bail!("TASK_CHECKPOINT_HOME_INVALID: object home must be an absolute worktree");
        }
        let common = crate::git::git_common_dir(&state.workspace)?;
        let reference = state.reference();
        let bytes = serde_json::to_vec(state)?;
        by_repo
            .entry(common)
            .or_default()
            .push((index, reference, bytes));
    }
    for (_common, items) in by_repo {
        let batch_root = &states[items[0].0].workspace;
        let entries: Vec<crate::git::GitCheckpointEntry<'_>> = items
            .iter()
            .map(|(index, reference, bytes)| crate::git::GitCheckpointEntry {
                reference,
                bytes,
                parents: &all_parents[*index],
            })
            .collect();
        // Register rollback compensation before fast-import mutates any ref in
        // this batch so a partial multi-ref failure still cleans up updated refs.
        let indices: Vec<usize> = items.iter().map(|(index, _, _)| *index).collect();
        before_repo_publish(&indices, &all_parents)?;
        crate::git::git_checkpoints_batch(batch_root, &entries)?;
        let release_items: Vec<(&str, &str)> = items
            .iter()
            .filter_map(|(index, reference, _)| {
                states[*index]
                    .intent
                    .is_none()
                    .then_some((states[*index].task_id.as_str(), reference.as_str()))
            })
            .collect();
        crate::git::release_provisional_batch(batch_root, &release_items)?;
    }
    Ok(all_parents)
}

fn validate_cohort_states(states: &[ActiveCheckpoint]) -> Result<()> {
    let mut unique_keys = BTreeSet::<(PathBuf, String)>::new();
    for state in states {
        if state.version != 1 || state.intent.is_some() {
            bail!("TASK_RECOVERY_AMBIGUOUS: unresolved task intent or unsupported checkpoint version; acknowledged state cannot be inferred");
        }
        for (root, reference) in &state.cohort {
            unique_keys.insert((root.clone(), reference.clone()));
        }
    }
    if unique_keys.is_empty() {
        return Ok(());
    }
    let pairs: Vec<(PathBuf, String)> = unique_keys.into_iter().collect();
    let loaded = read_checkpoints_batch(&pairs)?;
    let mut peers = BTreeMap::<(PathBuf, String), ActiveCheckpoint>::new();
    for (key, loaded_cp) in pairs.into_iter().zip(loaded) {
        let reference = &key.1;
        let peer = loaded_cp.map_err(|_| {
            anyhow::anyhow!("TASK_RECOVERY_CHECKPOINT_INVALID: cannot read {reference}")
        })?;
        peers.insert(key, peer);
    }
    for state in states {
        for (root, reference) in &state.cohort {
            let peer = &peers[&(root.clone(), reference.clone())];
            if peer.intent.is_some()
                || peer.cohort != state.cohort
                || peer.cohort_id != state.cohort_id
            {
                bail!("TASK_RECOVERY_AMBIGUOUS: incomplete task publication cohort");
            }
        }
    }
    Ok(())
}

fn validate_parsed_checkpoint(
    root: &Path,
    expected_common: &Path,
    common_cache: &mut BTreeMap<PathBuf, PathBuf>,
    reference: &str,
    checkpoint: ActiveCheckpoint,
) -> Result<ActiveCheckpoint> {
    if !checkpoint.workspace.exists() {
        bail!(
            "TASK_RECOVERY_CHECKPOINT_INVALID: checkpoint '{reference}' references missing worktree '{}'. The worktree was removed from disk.",
            checkpoint.workspace.display()
        );
    }
    let home_common = if checkpoint.workspace == root {
        expected_common.to_path_buf()
    } else if let Some(cached) = common_cache.get(&checkpoint.workspace) {
        cached.clone()
    } else {
        let resolved = crate::git::git_common_dir(&checkpoint.workspace)
            .map_err(|error| anyhow::anyhow!("TASK_RECOVERY_CHECKPOINT_INVALID: {error}"))?;
        common_cache.insert(checkpoint.workspace.clone(), resolved.clone());
        resolved
    };
    if checkpoint.reference() != reference || home_common != expected_common {
        bail!(
            "TASK_RECOVERY_CHECKPOINT_INVALID: checkpoint owner does not match its ref or worktree"
        );
    }
    for part in [
        &checkpoint.repository,
        &checkpoint.project,
        &checkpoint.milestone_id,
    ] {
        crate::tasks::valid_identity(part)?;
    }
    Ok(checkpoint)
}

pub(crate) fn try_read_checkpoints_batch(
    queries: &[(PathBuf, String)],
) -> Result<Vec<Result<Option<ActiveCheckpoint>>>> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let mut results: Vec<Option<Result<Option<ActiveCheckpoint>>>> =
        (0..queries.len()).map(|_| None).collect();
    let mut by_root = BTreeMap::<&Path, Vec<(usize, &str)>>::new();
    for (index, (root, reference)) in queries.iter().enumerate() {
        by_root.entry(root.as_path()).or_default().push((index, reference.as_str()));
    }
    let mut common_cache = BTreeMap::<PathBuf, PathBuf>::new();
    for (root, items) in by_root {
        let specs: Vec<String> = items
            .iter()
            .map(|(_, reference)| format!("{reference}:checkpoint.json"))
            .collect();
        let blobs = crate::git::cat_file_batch(root, &specs)?;
        let expected_common = if let Some(cached) = common_cache.get(root) {
            cached.clone()
        } else {
            let resolved = crate::git::git_common_dir(root)
                .map_err(|error| anyhow::anyhow!("TASK_RECOVERY_CHECKPOINT_INVALID: {error}"))?;
            common_cache.insert(root.to_path_buf(), resolved.clone());
            resolved
        };
        for ((index, reference), payload) in items.into_iter().zip(blobs) {
            let parsed = match payload {
                None => Ok(None),
                Some(bytes) => match serde_json::from_slice::<ActiveCheckpoint>(&bytes) {
                    Ok(cp) => validate_parsed_checkpoint(
                        root,
                        &expected_common,
                        &mut common_cache,
                        reference,
                        cp,
                    )
                    .map(Some),
                    Err(error) => Err(error.into()),
                },
            };
            results[index] = Some(parsed);
        }
    }
    Ok(results.into_iter().map(|slot| slot.expect("slot populated")).collect())
}

pub(crate) fn read_checkpoints_batch(
    queries: &[(PathBuf, String)],
) -> Result<Vec<Result<ActiveCheckpoint>>> {
    Ok(try_read_checkpoints_batch(queries)?
        .into_iter()
        .zip(queries)
        .map(|(result, (_, reference))| match result {
            Ok(Some(cp)) => Ok(cp),
            Ok(None) => Err(anyhow::anyhow!(
                "TASK_RECOVERY_CHECKPOINT_INVALID: cannot read {reference}"
            )),
            Err(error) => Err(error),
        })
        .collect())
}

/// Reads a strict checkpoint from a real task ref, leaving all Git objects unchanged.
pub fn read_checkpoint(root: &Path, reference: &str) -> Result<ActiveCheckpoint> {
    read_checkpoint_revision(root, reference, reference)
}

fn read_checkpoint_revision(
    root: &Path,
    reference: &str,
    revision: &str,
) -> Result<ActiveCheckpoint> {
    let output = git_command(root, &["show", &format!("{revision}:checkpoint.json")])?;
    if !output.status.success() {
        bail!("TASK_RECOVERY_CHECKPOINT_INVALID: cannot read {reference}");
    }
    let checkpoint: ActiveCheckpoint = serde_json::from_slice(&output.stdout)?;
    let expected_common = crate::git::git_common_dir(root)
        .map_err(|error| anyhow::anyhow!("TASK_RECOVERY_CHECKPOINT_INVALID: {error}"))?;
    let mut common_cache = BTreeMap::<PathBuf, PathBuf>::new();
    validate_parsed_checkpoint(
        root,
        &expected_common,
        &mut common_cache,
        reference,
        checkpoint,
    )
}

pub(crate) enum StoreHealth {
    Healthy,
    Corrupt,
}

/// Probe only a private byte-for-byte copy, so refused stores retain their WAL/SHM bytes.
pub(crate) fn probe_copy(path: &Path) -> Result<StoreHealth> {
    fn inspect(path: &Path) -> Result<StoreHealth> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let index: u64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('nodes','edges','files')",
            [],
            |r| r.get(0),
        )?;
        if index >= 2 {
            bail!("TASK_DATABASE_COLLISION: recovery refuses a code map database");
        }
        let metadata: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='task_store_metadata' AND type='table')",
            [],
            |r| r.get(0),
        )?;
        if !metadata {
            bail!("TASK_RECOVERY_DATABASE_UNKNOWN: no task store metadata; preserve the original database");
        }
        let version: String = connection.query_row(
            "SELECT value FROM task_store_metadata WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )?;
        if !matches!(version.as_str(), "5" | "6" | "7") {
            bail!("TASK_RECOVERY_SCHEMA_UNSUPPORTED: schema {version}; preserve the original database");
        }
        let integrity: String = connection.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(if integrity == "ok" {
            StoreHealth::Healthy
        } else {
            StoreHealth::Corrupt
        })
    }
    match inspect(path) {
        Err(error)
            if error
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseCorrupt
                                | rusqlite::ErrorCode::NotADatabase
                        )
                    )
                }) =>
        {
            Ok(StoreHealth::Corrupt)
        }
        result => result,
    }
}

#[derive(Clone)]
pub(crate) struct CheckpointContext {
    root: PathBuf,
    database: PathBuf,
    _fence: DatabaseFence,
    preparations: Vec<PreparationOwner>,
}

#[derive(Clone)]
pub(crate) struct PreparationOwner {
    pub identity: String,
    pub worktree: PathBuf,
    pub proofs: BTreeSet<String>,
}

pub(crate) struct ManagedTransaction<'a> {
    tx: Option<Transaction<'a>>,
    context: Option<CheckpointContext>,
    database_path: Option<PathBuf>,
    publication_fence: Option<DatabaseFence>,
    manifest_replacement: Option<ManifestReplacement>,
    before: Vec<ActiveCheckpoint>,
    known_parents: BTreeMap<(PathBuf, String), BTreeSet<String>>,
    frozen: Option<Vec<ActiveCheckpoint>>,
    retired: Option<String>,
    new_admissions: BTreeSet<String>,
    covered: BTreeSet<String>,
    original_rows: BTreeMap<String, Vec<String>>,
    intent_token: String,
}
impl<'a> Deref for ManagedTransaction<'a> {
    type Target = Transaction<'a>;
    fn deref(&self) -> &Self::Target {
        self.tx.as_ref().expect("active transaction")
    }
}

pub(crate) fn transaction(
    connection: &mut Connection,
    context: Option<CheckpointContext>,
) -> Result<ManagedTransaction<'_>> {
    let database_path = context
        .as_ref()
        .map(|context| context.database.clone())
        .or_else(|| connection.path().map(PathBuf::from));
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_observable(&tx)?;
    let original_rows = if context.is_some() {
        task_rows(&tx)?
    } else {
        BTreeMap::new()
    };
    let managed = ManagedTransaction {
        tx: Some(tx),
        context,
        database_path,
        publication_fence: None,
        manifest_replacement: None,
        before: Vec::new(),
        known_parents: BTreeMap::new(),
        frozen: None,
        retired: None,
        new_admissions: BTreeSet::new(),
        covered: BTreeSet::new(),
        original_rows,
        intent_token: format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ),
    };
    Ok(managed)
}

// Comparison-only observation: unrelated history is never checkpoint payload or
// enrollment input. Any undeclared task delta aborts before SQLite COMMIT.
fn task_rows(connection: &Connection) -> Result<BTreeMap<String, Vec<String>>> {
    let mut rows = BTreeMap::<String, Vec<String>>::new();
    for table in [
        "tasks",
        "task_revisions",
        "task_claims",
        "task_gates",
        "task_findings",
        "task_scope_paths",
        "task_dependencies",
        "task_subtasks",
        "task_submissions",
    ] {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let count = statement.column_count();
        let values = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (0..count)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            ))
        })?;
        for value in values {
            let (id, value) = value?;
            rows.entry(id)
                .or_default()
                .push(format!("{table}:{value:?}"));
        }
    }
    Ok(rows)
}

fn publish_cohort(
    states: &[ActiveCheckpoint],
    known: &BTreeMap<(PathBuf, String), BTreeSet<String>>,
) -> Result<()> {
    publish_checkpoints_batch(states, known)?;
    validate_cohort_states(states)?;
    Ok(())
}

impl ManagedTransaction<'_> {
    fn persistent_database_path(&mut self) -> Result<PathBuf> {
        let path = self
            .database_path
            .as_deref()
            .context("manifest publication requires a file-backed task store")?;
        if path == Path::new(":memory:") || path.to_string_lossy().starts_with("file:") {
            bail!("manifest publication requires a file-backed task store");
        }
        let path = path.canonicalize()?;
        if !path.is_file() {
            bail!("manifest publication requires a file-backed task store");
        }
        self.database_path = Some(path.clone());
        Ok(path)
    }

    /// Stage a manifest replacement in the same durable publication as this SQL transaction.
    pub(crate) fn stage_manifest_replacement(
        &mut self,
        replacement: ManifestReplacement,
    ) -> Result<()> {
        if self.manifest_replacement.is_some() {
            bail!("TASK_MANIFEST_PUBLICATION_INVALID: only one manifest replacement may be staged");
        }
        if self.context.is_none() {
            let database = self.persistent_database_path()?;
            self.publication_fence = Some(lock_database(&database)?);
        }
        replacement.validate_preimage()?;
        self.manifest_replacement = Some(replacement);
        Ok(())
    }

    /// Declare potential writers before any owned file or SQL side effect.
    /// Initial enrollment includes genuine peers of the same admitted milestone;
    /// subsequent publication preserves their cohort without allocating peer revisions.
    pub(crate) fn enroll(&mut self, ids: &[String], readmission: bool) -> Result<()> {
        let Some(context) = self.context.clone() else {
            return Ok(());
        };
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        let mut covered = requested.clone();
        for id in &requested {
            if let Some(task) = super::load(self, id)? {
                let milestone =
                    validate_owner(&task, &task_home(self, &task, &context)?, readmission)?;
                covered.extend(milestone.tasks.iter().map(|spec| milestone.task_id(spec)));
            }
        }
        // Old cohorts must be replaced together: no peer may retain an ambiguous
        // acknowledgement merely because another peer receives a newer checkpoint.
        let mut visited_cohorts = BTreeSet::new();
        let mut checked_refs = BTreeSet::new();
        let mut before = loop {
            let current = states(self, &context, &covered, readmission)?;
            let pending_queries: Vec<(PathBuf, String)> = current
                .iter()
                .map(|s| (s.workspace.clone(), s.reference()))
                .filter(|pair| checked_refs.insert(pair.clone()))
                .collect();
            let mut extended = false;
            if !pending_queries.is_empty() {
                let checkpoints = try_read_checkpoints_batch(&pending_queries)?;
                for result in checkpoints {
                    let Some(previous) = result? else {
                        continue;
                    };
                    if visited_cohorts.insert(previous.cohort.clone()) {
                        let new_peers: Vec<(PathBuf, String)> = previous
                            .cohort
                            .into_iter()
                            .filter(|pair| checked_refs.insert(pair.clone()))
                            .collect();
                        if !new_peers.is_empty() {
                            for peer_res in read_checkpoints_batch(&new_peers)? {
                                let peer = peer_res?;
                                extended |= covered.insert(peer.task_id);
                            }
                        }
                    }
                }
            }
            if !extended {
                break current;
            }
        };
        if !self.covered.is_empty() {
            before.retain(|state| !self.covered.contains(&state.task_id));
        }
        // Validate the whole requested cohort before publishing even its first intent.
        for state in &mut before {
            if requested.contains(&state.task_id) {
                state.max_revision = state
                    .max_revision
                    .checked_add(1)
                    .context("task revision exhausted")?;
            }
        }
        let intents: Vec<ActiveCheckpoint> = before
            .iter()
            .map(|state| {
                let mut intent = state.clone();
                intent.intent = Some(self.intent_token.clone());
                intent
            })
            .collect();
        self.record_intents_with_compensation(before, &intents, |state| {
            requested.contains(&state.task_id)
        })?;
        self.covered.extend(covered);
        Ok(())
    }

    fn record_intents_with_compensation(
        &mut self,
        states: Vec<ActiveCheckpoint>,
        intents: &[ActiveCheckpoint],
        record_revision: impl Fn(&ActiveCheckpoint) -> bool,
    ) -> Result<()> {
        let mut slots: Vec<Option<ActiveCheckpoint>> = states.into_iter().map(Some).collect();
        let mut revision_updates: Vec<(String, u64)> = Vec::new();
        publish_checkpoints_batch_on_repo(intents, &BTreeMap::new(), |indices, all_parents| {
            for &index in indices {
                let state = slots[index].take().expect("unique intent index");
                self.known_parents
                    .entry((state.workspace.clone(), state.task_id.clone()))
                    .or_default()
                    .extend(all_parents[index].iter().cloned());
                if record_revision(&state) {
                    revision_updates.push((state.task_id.clone(), state.max_revision - 1));
                }
                self.before.push(state);
            }
            Ok(())
        })?;
        for (task_id, revision) in revision_updates {
            self.execute(
                "INSERT INTO task_revisions VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET revision=MAX(revision,excluded.revision)",
                params![task_id, revision],
            )?;
        }
        Ok(())
    }

    pub(crate) fn enroll_dependencies(&mut self, changes: &[(String, bool)]) -> Result<()> {
        let mut ids = BTreeSet::new();
        for (dependency, satisfied) in changes {
            ids.extend(self.prepare("SELECT task_id FROM task_dependencies WHERE dependency_id=?1 AND satisfied!=?2")?
                .query_map(params![dependency, satisfied], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        for id in &ids {
            if super::load(self, id)?.is_none() {
                bail!("TASK_RECOVERY_LEGACY_UNENROLLED: {id} has no admitted dependency owner");
            }
        }
        self.enroll(&ids.into_iter().collect::<Vec<_>>(), false)
    }

    fn ensure_coverage(&self) -> Result<()> {
        if self.context.is_none() {
            return Ok(());
        }
        let after = task_rows(self)?;
        for id in self.original_rows.keys().chain(after.keys()) {
            if self.original_rows.get(id) != after.get(id)
                && (!self.covered.contains(id)
                    || !self.before.iter().any(|state| &state.task_id == id))
            {
                bail!("TASK_CHECKPOINT_COHORT_UNDECLARED: {id}; SQL mutation has no validated owner intent");
            }
        }
        Ok(())
    }
    pub(crate) fn is_new_admission(&self, id: &str) -> bool {
        self.new_admissions.contains(id)
    }
    pub(crate) fn reserve_admissions(
        &mut self,
        milestone: &crate::tasks::Milestone,
        selected: Option<&str>,
    ) -> Result<()> {
        let Some(context) = &self.context else {
            return Ok(());
        };
        let mut fences = Vec::new();
        let workspace = context.root.canonicalize()?;
        for spec in &milestone.tasks {
            if selected.is_some_and(|selected| selected != spec.task_ref) {
                continue;
            }
            let id = milestone.task_id(spec);
            self.covered.insert(id.clone());
            if self.before.iter().any(|state| state.task_id == id) {
                continue;
            }
            milestone.digest(spec)?;
            let revision = self
                .query_row(
                    "SELECT revision FROM task_revisions WHERE task_id=?1",
                    [&id],
                    |row| row.get::<_, u64>(0),
                )
                .optional()?;
            if revision.is_none() {
                self.new_admissions.insert(id.clone());
            }
            fences.push(ActiveCheckpoint {
                version: 1,
                repository: milestone.repository.clone(),
                project: milestone.project.clone(),
                milestone_id: milestone.id.clone(),
                task_id: id,
                workspace: workspace.clone(),
                task: None,
                max_revision: revision.unwrap_or(0),
                claims: Vec::new(),
                claim_session_fingerprints: BTreeMap::new(),
                gates: Vec::new(),
                findings: Vec::new(),
                scopes: Vec::new(),
                dependencies: Vec::new(),
                submissions: Vec::new(),
                prepared_proofs: BTreeSet::new(),
                intent: None,
                cohort: Vec::new(),
                cohort_id: String::new(),
            });
        }
        if fences.is_empty() {
            return Ok(());
        }
        let queries: Vec<(PathBuf, String)> = fences
            .iter()
            .map(|fence| (fence.workspace.clone(), fence.reference()))
            .collect();
        let previous_checkpoints = try_read_checkpoints_batch(&queries)?;
        let token = format!("admission-{}-{}", std::process::id(), now());
        for (fence, prev_result) in fences.iter_mut().zip(previous_checkpoints) {
            if let Some(previous) = prev_result? {
                fence.max_revision = fence.max_revision.max(previous.max_revision);
            }
            fence.max_revision = fence
                .max_revision
                .checked_add(1)
                .context("task revision exhausted")?;
        }
        let intents: Vec<ActiveCheckpoint> = fences
            .iter()
            .map(|fence| {
                let mut intent = fence.clone();
                intent.intent = Some(token.clone());
                intent
            })
            .collect();
        self.record_intents_with_compensation(fences, &intents, |_| true)?;
        Ok(())
    }
    /// Freeze a handoff's outgoing and foreign dependency deltas before the pruning decision.
    pub(crate) fn freeze_handoff(
        &mut self,
        outgoing: &str,
        completed: &BTreeSet<String>,
    ) -> Result<()> {
        if let Some(context) = &self.context {
            self.ensure_coverage()?;
            let mut after = states(self, context, &self.covered, true)?;
            for state in &mut after {
                for (dependency, satisfied) in &mut state.dependencies {
                    if completed.contains(dependency) {
                        *satisfied = true;
                    }
                }
            }
            after.retain(|state| !state.task_id.starts_with(outgoing));
            self.retired = Some(outgoing.to_owned());
            self.frozen = Some(after);
        }
        Ok(())
    }
    pub(crate) fn commit(self) -> Result<()> {
        self.commit_with_publisher(|replacement| replacement.publish())
    }

    fn commit_with_publisher(
        mut self,
        mut publish_manifest: impl FnMut(&ManifestReplacement) -> Result<()>,
    ) -> Result<()> {
        let Some(context) = self.context.clone() else {
            let Some(replacement) = self.manifest_replacement.take() else {
                self.tx.take().expect("active transaction").commit()?;
                return Ok(());
            };
            let database = self.persistent_database_path()?;
            if self.publication_fence.is_none() {
                bail!("manifest publication requires the task database fence");
            }
            let publication = Publication {
                active: Vec::new(),
                retired: Vec::new(),
                manifest_replacement: Some(replacement),
            };
            self.execute(
                "INSERT INTO task_store_metadata VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![PENDING, serde_json::to_string(&publication)?],
            )?;
            self.tx.take().expect("active transaction").commit()?;
            publication
                .publish_with(&mut publish_manifest)
                .map_err(|error| {
                    anyhow::Error::new(CheckpointPending).context(error.to_string())
                })?;
            clear_pending(&database).map_err(|error| {
                anyhow::Error::new(CheckpointPending).context(error.to_string())
            })?;
            return Ok(());
        };
        let mut after = match self.frozen.take() {
            Some(after) => after,
            None => {
                self.ensure_coverage()?;
                states(&self, &context, &self.covered, true)?
            }
        };
        // Deleted tasks retain their genuine task-owned fence until milestone handoff.
        for prior in &self.before {
            if !after.iter().any(|state| state.task_id == prior.task_id)
                && !self
                    .retired
                    .as_ref()
                    .is_some_and(|prefix| prior.task_id.starts_with(prefix))
            {
                after.push(prior.clone().allocation_fence());
            }
        }
        let cohort = after
            .iter()
            .filter(|state| state.task.is_some())
            .map(|state| (state.workspace.clone(), state.reference()))
            .collect::<Vec<_>>();
        let cohort_id = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        for state in &mut after {
            state.cohort = if state.task.is_some() {
                cohort.clone()
            } else {
                Vec::new()
            };
            state.cohort_id = cohort_id.clone();
        }
        let retired = self
            .before
            .iter()
            .filter(|state| {
                self.retired
                    .as_ref()
                    .is_some_and(|prefix| state.task_id.starts_with(prefix))
            })
            .cloned()
            .map(ActiveCheckpoint::allocation_fence)
            .collect();
        let publication = Publication {
            active: after,
            retired,
            manifest_replacement: self.manifest_replacement.take(),
        };
        self.execute("INSERT INTO task_store_metadata VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![PENDING, serde_json::to_string(&publication)?])?;
        self.execute("INSERT INTO task_store_metadata VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![ROOT, context.root.to_string_lossy()])?;
        if let Err(error) = self.tx.take().expect("active transaction").commit() {
            for state in &self.before {
                state.publish()?;
            }
            return Err(error.into());
        }
        // No Drop compensation is legal after SQL COMMIT, even when Git publication fails.
        self.before.clear();
        publication
            .publish_with_known(&self.known_parents, &mut publish_manifest)
            .map_err(|error| anyhow::Error::new(CheckpointPending).context(error.to_string()))?;
        clear_pending(&context.database)
            .map_err(|error| anyhow::Error::new(CheckpointPending).context(error.to_string()))?;
        Ok(())
    }
}

fn clear_pending(database: &Path) -> Result<()> {
    let connection = Connection::open(database)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute("DELETE FROM task_store_metadata WHERE key=?1", [PENDING])?;
    Ok(())
}
impl Drop for ManagedTransaction<'_> {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            // Rollback only the uncommitted SQL transaction. Fence maxima remain allocated.
            if tx.rollback().is_ok() {
                for state in &self.before {
                    let _ = state.publish();
                }
            }
        }
    }
}

mod replay;
use replay::{states, task_home, validate_owner};
#[cfg(test)]
mod tests;
