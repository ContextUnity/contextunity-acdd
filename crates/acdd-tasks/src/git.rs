use anyhow::{bail, Context, Result};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output},
};

/// Resolve a worktree's Git directory locally when `.git` is present, falling back to `git rev-parse`.
pub fn git_dir(worktree: &Path) -> Result<PathBuf> {
    if !worktree.exists() {
        bail!(
            "TASK_GIT_WORKTREE_MISSING: worktree '{}' does not exist on disk",
            worktree.display()
        );
    }
    let dot_git = worktree.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.is_dir() => return Ok(dot_git.canonicalize()?),
        Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
            if dot_git.is_dir() {
                return Ok(dot_git.canonicalize()?);
            }
            if let Ok(text) = std::fs::read_to_string(&dot_git) {
                if let Some(target) = text.trim().strip_prefix("gitdir:") {
                    let raw = Path::new(target.trim());
                    let resolved = if raw.is_absolute() {
                        raw.to_path_buf()
                    } else {
                        worktree.join(raw)
                    };
                    if let Ok(canonical) = resolved.canonicalize() {
                        return Ok(canonical);
                    }
                }
            }
        }
        _ => {}
    }
    let resolved = git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
        "TASK_GIT_DIR_INVALID",
    )?;
    Ok(PathBuf::from(resolved).canonicalize()?)
}

/// Resolve the repository common `.git` object home locally when `.git` is present.
pub fn git_common_dir(worktree: &Path) -> Result<PathBuf> {
    if let Ok(dir) = git_dir(worktree) {
        let commondir = dir.join("commondir");
        if commondir.is_file() {
            if let Ok(rel) = std::fs::read_to_string(&commondir) {
                let raw = Path::new(rel.trim());
                let resolved = if raw.is_absolute() {
                    raw.to_path_buf()
                } else {
                    dir.join(raw)
                };
                if let Ok(canonical) = resolved.canonicalize() {
                    return Ok(canonical);
                }
            }
        }
        return Ok(dir);
    }
    let resolved = git_stdout(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "TASK_GIT_DIR_INVALID",
    )?;
    Ok(PathBuf::from(resolved).canonicalize()?)
}

/// Short, fail-closed participation in Git's own repository GC exclusion protocol.
/// Held only while constructing a kept pack, never across a SQLite transaction.
pub(crate) struct GitGcProtection {
    path: std::path::PathBuf,
    token: String,
}

impl GitGcProtection {
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        let common = git_common_dir(root)
            .map_err(|error| anyhow::anyhow!("TASK_OBJECT_PROTECTION_FAILED: {error}"))?;
        let path = common.join("gc.pid");
        let lock = common.join("gc.pid.lock");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)?;
        let acquired = (|| -> Result<String> {
            if path.exists() {
                bail!("TASK_OBJECT_PROTECTION_FAILED: Git GC already owns the repository");
            }
            static HOSTNAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let hostname = HOSTNAME.get_or_init(|| {
                let mut buffer = [0u8; 256];
                // SAFETY: buffer is writable for its full length.
                let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
                if result != 0 {
                    return "localhost".to_owned();
                }
                let end = buffer
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(buffer.len());
                String::from_utf8_lossy(&buffer[..end]).trim().to_owned()
            });
            let token = format!("{} {}", std::process::id(), hostname);
            file.write_all(token.as_bytes())?;
            std::fs::rename(&lock, &path)?;
            Ok(token)
        })();
        if acquired.is_err() {
            let _ = std::fs::remove_file(lock);
        }
        Ok(Self {
            path,
            token: acquired?,
        })
    }
}

impl Drop for GitGcProtection {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).ok().as_ref() == Some(&self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionalPackOwner {
    kind: String,
    task_id: String,
    object: String,
}

/// A genuine task's provisional object home, independent of logical gate authority.
pub(crate) struct KeptObjects {
    path: std::path::PathBuf,
    retain: bool,
}
impl KeptObjects {
    /// A committed SQL pending publication must retain its object home across process exit.
    pub(crate) fn retain_pending(&mut self) {
        self.retain = true;
    }
}
impl Drop for KeptObjects {
    fn drop(&mut self) {
        if !self.retain {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// GC exclusion must already cover object construction. Transfer protection to a
/// unique kept pack before releasing exclusion or returning commit-tree stdout.
pub(crate) fn keep_provisional(
    root: &Path,
    task_id: &str,
    object: &str,
    _gc: &GitGcProtection,
) -> Result<KeptObjects> {
    let directory = git_common_dir(root)
        .map_err(|error| anyhow::anyhow!("TASK_OBJECT_PROTECTION_FAILED: {error}"))?
        .join("objects/pack");
    std::fs::create_dir_all(&directory)?;
    // A distinct, genuine ownership blob prevents two concurrent guards for an
    // identical candidate from sharing (and prematurely releasing) a keep file.
    let owner = ProvisionalPackOwner {
        kind: "acdd-provisional-v1".into(),
        task_id: task_id.into(),
        object: object.into(),
    };
    let bytes = serde_json::to_vec(&owner)?;
    let nonce = format!(
        "{}:{}:{}",
        String::from_utf8(bytes.clone())?,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let blob = git_input(root, &["hash-object", "-w", "--stdin"], nonce.as_bytes())?;
    let prefix = directory.join("pack");
    let packed = git_input(
        root,
        &[
            "pack-objects",
            "--revs",
            prefix
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 object home"))?,
        ],
        format!("{object}\n{blob}\n").as_bytes(),
    )?;
    if !matches!(packed.len(), 40 | 64) || !packed.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("TASK_OBJECT_PROTECTION_FAILED: invalid pack identity");
    }
    let path = directory.join(format!("pack-{packed}.keep"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::File::open(&directory)?.sync_all()?;
    Ok(KeptObjects {
        path,
        retain: false,
    })
}

/// Release matching provisional packs for a batch of `(task_id, reference)` checkpoints
/// without invoking `git` at all when no `.keep` files match any task in the batch.
pub(crate) fn release_provisional_batch(root: &Path, items: &[(&str, &str)]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let directory = git_common_dir(root)
        .map_err(|error| anyhow::anyhow!("TASK_OBJECT_PROTECTION_FAILED: {error}"))?
        .join("objects/pack");
    if !directory.exists() {
        return Ok(());
    }
    let mut refs_by_task = std::collections::BTreeMap::<&str, &str>::new();
    for (task_id, reference) in items {
        refs_by_task.insert(*task_id, *reference);
    }
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "keep") {
            continue;
        }
        let Ok(owner) = serde_json::from_slice::<ProvisionalPackOwner>(&std::fs::read(&path)?)
        else {
            continue;
        };
        if owner.kind == "acdd-provisional-v1" {
            if let Some(&reference) = refs_by_task.get(owner.task_id.as_str()) {
                matches.push((path, owner.object, reference));
            }
        }
    }
    if matches.is_empty() {
        return Ok(());
    }
    let mut resolved = std::collections::BTreeMap::<&str, String>::new();
    for (path, object, reference) in matches {
        let keeper = if let Some(keeper) = resolved.get(reference) {
            keeper.clone()
        } else {
            let keeper = git_stdout(
                root,
                &["rev-parse", "--verify", reference],
                "TASK_CHECKPOINT_GIT_FAILED",
            )?;
            resolved.insert(reference, keeper.clone());
            keeper
        };
        if git_ancestor(root, &object, &keeper)? {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Batch-read blob contents for arbitrary `<rev>:<path>` specs in one `git cat-file --batch` call.
pub(crate) fn cat_file_batch(root: &Path, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>> {
    use std::process::Stdio;
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    for spec in specs {
        if spec.contains(['\n', '\r']) {
            bail!("TASK_CHECKPOINT_GIT_FAILED: invalid batch object spec");
        }
    }
    if !root.exists() {
        bail!(
            "TASK_GIT_WORKTREE_MISSING: worktree '{}' does not exist on disk",
            root.display()
        );
    }
    let mut input = String::with_capacity(specs.iter().map(|s| s.len() + 1).sum());
    for spec in specs {
        input.push_str(spec);
        input.push('\n');
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to execute git cat-file --batch in '{}'", root.display()))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    let write_res = writer.join().expect("git cat-file stdin writer panicked");
    write_res?;
    if !output.status.success() {
        bail!(
            "TASK_CHECKPOINT_GIT_FAILED: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = &output.stdout;
    let mut pos = 0usize;
    let mut results = Vec::with_capacity(specs.len());
    for spec in specs {
        let Some(rel_end) = stdout[pos..].iter().position(|&b| b == b'\n') else {
            bail!("TASK_CHECKPOINT_GIT_FAILED: truncated git cat-file --batch header");
        };
        let header = &stdout[pos..pos + rel_end];
        pos += rel_end + 1;
        if header.ends_with(b" missing") || header.ends_with(b" ambiguous") {
            results.push(None);
            continue;
        }
        let mut parts = header.split(|&b| b == b' ');
        let _oid = parts.next();
        let Some(kind) = parts.next() else {
            bail!("TASK_CHECKPOINT_GIT_FAILED: malformed git cat-file --batch header");
        };
        let Some(size_bytes) = parts.next() else {
            bail!("TASK_CHECKPOINT_GIT_FAILED: missing object size in git cat-file --batch");
        };
        let size: usize = std::str::from_utf8(size_bytes)
            .context("invalid git cat-file size")?
            .parse()
            .context("non-numeric git cat-file size")?;
        let data_end = pos
            .checked_add(size)
            .context("overflow in git cat-file size")?;
        if data_end + 1 > stdout.len() {
            bail!("TASK_CHECKPOINT_GIT_FAILED: truncated git cat-file --batch body");
        }
        if kind != b"blob" {
            bail!(
                "TASK_RECOVERY_CHECKPOINT_INVALID: expected blob for '{spec}', found {}",
                String::from_utf8_lossy(kind)
            );
        }
        let payload = stdout[pos..data_end].to_vec();
        pos = data_end + 1;
        results.push(Some(payload));
    }
    Ok(results)
}

/// Checkpoint payload entry for batched `git fast-import` publication.
pub(crate) struct GitCheckpointEntry<'a> {
    pub reference: &'a str,
    pub bytes: &'a [u8],
    pub parents: &'a [String],
}

/// Publish one or more task checkpoints to a single repository in one durable `git fast-import`.
pub(crate) fn git_checkpoints_batch(
    root: &Path,
    entries: &[GitCheckpointEntry<'_>],
) -> Result<()> {
    use std::process::Stdio;
    if entries.is_empty() {
        return Ok(());
    }
    if !root.is_absolute() {
        bail!("TASK_CHECKPOINT_HOME_INVALID: object home must be an absolute worktree");
    }
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let total_bytes: usize = entries.iter().map(|e| e.bytes.len() + 256).sum();
    let mut stream = Vec::with_capacity(total_bytes);
    for entry in entries {
        if entry.reference.contains(['\n', '\r'])
            || entry.parents.iter().any(|p| p.contains(['\n', '\r']))
        {
            bail!("TASK_CHECKPOINT_GIT_FAILED: invalid newline in fast-import metadata");
        }
        if entry.parents.is_empty() {
            writeln!(stream, "reset {}\n", entry.reference)?;
        }
        writeln!(stream, "commit {}", entry.reference)?;
        writeln!(
            stream,
            "committer ACDD checkpoint <checkpoint@acdd.invalid> {now_secs} +0000"
        )?;
        writeln!(stream, "data 16")?;
        stream.extend_from_slice(b"Task checkpoint\n");
        if let Some((first, rest)) = entry.parents.split_first() {
            writeln!(stream, "from {first}")?;
            for parent in rest {
                writeln!(stream, "merge {parent}")?;
            }
        }
        writeln!(stream, "deleteall")?;
        writeln!(stream, "M 100644 inline checkpoint.json")?;
        writeln!(stream, "data {}", entry.bytes.len())?;
        stream.extend_from_slice(entry.bytes);
        writeln!(stream, "\n")?;
    }
    writeln!(stream, "done")?;
    let mut child = Command::new("git")
        .args([
            "-c",
            "core.fsync=committed",
            "-c",
            "core.logAllRefUpdates=always",
            "fast-import",
            "--quiet",
            "--force",
        ])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_pid = child.id();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&stream));
    let output = child.wait_with_output()?;
    let write_res = writer.join().expect("git fast-import stdin writer panicked");
    if !output.status.success() {
        if let Ok(dir) = git_dir(root) {
            let _ = std::fs::remove_file(dir.join(format!("fast_import_crash_{child_pid}")));
        }
        bail!(
            "TASK_CHECKPOINT_GIT_FAILED: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    write_res?;
    Ok(())
}

/// Write a Git object using explicit bytes on stdin.
pub fn git_input(worktree: &Path, args: &[&str], bytes: &[u8]) -> Result<String> {
    use std::process::Stdio;
    let mut child = Command::new("git")
        .args(["-c", "core.fsync=committed"])
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped stdin").write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "TASK_CHECKPOINT_GIT_FAILED: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Publish a checkpoint blob as a commit whose parents retain the supplied objects.
/// The caller owns the ref and its publication fence; this function never creates global pins.
pub fn git_checkpoint(
    root: &Path,
    reference: &str,
    bytes: &[u8],
    parents: &[String],
) -> Result<String> {
    if !root.is_absolute() {
        bail!("TASK_CHECKPOINT_HOME_INVALID: object home must be an absolute worktree");
    }
    let blob = git_input(root, &["hash-object", "-w", "--stdin"], bytes)?;
    let tree = git_input(
        root,
        &["mktree"],
        format!("100644 blob {blob}\tcheckpoint.json\n").as_bytes(),
    )?;
    let mut args = vec![
        "-c",
        "user.name=ACDD checkpoint",
        "-c",
        "user.email=checkpoint@acdd.invalid",
        "commit-tree",
        &tree,
    ];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    let commit = git_input(root, &args, b"Task checkpoint\n")?;
    let previous = git_command(root, &["rev-parse", "--verify", reference])?;
    let previous = if previous.status.success() {
        String::from_utf8(previous.stdout)?.trim().to_owned()
    } else {
        "0".repeat(commit.len())
    };
    let output = git_command(
        root,
        &[
            "-c",
            "core.fsync=committed",
            "update-ref",
            "--create-reflog",
            reference,
            &commit,
            &previous,
        ],
    )?;
    if !output.status.success() {
        bail!(
            "TASK_CHECKPOINT_GIT_FAILED: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(commit)
}

/// Run a Git command in the given worktree and capture its output.
pub fn git_command(worktree: &Path, args: &[&str]) -> Result<Output> {
    if !worktree.exists() {
        bail!(
            "TASK_GIT_WORKTREE_MISSING: worktree '{}' does not exist on disk",
            worktree.display()
        );
    }
    Command::new("git")
        .args(args)
        .current_dir(worktree)
        .output()
        .with_context(|| {
            format!(
                "failed to execute git {:?} in '{}'",
                args,
                worktree.display()
            )
        })
}

/// Read non-empty UTF-8-lossy standard output from a successful Git command.
pub fn git_stdout(worktree: &Path, args: &[&str], error_code: &str) -> Result<String> {
    let output = git_command(worktree, args)?;
    if !output.status.success() {
        bail!("{error_code}: {}", String::from_utf8_lossy(&output.stderr));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        bail!("{error_code}: empty Git response");
    }
    Ok(value)
}

/// Return whether a Git command completed successfully.
pub fn git_status(worktree: &Path, args: &[&str]) -> Result<ExitStatus> {
    Ok(git_command(worktree, args)?.status)
}

/// Check whether a set of commit objects exist in the worktree's repository in a single `git cat-file --batch-check`.
pub fn git_commits_exist(
    worktree: &Path,
    candidates: &std::collections::BTreeSet<String>,
) -> Result<std::collections::BTreeSet<String>> {
    use std::process::Stdio;
    let candidates: std::collections::BTreeSet<_> = candidates
        .iter()
        .filter(|sha| !sha.contains(['\n', '\r']))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(std::collections::BTreeSet::new());
    }
    if !worktree.exists() {
        bail!(
            "TASK_GIT_WORKTREE_MISSING: worktree '{}' does not exist on disk",
            worktree.display()
        );
    }
    let args = ["cat-file", "--batch-check"];
    let mut child = Command::new("git")
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to execute git {:?} in '{}'",
                args,
                worktree.display()
            )
        })?;
    let input = candidates
        .iter()
        .map(|sha| format!("{sha}^{{commit}}\n"))
        .collect::<String>();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    let write_result = writer.join().expect("git stdin writer panicked");
    if !output.status.success() {
        return Ok(std::collections::BTreeSet::new());
    }
    write_result?;
    let lines: Vec<_> = output
        .stdout
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() != candidates.len() {
        bail!("TASK_CHECKPOINT_GIT_FAILED: incomplete cat-file response");
    }
    Ok(candidates
        .iter()
        .zip(lines)
        .filter_map(|(sha, line)| {
            line.split(|&byte| byte == b' ')
                .nth(1)
                .filter(|kind| *kind == b"commit")
                .map(|_| sha.clone())
        })
        .collect())
}

/// Check whether a commit object exists in the worktree's repository.
pub fn git_commit_exists(worktree: &Path, commit: &str) -> Result<bool> {
    let object = format!("{commit}^{{commit}}");
    Ok(git_status(worktree, &["cat-file", "-e", &object])?.success())
}

/// Validate a batch of resolved proof objects in one `git cat-file --batch-check`.
pub fn retain_proof_commits(
    worktree: &Path,
    commits: &std::collections::BTreeSet<String>,
) -> Result<()> {
    for commit in commits {
        if !matches!(commit.len(), 40 | 64)
            || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("TASK_PROOF_INVALID: expected a resolved proof object ID");
        }
    }
    let existing = git_commits_exist(worktree, commits)?;
    for commit in commits {
        if !existing.contains(commit) {
            bail!("TASK_PROOF_RETENTION_FAILED: proof object {commit} is unavailable");
        }
    }
    Ok(())
}

/// Validate a resolved proof object. Retention belongs to its task keeper.
pub fn retain_proof_commit(worktree: &Path, commit: &str) -> Result<()> {
    retain_proof_commits(worktree, &std::collections::BTreeSet::from([commit.to_owned()]))
}

/// Return the list of files modified, added, or deleted by a commit.
pub fn git_commit_changed_files(worktree: &Path, commit: &str) -> Result<Vec<String>> {
    let parents_raw = git_stdout(
        worktree,
        &["rev-list", "--parents", "-n", "1", commit],
        "TASK_COMMIT_INVALID",
    )?;
    let parents: Vec<&str> = parents_raw.split_whitespace().collect();
    let args = if parents.len() <= 1 {
        vec![
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            commit,
        ]
    } else if parents.len() == 2 {
        vec!["diff-tree", "--no-commit-id", "--name-only", "-r", commit]
    } else {
        vec![
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--first-parent",
            "-m",
            commit,
        ]
    };
    let output = git_command(worktree, &args)?;
    if !output.status.success() {
        bail!("TASK_COMMIT_INVALID: unable to inspect changed files for commit {commit}");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Check whether `ancestor` is an ancestor of `descendant` in the Git commit graph.
pub fn git_ancestor(worktree: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = git_command(
        worktree,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "TASK_ANCESTRY_FAILED: unable to determine commit ancestry between {ancestor} and {descendant}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

/// Check whether two commits differ in the tree for the given task scope paths.
/// If `scope` is empty, compares the full repository tree between the two commits.
pub fn git_scoped_tree_changed(
    worktree: &Path,
    commit_a: &str,
    commit_b: &str,
    scope: &[String],
) -> Result<bool> {
    let mut args = vec!["diff", "--quiet", commit_a, commit_b];
    if !scope.is_empty() {
        args.push("--");
        args.extend(scope.iter().map(String::as_str));
    }
    let output = git_command(worktree, &args)?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => bail!(
            "TASK_DIFF_FAILED: unable to compare task-scoped trees between {commit_a} and {commit_b}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

/// Return the list of files differing between two commits within the given scope.
pub fn git_scoped_differing_files(
    worktree: &Path,
    commit_a: &str,
    commit_b: &str,
    scope: &[String],
) -> Result<Vec<String>> {
    let mut args = vec!["diff", "--name-only", commit_a, commit_b];
    if !scope.is_empty() {
        args.push("--");
        args.extend(scope.iter().map(String::as_str));
    }
    let output = git_command(worktree, &args)?;
    if !output.status.success() {
        bail!(
            "TASK_DIFF_FAILED: unable to list differing files between {commit_a} and {commit_b}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

fn file_entry(worktree: &Path, commit: &str, path: &str) -> Result<Option<(String, String)>> {
    let output = git_command(
        worktree,
        &["--literal-pathspecs", "ls-tree", "-z", commit, "--", path],
    )?;
    if !output.status.success() {
        bail!("TASK_MERGE_INVALID: unable to inspect file entry");
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let header = output
        .stdout
        .split(|byte| *byte == b'\t')
        .next()
        .unwrap_or_default();
    let fields = std::str::from_utf8(header)?
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() != 3 || fields[1] != "blob" {
        bail!("TASK_MERGE_INVALID: expected a file entry");
    }
    Ok(Some((fields[0].into(), fields[2].into())))
}

/// Verify a file's clean three-way merge from committed inputs without changing the index.
pub fn git_clean_file_merge_matches(
    worktree: &Path,
    ours: &str,
    theirs: &str,
    result: &str,
    path: &str,
) -> Result<bool> {
    let bases = git_stdout(
        worktree,
        &["merge-base", "--all", ours, theirs],
        "TASK_MERGE_INVALID",
    )?;
    let bases = bases.lines().collect::<Vec<_>>();
    if bases.len() != 1 {
        return Ok(false);
    }
    let base = file_entry(worktree, bases[0], path)?;
    let ours = file_entry(worktree, ours, path)?;
    let theirs = file_entry(worktree, theirs, path)?;
    let result = file_entry(worktree, result, path)?;
    if ours == base {
        return Ok(result == theirs);
    }
    if theirs == base || ours == theirs {
        return Ok(result == ours);
    }
    let (Some(base), Some(ours), Some(theirs), Some(result)) = (base, ours, theirs, result) else {
        return Ok(false);
    };
    if !matches!(base.0.as_str(), "100644" | "100755")
        || base.0 != ours.0
        || base.0 != theirs.0
        || base.0 != result.0
    {
        return Ok(false);
    }
    struct MergeFiles(Vec<std::path::PathBuf>);
    impl Drop for MergeFiles {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let mut files = MergeFiles(Vec::new());
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    for (index, entry) in [&ours, &base, &theirs].iter().enumerate() {
        let blob = git_command(worktree, &["cat-file", "blob", &entry.1])?;
        if !blob.status.success() {
            bail!("TASK_MERGE_INVALID: file blob is unavailable");
        }
        let path = std::env::temp_dir().join(format!(
            "acdd-merge-{}-{nonce}-{index}",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        files.0.push(path);
        std::io::Write::write_all(&mut file, &blob.stdout)?;
    }
    let paths = files
        .0
        .iter()
        .map(|path| {
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("TASK_MERGE_INVALID: temporary path is not UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    let merged = git_command(
        worktree,
        &["merge-file", "-p", "--", paths[0], paths[1], paths[2]],
    )?;
    if !merged.status.success() {
        return Ok(false);
    }
    let expected = git_command(worktree, &["cat-file", "blob", &result.1])?;
    if !expected.status.success() {
        bail!("TASK_MERGE_INVALID: result blob is unavailable");
    }
    Ok(merged.stdout == expected.stdout)
}

/// Create a scoped commit on branch HEAD containing only the files in `scope`,
/// preserving unrelated worktree and index changes.
pub fn git_scoped_commit(worktree: &Path, scope: &[String], msg: &str) -> Result<String> {
    if git_dir(worktree).is_err() {
        bail!("TASK_PERSISTENCE_FAILED: scoped commit requires a Git worktree");
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_index = std::env::temp_dir().join(format!(
        "acdd-scoped-commit-{}-{nonce}.index",
        std::process::id()
    ));
    struct IndexCleanup(std::path::PathBuf);
    impl Drop for IndexCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = IndexCleanup(temp_index.clone());
    let temp_index_str = temp_index.to_string_lossy();

    // 1. Initialize temp index with current HEAD tree
    let read_tree = Command::new("git")
        .args(["read-tree", "HEAD"])
        .env("GIT_INDEX_FILE", &*temp_index_str)
        .current_dir(worktree)
        .output()
        .context("TASK_PERSISTENCE_FAILED: git read-tree failed")?;
    if !read_tree.status.success() {
        let empty_read = Command::new("git")
            .args(["read-tree", "--empty"])
            .env("GIT_INDEX_FILE", &*temp_index_str)
            .current_dir(worktree)
            .output()
            .context("TASK_PERSISTENCE_FAILED: git read-tree --empty failed")?;
        if !empty_read.status.success() {
            let stderr = String::from_utf8_lossy(&empty_read.stderr);
            bail!(
                "TASK_PERSISTENCE_FAILED: git read-tree failed: {}",
                stderr.trim()
            );
        }
    }

    // 2. Stage only paths in scope into temp index
    let mut valid_paths = Vec::new();
    for path in scope {
        if worktree.join(path).exists() {
            valid_paths.push(path.as_str());
        } else {
            let tracked = Command::new("git")
                .args(["ls-files", "--", path])
                .env("GIT_INDEX_FILE", &*temp_index_str)
                .current_dir(worktree)
                .output()
                .map(|out| out.status.success() && !out.stdout.is_empty())
                .unwrap_or(false);
            if tracked {
                valid_paths.push(path.as_str());
            }
        }
    }

    if !valid_paths.is_empty() {
        let mut cmd = Command::new("git");
        cmd.args(["add", "-A", "--"]);
        cmd.args(&valid_paths);
        cmd.env("GIT_INDEX_FILE", &*temp_index_str);
        cmd.current_dir(worktree);
        let out = cmd
            .output()
            .context("TASK_PERSISTENCE_FAILED: git add failed")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("TASK_PERSISTENCE_FAILED: git add failed: {}", stderr.trim());
        }
    }

    // 3. Commit from temp index (runs all git hooks!)
    let mut cmd = Command::new("git");
    cmd.args(["commit", "-m", msg, "--allow-empty"]);
    cmd.env("GIT_INDEX_FILE", &*temp_index_str);
    cmd.current_dir(worktree);
    let commit_res = cmd
        .output()
        .context("TASK_PERSISTENCE_FAILED: git commit failed")?;
    if !commit_res.status.success() {
        let stderr = String::from_utf8_lossy(&commit_res.stderr);
        bail!(
            "TASK_PERSISTENCE_FAILED: commit hook failed: {}",
            stderr.trim()
        );
    }

    let head = git_stdout(worktree, &["rev-parse", "HEAD"], "TASK_GIT_FAILED")?;
    let head = head.trim().to_owned();
    retain_proof_commit(worktree, &head)?;

    // Synchronize only the scoped committed paths in the worktree's default index with HEAD
    if !valid_paths.is_empty() {
        use std::io::Write;
        let mut child = Command::new("git")
            .args([
                "--literal-pathspecs",
                "reset",
                "--quiet",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
                &head,
                "--",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .current_dir(worktree)
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            for path in &valid_paths {
                stdin.write_all(path.as_bytes())?;
                stdin.write_all(b"\0")?;
            }
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "TASK_PERSISTENCE_FAILED: index synchronization failed: {}",
                stderr.trim()
            );
        }
    }

    Ok(head)
}
